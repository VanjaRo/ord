use super::*;
use anyhow::anyhow;
use ordinals::{AuthorityBits, AuthorityKind, CompactScript};
use std::collections::HashMap;

mod allocation;
mod authority;
mod cache;
mod executor;

pub(crate) use authority::AuthorityContextCache;
pub(super) use cache::ScriptCache;

use self::{allocation::Allocation, authority::Authority, executor::Executor};

pub(super) struct RuneUpdater<'a, 'tx, 'client> {
  pub(super) block_time: u32,
  pub(super) burned: HashMap<RuneId, Lot>,
  pub(super) client: &'client Client,
  pub(super) event_sender: Option<&'a mpsc::Sender<Event>>,
  pub(super) height: u32,
  pub(super) id_to_entry: &'a mut Table<'tx, RuneIdValue, RuneEntryValue>,
  pub(super) inscription_id_to_sequence_number: &'a Table<'tx, InscriptionIdValue, u32>,
  pub(super) minimum: Rune,
  pub(super) outpoint_to_balances: &'a mut Table<'tx, &'static OutPointValue, &'static [u8]>,
  pub(super) rune_to_id: &'a mut Table<'tx, u128, RuneIdValue>,
  pub(super) runes: u64,
  pub(super) sequence_number_to_rune_id: &'a mut Table<'tx, u32, RuneIdValue>,
  pub(super) statistic_to_count: &'a mut Table<'tx, u64, u64>,
  pub(super) transaction_id_to_rune: &'a mut Table<'tx, &'static TxidValue, u128>,
  pub(super) rune_id_to_authority_flags: &'a mut Table<'tx, RuneIdValue, u8>,
  pub(super) rune_id_to_authority_scripts: &'a mut Table<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_minters: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_blacklist: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_supply_extra: &'a mut Table<'tx, RuneIdValue, u128>,
  pub(super) script_cache: ScriptCache,
  pub(super) authority_cache: AuthorityContextCache,
}

impl RuneUpdater<'_, '_, '_> {
  pub(super) fn index_runes(&mut self, tx_index: u32, tx: &Transaction, txid: Txid) -> Result<()> {
    let artifact = Runestone::decipher(tx);

    let mut unallocated = {
      let mut authority = Authority::new(
        self.client,
        self.rune_id_to_authority_flags,
        self.rune_id_to_authority_scripts,
        self.rune_id_to_minters,
        self.rune_id_to_blacklist,
        &mut self.script_cache,
        self.rune_id_to_supply_extra,
        &mut self.authority_cache,
      );

      let mut allocation = Allocation::new(self.outpoint_to_balances);
      allocation.calculate_unallocated(tx, |id, outpoint| {
        if let Some(script_pubkey) = authority.script_cache.get_script_pubkey(
          authority.client,
          &outpoint.txid,
          outpoint.vout,
        )? {
          authority.is_blacklisted(id, script_pubkey.as_ref())
        } else {
          Ok(false)
        }
      })?
    };

    let mut allocated: Vec<HashMap<RuneId, Lot>> = vec![HashMap::new(); tx.output.len()];

    if let Some(artifact) = &artifact {
      self.process_mint(artifact, tx, txid, &mut unallocated)?;

      let etched = self.etched(tx_index, tx, artifact)?;

      self.process_premine(artifact, etched, &mut unallocated);

      if let Artifact::Runestone(runestone) = artifact {
        let authority = Authority::new(
          self.client,
          self.rune_id_to_authority_flags,
          self.rune_id_to_authority_scripts,
          self.rune_id_to_minters,
          self.rune_id_to_blacklist,
          &mut self.script_cache,
          self.rune_id_to_supply_extra,
          &mut self.authority_cache,
        );

        let mut executor = Executor::new(authority);
        executor.process_runestone(tx, runestone, etched, &mut unallocated, &mut allocated)?;
      }

      if let Some((id, rune)) = etched {
        self.create_rune_entry(txid, artifact, id, rune, tx)?;
      }
    }

    let burned =
      self.process_cenotaph_and_balances(&artifact, unallocated, &mut allocated, tx, txid)?;

    self.update_burned(burned, txid)?;

    Ok(())
  }

  fn process_mint(
    &mut self,
    artifact: &Artifact,
    _tx: &Transaction,
    txid: Txid,
    unallocated: &mut HashMap<RuneId, Lot>,
  ) -> Result<()> {
    if let Some(id) = artifact.mint()
      && let Some(amount) = self.mint(id)?
    {
      *unallocated.entry(id).or_default() += amount;

      if let Some(sender) = self.event_sender {
        sender.blocking_send(Event::RuneMinted {
          block_height: self.height,
          txid,
          rune_id: id,
          amount: amount.n(),
        })?;
      }
    }
    Ok(())
  }

  fn process_premine(
    &mut self,
    artifact: &Artifact,
    etched: Option<(RuneId, Rune)>,
    unallocated: &mut HashMap<RuneId, Lot>,
  ) {
    if let Artifact::Runestone(runestone) = artifact
      && let Some((id, ..)) = etched
    {
      *unallocated.entry(id).or_default() += runestone.etching.unwrap().premine.unwrap_or_default();
    }
  }

  fn process_cenotaph_and_balances(
    &mut self,
    artifact: &Option<Artifact>,
    unallocated: HashMap<RuneId, Lot>,
    allocated: &mut [HashMap<RuneId, Lot>],
    tx: &Transaction,
    txid: Txid,
  ) -> Result<HashMap<RuneId, Lot>> {
    // Build an authority helper to check blacklist for default allocations.
    let mut authority = Authority::new(
      self.client,
      self.rune_id_to_authority_flags,
      self.rune_id_to_authority_scripts,
      self.rune_id_to_minters,
      self.rune_id_to_blacklist,
      &mut self.script_cache,
      self.rune_id_to_supply_extra,
      &mut self.authority_cache,
    );

    let mut burned: HashMap<RuneId, Lot> = HashMap::new();

    if let Some(Artifact::Cenotaph(_)) = artifact {
      for (id, balance) in unallocated {
        *burned.entry(id).or_default() += balance;
      }
    } else {
      let pointer = artifact
        .as_ref()
        .map(|artifact| match artifact {
          Artifact::Runestone(runestone) => runestone.pointer,
          Artifact::Cenotaph(_) => unreachable!(),
        })
        .unwrap_or_default();

      // assign all un-allocated runes to the default output, or the first non
      // OP_RETURN output if there is no default
      if let Some(vout) = pointer
        .map(|pointer| pointer.into_usize())
        .filter(|&pointer| pointer < allocated.len())
        .or_else(|| {
          tx.output
            .iter()
            .enumerate()
            .find(|(_vout, tx_out)| !tx_out.script_pubkey.is_op_return())
            .map(|(vout, _tx_out)| vout)
        })
      {
        for (id, balance) in unallocated {
          if balance == 0 {
            continue;
          }

          // If the chosen vout is blacklisted, keep balance with sender (no burn, no credit).
          let dest_script = &tx.output[vout].script_pubkey;
          if authority.is_blacklisted(id, dest_script)? {
            log::info!(
              "Default allocation for {:?} blocked by blacklist; keeping with sender (tx={})",
              id,
              txid
            );
          } else {
            *allocated[vout].entry(id).or_default() += balance;
          }
        }
      } else {
        for (id, balance) in unallocated {
          if balance > 0 {
            *burned.entry(id).or_default() += balance;
          }
        }
      }
    }

    // update outpoint balances
    let mut buffer: Vec<u8> = Vec::new();
    for (vout, balances) in allocated.iter_mut().enumerate() {
      if balances.is_empty() {
        continue;
      }

      // increment burned balances
      if tx.output[vout].script_pubkey.is_op_return() {
        for (id, balance) in balances.iter() {
          *burned.entry(*id).or_default() += *balance;
        }
        continue;
      }

      buffer.clear();

      let mut balances = balances.drain().collect::<Vec<(RuneId, Lot)>>();

      // Sort balances by id so tests can assert balances in a fixed order
      balances.sort();

      let outpoint = OutPoint {
        txid,
        vout: vout.try_into().unwrap(),
      };

      for (id, balance) in balances {
        Index::encode_rune_balance(id, balance.n(), &mut buffer);

        if let Some(sender) = self.event_sender {
          sender.blocking_send(Event::RuneTransferred {
            outpoint,
            block_height: self.height,
            txid,
            rune_id: id,
            amount: balance.0,
          })?;
        }
      }

      self
        .outpoint_to_balances
        .insert(&outpoint.store(), buffer.as_slice())?;
    }

    Ok(burned)
  }

  fn update_burned(&mut self, burned: HashMap<RuneId, Lot>, txid: Txid) -> Result<()> {
    for (id, amount) in burned {
      *self.burned.entry(id).or_default() += amount;

      if let Some(sender) = self.event_sender {
        sender.blocking_send(Event::RuneBurned {
          block_height: self.height,
          txid,
          rune_id: id,
          amount: amount.n(),
        })?;
      }
    }
    Ok(())
  }

  pub(super) fn update(self) -> Result {
    for (rune_id, burned) in self.burned {
      let mut entry = RuneEntry::load(self.id_to_entry.get(&rune_id.store())?.unwrap().value());
      entry.burned = entry.burned.checked_add(burned.n()).unwrap();
      self.id_to_entry.insert(&rune_id.store(), entry.store())?;
    }

    Ok(())
  }

  fn create_rune_entry(
    &mut self,
    txid: Txid,
    artifact: &Artifact,
    id: RuneId,
    rune: Rune,
    tx: &Transaction,
  ) -> Result {
    self.rune_to_id.insert(rune.store(), id.store())?;
    self
      .transaction_id_to_rune
      .insert(&txid.store(), rune.store())?;

    let number = self.runes;
    self.runes += 1;

    self
      .statistic_to_count
      .insert(&Statistic::Runes.into(), self.runes)?;

    let entry = match artifact {
      Artifact::Cenotaph(_) => RuneEntry {
        block: id.block,
        burned: 0,
        divisibility: 0,
        etching: txid,
        terms: None,
        mints: 0,
        number,
        premine: 0,
        spaced_rune: SpacedRune { rune, spacers: 0 },
        symbol: None,
        timestamp: self.block_time.into(),
        turbo: false,
      },
      Artifact::Runestone(Runestone { etching, .. }) => {
        let Etching {
          divisibility,
          terms,
          premine,
          spacers,
          symbol,
          turbo,
          ..
        } = etching.unwrap();

        let allow_minting = terms.map(|t| t.allow_minting).unwrap_or(false);
        let allow_blacklisting = terms.map(|t| t.allow_blacklisting).unwrap_or(false);

        let mut flags = AuthorityBits::empty().extend(AuthorityKind::Master);
        if allow_minting {
          flags.insert(AuthorityKind::Mint);
        }

        if allow_blacklisting {
          flags.insert(AuthorityKind::Blacklist);
        }

        // Capture the etcher's script (prevout of the commitment input) and seed all authorities to it.
        let authority_script = tx.input.first().and_then(|input| {
          self
            .script_cache
            .get_script_pubkey(
              self.client,
              &input.previous_output.txid,
              input.previous_output.vout,
            )
            .transpose()
        });

        self
          .rune_id_to_authority_flags
          .insert(id.store(), flags.bits())?;

        if let Some(Ok(script_pubkey)) = authority_script {
          if let Some(compact) = CompactScript::try_from_script(script_pubkey.as_ref()) {
            let scripts_blob = Self::build_initial_authority_scripts_blob(&compact)?;

            self
              .rune_id_to_authority_scripts
              .insert(id.store(), scripts_blob.as_slice())?;
          } else {
            log::warn!(
              "Skipping authority capture for {:?}: unsupported script {:?}",
              id,
              script_pubkey
            );
          }
        }

        RuneEntry {
          block: id.block,
          burned: 0,
          divisibility: divisibility.unwrap_or_default(),
          etching: txid,
          terms,
          mints: 0,
          number,
          premine: premine.unwrap_or_default(),
          spaced_rune: SpacedRune {
            rune,
            spacers: spacers.unwrap_or_default(),
          },
          symbol,
          timestamp: self.block_time.into(),
          turbo,
        }
      }
    };

    self.id_to_entry.insert(id.store(), entry.store())?;

    if let Some(sender) = self.event_sender {
      sender.blocking_send(Event::RuneEtched {
        block_height: self.height,
        txid,
        rune_id: id,
      })?;
    }

    let inscription_id = InscriptionId { txid, index: 0 };

    if let Some(sequence_number) = self
      .inscription_id_to_sequence_number
      .get(&inscription_id.store())?
    {
      self
        .sequence_number_to_rune_id
        .insert(sequence_number.value(), id.store())?;
    }

    Ok(())
  }

  fn build_initial_authority_scripts_blob(compact: &CompactScript) -> Result<Vec<u8>> {
    let presence = AuthorityBits::from(
      AuthorityKind::Mint.mask() | AuthorityKind::Blacklist.mask() | AuthorityKind::Master.mask(),
    );
    let compact_body_len = u8::try_from(compact.body.len())
      .map_err(|_| anyhow!("compact script body length exceeds u8"))?;

    let mut scripts_blob = Vec::new();
    scripts_blob.push(presence.bits());

    for _kind in [
      AuthorityKind::Mint,
      AuthorityKind::Blacklist,
      AuthorityKind::Master,
    ] {
      scripts_blob.push(compact.kind as u8);
      scripts_blob.push(compact_body_len);
      scripts_blob.extend(&compact.body);
    }

    Ok(scripts_blob)
  }

  fn etched(
    &mut self,
    tx_index: u32,
    tx: &Transaction,
    artifact: &Artifact,
  ) -> Result<Option<(RuneId, Rune)>> {
    let rune = match artifact {
      Artifact::Runestone(runestone) => match runestone.etching {
        Some(etching) => etching.rune,
        None => return Ok(None),
      },
      Artifact::Cenotaph(cenotaph) => match cenotaph.etching {
        Some(rune) => Some(rune),
        None => return Ok(None),
      },
    };

    let rune = if let Some(rune) = rune {
      let too_low = rune < self.minimum;
      let reserved = rune.is_reserved();
      let already = self.rune_to_id.get(rune.0)?.is_some();
      let commits = self.tx_commits_to_rune(tx, rune)?;

      if too_low || reserved || already || !commits {
        return Ok(None);
      }
      rune
    } else {
      let reserved_runes = self
        .statistic_to_count
        .get(&Statistic::ReservedRunes.into())?
        .map(|entry| entry.value())
        .unwrap_or_default();

      self
        .statistic_to_count
        .insert(&Statistic::ReservedRunes.into(), reserved_runes + 1)?;

      Rune::reserved(self.height.into(), tx_index)
    };

    Ok(Some((
      RuneId {
        block: self.height.into(),
        tx: tx_index,
      },
      rune,
    )))
  }

  fn mint(&mut self, id: RuneId) -> Result<Option<Lot>> {
    let Some(entry) = self.id_to_entry.get(&id.store())? else {
      return Ok(None);
    };

    let mut rune_entry = RuneEntry::load(entry.value());

    let Ok(amount) = rune_entry.mintable(self.height.into()) else {
      return Ok(None);
    };

    drop(entry);

    rune_entry.mints += 1;

    self.id_to_entry.insert(&id.store(), rune_entry.store())?;

    Ok(Some(Lot(amount)))
  }

  fn tx_commits_to_rune(&mut self, tx: &Transaction, rune: Rune) -> Result<bool> {
    let commitment = rune.commitment();

    for input in &tx.input {
      // extracting a tapscript does not indicate that the input being spent
      // was actually a taproot output. this is checked below, when we load the
      // output's entry from the database
      let Some(tapscript) = unversioned_leaf_script_from_witness(&input.witness) else {
        continue;
      };

      for instruction in tapscript.instructions() {
        // ignore errors, since the extracted script may not be valid
        let Ok(instruction) = instruction else { break };

        let Some(pushbytes) = instruction.push_bytes() else {
          continue;
        };

        if pushbytes.as_bytes() != commitment {
          continue;
        }

        let Some(script_pubkey) = self.script_cache.get_script_pubkey(
          self.client,
          &input.previous_output.txid,
          input.previous_output.vout,
        )?
        else {
          panic!(
            "can't get input transaction: {}",
            input.previous_output.txid
          );
        };

        let taproot = script_pubkey.as_ref().is_p2tr();

        if !taproot {
          continue;
        }

        // Need full tx_info for blockhash
        let Some(tx_info) = self
          .client
          .get_raw_transaction_info(&input.previous_output.txid, None)
          .into_option()?
        else {
          continue;
        };

        let commit_tx_height = self
          .client
          .get_block_header_info(&tx_info.blockhash.unwrap())
          .into_option()?
          .unwrap()
          .height;

        let confirmations = self
          .height
          .checked_sub(commit_tx_height.try_into().unwrap())
          .unwrap()
          + 1;

        if confirmations >= u32::from(Runestone::COMMIT_CONFIRMATIONS) {
          return Ok(true);
        }
      }
    }

    Ok(false)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use anyhow::Result;
  use ordinals::CompactScriptKind;

  #[test]
  fn initial_authority_blob_sets_all_authorities() -> Result<()> {
    let body = vec![0xAB; 32];
    let compact = CompactScript {
      kind: CompactScriptKind::P2TR,
      body: body.clone(),
    };

    let blob = RuneUpdater::build_initial_authority_scripts_blob(&compact)?;
    let presence = AuthorityBits::from(
      AuthorityKind::Mint.mask() | AuthorityKind::Blacklist.mask() | AuthorityKind::Master.mask(),
    );

    assert_eq!(blob.first().copied(), Some(presence.bits()));

    let mut offset = 1;
    for _ in 0..3 {
      assert_eq!(blob[offset], compact.kind as u8);
      let len = blob[offset + 1] as usize;
      assert_eq!(len, body.len());
      assert_eq!(&blob[offset + 2..offset + 2 + len], &body);
      offset += 2 + len;
    }

    assert_eq!(offset, blob.len());
    Ok(())
  }
}

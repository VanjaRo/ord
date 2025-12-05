use super::{authority::Authority, *};
use anyhow::anyhow;
use ordinals::{AuthorityBits, AuthorityKind, CompactScript, CompactScriptKind};
use std::collections::HashSet;

pub(super) struct Executor<'a, 'tx, 'client> {
  pub(super) authority: Authority<'a, 'tx, 'client>,
}

impl<'a, 'tx, 'client> Executor<'a, 'tx, 'client> {
  pub(super) fn new(authority: Authority<'a, 'tx, 'client>) -> Self {
    Self { authority }
  }

  pub(super) fn process_runestone(
    &mut self,
    tx: &Transaction,
    runestone: &Runestone,
    etched: Option<(RuneId, Rune)>,
    unallocated: &mut HashMap<RuneId, Lot>,
    allocated: &mut [HashMap<RuneId, Lot>],
  ) -> Result<()> {
    // 1. SetAuthority
    if let Some(set_authority) = &runestone.set_authority {
      // Determine which rune this applies to:
      // - Prefer the rune id from the first edict
      let target_rune_id = runestone
        .edicts
        .first()
        .and_then(|edict| {
          if edict.id == RuneId::default() {
            etched.map(|(id, _)| id)
          } else {
            Some(edict.id)
          }
        })
        .or(runestone.mint)
        .or_else(|| etched.map(|(id, _)| id));

      if let Some(target_rune_id) = target_rune_id {
        self.process_set_authority(tx, set_authority, target_rune_id)?;
      }
    }

    // 2. Authority Updates (Minter/Blacklist)
    if let Some(authority_updates) = &runestone.authority {
      let target_rune_id = runestone
        .mint
        .or_else(|| etched.map(|(id, _)| id))
        .ok_or_else(|| anyhow!("No target rune for authority update"))
        .ok();

      if let Some(target_rune_id) = target_rune_id {
        self.process_authority_updates(tx, authority_updates, target_rune_id)?;
      }
    }

    // 3. Edicts
    self.process_edicts(tx, runestone, etched, unallocated, allocated)?;

    Ok(())
  }

  fn process_set_authority(
    &mut self,
    tx: &Transaction,
    set_authority: &ordinals::SetAuthority,
    target_rune_id: RuneId,
  ) -> Result<()> {
    let mut authorities = set_authority.authorities;

    if authorities.contains(AuthorityKind::Blacklist) && !self.has_blacklist_flag(target_rune_id)? {
      log::debug!(
        "Ignoring blacklist authority update for {:?}: allow_blacklisting not enabled",
        target_rune_id
      );
      authorities = AuthorityBits::from(authorities.bits() & !AuthorityKind::Blacklist.mask());
    }

    if authorities.is_empty() {
      return Ok(());
    }

    let mut authorized = true;
    for kind in authorities.kinds() {
      authorized &= self.authority.check_authority(tx, target_rune_id, kind)?;
    }

    if authorized {
      let default_kind = self
        .authority
        .get_authority_script(target_rune_id, AuthorityKind::Master)?
        .map(|script| script.kind)
        .unwrap_or(CompactScriptKind::P2TR);

      let compact = CompactScript {
        kind: default_kind,
        body: set_authority.script_pubkey_compact.clone(),
      };
      let compact_body_len = u8::try_from(compact.body.len())
        .map_err(|_| anyhow!("compact script body length exceeds u8"))?;

      let mut flags = self
        .authority
        .rune_id_to_authority_flags
        .get(&target_rune_id.store())?
        .map(|e| AuthorityBits::from(e.value()))
        .unwrap_or_else(AuthorityBits::empty);

      for kind in authorities.kinds() {
        flags.insert(kind);
      }

      self
        .authority
        .rune_id_to_authority_flags
        .insert(target_rune_id.store(), flags.bits())?;

      let existing_blob = self
        .authority
        .rune_id_to_authority_scripts
        .get(&target_rune_id.store())?
        .map(|e| e.value().to_vec())
        .unwrap_or_else(|| vec![0]);

      let existing_presence = AuthorityBits::from(existing_blob.first().copied().unwrap_or(0));
      let mut presence = existing_presence;
      for kind in authorities.kinds() {
        presence.insert(kind);
      }

      let scripts_blob = Self::merge_authority_scripts(
        &authorities,
        &existing_blob,
        existing_presence,
        presence,
        &compact,
        compact_body_len,
      )?;

      self
        .authority
        .rune_id_to_authority_scripts
        .insert(target_rune_id.store(), scripts_blob.as_slice())?;

      self.authority.context_cache.invalidate(target_rune_id);
    }
    Ok(())
  }

  fn merge_authority_scripts(
    authorities: &AuthorityBits,
    existing_blob: &[u8],
    existing_presence: AuthorityBits,
    presence: AuthorityBits,
    compact: &CompactScript,
    compact_body_len: u8,
  ) -> Result<Vec<u8>> {
    let mut scripts_blob = Vec::new();
    scripts_blob.push(presence.bits());

    let mut offset = 1;
    let append_script =
      |kind: AuthorityKind, scripts_blob: &mut Vec<u8>, offset: &mut usize| -> Result<()> {
        if !presence.contains(kind) {
          return Ok(());
        }

        if authorities.contains(kind) {
          // Write updated script and only advance offset when replacing an existing one.
          scripts_blob.push(compact.kind as u8);
          scripts_blob.push(compact_body_len);
          scripts_blob.extend(&compact.body);

          if existing_presence.contains(kind) && *offset + 1 < existing_blob.len() {
            *offset += 2 + existing_blob[*offset + 1] as usize;
          }
        } else if existing_presence.contains(kind) && *offset + 1 < existing_blob.len() {
          // Reuse existing script if present, advancing offset only when we read it.
          let body_len = existing_blob[*offset + 1] as usize;
          if *offset + 2 + body_len <= existing_blob.len() {
            scripts_blob.extend(&existing_blob[*offset..*offset + 2 + body_len]);
            *offset += 2 + body_len;
          }
        }

        Ok(())
      };

    append_script(AuthorityKind::Mint, &mut scripts_blob, &mut offset)?;
    append_script(AuthorityKind::Blacklist, &mut scripts_blob, &mut offset)?;
    append_script(AuthorityKind::Master, &mut scripts_blob, &mut offset)?;

    Ok(scripts_blob)
  }

  fn process_authority_updates(
    &mut self,
    tx: &Transaction,
    authority_updates: &ordinals::AuthorityUpdates,
    target_rune_id: RuneId,
  ) -> Result<()> {
    let mut changed = false;
    let master_updates_present =
      authority_updates.add_minter.is_some() || authority_updates.remove_minter.is_some();

    if master_updates_present
      && self
        .authority
        .check_authority(tx, target_rune_id, AuthorityKind::Master)?
    {
      changed |= Self::apply_entries(authority_updates.add_minter.as_deref(), |entry| {
        self
          .authority
          .rune_id_to_minters
          .insert(target_rune_id.store(), entry)?;
        Ok(true)
      })?;

      changed |= Self::apply_entries(authority_updates.remove_minter.as_deref(), |entry| {
        self
          .authority
          .rune_id_to_minters
          .remove(target_rune_id.store(), entry)?;
        Ok(true)
      })?;
    }

    let has_blacklist_requests =
      authority_updates.blacklist.is_some() || authority_updates.unblacklist.is_some();
    let allow_blacklisting = self.has_blacklist_flag(target_rune_id)?;
    let blacklist_authorized = if allow_blacklisting && has_blacklist_requests {
      self
        .authority
        .check_authority(tx, target_rune_id, AuthorityKind::Blacklist)?
    } else {
      false
    };

    if let Some(blacklist) = authority_updates.blacklist.as_deref() {
      if !allow_blacklisting {
        log::debug!(
          "Ignoring blacklist additions for {:?}: allow_blacklisting not enabled",
          target_rune_id
        );
      } else if blacklist_authorized {
        // Track seen entries in the current blacklist array to prevent duplicates
        let mut seen_entries: HashSet<Vec<u8>> = HashSet::new();

        changed |= Self::apply_entries(Some(blacklist), |entry| {
          let entry_owned = entry.to_vec();

          if seen_entries.contains(&entry_owned) {
            log::debug!(
              "Skipping duplicate blacklist entry for {:?}: {:?}",
              target_rune_id,
              entry
            );
            return Ok(false);
          }

          // Decode entry to check if it's already blacklisted
          if let Some(script) = self.authority.decode_entry_to_script(entry, target_rune_id) {
            if self.authority.is_blacklisted(target_rune_id, &script)? {
              log::debug!(
                "Skipping already blacklisted entry for {:?}: {:?}",
                target_rune_id,
                entry
              );
              return Ok(false);
            }
          } else {
            // Invalid entry format, skip it
            return Ok(false);
          }

          // Entry is valid, not duplicate, and not already blacklisted
          seen_entries.insert(entry_owned);
          self
            .authority
            .rune_id_to_blacklist
            .insert(target_rune_id.store(), entry)?;
          Ok(true)
        })?;
      }
    }

    if let Some(unblacklist) = authority_updates.unblacklist.as_deref() {
      if !allow_blacklisting {
        log::debug!(
          "Ignoring unblacklist operations for {:?}: allow_blacklisting not enabled",
          target_rune_id
        );
      } else if blacklist_authorized {
        changed |= Self::apply_entries(Some(unblacklist), |entry| {
          self
            .authority
            .rune_id_to_blacklist
            .remove(target_rune_id.store(), entry)?;
          Ok(true)
        })?;
      }
    }

    if changed {
      self.authority.context_cache.invalidate(target_rune_id);
    }
    Ok(())
  }

  fn has_blacklist_flag(&mut self, id: RuneId) -> Result<bool> {
    Ok(
      self
        .authority
        .get_context(id)?
        .flags
        .contains(AuthorityKind::Blacklist),
    )
  }

  fn has_mint_flag(&mut self, id: RuneId) -> Result<bool> {
    Ok(
      self
        .authority
        .get_context(id)?
        .flags
        .contains(AuthorityKind::Mint),
    )
  }

  fn apply_entries<F>(entries: Option<&[Vec<u8>]>, mut op: F) -> Result<bool>
  where
    F: FnMut(&[u8]) -> Result<bool>,
  {
    let mut changed = false;
    if let Some(entries) = entries {
      for entry in entries {
        if entry.is_empty() {
          continue;
        }

        if op(entry.as_slice())? {
          changed = true;
        }
      }
    }
    Ok(changed)
  }

  fn process_edicts(
    &mut self,
    tx: &Transaction,
    runestone: &Runestone,
    etched: Option<(RuneId, Rune)>,
    unallocated: &mut HashMap<RuneId, Lot>,
    allocated: &mut [HashMap<RuneId, Lot>],
  ) -> Result<()> {
    for Edict { id, amount, output } in runestone.edicts.iter().copied() {
      let amount = Lot(amount);
      let output = usize::try_from(output).unwrap_or(usize::MAX);

      if output > tx.output.len() {
        continue;
      }

      let id = if id == RuneId::default() {
        if let Some((id, ..)) = etched {
          id
        } else {
          continue;
        }
      } else {
        id
      };

      let balance = unallocated.entry(id).or_default();

      // Authority minting check
      let mut allow_mint_beyond_balance = false;
      let has_mint_flag = self.has_mint_flag(id)?;

      if has_mint_flag {
        let check_auth = self
          .authority
          .check_authority(tx, id, AuthorityKind::Mint)?;
        let check_minter = self.authority.check_is_minter(tx, id)?;

        log::debug!(
          "Mint authorization for {:?}: authority_match={}, delegated={}",
          id,
          check_auth,
          check_minter
        );

        if check_auth || check_minter {
          allow_mint_beyond_balance = true;
        }
      }

      if allow_mint_beyond_balance && amount > *balance {
        let delta = amount - *balance;
        *balance += delta;
        let current_extra = self.authority.get_supply_extra(id)?;
        let new_extra = current_extra + delta.n();
        self.authority.set_supply_extra(id, new_extra)?;

        log::info!(
          "Authority mint for {:?}: minted {} beyond balance, supply_extra now {}",
          id,
          delta.n(),
          new_extra
        );
      }

      let mut allocate = |balance: &mut Lot, amount: Lot, output: usize| -> Result<()> {
        if amount > 0 {
          // Check blacklist
          let script_pubkey = &tx.output[output].script_pubkey;
          if self.authority.is_blacklisted(id, script_pubkey)? {
            // Reject the edict and keep balance with the sender
            return Ok(());
          }

          *balance -= amount;
          *allocated[output].entry(id).or_default() += amount;
        }
        Ok(())
      };

      if output == tx.output.len() {
        // Distribute to all non-OP_RETURN outputs
        let destinations: Vec<usize> = tx
          .output
          .iter()
          .enumerate()
          .filter(|(_, tx_out)| !tx_out.script_pubkey.is_op_return())
          .map(|(i, _)| i)
          .collect();

        if !destinations.is_empty() {
          if amount == 0 {
            let amount_per_out = *balance / destinations.len() as u128;
            let remainder = (*balance % destinations.len() as u128).n() as usize;

            for (i, out_idx) in destinations.iter().enumerate() {
              let amt = if i < remainder {
                amount_per_out + 1
              } else {
                amount_per_out
              };
              allocate(balance, amt, *out_idx)?;
            }
          } else {
            for out_idx in destinations {
              allocate(balance, amount.min(*balance), out_idx)?;
            }
          }
        }
      } else {
        let amt = if amount == 0 {
          *balance
        } else {
          amount.min(*balance)
        };
        allocate(balance, amt, output)?;
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use anyhow::Result;

  #[test]
  fn merge_authority_scripts_preserves_existing_order_after_update() -> Result<()> {
    // Existing blob has Mint and Blacklist; only Mint is being updated.
    let mint_body_old = vec![0x11; 20];
    let blacklist_body = vec![0x22; 32];

    let existing_presence =
      AuthorityBits::from(AuthorityKind::Mint.mask() | AuthorityKind::Blacklist.mask());

    let mut existing_blob = Vec::new();
    existing_blob.push(existing_presence.bits());
    existing_blob.push(CompactScriptKind::P2WPKH as u8);
    let mint_len = u8::try_from(mint_body_old.len()).expect("compact script body fits in u8");
    existing_blob.push(mint_len);
    existing_blob.extend(&mint_body_old);
    existing_blob.push(CompactScriptKind::P2TR as u8);
    let blacklist_len = u8::try_from(blacklist_body.len()).expect("compact script body fits in u8");
    existing_blob.push(blacklist_len);
    existing_blob.extend(&blacklist_body);

    let authorities = AuthorityBits::from(AuthorityKind::Mint.mask());
    let mut presence = existing_presence;
    for kind in authorities.kinds() {
      presence.insert(kind);
    }

    let new_mint_body = vec![0x33; 32];
    let compact = CompactScript {
      kind: CompactScriptKind::P2WSH,
      body: new_mint_body.clone(),
    };
    let compact_body_len = u8::try_from(compact.body.len()).unwrap();

    let merged = Executor::merge_authority_scripts(
      &authorities,
      &existing_blob,
      existing_presence,
      presence,
      &compact,
      compact_body_len,
    )?;

    let mut expected = Vec::new();
    expected.push(presence.bits());
    expected.push(compact.kind as u8);
    expected.push(compact_body_len);
    expected.extend(&new_mint_body);
    expected.push(CompactScriptKind::P2TR as u8);
    expected.push(blacklist_len);
    expected.extend(&blacklist_body);

    assert_eq!(merged, expected);
    Ok(())
  }
}

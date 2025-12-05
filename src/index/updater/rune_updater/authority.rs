use super::{cache::ScriptCache, *};
use bitcoin::ScriptBuf;
use ordinals::{AuthorityBits, AuthorityKind, CompactScript, CompactScriptKind};
use std::{
  collections::{HashMap, VecDeque},
  hash::{Hash, Hasher},
  sync::Arc,
};

const AUTHORITY_INPUT_LIMIT: usize = 10;

#[derive(Clone)]
pub(super) struct CachedScript {
  compact: CompactScript,
  script: Option<Arc<ScriptBuf>>,
}

impl CachedScript {
  fn new(compact: CompactScript) -> Self {
    let script = compact.to_script().map(Arc::new);
    Self { compact, script }
  }

  fn script(&self) -> Option<&ScriptBuf> {
    self.script.as_deref()
  }

  fn size_bytes(&self) -> usize {
    let script_len = self
      .script
      .as_ref()
      .map(|s| s.as_bytes().len())
      .unwrap_or(0);
    // Include compact body and a small overhead for bookkeeping
    self.compact.body.len() + script_len + 16
  }
}

#[derive(Default)]
pub(super) struct AuthorityScripts {
  mint: Option<CachedScript>,
  blacklist: Option<CachedScript>,
  master: Option<CachedScript>,
}

impl AuthorityScripts {
  fn get(&self, kind: AuthorityKind) -> Option<&CachedScript> {
    match kind {
      AuthorityKind::Mint => self.mint.as_ref(),
      AuthorityKind::Blacklist => self.blacklist.as_ref(),
      AuthorityKind::Master => self.master.as_ref(),
    }
  }

  fn size_bytes(&self) -> usize {
    self
      .mint
      .as_ref()
      .map(|s| s.size_bytes())
      .unwrap_or_default()
      + self
        .blacklist
        .as_ref()
        .map(|s| s.size_bytes())
        .unwrap_or_default()
      + self
        .master
        .as_ref()
        .map(|s| s.size_bytes())
        .unwrap_or_default()
  }
}

#[derive(Clone)]
pub(super) struct ScriptBloom {
  bits: Vec<u64>,
  mask: u64,
}

impl ScriptBloom {
  fn new(entries: usize) -> Option<Self> {
    if entries == 0 {
      return None;
    }

    // Keep bloom small: 8 bits per entry, rounded up to power of two, capped at ~1MiB.
    let bit_count = (entries.next_power_of_two().saturating_mul(8)).clamp(64, 1 << 20);
    let words = bit_count.div_ceil(64);
    Some(Self {
      bits: vec![0; words],
      mask: u64::try_from(bit_count).ok()?.saturating_sub(1),
    })
  }

  fn hash(data: &[u8], seed: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    data.hash(&mut hasher);
    hasher.finish()
  }

  fn indices(&self, data: &[u8]) -> (usize, usize) {
    let mask = self.mask;
    let hash_a = Self::hash(data, 0) & mask;
    let hash_b = Self::hash(data, 0x9e3779b97f4a7c15) & mask;
    let idx_a = usize::try_from(hash_a).expect("mask fits usize");
    let idx_b = usize::try_from(hash_b).expect("mask fits usize");
    (idx_a, idx_b)
  }

  fn set_bit(bits: &mut [u64], idx: usize) {
    let word = idx / 64;
    let bit = idx % 64;
    if let Some(entry) = bits.get_mut(word) {
      *entry |= 1u64 << bit;
    }
  }

  fn test_bit(bits: &[u64], idx: usize) -> bool {
    let word = idx / 64;
    let bit = idx % 64;
    bits
      .get(word)
      .map(|entry| entry & (1u64 << bit) != 0)
      .unwrap_or(false)
  }

  fn insert(&mut self, data: &[u8]) {
    let (a, b) = self.indices(data);
    Self::set_bit(&mut self.bits, a);
    Self::set_bit(&mut self.bits, b);
  }

  fn might_contain(&self, data: &[u8]) -> bool {
    let (a, b) = self.indices(data);
    Self::test_bit(&self.bits, a) && Self::test_bit(&self.bits, b)
  }

  fn byte_size(&self) -> usize {
    self.bits.len() * std::mem::size_of::<u64>()
  }
}

pub(super) struct AuthorityContext {
  pub(super) flags: AuthorityBits,
  pub(super) scripts: AuthorityScripts,
  pub(super) minters: Vec<CachedScript>,
  pub(super) blacklist: Vec<CachedScript>,
  pub(super) blacklist_bloom: Option<ScriptBloom>,
  pub(super) supply_extra: u128,
}

impl AuthorityContext {
  fn size_bytes(&self) -> usize {
    std::mem::size_of::<AuthorityBits>()
      + self.scripts.size_bytes()
      + self.minters.iter().map(|s| s.size_bytes()).sum::<usize>()
      + self.blacklist.iter().map(|s| s.size_bytes()).sum::<usize>()
      + self
        .blacklist_bloom
        .as_ref()
        .map(|bloom| bloom.byte_size())
        .unwrap_or_default()
      + std::mem::size_of::<u128>()
  }
}

pub(crate) struct AuthorityContextCache {
  contexts: HashMap<RuneId, AuthorityContext>,
  access_order: VecDeque<RuneId>,
  max_bytes: usize,
  current_bytes: usize,
}

impl AuthorityContextCache {
  pub(crate) fn new(max_bytes: usize) -> Self {
    Self {
      contexts: HashMap::new(),
      access_order: VecDeque::new(),
      max_bytes,
      current_bytes: 0,
    }
  }

  pub(super) fn contains(&self, rune_id: RuneId) -> bool {
    self.contexts.contains_key(&rune_id)
  }

  pub(super) fn invalidate(&mut self, rune_id: RuneId) {
    if let Some(removed) = self.contexts.remove(&rune_id) {
      self.current_bytes = self.current_bytes.saturating_sub(removed.size_bytes());
      self.access_order.retain(|id| id != &rune_id);
    }
  }

  pub(super) fn update_supply_extra(&mut self, rune_id: RuneId, value: u128) {
    if let Some(ctx) = self.contexts.get_mut(&rune_id) {
      ctx.supply_extra = value;
    }
  }

  fn touch(&mut self, rune_id: RuneId) {
    self.access_order.retain(|id| id != &rune_id);
    self.access_order.push_front(rune_id);
  }

  fn insert(&mut self, rune_id: RuneId, context: AuthorityContext, size: usize) {
    // Prevent immediate eviction when configured budget is smaller than a single context.
    let effective_max = self.max_bytes.max(size);

    self.current_bytes = self.current_bytes.saturating_add(size);
    self.contexts.insert(rune_id, context);
    self.touch(rune_id);

    while self.current_bytes > effective_max {
      if let Some(oldest) = self.access_order.pop_back() {
        if let Some(evicted) = self.contexts.remove(&oldest) {
          self.current_bytes = self.current_bytes.saturating_sub(evicted.size_bytes());
        }
      } else {
        break;
      }
    }
  }

  pub(super) fn insert_and_get(
    &mut self,
    rune_id: RuneId,
    context: AuthorityContext,
  ) -> &AuthorityContext {
    let size = context.size_bytes();
    self.insert(rune_id, context, size);
    self.contexts.get(&rune_id).unwrap()
  }

  pub(super) fn get_existing(&mut self, rune_id: RuneId) -> &AuthorityContext {
    self.touch(rune_id);
    self
      .contexts
      .get(&rune_id)
      .expect("context should exist when get_existing is called")
  }
}

pub(super) struct Authority<'a, 'tx, 'client> {
  pub(super) client: &'client Client,
  pub(super) rune_id_to_authority_flags: &'a mut Table<'tx, RuneIdValue, u8>,
  pub(super) rune_id_to_authority_scripts: &'a mut Table<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_minters: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_blacklist: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
  pub(super) rune_id_to_supply_extra: &'a mut Table<'tx, RuneIdValue, u128>,
  pub(super) script_cache: &'a mut ScriptCache,
  pub(super) context_cache: &'a mut AuthorityContextCache,
}

impl<'a, 'tx, 'client> Authority<'a, 'tx, 'client> {
  pub(super) fn new(
    client: &'client Client,
    rune_id_to_authority_flags: &'a mut Table<'tx, RuneIdValue, u8>,
    rune_id_to_authority_scripts: &'a mut Table<'tx, RuneIdValue, &'static [u8]>,
    rune_id_to_minters: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
    rune_id_to_blacklist: &'a mut MultimapTable<'tx, RuneIdValue, &'static [u8]>,
    script_cache: &'a mut ScriptCache,
    rune_id_to_supply_extra: &'a mut Table<'tx, RuneIdValue, u128>,
    context_cache: &'a mut AuthorityContextCache,
  ) -> Self {
    Self {
      client,
      rune_id_to_authority_flags,
      rune_id_to_authority_scripts,
      rune_id_to_minters,
      rune_id_to_blacklist,
      rune_id_to_supply_extra,
      script_cache,
      context_cache,
    }
  }

  fn first_matching_input<F>(
    &mut self,
    tx: &Transaction,
    rune_id: RuneId,
    purpose: &str,
    mut predicate: F,
  ) -> Result<Option<usize>>
  where
    F: FnMut(&ScriptBuf) -> bool,
  {
    for (i, input) in tx.input.iter().take(AUTHORITY_INPUT_LIMIT).enumerate() {
      if let Some(script_pubkey) = self.script_cache.get_script_pubkey(
        self.client,
        &input.previous_output.txid,
        input.previous_output.vout,
      )? {
        if self.is_blacklisted(rune_id, script_pubkey.as_ref())? {
          log::debug!(
            "Ignoring blacklisted script when checking {} for {:?}",
            purpose,
            rune_id
          );
          continue;
        }

        if predicate(script_pubkey.as_ref()) {
          return Ok(Some(i));
        }
      }
    }

    Ok(None)
  }

  pub(super) fn check_authority(
    &mut self,
    tx: &Transaction,
    rune_id: RuneId,
    authority_type: AuthorityKind,
  ) -> Result<bool> {
    let expected_script = {
      let context = self.get_context(rune_id)?;
      let Some(authority_script) = context.scripts.get(authority_type) else {
        return Ok(false);
      };

      let Some(expected_script) = authority_script.script() else {
        log::warn!(
          "Skipping authority check for {:?} on {:?}: invalid compact script",
          authority_type,
          rune_id
        );
        return Ok(false);
      };

      expected_script.clone()
    };

    let purpose = match authority_type {
      AuthorityKind::Mint => "mint authority",
      AuthorityKind::Blacklist => "blacklist authority",
      AuthorityKind::Master => "master authority",
    };

    if let Some(i) = self.first_matching_input(tx, rune_id, purpose, |candidate| {
      candidate == &expected_script
    })? {
      log::debug!(
        "Authority matched on input {} for {:?} ({:?})",
        i,
        rune_id,
        authority_type
      );
      return Ok(true);
    }

    log::debug!(
      "Authority NOT matched for {:?} ({:?}); expected script {:?}, txid={}",
      rune_id,
      authority_type,
      expected_script,
      tx.compute_txid()
    );

    Ok(false)
  }

  pub(super) fn check_is_minter(&mut self, tx: &Transaction, rune_id: RuneId) -> Result<bool> {
    // Check if caller is master minter
    if self.check_authority(tx, rune_id, AuthorityKind::Master)? {
      return Ok(true);
    }

    let minter_scripts: Vec<ScriptBuf> = {
      let context = self.get_context(rune_id)?;
      if context.minters.is_empty() {
        return Ok(false);
      }

      context
        .minters
        .iter()
        .filter_map(|m| m.script().cloned())
        .collect()
    };

    if let Some(i) = self.first_matching_input(tx, rune_id, "delegated minter", |candidate| {
      minter_scripts.iter().any(|script| candidate == script)
    })? {
      log::debug!("Delegated minter matched on input {} for {:?}", i, rune_id);
      return Ok(true);
    }

    Ok(false)
  }

  pub(super) fn is_blacklisted(
    &mut self,
    rune_id: RuneId,
    script_pubkey: &ScriptBuf,
  ) -> Result<bool> {
    let context = self.get_context(rune_id)?;

    if let Some(bloom) = &context.blacklist_bloom
      && !bloom.might_contain(script_pubkey.as_bytes())
    {
      return Ok(false);
    }

    for entry in &context.blacklist {
      if let Some(candidate_script) = entry.script()
        && script_pubkey == candidate_script
      {
        return Ok(true);
      }
    }

    Ok(false)
  }

  pub(super) fn get_authority_script(
    &mut self,
    rune_id: RuneId,
    authority_type: AuthorityKind,
  ) -> Result<Option<CompactScript>> {
    Ok(
      self
        .get_context(rune_id)?
        .scripts
        .get(authority_type)
        .map(|cached| cached.compact.clone()),
    )
  }

  pub(super) fn get_supply_extra(&mut self, rune_id: RuneId) -> Result<u128> {
    Ok(self.get_context(rune_id)?.supply_extra)
  }

  pub(super) fn set_supply_extra(&mut self, rune_id: RuneId, value: u128) -> Result<()> {
    if value == 0 {
      // No-op for zero; we don't persist redundant rows.
      return Ok(());
    }

    self
      .rune_id_to_supply_extra
      .insert(rune_id.store(), value)?;
    self.context_cache.update_supply_extra(rune_id, value);
    Ok(())
  }

  /// Decode a blacklist/minter entry (format: [kind, body...]) to a ScriptBuf
  /// Returns None if the entry format is invalid or cannot be converted to a script
  pub(super) fn decode_entry_to_script(&self, entry: &[u8], rune_id: RuneId) -> Option<ScriptBuf> {
    self
      .decode_compact_entry(entry, rune_id, "entry")
      .and_then(|compact| compact.to_script())
  }

  pub(super) fn get_context(&mut self, rune_id: RuneId) -> Result<&AuthorityContext> {
    if self.context_cache.contains(rune_id) {
      return Ok(self.context_cache.get_existing(rune_id));
    }

    let context = self.load_context(rune_id)?;
    Ok(self.context_cache.insert_and_get(rune_id, context))
  }

  fn load_context(&mut self, rune_id: RuneId) -> Result<AuthorityContext> {
    let flags_entry = self.rune_id_to_authority_flags.get(&rune_id.store())?;
    let flags = flags_entry
      .as_ref()
      .map(|entry| AuthorityBits::from(entry.value()))
      .unwrap_or_else(AuthorityBits::empty);
    drop(flags_entry);

    let (scripts, presence) = self.decode_authority_scripts(rune_id)?;
    let flags = if flags.is_empty() { presence } else { flags };

    // Minters
    let mut minters = Vec::new();
    for entry_result in self.rune_id_to_minters.get(rune_id.store())? {
      if let Some(compact) = self.decode_compact_entry(entry_result?.value(), rune_id, "minter") {
        minters.push(CachedScript::new(compact));
      }
    }

    // Blacklist
    let mut blacklist = Vec::new();
    for entry_result in self.rune_id_to_blacklist.get(rune_id.store())? {
      if let Some(compact) = self.decode_compact_entry(entry_result?.value(), rune_id, "blacklist")
      {
        blacklist.push(CachedScript::new(compact));
      }
    }

    let mut blacklist_bloom = ScriptBloom::new(blacklist.len());
    if let Some(bloom) = blacklist_bloom.as_mut() {
      for entry in &blacklist {
        if let Some(script) = entry.script() {
          bloom.insert(script.as_bytes());
        }
      }
    }

    let supply_extra = self
      .rune_id_to_supply_extra
      .get(&rune_id.store())?
      .map(|entry| entry.value())
      .unwrap_or(0);

    let context = AuthorityContext {
      flags,
      scripts,
      minters,
      blacklist,
      blacklist_bloom,
      supply_extra,
    };

    Ok(context)
  }

  fn decode_authority_scripts(
    &mut self,
    rune_id: RuneId,
  ) -> Result<(AuthorityScripts, AuthorityBits)> {
    let mut scripts = AuthorityScripts::default();

    let scripts_blob = self.rune_id_to_authority_scripts.get(&rune_id.store())?;

    let Some(scripts_blob) = scripts_blob else {
      return Ok((scripts, AuthorityBits::empty()));
    };

    let blob = scripts_blob.value();
    if blob.is_empty() {
      return Ok((scripts, AuthorityBits::empty()));
    }

    let presence = AuthorityBits::from(blob[0]);

    // Decode scripts: [presence, mint_script?, blacklist_script?, master_minter_script?]
    let mut offset = 1;

    for current in [
      AuthorityKind::Mint,
      AuthorityKind::Blacklist,
      AuthorityKind::Master,
    ] {
      if presence.contains(current) {
        if offset + 2 > blob.len() {
          break;
        }

        let kind = blob[offset];
        let body_len = blob[offset + 1] as usize;

        if body_len == 0 || body_len > 32 || offset + 2 + body_len > blob.len() {
          log::warn!(
            "Invalid authority encoding for {:?} ({:?}): body_len={}",
            rune_id,
            current,
            body_len
          );
          break;
        }

        let candidate = CompactScript {
          kind: match kind {
            0 => CompactScriptKind::P2TR,
            1 => CompactScriptKind::P2WPKH,
            2 => CompactScriptKind::P2WSH,
            _ => {
              offset += 2 + body_len;
              continue;
            }
          },
          body: blob[offset + 2..offset + 2 + body_len].to_vec(),
        };

        let cached = CachedScript::new(candidate);

        match current {
          AuthorityKind::Mint => scripts.mint = Some(cached),
          AuthorityKind::Blacklist => scripts.blacklist = Some(cached),
          AuthorityKind::Master => scripts.master = Some(cached),
        }

        offset += 2 + body_len;
      }
    }

    Ok((scripts, presence))
  }

  fn decode_compact_entry(
    &self,
    entry_bytes: &[u8],
    rune_id: RuneId,
    label: &str,
  ) -> Option<CompactScript> {
    if entry_bytes.is_empty() {
      return None;
    }

    let kind = entry_bytes[0];
    let body = &entry_bytes[1..];

    if body.is_empty() || body.len() > 32 {
      log::warn!(
        "Skipping malformed {label} entry for {:?}: body_len={}",
        rune_id,
        body.len()
      );
      return None;
    }

    let compact = CompactScript {
      kind: match kind {
        0 => CompactScriptKind::P2TR,
        1 => CompactScriptKind::P2WPKH,
        2 => CompactScriptKind::P2WSH,
        _ => return None,
      },
      body: body.to_vec(),
    };

    if compact.to_script().is_none() {
      log::warn!(
        "Skipping malformed {label} entry for {:?}: kind {:?} body len {}",
        rune_id,
        compact.kind,
        compact.body.len()
      );
      return None;
    }

    Some(compact)
  }
}

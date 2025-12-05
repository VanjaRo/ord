use super::*;
use std::{collections::VecDeque, sync::Arc};

const SCRIPT_CACHE_ENTRY_OVERHEAD: usize = 64;

#[allow(dead_code)]
#[derive(Default, Clone, Copy)]
pub(crate) struct ScriptCacheStats {
  pub hits: u64,
  pub misses: u64,
}

/// LRU cache for script_pubkey lookups keyed by (txid, vout).
pub(crate) struct ScriptCache {
  cache: HashMap<(Txid, u32), Arc<ScriptBuf>>,
  access_order: VecDeque<(Txid, u32)>,
  max_bytes: usize,
  current_bytes: usize,
  stats: ScriptCacheStats,
}

impl ScriptCache {
  pub(crate) fn new(max_bytes: usize) -> Self {
    Self {
      cache: HashMap::new(),
      access_order: VecDeque::new(),
      max_bytes,
      current_bytes: 0,
      stats: ScriptCacheStats::default(),
    }
  }

  pub(super) fn get_script_pubkey(
    &mut self,
    client: &Client,
    txid: &Txid,
    vout: u32,
  ) -> Result<Option<Arc<ScriptBuf>>> {
    let key = (*txid, vout);

    // Check cache first
    if let Some(script) = self.get(&key).cloned() {
      self.stats.hits += 1;
      return Ok(Some(script));
    }

    self.stats.misses += 1;

    // Fetch from RPC and cache
    if let Some(tx_info) = client.get_raw_transaction_info(txid, None).into_option()? {
      let vout_idx = vout as usize;
      if vout_idx >= tx_info.vout.len() {
        return Ok(None);
      }

      let script = tx_info.vout[vout_idx].script_pub_key.script()?;
      let arc = Arc::new(script);
      self.put(key, arc.clone());
      Ok(Some(arc))
    } else {
      Ok(None)
    }
  }

  #[allow(dead_code)]
  pub(super) fn stats(&self) -> ScriptCacheStats {
    self.stats
  }

  fn entry_size(script: &ScriptBuf) -> usize {
    script.as_bytes().len() + SCRIPT_CACHE_ENTRY_OVERHEAD
  }

  fn get(&mut self, key: &(Txid, u32)) -> Option<&Arc<ScriptBuf>> {
    if self.cache.contains_key(key) {
      // Move to front (most recently used)
      self.access_order.retain(|k| k != key);
      self.access_order.push_front(*key);
      self.cache.get(key)
    } else {
      None
    }
  }

  fn put(&mut self, key: (Txid, u32), value: Arc<ScriptBuf>) {
    let new_size = Self::entry_size(&value);

    if let Some(existing) = self.cache.insert(key, value.clone()) {
      // Update existing entry size
      let existing_size = Self::entry_size(&existing);
      self.current_bytes = self
        .current_bytes
        .saturating_add(new_size)
        .saturating_sub(existing_size);
      self.access_order.retain(|k| k != &key);
      self.access_order.push_front(key);
    } else {
      self.current_bytes = self.current_bytes.saturating_add(new_size);
      self.access_order.push_front(key);
    }

    // Evict while we're over budget
    while self.current_bytes > self.max_bytes {
      if let Some(oldest) = self.access_order.pop_back() {
        if let Some(evicted) = self.cache.remove(&oldest) {
          let evicted_size = Self::entry_size(&evicted);
          self.current_bytes = self.current_bytes.saturating_sub(evicted_size);
        }
      } else {
        break;
      }
    }
  }
}

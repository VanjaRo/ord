use super::*;
use std::collections::HashMap;

pub(super) struct Allocation<'a, 'tx> {
  pub(super) outpoint_to_balances: &'a mut Table<'tx, &'static OutPointValue, &'static [u8]>,
}

impl<'a, 'tx> Allocation<'a, 'tx> {
  pub(super) fn new(
    outpoint_to_balances: &'a mut Table<'tx, &'static OutPointValue, &'static [u8]>,
  ) -> Self {
    Self {
      outpoint_to_balances,
    }
  }

  pub(super) fn calculate_unallocated<F>(
    &mut self,
    tx: &Transaction,
    mut is_blacklisted: F,
  ) -> Result<HashMap<RuneId, Lot>>
  where
    F: FnMut(RuneId, &OutPoint) -> Result<bool>,
  {
    // map of rune ID to un-allocated balance of that rune
    let mut unallocated: HashMap<RuneId, Lot> = HashMap::new();

    // increment unallocated runes with the runes in tx inputs
    for input in &tx.input {
      let key = input.previous_output.store();

      let locked_buffer = {
        let removed = self.outpoint_to_balances.remove(&key)?;

        if let Some(guard) = removed {
          let buffer = guard.value().to_vec();
          drop(guard);
          let mut i = 0;
          let mut locked: Vec<(RuneId, u128)> = Vec::new();

          while i < buffer.len() {
            let ((id, balance), len) = Index::decode_rune_balance(&buffer[i..]).unwrap();
            i += len;

            if is_blacklisted(id, &input.previous_output)? {
              locked.push((id, balance));
            } else {
              *unallocated.entry(id).or_default() += balance;
            }
          }

          if locked.is_empty() {
            None
          } else {
            let mut locked_buffer = Vec::new();
            for (id, balance) in locked {
              Index::encode_rune_balance(id, balance, &mut locked_buffer);
            }
            Some(locked_buffer)
          }
        } else {
          None
        }
      };

      if let Some(buffer) = locked_buffer {
        self.outpoint_to_balances.insert(&key, buffer.as_slice())?;
      }
    }

    Ok(unallocated)
  }
}

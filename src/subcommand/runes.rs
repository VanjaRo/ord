use super::*;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
  pub runes: BTreeMap<Rune, RuneInfo>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RuneInfo {
  pub block: u64,
  pub burned: u128,
  pub divisibility: u8,
  pub etching: Txid,
  pub id: RuneId,
  pub mints: u128,
  pub number: u64,
  pub premine: u128,
  pub rune: SpacedRune,
  pub supply: u128,
  pub symbol: Option<char>,
  pub terms: Option<Terms>,
  pub timestamp: DateTime<Utc>,
  pub turbo: bool,
  pub tx: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub authority_flags: Option<AuthorityFlags>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub supply_extra: Option<u128>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub minter_count: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub blacklist_count: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub authority: Option<AuthorityDetail>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthorityFlags {
  pub allow_minting: bool,
  pub allow_blacklisting: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct ScriptDetail {
  pub compact: ordinals::CompactScript,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub script_pubkey: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub address: Option<String>,
}

impl ScriptDetail {
  pub fn from_compact(compact: ordinals::CompactScript, chain: Option<crate::Chain>) -> Self {
    let script = compact.to_script();

    let script_pubkey = script.as_ref().map(|script| hex::encode(script.as_bytes()));

    let address = match (script.as_ref(), chain) {
      (Some(script), Some(chain)) => chain
        .address_from_script(script)
        .ok()
        .map(|a| a.to_string()),
      _ => None,
    };

    Self {
      compact,
      script_pubkey,
      address,
    }
  }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default, Clone)]
pub struct AuthorityDetail {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub mint: Option<ScriptDetail>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub blacklist: Option<ScriptDetail>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub master: Option<ScriptDetail>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub minters: Vec<ScriptDetail>,
  pub minters_more: bool,
  #[serde(default)]
  pub minter_page: usize,
  #[serde(default)]
  pub minter_page_size: usize,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub blacklist_entries: Vec<ScriptDetail>,
  pub blacklist_more: bool,
  #[serde(default)]
  pub blacklist_page: usize,
  #[serde(default)]
  pub blacklist_page_size: usize,
}

pub(crate) fn run(settings: Settings) -> SubcommandResult {
  let index = Index::open(&settings)?;

  ensure!(
    index.has_rune_index(),
    "`ord runes` requires index created with `--index-runes` flag",
  );

  index.update()?;

  Ok(Some(Box::new(Output {
    runes: index
      .runes()?
      .into_iter()
      .map(
        |(
          id,
          entry @ RuneEntry {
            block,
            burned,
            divisibility,
            etching,
            mints,
            number,
            premine,
            spaced_rune,
            symbol,
            terms,
            timestamp,
            turbo,
          },
        )| {
          // Get authority flags from stored terms
          let authority_flags = AuthorityFlags {
            allow_minting: terms.map(|terms| terms.allow_minting).unwrap_or(false),
            allow_blacklisting: terms.map(|terms| terms.allow_blacklisting).unwrap_or(false),
          };

          // Get supply_extra, minter_count, blacklist_count
          let supply_extra = index.get_supply_extra(id).unwrap_or_default();
          let minter_count = index.get_minter_count(id).unwrap_or(0);
          let blacklist_count = index.get_blacklist_count(id).unwrap_or(0);
          // Report the base supply separately from authority-issued extra supply.
          let supply = entry.supply();

          (
            spaced_rune.rune,
            RuneInfo {
              block,
              burned,
              divisibility,
              etching,
              id,
              mints,
              number,
              premine,
              rune: spaced_rune,
              supply,
              symbol,
              terms,
              timestamp: crate::timestamp(timestamp),
              turbo,
              tx: id.tx,
              authority_flags: Some(authority_flags),
              supply_extra,
              minter_count: Some(minter_count),
              blacklist_count: Some(blacklist_count),
              authority: None,
            },
          )
        },
      )
      .collect::<BTreeMap<Rune, RuneInfo>>(),
  })))
}

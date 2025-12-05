use bitcoin::hashes::Hash;
use {super::*, ord::subcommand::runes::Output};

#[test]
fn flag_is_required() {
  let core = mockcore::builder().network(Network::Regtest).build();

  let ord = TestServer::spawn_with_server_args(&core, &["--regtest"], &[]);

  CommandBuilder::new("--regtest runes")
    .core(&core)
    .ord(&ord)
    .expected_exit_code(1)
    .expected_stderr("error: `ord runes` requires index created with `--index-runes` flag\n")
    .run_and_extract_stdout();
}

#[test]
fn no_runes() {
  let core = mockcore::builder().network(Network::Regtest).build();

  assert_eq!(
    CommandBuilder::new("--index-runes --regtest runes")
      .core(&core)
      .run_and_deserialize_output::<Output>(),
    Output {
      runes: BTreeMap::new(),
    }
  );
}

#[test]
fn one_rune() {
  let core = mockcore::builder().network(Network::Regtest).build();

  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);

  create_wallet(&core, &ord);

  let etch = etch(&core, &ord, Rune(RUNE));

  pretty_assert_eq!(
    CommandBuilder::new("--index-runes --regtest runes")
      .core(&core)
      .run_and_deserialize_output::<Output>(),
    Output {
      runes: vec![(
        Rune(RUNE),
        RuneInfo {
          block: 7,
          burned: 0,
          divisibility: 0,
          etching: etch.output.reveal,
          id: RuneId { block: 7, tx: 1 },
          terms: None,
          mints: 0,
          number: 0,
          premine: 1000,
          rune: SpacedRune {
            rune: Rune(RUNE),
            spacers: 0
          },
          supply: 1000,
          symbol: Some('¢'),
          timestamp: ord::timestamp(7),
          turbo: false,
          tx: 1,
          authority_flags: Some(ord::subcommand::runes::AuthorityFlags {
            allow_minting: false,
            allow_blacklisting: false
          }),
          authority: None,
          supply_extra: None,
          minter_count: Some(0),
          blacklist_count: Some(0),
        }
      )]
      .into_iter()
      .collect(),
    }
  );
}

#[test]
fn two_runes() {
  let core = mockcore::builder().network(Network::Regtest).build();

  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);

  create_wallet(&core, &ord);

  let a = etch(&core, &ord, Rune(RUNE));
  let b = etch(&core, &ord, Rune(RUNE + 1));

  pretty_assert_eq!(
    CommandBuilder::new("--index-runes --regtest runes")
      .core(&core)
      .run_and_deserialize_output::<Output>(),
    Output {
      runes: vec![
        (
          Rune(RUNE),
          RuneInfo {
            block: 7,
            burned: 0,
            divisibility: 0,
            etching: a.output.reveal,
            id: RuneId { block: 7, tx: 1 },
            terms: None,
            mints: 0,
            number: 0,
            premine: 1000,
            rune: SpacedRune {
              rune: Rune(RUNE),
              spacers: 0
            },
            supply: 1000,
            symbol: Some('¢'),
            timestamp: ord::timestamp(7),
            turbo: false,
            tx: 1,
            authority_flags: Some(ord::subcommand::runes::AuthorityFlags {
              allow_minting: false,
              allow_blacklisting: false
            }),
            authority: None,
            supply_extra: None,
            minter_count: Some(0),
            blacklist_count: Some(0),
          }
        ),
        (
          Rune(RUNE + 1),
          RuneInfo {
            block: 14,
            burned: 0,
            divisibility: 0,
            etching: b.output.reveal,
            id: RuneId { block: 14, tx: 1 },
            terms: None,
            mints: 0,
            number: 1,
            premine: 1000,
            rune: SpacedRune {
              rune: Rune(RUNE + 1),
              spacers: 0
            },
            supply: 1000,
            symbol: Some('¢'),
            timestamp: ord::timestamp(14),
            turbo: false,
            tx: 1,
            authority_flags: Some(ord::subcommand::runes::AuthorityFlags {
              allow_minting: false,
              allow_blacklisting: false
            }),
            authority: None,
            supply_extra: None,
            minter_count: Some(0),
            blacklist_count: Some(0),
          }
        )
      ]
      .into_iter()
      .collect(),
    }
  );
}

#[test]
fn mint_with_authority() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);

  create_wallet(&core, &ord);

  // Mine to get initial coins
  core.mine_blocks(10);

  // Authority Address (P2WPKH)
  let pubkey_hash = bitcoin::WPubkeyHash::from_byte_array([1u8; 20]);
  let script = ScriptBuf::new_p2wpkh(&pubkey_hash);
  let authority_address = Address::from_script(&script, Network::Regtest).unwrap();

  // 1. Fund Authority
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint, _) = utxos.iter().next().unwrap();
  let (block, tx) = core.tx_index(coinbase_outpoint.txid);

  let fund_tx = TransactionTemplate {
    inputs: &[(block, tx, coinbase_outpoint.vout as usize, Witness::new())],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  };
  let fund_txid = core.broadcast_tx(fund_tx);
  core.mine_blocks(1);

  // 2. Etch with Authority
  let (fund_block, fund_tx_idx) = core.tx_index(fund_txid);
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: None,
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_minting: true,
        ..default()
      }),
      ..default()
    }),
    ..default()
  };

  let etch_tx = TransactionTemplate {
    inputs: &[(fund_block, fund_tx_idx, 0, Witness::new())],
    recipient: Some(authority_address.clone()), // Change back to authority
    outputs: 1,
    op_return: Some(runestone.encipher()),
    op_return_index: Some(0), // OP_RETURN at 0
    ..default()
  };

  let etch_txid = core.broadcast_tx(etch_tx);
  core.mine_blocks(1);

  // 3. Mint with Authority
  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);

  // Get ord wallet address to receive minted tokens
  let ord_addresses = CommandBuilder::new("--regtest --index-runes wallet receive")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::wallet::receive::Output>()
    .addresses;
  let ord_address = ord_addresses
    .first()
    .unwrap()
    .clone()
    .require_network(Network::Regtest)
    .unwrap();

  let mint_runestone = Runestone {
    edicts: vec![Edict {
      id: RuneId {
        block: etch_block as u64,
        tx: etch_tx_idx as u32,
      },
      amount: 1000,
      output: 1, // To recipient (Output 1)
    }],
    ..default()
  };

  let mint_tx = TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::new())], // Input 0 is Change (Output 1 of Etch TX)
    recipient: Some(ord_address),                            // Destination
    outputs: 1,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    ..default()
  };

  core.broadcast_tx(mint_tx);
  core.mine_blocks(1);

  // Check supply
  let output = CommandBuilder::new("--index-runes --regtest runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Output>();

  let rune_info = output.runes.values().next().expect("Rune not found");
  assert_eq!(rune_info.supply, 0);
  assert_eq!(rune_info.supply_extra, Some(1000));
  let rune = output.runes.keys().next().unwrap();

  // 4. Check Balance
  let balances = CommandBuilder::new("--index-runes --regtest balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let spaced_rune = SpacedRune {
    rune: *rune,
    spacers: 0,
  };
  let balance = balances
    .runes
    .get(&spaced_rune)
    .map(|utxo_map| utxo_map.values().map(|pile| pile.amount).sum::<u128>());

  assert_eq!(balance, Some(1000));
}

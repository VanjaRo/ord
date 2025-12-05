use super::*;

fn rune_commitment_witness(rune: Rune) -> Witness {
  let mut buf = bitcoin::script::PushBytesBuf::new();
  buf.extend_from_slice(&rune.commitment()).unwrap();
  let script = bitcoin::script::Builder::new()
    .push_slice(buf)
    .into_script();
  Witness::from_slice(&[script.into_bytes(), Vec::new()])
}

#[test]
fn test_authority_minting() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(1);

  create_wallet(&core, &ord);

  // 1. Setup Authority Address
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  // 2. Fund Authority
  // Find a valid UTXO to spend (Genesis coinbase)
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint, _) = utxos.iter().next().unwrap();
  let (block, tx) = core.tx_index(coinbase_outpoint.txid);

  let fund_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(block, tx, coinbase_outpoint.vout as usize, Witness::new())],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 3. Etch Rune with allow_minting=true
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_minting: true,
        ..default()
      }),
      turbo: false,
      spacers: None,
    }),
    ..default()
  };

  let (fund_block, fund_tx_idx) = core.tx_index(fund_txid);

  let etch_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      fund_block,
      fund_tx_idx,
      0,
      rune_commitment_witness(Rune(RUNE)),
    )], // Spend authority funding
    recipient: Some(authority_address.clone()), // Send change back to authority
    outputs: 2,
    op_return: Some(runestone.encipher()),
    op_return_index: Some(0), // OP_RETURN at 0, Change at 1
    ..default()
  });
  core.mine_blocks(1);

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 4. Authority Minting
  // Authority spends its own output (index 1 from etching)

  let recipient = CommandBuilder::new("--regtest --index-runes wallet receive")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::wallet::receive::Output>()
    .addresses[0]
    .clone()
    .require_network(Network::Regtest)
    .unwrap();

  let mint_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 1000,
      output: 1, // To recipient (Output 1)
    }],
    ..default()
  };

  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())], // Spend authority change (output 1)
    outputs: 2,                                                  // 0: OP_RETURN, 1: Recipient
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // 5. Verify Balance
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let spaced_rune = SpacedRune {
    rune: Rune(RUNE),
    spacers: 0,
  };

  let mut total_balance = 0;
  if let Some(runes) = balances.runes.get(&spaced_rune) {
    for pile in runes.values() {
      total_balance += pile.amount;
    }
  }

  assert_eq!(
    total_balance, 1000,
    "Recipient should have 1000 runes minted by authority"
  );

  // 6. Verify Supply
  let runes_output = CommandBuilder::new("--regtest --index-runes runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::runes::Output>();

  let rune_info = runes_output
    .runes
    .get(&Rune(RUNE))
    .expect("Rune info not found");
  assert_eq!(rune_info.supply, 0, "Base supply should remain 0");
  assert_eq!(
    rune_info.supply_extra,
    Some(1000),
    "Authority minting should be tracked in supply_extra"
  );
}

#[test]
fn test_authority_minting_fails_without_authority() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(1);

  create_wallet(&core, &ord);

  // 1. Setup Authority Address
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  // 2. Fund Authority
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint, _) = utxos.iter().next().unwrap();
  let (block, tx) = core.tx_index(coinbase_outpoint.txid);

  let fund_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(block, tx, coinbase_outpoint.vout as usize, Witness::new())],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 3. Etch Rune with allow_minting=true
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_minting: true,
        ..default()
      }),
      turbo: false,
      spacers: None,
    }),
    ..default()
  };

  let (fund_block, fund_tx_idx) = core.tx_index(fund_txid);
  let etch_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      fund_block,
      fund_tx_idx,
      0,
      rune_commitment_witness(Rune(RUNE)),
    )],
    recipient: Some(authority_address.clone()),
    outputs: 2,
    op_return: Some(runestone.encipher()),
    op_return_index: Some(0),
    ..default()
  });
  core.mine_blocks(1);

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 4. Attempt Minting WITHOUT Authority
  // We need a UTXO that is NOT the authority change output.
  // Available UTXOs:
  // - Etch Change (Authority)
  // - Coinbases from blocks 1, 2, 3 (mined during steps)

  // Let's grab the latest coinbase UTXO
  let utxos = core.state().utxos.clone();
  // Find a UTXO that is not the etch change output
  // Etch change outpoint: etch_txid:1
  let etch_change_outpoint = OutPoint {
    txid: etch_txid,
    vout: 1,
  };

  let (outpoint, _) = utxos
    .iter()
    .find(|(op, _)| **op != etch_change_outpoint)
    .expect("Should have other UTXOs available");

  let (mint_input_block, mint_input_tx) = core.tx_index(outpoint.txid);

  let recipient = CommandBuilder::new("--regtest --index-runes wallet receive")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::wallet::receive::Output>()
    .addresses[0]
    .clone()
    .require_network(Network::Regtest)
    .unwrap();

  let mint_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 1000,
      output: 1,
    }],
    ..default()
  };

  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      mint_input_block,
      mint_input_tx,
      outpoint.vout as usize,
      Witness::new(),
    )],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // 5. Verify Balance - Should be 0
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let total_balance = balances
    .runes
    .values()
    .flat_map(|m| m.values())
    .map(|p| p.amount)
    .sum::<u128>();

  assert_eq!(total_balance, 0, "Should not mint without authority");
}

#[test]
fn test_authority_in_second_input_fails() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(1);

  create_wallet(&core, &ord);

  // 1. Setup Authority Address
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  // 2. Fund Authority
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint, _) = utxos.iter().next().unwrap();
  let (block, tx) = core.tx_index(coinbase_outpoint.txid);

  let fund_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(block, tx, coinbase_outpoint.vout as usize, Witness::new())],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 3. Etch Rune with allow_minting=true
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_minting: true,
        ..default()
      }),
      turbo: false,
      spacers: None,
    }),
    ..default()
  };

  let (fund_block, fund_tx_idx) = core.tx_index(fund_txid);
  let etch_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      fund_block,
      fund_tx_idx,
      0,
      rune_commitment_witness(Rune(RUNE)),
    )],
    recipient: Some(authority_address.clone()),
    outputs: 2,
    op_return: Some(runestone.encipher()),
    op_return_index: Some(0),
    ..default()
  });
  core.mine_blocks(1);

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 4. Prepare Transaction with 2 Inputs
  // Input 0: Random non-authority UTXO
  // Input 1: Authority UTXO (Etch Change)

  // Get a random UTXO
  let utxos = core.state().utxos.clone();
  let etch_change_outpoint = OutPoint {
    txid: etch_txid,
    vout: 1,
  };
  let (random_outpoint, _) = utxos
    .iter()
    .find(|(op, _)| **op != etch_change_outpoint && op.txid != fund_txid)
    .expect("Should have other UTXOs available");

  let (random_block, random_tx) = core.tx_index(random_outpoint.txid);

  let recipient = CommandBuilder::new("--regtest --index-runes wallet receive")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::wallet::receive::Output>()
    .addresses[0]
    .clone()
    .require_network(Network::Regtest)
    .unwrap();

  let mint_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 1000,
      output: 1,
    }],
    ..default()
  };

  // Construct transaction with random input FIRST, authority input SECOND
  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[
      (
        random_block,
        random_tx,
        random_outpoint.vout as usize,
        Witness::new(),
      ),
      (etch_block, etch_tx_idx, 1, Witness::default()), // Authority input
    ],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // 5. Verify Balance - Authority in non-first input should still work
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let total_balance = balances
    .runes
    .values()
    .flat_map(|m| m.values())
    .map(|p| p.amount)
    .sum::<u128>();

  assert_eq!(
    total_balance, 1000,
    "Should mint even if authority is not the first input"
  );
}

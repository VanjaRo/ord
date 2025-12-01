use super::*;
use ordinals::{
  AuthorityBits, AuthorityUpdates, CompactScriptKind, Etching, Rune, Runestone, SetAuthority, Terms,
};

fn entry(kind: CompactScriptKind, body: &[u8]) -> Vec<u8> {
  let mut data = Vec::with_capacity(1 + body.len());
  data.push(kind as u8);
  data.extend_from_slice(body);
  data
}

fn rune_commitment_witness(rune: Rune) -> Witness {
  let mut buf = bitcoin::script::PushBytesBuf::new();
  buf.extend_from_slice(&rune.commitment()).unwrap();
  let script = bitcoin::script::Builder::new()
    .push_slice(buf)
    .into_script();
  Witness::from_slice(&[script.into_bytes(), Vec::new()])
}

#[test]
fn test_delegated_minting() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  create_wallet(&core, &ord);

  // 1. Setup Authority Address
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  // 2. Setup Minter Address
  let minter_script =
    ScriptBuf::from_hex("51201111111111111111111111111111111111111111111111111111111111111111")
      .unwrap();
  let minter_address = Address::from_script(&minter_script, Network::Regtest).unwrap();

  // 3. Fund Authority
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

  // 4. Etch Rune with allow_minting=true (Creator is Master Minter)
  let runestone = Runestone {
    etching: Some(Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(Terms {
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
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));
  eprintln!("DEBUG etch_txid: {etch_txid}");

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 5. Master Minter Adds Minter
  // Use 'etch_txid:1' which is the change output back to authority
  let add_minter_runestone = Runestone {
    authority: Some(AuthorityUpdates {
      add_minter: Some(vec![entry(
        CompactScriptKind::P2TR,
        &minter_script.as_bytes()[2..],
      )]),
      ..default()
    }),
    mint: Some(rune_id), // Target the rune
    ..default()
  };

  let add_minter_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(add_minter_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(authority_address.clone()), // Change back to authority
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));
  eprintln!("DEBUG add_minter_txid: {add_minter_txid}");

  // 6. Fund Minter
  // Need to fund the minter address so it can send a transaction
  // Use another coinbase
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_2, _) = utxos
    .iter()
    .find(|(op, _)| op.txid != fund_txid && op.txid != etch_txid && op.txid != add_minter_txid)
    .unwrap();
  let (block_2, tx_2) = core.tx_index(coinbase_outpoint_2.txid);

  let fund_minter_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_2,
      tx_2,
      coinbase_outpoint_2.vout as usize,
      Witness::new(),
    )],
    recipient: Some(minter_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(1);

  // 7. Minter Mints
  let (fund_minter_block, fund_minter_tx_idx) = core.tx_index(fund_minter_txid);

  let recipient_addr = CommandBuilder::new("--regtest --index-runes wallet receive")
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
      amount: 500,
      output: 1,
    }],
    mint: Some(rune_id),
    ..default()
  };

  let mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(fund_minter_block, fund_minter_tx_idx, 0, Witness::default())],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
    ..default()
  });
  core.mine_blocks(1);
  eprintln!("DEBUG mint_txid: {mint_txid}");

  // 8. Verify Balance
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let spaced_rune = SpacedRune {
    rune: Rune(RUNE),
    spacers: 0,
  };

  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);

  assert_eq!(total_balance, 500, "Minter should have minted 500 runes");
}

#[test]
fn test_revoked_minting() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(1);

  create_wallet(&core, &ord);

  // Setup Addresses
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  let minter_script =
    ScriptBuf::from_hex("51201111111111111111111111111111111111111111111111111111111111111111")
      .unwrap();
  let minter_address = Address::from_script(&minter_script, Network::Regtest).unwrap();

  // Fund Authority
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

  // Etch Rune
  let runestone = Runestone {
    etching: Some(Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(Terms {
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

  // Add Minter
  let add_minter_runestone = Runestone {
    authority: Some(AuthorityUpdates {
      add_minter: Some(vec![entry(
        CompactScriptKind::P2TR,
        &minter_script.as_bytes()[2..],
      )]),
      ..default()
    }),
    mint: Some(rune_id),
    ..default()
  };

  let add_minter_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(add_minter_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(authority_address.clone()),
    ..default()
  });
  core.mine_blocks(1);
  let (add_minter_block, add_minter_tx_idx) = core.tx_index(add_minter_txid);

  // Fund Minter
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_2, _) = utxos
    .iter()
    .find(|(op, _)| op.txid != fund_txid && op.txid != etch_txid && op.txid != add_minter_txid)
    .unwrap();
  let (block_2, tx_2) = core.tx_index(coinbase_outpoint_2.txid);

  let fund_minter_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_2,
      tx_2,
      coinbase_outpoint_2.vout as usize,
      Witness::new(),
    )],
    recipient: Some(minter_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(1);

  // Revoke Minter
  let remove_minter_runestone = Runestone {
    authority: Some(AuthorityUpdates {
      remove_minter: Some(vec![entry(
        CompactScriptKind::P2TR,
        &minter_script.as_bytes()[2..],
      )]),
      ..default()
    }),
    mint: Some(rune_id),
    ..default()
  };

  let _revoke_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(add_minter_block, add_minter_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(remove_minter_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(authority_address.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Minter Tries to Mint (Should Fail)
  let (fund_minter_block, fund_minter_tx_idx) = core.tx_index(fund_minter_txid);

  let recipient_addr = CommandBuilder::new("--regtest --index-runes wallet receive")
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
      amount: 500,
      output: 1,
    }],
    mint: Some(rune_id),
    ..default()
  };

  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(fund_minter_block, fund_minter_tx_idx, 0, Witness::default())],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Verify Balance (Should be 0)
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let spaced_rune = SpacedRune {
    rune: Rune(RUNE),
    spacers: 0,
  };

  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);

  assert_eq!(
    total_balance, 0,
    "Revoked minter should not be able to mint"
  );
}

#[test]
fn test_master_minter_transfer() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(1);

  create_wallet(&core, &ord);

  // Setup Addresses
  let authority_script =
    ScriptBuf::from_hex("51200000000000000000000000000000000000000000000000000000000000000001")
      .unwrap();
  let authority_address = Address::from_script(&authority_script, Network::Regtest).unwrap();

  let new_authority_script =
    ScriptBuf::from_hex("51202222222222222222222222222222222222222222222222222222222222222222")
      .unwrap();
  let new_authority_address =
    Address::from_script(&new_authority_script, Network::Regtest).unwrap();

  let minter_script =
    ScriptBuf::from_hex("51201111111111111111111111111111111111111111111111111111111111111111")
      .unwrap();
  let minter_address = Address::from_script(&minter_script, Network::Regtest).unwrap();

  // Fund Authority
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

  // Etch Rune
  let runestone = Runestone {
    etching: Some(Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(Terms {
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

  // Fund New Authority (so it can sign later)
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_2, _) = utxos
    .iter()
    .find(|(op, _)| op.txid != fund_txid && op.txid != etch_txid)
    .unwrap();
  let (block_2, tx_2) = core.tx_index(coinbase_outpoint_2.txid);
  let fund_new_auth_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_2,
      tx_2,
      coinbase_outpoint_2.vout as usize,
      Witness::new(),
    )],
    recipient: Some(new_authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(1);
  let (fund_new_auth_block, fund_new_auth_tx_idx) = core.tx_index(fund_new_auth_txid);

  // Transfer Master Minter Authority (Bit 2)
  // Authority (old) sends SetAuthority
  let set_auth_runestone = Runestone {
    set_authority: Some(SetAuthority {
      authorities: AuthorityBits::from(0b100), // Master Minter only
      script_pubkey_compact: new_authority_script.as_bytes()[2..].to_vec(),
    }),
    mint: Some(rune_id),
    ..default()
  };

  let transfer_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(set_auth_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(authority_address.clone()), // Change back to old auth (irrelevant for auth power)
    ..default()
  });
  core.mine_blocks(1);
  let (transfer_block, transfer_tx_idx) = core.tx_index(transfer_txid);

  // Old Authority Tries to Add Minter (Should Fail)
  let add_minter_runestone = Runestone {
    authority: Some(AuthorityUpdates {
      add_minter: Some(vec![entry(
        CompactScriptKind::P2TR,
        &minter_script.as_bytes()[2..],
      )]),
      ..default()
    }),
    mint: Some(rune_id),
    ..default()
  };

  let _fail_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(transfer_block, transfer_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(add_minter_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(authority_address.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Fund Minter (need for minting later)
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_3, _) = utxos
    .iter()
    .find(|(op, _)| {
      op.txid != fund_txid
        && op.txid != etch_txid
        && op.txid != fund_new_auth_txid
        && op.txid != transfer_txid
        && op.txid != _fail_txid
    })
    .unwrap();
  let (block_3, tx_3) = core.tx_index(coinbase_outpoint_3.txid);
  let fund_minter_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_3,
      tx_3,
      coinbase_outpoint_3.vout as usize,
      Witness::new(),
    )],
    recipient: Some(minter_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(1);
  let (fund_minter_block, fund_minter_tx_idx) = core.tx_index(fund_minter_txid);

  // Try to mint with minter (should fail as it wasn't added successfully)
  let recipient_addr = CommandBuilder::new("--regtest --index-runes wallet receive")
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
      amount: 100,
      output: 1,
    }],
    mint: Some(rune_id),
    ..default()
  };

  let _fail_mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(fund_minter_block, fund_minter_tx_idx, 0, Witness::default())],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Verify Balance (0)
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();
  let spaced_rune = SpacedRune {
    rune: Rune(RUNE),
    spacers: 0,
  };
  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);
  assert_eq!(total_balance, 0, "Failed add_minter should mean no minting");

  // New Authority Adds Minter (Should Succeed)
  // We need a new input for minter funding since the previous one was used
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_4, _) = utxos
    .iter()
    .find(|(op, _)| {
      op.txid != fund_txid
        && op.txid != etch_txid
        && op.txid != fund_new_auth_txid
        && op.txid != transfer_txid
        && op.txid != _fail_txid
        && op.txid != fund_minter_txid
        && op.txid != _fail_mint_txid
    })
    .unwrap();
  let (block_4, tx_4) = core.tx_index(coinbase_outpoint_4.txid);
  let fund_minter_txid_2 = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_4,
      tx_4,
      coinbase_outpoint_4.vout as usize,
      Witness::new(),
    )],
    recipient: Some(minter_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(1);
  let (fund_minter_block_2, fund_minter_tx_idx_2) = core.tx_index(fund_minter_txid_2);

  let _add_minter_txid_2 = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      fund_new_auth_block,
      fund_new_auth_tx_idx,
      0,
      Witness::default(),
    )], // Spend new authority funds
    outputs: 2,
    op_return: Some(add_minter_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(new_authority_address.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Minter Mints (Should Succeed)
  let _success_mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      fund_minter_block_2,
      fund_minter_tx_idx_2,
      0,
      Witness::default(),
    )],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // Verify Balance (100)
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();
  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);
  assert_eq!(
    total_balance, 100,
    "New authority successfully added minter"
  );
}

#[test]
fn test_mint_authority_direct_minting() {
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

  // 3. Etch Rune with allow_minting=true.
  // The authority_script becomes MintAuthority (and MasterMinter).
  let runestone = Runestone {
    etching: Some(Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(Terms {
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

  // 4. Mint Authority Mints directly via Edict
  let recipient_addr = CommandBuilder::new("--regtest --index-runes wallet receive")
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
    mint: Some(rune_id),
    ..default()
  };

  // Use the change output from etching (etch_txid:1) which went back to authority
  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
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

  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);

  assert_eq!(
    total_balance, 1000,
    "MintAuthority should be able to mint directly via edict"
  );

  // 6. Verify Supply Extra
  let runes_output = CommandBuilder::new("--regtest --index-runes runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::runes::Output>();

  let rune_info = runes_output.runes.get(&spaced_rune.rune).unwrap();

  assert_eq!(
    rune_info.supply, 0,
    "Regular supply should be 0 (no open mints)"
  );
  assert_eq!(
    rune_info.supply_extra,
    Some(1000),
    "Supply extra should be 1000"
  );
}

#[test]
fn test_authority_mint_mixed_with_open_mint() {
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

  // 3. Etch Rune with allow_minting=true and Open Mint Terms
  let runestone = Runestone {
    etching: Some(Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(0),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        amount: Some(100),
        cap: Some(10),
        height: (None, None),
        offset: (None, None),
        allow_minting: true,
        allow_blacklisting: false,
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
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 4. Authority Mints via Edict AND Open Mint (mint field) in same tx
  let recipient_addr = CommandBuilder::new("--regtest --index-runes wallet receive")
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
    mint: Some(rune_id), // This triggers open mint (100 amount)
    ..default()
  };

  // Use the change output from etching
  let _mint_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(mint_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(recipient_addr.clone()),
    ..default()
  });
  core.mine_blocks(1);

  // 5. Verify Balance
  // Expected: 1000 (edict specifies amount to transfer, it consumes open mint)
  let balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  let spaced_rune = SpacedRune {
    rune: Rune(RUNE),
    spacers: 0,
  };

  let total_balance = balances
    .runes
    .get(&spaced_rune)
    .map(|runes| runes.values().map(|p| p.amount).sum::<u128>())
    .unwrap_or(0);

  assert_eq!(
    total_balance, 1000,
    "Authority should be able to mint via edict (1000) consuming open mint (100) + extra (900)"
  );

  // 6. Verify Supply
  let runes_output = CommandBuilder::new("--regtest --index-runes runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::runes::Output>();

  let rune_info = runes_output.runes.get(&spaced_rune.rune).unwrap();

  assert_eq!(
    rune_info.supply, 100,
    "Regular supply should be 100 (1 open mint)"
  );
  assert_eq!(
    rune_info.supply_extra,
    Some(900),
    "Supply extra should be 900 (1000 - 100)"
  );
}

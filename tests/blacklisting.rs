use super::*;
use ordinals::CompactScriptKind;

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
fn test_blacklisting_flow() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  create_wallet(&core, &ord);

  // 1. Setup Authority Address (Blacklist Authority)
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

  // 3. Etch Rune with allow_blacklisting=true
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(1000), // Premine 1000 to the etcher (authority)
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_blacklisting: true,
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
    recipient: Some(authority_address.clone()), // Change back to authority (holds premine)
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

  // 4. Send Runes to User A (Victim)
  let user_a_script =
    ScriptBuf::from_hex("51201111111111111111111111111111111111111111111111111111111111111111")
      .unwrap();
  let user_a_address = Address::from_script(&user_a_script, Network::Regtest).unwrap();

  let transfer_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 500,
      output: 1,
    }],
    ..default()
  };

  // Authority sends 500 to User A
  let transfer_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())], // Spend authority change
    outputs: 2,
    op_return: Some(transfer_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(user_a_address.clone()),
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // Verify User A balance
  let _balances = CommandBuilder::new("--regtest --index-runes balances")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<Balances>();

  // ...

  // 5. Blacklist User A
  // Authority creates a transaction with Blacklist tag
  // Needs to prove authority (Input 0 must be authority)

  // We need to fund Authority again.
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_2, _) = utxos
    .iter()
    .find(|(outpoint, _)| {
      outpoint.txid != fund_txid && outpoint.txid != etch_txid && outpoint.txid != transfer_txid
    })
    .expect("Should find a fresh coinbase");
  let (block_2, tx_2) = core.tx_index(coinbase_outpoint_2.txid);

  let fund_auth_2 = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_2,
      tx_2,
      coinbase_outpoint_2.vout as usize,
      Witness::new(),
    )],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));
  let (fund_block_2, fund_tx_idx_2) = core.tx_index(fund_auth_2);

  // Authority blacklists User A
  let user_a_xonly = &user_a_script.as_bytes()[2..]; // Skip 51 20

  let blacklist_runestone = Runestone {
    mint: Some(rune_id),
    authority: Some(ordinals::AuthorityUpdates {
      blacklist: Some(vec![entry(CompactScriptKind::P2TR, user_a_xonly)]),
      ..default()
    }),
    ..default()
  };

  let _blacklist_txid = core.broadcast_tx(TransactionTemplate {
    inputs: &[(fund_block_2, fund_tx_idx_2, 0, Witness::default())], // Authority as Input 0
    outputs: 1,
    op_return: Some(blacklist_runestone.encipher()),
    op_return_index: Some(0),
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 6. User A tries to send to User B -> Should FAIL (Runes burned)
  let user_b_script =
    ScriptBuf::from_hex("51202222222222222222222222222222222222222222222222222222222222222222")
      .unwrap();
  let user_b_address = Address::from_script(&user_b_script, Network::Regtest).unwrap();

  // User A's UTXO is output 1 of transfer_txid
  let user_a_outpoint = OutPoint {
    txid: transfer_txid,
    vout: 1,
  };

  let (transfer_block, transfer_tx_idx) = core.tx_index(transfer_txid);

  let send_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 100,
      output: 1,
    }],
    ..default()
  };

  core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      transfer_block,
      transfer_tx_idx,
      user_a_outpoint.vout as usize,
      Witness::default(),
    )],
    outputs: 2,
    op_return: Some(send_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(user_b_address.clone()),
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 7. Verify User B received NOTHING (Burned)
  // We can check User B's address balance if we had a way to index it by address.
  // But we can check total supply (burned should increase).
  // Or check if there is an event?

  let runes_output = CommandBuilder::new("--regtest --index-runes runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::runes::Output>();

  let rune = Rune(RUNE);
  if let Some(rune_info) = runes_output.runes.get(&rune) {
    assert_eq!(
      rune_info.burned, 0,
      "Blacklisted transfers should be rejected without burning balances"
    );
  } else {
    panic!(
      "Rune {} not found in output. Available runes: {:?}",
      rune,
      runes_output.runes.keys()
    );
  }
}

#[test]
fn test_blacklisting_cant_receive() {
  let core = mockcore::builder().network(Network::Regtest).build();
  let ord = TestServer::spawn_with_server_args(&core, &["--regtest", "--index-runes"], &[]);
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

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

  // 3. Etch Rune
  let runestone = Runestone {
    etching: Some(ordinals::Etching {
      rune: Some(Rune(RUNE)),
      divisibility: Some(0),
      premine: Some(1000),
      symbol: Some('¢'),
      terms: Some(ordinals::Terms {
        allow_blacklisting: true,
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

  let (etch_block, etch_tx_idx) = core.tx_index(etch_txid);
  let rune_id = RuneId {
    block: etch_block as u64,
    tx: etch_tx_idx as u32,
  };

  // 4. Blacklist a Victim Address (User A)
  let user_a_script =
    ScriptBuf::from_hex("51201111111111111111111111111111111111111111111111111111111111111111")
      .unwrap();
  let user_a_address = Address::from_script(&user_a_script, Network::Regtest).unwrap();

  // Fund authority again for blacklisting tx
  let utxos = core.state().utxos.clone();
  let (coinbase_outpoint_2, _) = utxos
    .iter()
    .find(|(outpoint, _)| outpoint.txid != fund_txid && outpoint.txid != etch_txid)
    .expect("Should find a fresh coinbase");
  let (block_2, tx_2) = core.tx_index(coinbase_outpoint_2.txid);

  let fund_auth_2 = core.broadcast_tx(TransactionTemplate {
    inputs: &[(
      block_2,
      tx_2,
      coinbase_outpoint_2.vout as usize,
      Witness::new(),
    )],
    recipient: Some(authority_address.clone()),
    outputs: 1,
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));
  let (fund_block_2, fund_tx_idx_2) = core.tx_index(fund_auth_2);

  let user_a_xonly = &user_a_script.as_bytes()[2..];
  let blacklist_runestone = Runestone {
    mint: Some(rune_id),
    authority: Some(ordinals::AuthorityUpdates {
      blacklist: Some(vec![entry(CompactScriptKind::P2TR, user_a_xonly)]),
      ..default()
    }),
    ..default()
  };

  core.broadcast_tx(TransactionTemplate {
    inputs: &[(fund_block_2, fund_tx_idx_2, 0, Witness::default())],
    outputs: 1,
    op_return: Some(blacklist_runestone.encipher()),
    op_return_index: Some(0),
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 5. Try to Send Runes TO User A via Edict
  // Authority has 1000 runes.
  // Authority (etch_txid:1) -> User A (edict)

  let send_runestone = Runestone {
    edicts: vec![Edict {
      id: rune_id,
      amount: 100,
      output: 1,
    }],
    ..default()
  };

  core.broadcast_tx(TransactionTemplate {
    inputs: &[(etch_block, etch_tx_idx, 1, Witness::default())],
    outputs: 2,
    op_return: Some(send_runestone.encipher()),
    op_return_index: Some(0),
    recipient: Some(user_a_address.clone()),
    ..default()
  });
  core.mine_blocks(u64::from(Runestone::COMMIT_CONFIRMATIONS));

  // 6. Verify User A received NOTHING (Burned)
  let runes_output = CommandBuilder::new("--regtest --index-runes runes")
    .core(&core)
    .ord(&ord)
    .run_and_deserialize_output::<ord::subcommand::runes::Output>();

  let rune = Rune(RUNE);
  if let Some(rune_info) = runes_output.runes.get(&rune) {
    assert_eq!(
      rune_info.burned, 0,
      "Sending to a blacklisted address should be rejected without burning runes"
    );
  } else {
    panic!("Rune {} not found", rune);
  }
}

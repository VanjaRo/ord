use {
  super::*,
  bitcoin::{
    Amount, TxOut,
    blockdata::{
      locktime::absolute::LockTime,
      script::{self, Instruction, PushBytes, PushBytesBuf},
    },
    transaction::Version,
  },
  ordinals::{
    self, Artifact, AuthorityBits, AuthorityUpdates, CompactScript, CompactScriptKind, Etching,
    Rune, Runestone, SetAuthority, Terms,
  },
  pretty_assertions::assert_eq,
};

const TAG_FLAGS: u128 = 2;
const TAG_RUNE: u128 = 4;
const TAG_SET_AUTHORITY: u128 = 101;
const TAG_BLACKLIST: u128 = 103;
const TAG_UNBLACKLIST: u128 = 105;
const TAG_ADD_MINTER: u128 = 107;
const TAG_REMOVE_MINTER: u128 = 109;
const TAG_ALLOW_MINTING: u128 = 111;
const TAG_ALLOW_BLACKLISTING: u128 = 113;

fn tx_with_runestone(runestone: &Runestone) -> Transaction {
  Transaction {
    input: Vec::new(),
    output: vec![TxOut {
      script_pubkey: runestone.encipher(),
      value: Amount::from_sat(0),
    }],
    lock_time: LockTime::ZERO,
    version: Version(2),
  }
}

fn tx_from_integers(integers: &[u128]) -> Transaction {
  let mut payload = Vec::new();
  for integer in integers {
    ordinals::varint::encode_to_vec(*integer, &mut payload);
  }
  let push = PushBytesBuf::try_from(payload).unwrap();
  let script = script::Builder::new()
    .push_opcode(opcodes::all::OP_RETURN)
    .push_opcode(Runestone::MAGIC_NUMBER)
    .push_slice(push)
    .into_script();

  Transaction {
    input: Vec::new(),
    output: vec![TxOut {
      script_pubkey: script,
      value: Amount::from_sat(0),
    }],
    lock_time: LockTime::ZERO,
    version: Version(2),
  }
}

fn decode_varints(bytes: &[u8]) -> Vec<u128> {
  let mut integers = Vec::new();
  let mut index = 0;
  while index < bytes.len() {
    let mut value: u128 = 0;
    let mut shift = 0u32;
    loop {
      let byte = bytes[index];
      index += 1;
      value |= u128::from(byte & 0x7f) << shift;
      if byte & 0x80 == 0 {
        break;
      }
      shift += 7;
      assert!(shift < 128, "varint exceeds width");
      assert!(index < bytes.len(), "unterminated varint");
    }
    integers.push(value);
  }
  integers
}

fn runestone_varints(runestone: &Runestone) -> Vec<u128> {
  let script = runestone.encipher();
  let mut after_magic = false;
  let mut payload = Vec::new();

  for instruction in script.instructions() {
    let instruction = instruction.expect("valid instruction");
    match instruction {
      Instruction::Op(opcodes::all::OP_RETURN) => {}
      Instruction::Op(op) if op == Runestone::MAGIC_NUMBER => after_magic = true,
      Instruction::PushBytes(bytes) if after_magic => payload.extend_from_slice(bytes.as_bytes()),
      Instruction::Op(_) if after_magic => break,
      _ => {}
    }
  }

  assert!(after_magic, "runestone magic opcode missing");
  decode_varints(&payload)
}

fn entry(kind: CompactScriptKind, body: &[u8]) -> Vec<u8> {
  let mut data = Vec::with_capacity(1 + body.len());
  data.push(kind as u8);
  data.extend_from_slice(body);
  data
}

fn push_pair(ints: &mut Vec<u128>, tag: u128, value: u128) {
  ints.push(tag);
  ints.push(value);
}

fn push_set_authority(ints: &mut Vec<u128>, authority_bits: u8, script: &[u8]) {
  ints.push(TAG_SET_AUTHORITY);
  ints.push(authority_bits.into());
  ints.push(TAG_SET_AUTHORITY);
  ints.push(script.len() as u128);
  for byte in script {
    ints.push(TAG_SET_AUTHORITY);
    ints.push((*byte).into());
  }
}

fn push_list_entry(ints: &mut Vec<u128>, tag: u128, kind: CompactScriptKind, body: &[u8]) {
  ints.push(tag);
  ints.push(kind as u128);
  ints.push(tag);
  ints.push(body.len() as u128);
  for byte in body {
    ints.push(tag);
    ints.push((*byte).into());
  }
}

#[test]
fn authority_tags_encipher_matches_spec() {
  let mint_body = vec![11u8; 32];
  let blacklist_body = vec![22u8; 32];
  let unblacklist_body = vec![33u8; 32];
  let remove_minter_body = vec![44u8; 32];
  let authority_body = vec![55u8; 32];

  let runestone = Runestone {
    edicts: Vec::new(),
    etching: Some(Etching {
      rune: Some(Rune(777)),
      terms: Some(Terms {
        allow_minting: true,
        allow_blacklisting: true,
        ..default()
      }),
      ..default()
    }),
    mint: None,
    pointer: None,
    set_authority: Some(SetAuthority {
      authorities: AuthorityBits::from(0b011),
      script_pubkey_compact: authority_body.clone(),
    }),
    authority: Some(AuthorityUpdates {
      blacklist: Some(vec![entry(CompactScriptKind::P2TR, &blacklist_body)]),
      unblacklist: Some(vec![entry(CompactScriptKind::P2TR, &unblacklist_body)]),
      add_minter: Some(vec![entry(CompactScriptKind::P2TR, &mint_body)]),
      remove_minter: Some(vec![entry(CompactScriptKind::P2TR, &remove_minter_body)]),
    }),
  };

  let integers = runestone_varints(&runestone);

  let mut expected = Vec::new();
  let flags = 1 | (1 << 1); // Etching + Terms
  push_pair(&mut expected, TAG_FLAGS, flags);
  push_pair(&mut expected, TAG_RUNE, 777);
  push_pair(&mut expected, TAG_ALLOW_MINTING, 1);
  push_pair(&mut expected, TAG_ALLOW_BLACKLISTING, 1);
  push_set_authority(&mut expected, 0b011, &authority_body);
  push_list_entry(
    &mut expected,
    TAG_BLACKLIST,
    CompactScriptKind::P2TR,
    &blacklist_body,
  );
  push_list_entry(
    &mut expected,
    TAG_UNBLACKLIST,
    CompactScriptKind::P2TR,
    &unblacklist_body,
  );
  push_list_entry(
    &mut expected,
    TAG_ADD_MINTER,
    CompactScriptKind::P2TR,
    &mint_body,
  );
  push_list_entry(
    &mut expected,
    TAG_REMOVE_MINTER,
    CompactScriptKind::P2TR,
    &remove_minter_body,
  );

  assert_eq!(integers, expected);

  let tx = tx_with_runestone(&runestone);
  let Artifact::Runestone(decoded) = Runestone::decipher(&tx).unwrap() else {
    panic!("expected runestone artifact");
  };

  assert_eq!(decoded, runestone);
}

#[test]
fn set_authority_length_overflow_is_ignored() {
  // script length is 33 bytes (larger than supported P2TR body)
  let mut ints = vec![TAG_SET_AUTHORITY, 1, TAG_SET_AUTHORITY, 33];
  for _ in 0..33 {
    ints.push(TAG_SET_AUTHORITY);
    ints.push(0);
  }

  let tx = tx_from_integers(&ints);
  let artifact = Runestone::decipher(&tx).unwrap();

  let Artifact::Runestone(runestone) = artifact else {
    panic!("expected runestone artifact");
  };

  assert!(runestone.set_authority.is_none());
}

#[test]
fn blacklist_entry_without_body_is_ignored() {
  // declare a body of length 32 but provide only the kind (missing bytes)
  let tx = tx_from_integers(&[
    TAG_BLACKLIST,
    CompactScriptKind::P2TR as u128,
    TAG_BLACKLIST,
    32,
  ]);
  let artifact = Runestone::decipher(&tx).unwrap();

  let Artifact::Runestone(runestone) = artifact else {
    panic!("expected runestone artifact");
  };

  assert!(
    runestone
      .authority
      .as_ref()
      .and_then(|auth| auth.blacklist.as_ref())
      .map(|entries| entries.is_empty())
      .unwrap_or(true)
  );
}

#[test]
fn blacklist_entry_with_body_too_long_is_ignored() {
  // 33 byte payload should be ignored entirely
  let mut ints = vec![
    TAG_BLACKLIST,
    CompactScriptKind::P2TR as u128,
    TAG_BLACKLIST,
    33,
  ];
  for _ in 0..33 {
    ints.push(TAG_BLACKLIST);
    ints.push(0);
  }

  let tx = tx_from_integers(&ints);
  let artifact = Runestone::decipher(&tx).unwrap();

  let Artifact::Runestone(runestone) = artifact else {
    panic!("expected runestone artifact");
  };

  assert!(
    runestone
      .authority
      .as_ref()
      .and_then(|auth| auth.blacklist.as_ref())
      .map(|entries| entries.is_empty())
      .unwrap_or(true)
  );
}

#[test]
fn test_compact_script_p2tr() {
  let x_only = [42u8; 32];
  let script = script::Builder::new().push_opcode(opcodes::all::OP_PUSHNUM_1);
  let push: &PushBytes = x_only.as_slice().try_into().unwrap();
  let script = script.push_slice(push).into_script();

  let compact = CompactScript::try_from_script(&script).unwrap();
  assert_eq!(compact.kind, CompactScriptKind::P2TR);
  assert_eq!(compact.body, x_only.to_vec());

  let reconstructed = compact.to_script().unwrap();
  assert_eq!(reconstructed, script);
}

#[test]
fn test_compact_script_invalid_script() {
  let invalid_script = script::Builder::new()
    .push_opcode(opcodes::all::OP_DUP)
    .into_script();

  assert!(CompactScript::try_from_script(&invalid_script).is_none());
}

#[test]
fn test_terms_authority_defaults() {
  let terms = Terms::default();
  assert!(!terms.allow_minting);
  assert!(!terms.allow_blacklisting);
}

#[test]
fn test_runestone_encipher_with_authority_flags() {
  let runestone = Runestone {
    edicts: Vec::new(),
    etching: Some(Etching {
      rune: Some(Rune(100)),
      terms: Some(Terms {
        allow_minting: true,
        ..default()
      }),
      ..default()
    }),
    mint: None,
    pointer: None,
    set_authority: None,
    authority: None,
  };

  let script = runestone.encipher();

  let artifact = Runestone::decipher(&Transaction {
    input: Vec::new(),
    output: vec![TxOut {
      script_pubkey: script,
      value: Amount::from_sat(0),
    }],
    lock_time: LockTime::ZERO,
    version: Version(2),
  })
  .unwrap();

  if let Artifact::Runestone(deciphered) = artifact {
    let etching = deciphered.etching.unwrap();
    let terms = etching.terms.unwrap();
    assert!(terms.allow_minting);
    assert!(!terms.allow_blacklisting);
  } else {
    panic!("Expected Runestone artifact");
  }
}

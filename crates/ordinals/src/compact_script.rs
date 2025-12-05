use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Clone, Copy, Eq, Serialize, Deserialize)]
pub enum CompactScriptKind {
  P2TR = 0,
  P2WPKH = 1,
  P2WSH = 2,
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Deserialize)]
pub struct CompactScript {
  pub kind: CompactScriptKind,
  pub body: Vec<u8>,
}

impl CompactScript {
  pub fn try_from_script(script: &ScriptBuf) -> Option<Self> {
    let bytes = script.as_bytes();

    // P2TR: OP_1 <32 bytes>
    if bytes.len() == 34 && bytes[0] == opcodes::all::OP_PUSHNUM_1.to_u8() && bytes[1] == 32 {
      return Some(Self {
        kind: CompactScriptKind::P2TR,
        body: bytes[2..].to_vec(),
      });
    }

    // P2WPKH / P2WSH: OP_0 <len> <body>
    if bytes.len() >= 2 && bytes[0] == opcodes::all::OP_PUSHBYTES_0.to_u8() {
      let body_len = usize::from(bytes[1]);
      if body_len <= 32 && bytes.len() == 2 + body_len {
        let body = bytes[2..].to_vec();
        return match body_len {
          20 => Some(Self {
            kind: CompactScriptKind::P2WPKH,
            body,
          }),
          32 => Some(Self {
            kind: CompactScriptKind::P2WSH,
            body,
          }),
          _ => None,
        };
      }
    }

    None
  }

  pub fn to_script(&self) -> Option<ScriptBuf> {
    if self.body.is_empty() || self.body.len() > 32 {
      return None;
    }

    fn push_body(body: &[u8], mut builder: script::Builder) -> Option<ScriptBuf> {
      let Ok(push): Result<&script::PushBytes, _> = body.try_into() else {
        return None;
      };
      builder = builder.push_slice(push);
      Some(builder.into_script())
    }

    match self.kind {
      CompactScriptKind::P2TR => push_body(
        &self.body,
        script::Builder::new().push_opcode(opcodes::all::OP_PUSHNUM_1),
      ),
      CompactScriptKind::P2WPKH | CompactScriptKind::P2WSH => push_body(
        &self.body,
        script::Builder::new().push_opcode(opcodes::all::OP_PUSHBYTES_0),
      ),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn p2tr_roundtrip() {
    let x_only = [0u8; 32];
    let builder = script::Builder::new().push_opcode(opcodes::all::OP_PUSHNUM_1);
    let push: &script::PushBytes = x_only.as_slice().try_into().unwrap();
    let script = builder.push_slice(push).into_script();

    let compact = CompactScript::try_from_script(&script).unwrap();
    assert_eq!(compact.kind, CompactScriptKind::P2TR);
    assert_eq!(compact.body, x_only);

    let reconstructed = compact.to_script().unwrap();
    assert_eq!(reconstructed, script);
  }

  #[test]
  fn p2wpkh_roundtrip() {
    let hash = [1u8; 20];
    let builder = script::Builder::new().push_opcode(opcodes::all::OP_PUSHBYTES_0);
    let push: &script::PushBytes = hash.as_slice().try_into().unwrap();
    let script = builder.push_slice(push).into_script();

    let compact = CompactScript::try_from_script(&script).unwrap();
    assert_eq!(compact.kind, CompactScriptKind::P2WPKH);
    assert_eq!(compact.body, hash);

    let reconstructed = compact.to_script().unwrap();
    assert_eq!(reconstructed, script);
  }

  #[test]
  fn p2wsh_roundtrip() {
    let hash = [2u8; 32];
    let builder = script::Builder::new().push_opcode(opcodes::all::OP_PUSHBYTES_0);
    let push: &script::PushBytes = hash.as_slice().try_into().unwrap();
    let script = builder.push_slice(push).into_script();

    let compact = CompactScript::try_from_script(&script).unwrap();
    assert_eq!(compact.kind, CompactScriptKind::P2WSH);
    assert_eq!(compact.body, hash);

    let reconstructed = compact.to_script().unwrap();
    assert_eq!(reconstructed, script);
  }

  #[test]
  fn unsupported_script_returns_none() {
    let invalid_script = script::Builder::new()
      .push_opcode(opcodes::all::OP_PUSHNUM_1)
      .push_slice([0; 33])
      .into_script();

    assert!(CompactScript::try_from_script(&invalid_script).is_none());
  }

  #[test]
  fn to_script_rejects_invalid_body() {
    let compact = CompactScript {
      kind: CompactScriptKind::P2TR,
      body: Vec::new(),
    };

    assert!(compact.to_script().is_none());
  }

  #[test]
  fn try_from_script_rejects_invalid_witness_length() {
    for len in [1usize, 19, 21, 31, 33] {
      let mut bytes = vec![opcodes::all::OP_PUSHBYTES_0.to_u8(), len as u8];
      bytes.extend(std::iter::repeat(0xAA).take(len));
      let script = ScriptBuf::from_bytes(bytes);

      assert!(CompactScript::try_from_script(&script).is_none());
    }
  }
}

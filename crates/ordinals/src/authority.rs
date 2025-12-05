use serde::{Deserialize, Serialize};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityKind {
  Mint,
  Blacklist,
  Master,
}

impl AuthorityKind {
  pub const fn mask(self) -> u8 {
    match self {
      AuthorityKind::Mint => 1 << 0,
      AuthorityKind::Blacklist => 1 << 1,
      AuthorityKind::Master => 1 << 2,
    }
  }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuthorityBits(u8);

impl AuthorityBits {
  pub const fn empty() -> Self {
    Self(0)
  }

  pub const fn bits(self) -> u8 {
    self.0 & 0b111
  }

  pub const fn contains(self, kind: AuthorityKind) -> bool {
    self.bits() & kind.mask() != 0
  }

  pub fn insert(&mut self, kind: AuthorityKind) {
    self.0 |= kind.mask();
  }

  pub fn extend(mut self, kind: AuthorityKind) -> Self {
    self.insert(kind);
    self
  }

  pub const fn is_empty(self) -> bool {
    self.bits() == 0
  }

  pub const fn from_bits_truncated(bits: u8) -> Self {
    Self(bits & 0b111)
  }

  pub fn kinds(self) -> impl Iterator<Item = AuthorityKind> {
    [
      AuthorityKind::Mint,
      AuthorityKind::Blacklist,
      AuthorityKind::Master,
    ]
    .into_iter()
    .filter(move |kind| self.contains(*kind))
  }
}

impl From<u8> for AuthorityBits {
  fn from(bits: u8) -> Self {
    Self::from_bits_truncated(bits)
  }
}

impl From<AuthorityBits> for u8 {
  fn from(bits: AuthorityBits) -> Self {
    bits.bits()
  }
}

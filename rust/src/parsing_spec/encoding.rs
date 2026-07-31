use std::num::NonZero;

use crate::regex::Regex;

/// Index in the parsing specification, offset by/starting at 1.
/// `Option<EncodingIdx>` is ABI equivalent to `u16` (for FFI);
/// `None`/`0` represents no encoding.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(transparent)]
pub struct EncodingIdx(NonZero<u16>);

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Encoding {
	pub name: String,
	pub regex: Regex,
}

impl From<EncodingIdx> for NonZero<u16> {
	fn from(encoding_idx: EncodingIdx) -> Self {
		encoding_idx.0
	}
}

impl From<EncodingIdx> for u16 {
	fn from(encoding_idx: EncodingIdx) -> Self {
		encoding_idx.0.get()
	}
}

impl From<NonZero<u16>> for EncodingIdx {
	fn from(encoding_idx: NonZero<u16>) -> Self {
		Self(encoding_idx)
	}
}

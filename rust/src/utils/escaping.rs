use std::str::Chars;

/// A wrapper around a `char` that implements [`Display`] (and consequently [`ToString`])
/// by escaping special characters with backslash and a control character:
///
/// - backslash (`\\`),
/// - ASCII whitespace (`\t`, `\r`, `\n`),
/// - non-printable ASCII characters (outside the range `0x20..0x7E`)
///   and non-ASCII Unicode characters (e.g. `\u{80}`),
/// - single and double quotes (`\'`, `\"`).
///
/// Currently, the implementation simply calls [`char::escape_default`],
/// but this struct still exists as a layer of abstraction.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct Escaped {
	ch: char,
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum InvalidEscape {
	/// Reached EOF after the escaping backslash (empty input).
	Eof,
	/// Malformed Unicode code point (as hexadecimal digit pairs).
	MalformedCodePoint,
	/// Unknown escape control character.
	Unknown(char),
	/// Invalid Unicode code point.
	BadCodePoint(u32),
}

impl Escaped {
	/// Construct a new `Escaped` value.
	pub fn escape(ch: char) -> Self {
		Self { ch }
	}

	/// Almost the inverse of `Escaped::escape(ch).to_string()`;
	/// assumes the escaping backslash has already been encountered,
	/// and switches on the first character of `input`.
	///
	/// - a (second) backslash for a literal backslash.
	/// - `'` for a literal single quote.
	/// - `"` for a literal double quote.
	/// - `t`, `r`, `n`: tab, carriage return, and newline respectively.
	/// - `u{xx}`, `u{xxyy}`, `u{xxyyzz}` for a Unicode code point in hexadecimal representation.
	///   - Hex digits may be upper or lower case, and must come in pairs.
	pub fn unescape(input: &str) -> Result<(&str, char), InvalidEscape> {
		let mut chars: Chars<'_> = input.chars();

		let Some(ch): Option<char> = chars.next() else {
			return Err(InvalidEscape::Eof);
		};

		let ch: char = match ch {
			' ' | '\\' | '\'' | '"' => ch,
			't' => '\t',
			'r' => '\r',
			'n' => '\n',
			'u' => {
				const MAX_BITS_PER_CODE_POINT: u32 = (char::MAX as u32).ilog2() + 1;
				assert_eq!(MAX_BITS_PER_CODE_POINT, 21);
				const MAX_BYTES_PER_CODE_POINT: u32 = MAX_BITS_PER_CODE_POINT.div_ceil(u8::BITS);
				assert_eq!(MAX_BYTES_PER_CODE_POINT, 3);

				let Some(ch): Option<char> = chars.next() else {
					return Err(InvalidEscape::MalformedCodePoint);
				};
				if ch != '{' {
					return Err(InvalidEscape::MalformedCodePoint);
				}

				let Some(mut code_point): Option<u32> = parse_hex_digit_pair(&mut chars) else {
					return Err(InvalidEscape::MalformedCodePoint);
				};

				for _ in 1..MAX_BYTES_PER_CODE_POINT {
					let backup: Chars<'_> = chars.clone();
					if let Some(byte) = parse_hex_digit_pair(&mut chars) {
						code_point = (code_point << u8::BITS) | byte;
					} else {
						chars = backup;
						break;
					}
				}

				let Some(ch): Option<char> = chars.next() else {
					return Err(InvalidEscape::MalformedCodePoint);
				};
				if ch != '}' {
					return Err(InvalidEscape::MalformedCodePoint);
				}

				let ch: char = char::from_u32(code_point).ok_or(InvalidEscape::BadCodePoint(code_point))?;

				return Ok((chars.as_str(), ch));
			},
			_ => {
				return Err(InvalidEscape::Unknown(ch));
			},
		};

		Ok((chars.as_str(), ch))
	}
}

impl std::fmt::Display for Escaped {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.ch.escape_default().fmt(fmt)
	}
}

fn parse_hex_digit_pair(chars: &mut Chars<'_>) -> Option<u32> {
	if let Some(upper) = chars.next()
		&& let Some(lower) = chars.next()
	{
		match (upper.to_digit(16), lower.to_digit(16)) {
			(Some(upper), Some(lower)) => {
				return Some((upper << 4) + lower);
			},
			_ => (),
		}
	}

	None
}

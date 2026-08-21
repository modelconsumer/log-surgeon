mod pattern_parsing;

use std::num::NonZero;
use std::sync::Arc;

use nom::error::ErrorKind as NomErrorKind;
pub use pattern_parsing::RegexPlaceholderLookup;

use crate::parsing_spec::SubRule;
use crate::utils::DeepClone;
use crate::utils::Escaped;

/// Meta-characters that must be escaped, aside from inside bracketed ranges.
const SPECIAL_CHARACTERS: &str = r"\()[]{}*+?.|^$";

/// Meta-characters that must be escaped inside bracketed ranges.
const SPECIAL_CHARACTERS_IN_BRACKETED_RANGES: &str = r"\[]";

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnchoredRegex {
	pub anchor_before: bool,
	pub anchor_after: bool,
	pub regex: Regex,
	/// Total "captures" in the regex - total [`Regex::Capture`]s **plus 1**
	/// for the implicit capture of the entire regex.
	pub total_captures: NonZero<u16>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "&str", into = "String")]
pub enum Regex {
	/// Any character, including newline.
	AnyChar,
	Literal(char),
	/// If a placeholder contains a regex capture/[`SubRule`],
	/// each substitution of the placeholder should be a unique sub-rule.
	/// In other words, we must deep clone the regex of the placeholder when substituting.
	Capture(DeepClone<Arc<SubRule>>),
	BracketedRanges {
		negated: bool,
		items: Vec<(char, char)>,
	},
	KleeneClosure(Box<Regex>),
	/// In terms of matching (e.g. when building the NFA),
	/// this variant is equivalent to ("desugars as") a sequence of
	/// the inner item and the Kleene closure of the item
	/// (see [`Regex::wrap_as_desugared_kleene_plus`]).
	///
	/// However, when parsing patterns, we store `(item)+` explicitly so we can (re)serialize it
	/// without the "desguaring".
	KleenePlus(Box<Regex>),
	BoundedRepetition {
		min: u32,
		max: u32,
		item: Box<Regex>,
	},
	Sequence(Vec<Regex>),
	Alternation(Vec<Regex>),
	/// No effect on string matching;
	/// this variant is for "remembering" the original structure/definition
	/// of a pattern from a [`crate::parsing_spec::ParsingSpec`].
	Placeholder {
		name: Arc<str>,
		item: Box<Regex>,
	},
}

impl std::fmt::Debug for Regex {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(&self.to_pattern().escape_default(), fmt)
	}
}

// These fields are not currently explicitly used,
// but are relevant in the `Debug` implementation
// and would be needed to provide better (more specific)
// error messages in the future.
#[derive(Debug)]
pub struct RegexError {
	pub consumed: String,
	pub remaining: String,
	pub kind: RegexErrorKind,
}

/// Note: Most errors are parsing/syntax errors, but not all.
/// Strictly speaking, we could separate them, but would probably be excessive right now.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RegexErrorKind {
	/// Expected a certain character, e.g. '<' after '?' in a capture.
	ExpectedChar(char),
	/// Missing the closing delimiter for the following pair.
	ExpectedClose(char, char),
	/// "General" error kind, e.g. an isolated repetition suffix operator (e.g. the pattern "*").
	InvalidTerm,
	/// Expected a literal character in a bracketed expression.
	ExpectedLiteralInBracketedExpression,
	/// An empty bracketed expression "[]";
	/// not meaningful since it corresponds to an empty set of characters.
	/// Note that "[^]" is allowed, being equivalent to the wildcard ".".
	EmptyBrackets,
	/// Bracket range `min > max` (semantic error).
	InvalidBracketRange(char, char),
	/// Invalid escape character.
	InvalidEscape,
	/// Invalid repetition bound; `min > max` or `max == 0` (semantic error, like `InvalidBracketRange`).
	InvalidRepetitionBound(u32, u32),
	/// Too large of a repetition bound
	/// (~implementation detail/restriction; repetition bounds are stored as `u32`).
	NumberTooBig,
	/// Expected decimal digits (for repetition bound) (syntax error).
	ExpectedDecimalDigits,
	/// Expected hex digits (for unicode escape) (syntax error).
	ExpectedHexDigits,
	/// Invalid code point in unicode escape (semantic error).
	InvalidCodePoint(u32),
	/// Invalid capture name; only letters, numbers, and underscores allowed.
	InvalidCaptureName,
	/// Too many captures (implementation detail/restriction).
	TooManyCaptures,
	/// An escape class (e.g. "\\d") was used as the start/end point of a bracket range.
	EscapeClassInBracketRange,
	/// An inverted escape class (e.g. "\\D") was used inside a bracketed expression.
	InvertedEscapeClassInBrackets,
	/// Used for parsing a non-special character (`negate == true`)
	/// and for parsing an escaped special character (`negate == false`).
	/// This shouldn't actually bubble up publicly;
	/// it'll either get consumed by/turned into `ExpectedLiteralInBracketedExpression` or `InvalidTerm`,
	/// but exists because 1. it models "what's happening", and 2. it's useful for debugging.
	ExpectedOneOf { characters: &'static str, negate: bool },
	/// No definition for placeholder.
	/// TODO: this could be `Arc<str>`?
	UndefinedPlaceholder(String),
	/// A (sub)expression of a regex can match an empty string,
	/// which isn't meaningful for parsing.
	/// See [`Regex::is_nullable`].
	NullableExpression(Box<Regex>),
	/// An error from nom; should be caught/never bubble up,
	/// but used to implement the trait [`nom::error::ParseError`],
	/// and useful for debugging/failing gracefully.
	Nom(NomErrorKind),
}

impl std::fmt::Display for Regex {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.to_pattern().escape_default().fmt(fmt)
	}
}

impl TryFrom<&str> for AnchoredRegex {
	type Error = RegexError;

	fn try_from(pattern: &str) -> Result<Self, Self::Error> {
		AnchoredRegex::from_pattern_with_placeholders(pattern, "", &mut ())
	}
}

impl From<AnchoredRegex> for String {
	fn from(regex: AnchoredRegex) -> Self {
		regex.to_pattern()
	}
}

impl AnchoredRegex {
	pub fn to_pattern(&self) -> String {
		let pattern: String = self.regex.to_pattern();
		let anchor_before: &str = if self.anchor_before { "^" } else { "" };
		let anchor_after: &str = if self.anchor_after { "$" } else { "" };

		format!("{anchor_before}{pattern}{anchor_after}")
	}
}

impl TryFrom<&str> for Regex {
	type Error = RegexError;

	fn try_from(pattern: &str) -> Result<Self, Self::Error> {
		Regex::from_pattern_with_placeholders(pattern, &mut ())
	}
}

impl From<Regex> for String {
	fn from(regex: Regex) -> Self {
		regex.to_pattern()
	}
}

impl Regex {
	/// An "invalid" `Regex` value; conceptually (and literally) it matches no strings,
	/// so it's not meaningful in an actual parsing spec.
	///
	/// Currently, only used during pattern parsing for [`Regex::Placeholder`],
	/// where the actual placeholder value will be filled in later.
	pub const NIL: Self = Self::Alternation(Vec::new());

	/// String representation/"to pattern" conversion of this regex,
	/// replacing any leading or trailing space literal with `[ ]` for explicitness.
	pub fn to_pattern(&self) -> String {
		let mut pattern: String = self.to_pattern_internal();

		if let Some(suffix) = pattern.strip_prefix(' ') {
			pattern = format!("[ ]{suffix}");
		}
		if let Some(prefix) = pattern.strip_suffix(' ') {
			pattern = format!("{prefix}[ ]");
		}

		// Other whitespace should always be escaped.
		assert!(!pattern.starts_with(|ch: char| ch.is_whitespace()));
		assert!(!pattern.ends_with(|ch: char| ch.is_whitespace()));

		pattern
	}

	/// Direct string representation/"to pattern" conversion of this regex.
	fn to_pattern_internal(&self) -> String {
		match self {
			Self::AnyChar => ".".to_owned(),
			&Self::Literal(ch) => {
				if SPECIAL_CHARACTERS.contains(ch) {
					format!("\\{ch}")
				} else {
					Escaped::escape(ch).to_string()
				}
			},
			Self::BracketedRanges { negated, items } => {
				fn escape(ch: char, buffer: &mut String) {
					if SPECIAL_CHARACTERS_IN_BRACKETED_RANGES.contains(ch) {
						buffer.push('\\');
						buffer.push(ch);
					} else if ch == '-' {
						// This is needed, for example, for `[a\-z]` as 3 characters, but not `[a-]`.
						// However, we always escape it for simplicity and clarity.
						buffer.push_str("\\-");
					} else {
						buffer.push_str(&Escaped::escape(ch).to_string());
					}
				}

				let negation: &str = if *negated { "^" } else { "" };
				let serialized: String = items.iter().fold(String::new(), |mut accumulated, &(lo, hi)| {
					escape(lo, &mut accumulated);
					if lo != hi {
						accumulated.push('-');
						escape(hi, &mut accumulated);
					}
					accumulated
				});
				format!("[{negation}{serialized}]")
			},
			Self::Capture(sub_rule) => {
				format!("(?<{}>{})", sub_rule.name, sub_rule.regex.to_pattern_internal())
			},
			Self::Placeholder { name, .. } => {
				format!("(?<{}>)", name)
			},
			Self::KleeneClosure(item) => {
				format!("{}*", self.surround(item))
			},
			Self::KleenePlus(item) => {
				format!("{}+", self.surround(item))
			},
			Self::BoundedRepetition { min, max, item } => {
				let item_pattern: String = self.surround(item);
				if (*min, *max) == (0, 1) {
					format!("{item_pattern}?")
				} else if min == max {
					format!("{item_pattern}{{{min}}}")
				} else {
					format!("{item_pattern}{{{min},{max}}}")
				}
			},
			Self::Sequence(items) => items.iter().fold(String::new(), |mut accumulated, item| {
				accumulated.push_str(&self.surround(item));
				accumulated
			}),
			Self::Alternation(items) => {
				// There should be at least one alternative.
				let first: &Self = items.first().unwrap();
				items[1..]
					.iter()
					.fold(first.to_pattern_internal(), |mut accumulated, item| {
						accumulated.push('|');
						accumulated.push_str(&self.surround(item));
						accumulated
					})
			},
		}
	}

	/// Parenthesizes a subexpression if necessary; see [`Regex::precedence`].
	fn surround(&self, item: &Self) -> String {
		let sub_pattern: String = item.to_pattern_internal();
		if item.precedence() < self.precedence() {
			format!("({sub_pattern})")
		} else {
			sub_pattern
		}
	}

	/// Lower value is lower precedence;
	/// if an expression contains a subexpression with strictly lower precedence,
	/// the subexpression must be parenthesized,
	/// except for a capture, which is "already" parenthesized.
	fn precedence(&self) -> isize {
		match self {
			Self::Alternation(_) => 0,
			Self::Sequence(_) => 1,
			Self::KleeneClosure(_) | Self::KleenePlus(_) | Self::BoundedRepetition { .. } => 2,
			Self::Capture { .. }
			| Self::Placeholder { .. }
			| Self::AnyChar
			| Self::Literal(_)
			| Self::BracketedRanges { .. } => 3,
		}
	}
}

impl Regex {
	/// "Desugars" a pattern `(self)+` as `(self)(self)*`;
	/// see [`Regex::KleenePlus`] for more details.
	pub fn wrap_as_desugared_kleene_plus(&self) -> Self {
		Self::Sequence(vec![self.clone(), Self::KleeneClosure(Box::new(self.clone()))])
	}

	/// Whether this regex accepts an empty string;
	/// if so, return the first (minimal) child that is nullable,
	/// for diagnostics/reporting.
	///
	/// An empty should not be parsable,
	/// but repetition suffixes allow for empty matches.
	///
	/// ```rust
	/// use log_surgeon::regex::RegexErrorKind;
	/// use log_surgeon::regex::Regex;
	///
	/// assert_eq!(Regex::from_pattern("").unwrap_err().kind, RegexErrorKind::InvalidTerm);
	///
	/// std::assert_matches!(Regex::from_pattern("a*").unwrap_err().kind, RegexErrorKind::NullableExpression(_));
	/// std::assert_matches!(Regex::from_pattern("a{0,3}").unwrap_err().kind, RegexErrorKind::NullableExpression(_));
	///
	/// assert_eq!(Regex::from_pattern("a").unwrap().is_nullable(), None);
	/// ```
	pub fn is_nullable(&self) -> Option<&Self> {
		match self {
			Self::AnyChar | Self::Literal(..) | Self::BracketedRanges { .. } => None,
			Self::Capture(sub_rule) => sub_rule.regex.is_nullable(),
			Self::KleeneClosure(item) => Some(item.is_nullable().unwrap_or(self)),
			Self::KleenePlus(item) => item.is_nullable(),
			Self::BoundedRepetition { min, item, .. } => {
				if *min > 0 {
					item.is_nullable()
				} else {
					Some(item.is_nullable().unwrap_or(self))
				}
			},
			Self::Placeholder { item, .. } => item.is_nullable(),
			Self::Sequence(items) => {
				if items.iter().all(|item| item.is_nullable().is_some()) {
					Some(self)
				} else {
					None
				}
			},
			Self::Alternation(items) => {
				for item in items.iter() {
					if let Some(child) = item.is_nullable() {
						return Some(child);
					}
				}
				None
			},
		}
	}

	/*
	/// If a placeholder contains a regex capture/[`SubRule`],
	/// each substitution of the placeholder should be a unique sub-rule.
	/// In other words, we must deep clone the regex of the placeholder when substituting.
	fn deep_clone(&self) -> Self {
		match self {
			Self::AnyChar | Self::Literal(..) | Self::BracketedRanges { .. } => self.clone(),
			Self::Capture(sub_rule) => Self::Capture(Arc::new(SubRule {
				regex: sub_rule.regex.deep_clone(),
				..(**sub_rule).clone()
			})),
			Self::KleeneClosure(item) => Self::KleeneClosure(Box::new(item.deep_clone())),
			Self::KleenePlus(item) => Self::KleenePlus(Box::new(item.deep_clone())),
			Self::BoundedRepetition { min, max, item } => Self::BoundedRepetition {
				min: *min,
				max: *max,
				item: Box::new(item.deep_clone()),
			},
			Self::Placeholder { name, item } => Self::Placeholder {
				name: name.clone(),
				item: Box::new(item.deep_clone()),
			},
			Self::Sequence(items) => Self::Sequence(items.iter().map(Self::deep_clone).collect::<Vec<_>>()),
			Self::Alternation(items) => Self::Alternation(items.iter().map(Self::deep_clone).collect::<Vec<_>>()),
		}
	}
	*/
}

impl Regex {
	// Post-order DFS; visit children first.
	pub fn for_each_capture<E, F>(&mut self, func: &mut F) -> Result<(), E>
	where
		F: FnMut(&mut SubRule) -> Result<(), E>,
	{
		match self {
			Regex::AnyChar | Regex::Literal(..) | Regex::BracketedRanges { .. } => (),
			Regex::Capture(sub_rule) => {
				let sub_rule: &mut SubRule = Arc::get_mut(sub_rule).unwrap();
				sub_rule.regex.for_each_capture(func)?;
				func(sub_rule)?;
			},
			Regex::KleeneClosure(item)
			| Regex::KleenePlus(item)
			| Regex::BoundedRepetition { item, .. }
			| Regex::Placeholder { item, .. } => {
				item.for_each_capture(func)?;
			},
			Regex::Sequence(items) | Regex::Alternation(items) => {
				for child in items.iter_mut() {
					child.for_each_capture(func)?;
				}
			},
		}
		Ok(())
	}
}

impl std::fmt::Display for RegexError {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Debug::fmt(self, fmt)
	}
}

impl std::error::Error for RegexError {}

use std::sync::Arc;

use crate::dfa::JittedDfa;
use crate::dfa::MatchedRule;
use crate::dfa::TdfaExecution;
use crate::parsing_spec::EncodingIdx;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleIdx;
use crate::utils::utf8::Utf8Chars;

#[derive(Debug, Clone)]
pub struct Lexer {
	/// Strictly speaking, this field is unnecessary;
	/// `Parser` already has the spec and could pass it every time to `Lexer::next_token`.
	/// However, it's cheap and cleaner to clone it here for encapsulation.
	spec: Arc<ParsingSpec>,
	/// Copied out of [`ParsingSpec::jit_engine`]; that engine (owned by the spec) keeps the
	/// JIT-ed code mapped for as long as this lexer's `spec` is alive.
	maybe_jitted_dfa: Option<JittedDfa>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Token<'spec, 'input> {
	Variable {
		rule: &'spec RootRule,
		maybe_encoding_idx: Option<EncodingIdx>,
		lexeme: &'input str,
		has_captures: bool,
	},
	Newline,
	StaticText(&'input str),
	EndOfInput,
}

impl Lexer {
	pub fn new(spec: Arc<ParsingSpec>) -> Self {
		let maybe_jitted_dfa: Option<JittedDfa> = spec.jit_engine().maybe_jitted_dfa();

		Self { spec, maybe_jitted_dfa }
	}

	/// Return the next [`Token`] from `input` starting from `*pos`,
	/// and update `*pos` to index the next character after the returned token.
	///
	/// `dfa_execution` is cached memory/working space for the underlying DFA execution,
	/// and also contains the captured text for sub-rules.
	/// It should be created via [`TdfaExecution::new`].
	pub fn next_token<'spec, 'input>(
		&'spec self,
		input: &'input [u8],
		pos: &mut usize,
		dfa_execution: &mut TdfaExecution,
	) -> Token<'spec, 'input> {
		let start: usize = *pos;

		if start == input.len() {
			return Token::EndOfInput;
		}

		let (input_before, input_remaining): (&[u8], &[u8]) = input.split_at(start);

		let char_before: u32 = u32::from(Utf8Chars::new(input_before).next_back().unwrap_or('\n'));

		if let Some(MatchedRule {
			rule_idx,
			maybe_encoding_idx,
			lexeme,
		}) = self.execute_dfa::<{ cfg!(feature = "jit") }>(input_remaining, char_before)
		{
			let rule: &RootRule = &self.spec[rule_idx];
			assert_eq!(rule.idx, rule_idx);

			// Even if don't call [`Tdfa::execute_with_captures`],
			// we need to clear any possible captures from the previous call to [`Lexer::next_token`].
			dfa_execution.clear();

			let has_captures: bool = rule.has_captures();
			if has_captures {
				let matched: bool = rule.dfa.execute_with_captures(lexeme, dfa_execution, rule);
				assert!(matched);
			}
			*pos += lexeme.len();
			Token::Variable {
				rule,
				maybe_encoding_idx,
				lexeme,
				has_captures,
			}
		} else {
			let mut chars: Utf8Chars<'_> = Utf8Chars::new(&input[start..]);
			// We checked for `start == input.len()` above.
			let first: char = chars.next().unwrap();
			*pos += first.len_utf8();
			if first == '\n' {
				return Token::Newline;
			} else if !self.is_delimiter(first) {
				self.glob_static_text(input, pos);
			}
			// SAFETY: `[start..*pos]` spans whole UTF-8 scalars, decoded and validated above
			// (the first via `Utf8Chars::next`, the rest via `glob_static_text`).
			let static_text: &'input str = unsafe { std::str::from_utf8_unchecked(&input[start..*pos]) };
			Token::StaticText(static_text)
		}
	}

	/// Uniform "interface" to executing a TDFA via the JIT-ed or Rust implementation.
	fn execute_dfa<'input, const JIT: bool>(
		&self,
		input: &'input [u8],
		char_before: u32,
	) -> Option<MatchedRule<'input>> {
		if JIT {
			let input: std::ops::Range<*const u8> = input.as_ptr_range();
			let mut end: *const u8 = std::ptr::null();
			let mut maybe_encoding_idx: Option<EncodingIdx> = None;

			unsafe {
				let rule_idx: RuleIdx = (self.maybe_jitted_dfa.unwrap_unchecked())(
					input.start,
					input.end,
					char_before,
					&mut end,
					&mut maybe_encoding_idx,
				)?;
				let start: *const u8 = input.start;
				let len: isize = end.offset_from(start);
				assert!(len >= 0);
				let bytes: &[u8] = std::slice::from_raw_parts(start, len as usize);
				let lexeme: &str = std::str::from_utf8_unchecked(bytes);
				Some(MatchedRule {
					rule_idx,
					maybe_encoding_idx,
					lexeme,
				})
			}
		} else {
			self.spec.compressed_dfa_for_parsing.execute(input, char_before)
		}
	}

	/// See the [document on parsing](docs/parsing.md).
	fn glob_static_text(&self, input: &[u8], pos: &mut usize) {
		for ch in Utf8Chars::new(&input[*pos..]) {
			if ch == '\n' {
				break;
			}
			*pos += ch.len_utf8();
			if self.is_delimiter(ch) {
				break;
			}
		}
	}

	fn is_delimiter(&self, ch: char) -> bool {
		if let Ok(i) = u8::try_from(ch)
			&& let Some(ch_is_delimiter) = self.spec.ascii_delimiters.get(usize::from(i))
		{
			*ch_is_delimiter
		} else {
			self.spec.non_ascii_delimiters.contains(ch)
		}
	}
}

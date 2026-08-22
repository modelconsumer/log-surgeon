use std::str::Chars;
use std::sync::Arc;

use crate::dfa::Jit;
use crate::dfa::JittedDfa;
use crate::dfa::MatchedRule;
use crate::dfa::TdfaExecution;
use crate::parsing_spec::EncodingIdx;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleIdx;

#[derive(Debug, Clone)]
pub struct Lexer {
	/// Strictly speaking, this field is unnecessary;
	/// `Parser` already has the spec and could pass it every time to `Lexer::next_token`.
	/// However, it's cheap and cleaner to clone it here for encapsulation.
	spec: Arc<ParsingSpec>,
	#[allow(unused)]
	jit: Arc<Jit>,
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
		let mut jit: Jit = Jit::new();

		let maybe_jitted_dfa: Option<JittedDfa> = if cfg!(feature = "jit") {
			Some(jit.jit(&spec.dfa_for_parsing).unwrap())
		} else {
			None
		};

		Self {
			spec,
			jit: Arc::new(jit),
			maybe_jitted_dfa,
		}
	}

	/// Return the next [`Token`] from `input` starting from `*pos`,
	/// and update `*pos` to index the next character after the returned token.
	///
	/// `dfa_execution` is cached memory/working space for the underlying DFA execution,
	/// and also contains the captured text for sub-rules.
	/// It should be created via [`TdfaExecution::new`].
	pub fn next_token<'spec, 'input>(
		&'spec self,
		input: &'input str,
		pos: &mut usize,
		dfa_execution: &mut TdfaExecution,
	) -> Token<'spec, 'input> {
		let start: usize = *pos;

		/*
		for (offset, ch) in input[start..].char_indices() {
			if let Some(MatchedRule { rule_idx, lexeme }) =
				self.execute_dfa(input, pos + offset, last_was_delimited)
			{
				let rule: &RootRule = &self[rule_idx];
				let has_captures: bool = rule.has_captures();
				data.clear();
				if has_captures {
					let matched: bool = rule.dfa.execute_with_captures(lexeme, data, rule.idx);
					assert!(matched);
				}
				*pos += offset + lexeme.len();
				return Token::Variable {
					rule,
					lexeme,
					has_captures,
				};
			} else if ch == '\n' {
				*pos += ch.len_utf8();
				return Token::Newline;
			}
		}
		*pos = input.len();
		return Token::EndOfInput;
		*/

		if start == input.len() {
			return Token::EndOfInput;
		}

		let (input_before, input_remaining): (&str, &str) = input.split_at(start);

		let char_before: u32 = u32::from(input_before.chars().rev().next().unwrap_or('\n'));

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
				let matched: bool = rule.dfa.execute_with_captures(lexeme, dfa_execution, rule.idx);
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
			let mut chars: Chars<'_> = input[start..].chars();
			// We checked for `start == input.len()` above.
			let first: char = chars.next().unwrap();
			*pos += first.len_utf8();
			if first == '\n' {
				return Token::Newline;
			} else if !self.is_delimiter(first) {
				self.glob_static_text(input, pos);
			}
			Token::StaticText(&input[start..*pos])
		}
	}

	/// Uniform "interface" to executing a TDFA via the JIT-ed or Rust implementation.
	fn execute_dfa<'input, const JIT: bool>(
		&self,
		input: &'input str,
		char_before: u32,
	) -> Option<MatchedRule<'input>> {
		if JIT {
			let input: std::ops::Range<*const u8> = input.as_bytes().as_ptr_range();
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
			self.spec.dfa_for_parsing.execute_without_captures(input, char_before)
			// self.spec.optimized_dfa.execute(input, char_before)
		}
	}

	/// See the [document on parsing](docs/parsing.md).
	fn glob_static_text(&self, input: &str, pos: &mut usize) {
		for ch in input[*pos..].chars() {
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

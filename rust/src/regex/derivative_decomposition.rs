#![allow(unused)]

//! Based on <https://en.wikipedia.org/wiki/Brzozowski_derivative>.

use std::sync::Arc;

use super::Regex;
use crate::parsing_spec::SubRule;
use crate::search::SearchString;
use crate::search::SymbolicChar;

#[derive(Debug, Clone)]
pub enum SymbolicOutput {
	Literal(char),
	Wildcard,
	StartCapture(Arc<SubRule>),
	StopCapture(Arc<SubRule>),
}

impl std::fmt::Display for SymbolicOutput {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			&Self::Literal(ch) => {
				match ch {
					'\\' | '*' | '(' | ')' => {
						fmt.write_str("\\")?;
					},
					_ => (),
				}
				ch.fmt(fmt)
			},
			Self::Wildcard => fmt.write_str("*"),
			Self::StartCapture(sub_rule) => fmt.write_fmt(format_args!("(?<{}>", sub_rule.fully_qualified_name)),
			Self::StopCapture(_sub_rule) => fmt.write_str(")"),
		}
	}
}

impl Regex {
	pub fn derivative(&self, input: &[SymbolicChar]) -> Vec<Vec<SymbolicOutput>> {
		let mut interpretations: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = vec![(Vec::new(), vec![self.clone()])];
		let mut done: Vec<Vec<SymbolicOutput>> = Vec::new();
		for &ch in input.iter() {
			println!("===== ch {ch}");
			let mut new_interpretations: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = Vec::new();
			for (processed, to_process) in interpretations.into_iter() {
				let step: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = Self::process_sequence(&to_process, ch);
				for (mut old_processed, (new_processed, new_to_process)) in
					std::iter::zip(std::iter::repeat_n(processed.clone(), step.len()), step.into_iter())
				{
					old_processed.extend(new_processed);
					if new_to_process.is_empty() {
						done.push(old_processed);
					} else {
						new_interpretations.push((old_processed, new_to_process));
					}
				}
			}
			interpretations = new_interpretations;
			println!("interpretations is {interpretations:?}");
		}
		for (processed, to_process) in interpretations.into_iter() {
			if to_process.iter().all(|regex| regex.is_nullable().is_some()) {
				done.push(processed);
			}
		}
		done
	}

	fn derivative_step(&self, input: SymbolicChar) -> Vec<(Vec<SymbolicOutput>, Vec<Self>)> {
		println!("=== derivative step ({input})^{{-1}}({self})");
		match self {
			Self::AnyChar => match input {
				SymbolicChar::Literal(ch) => {
					vec![(vec![SymbolicOutput::Literal(ch)], Vec::new())]
				},
				SymbolicChar::GlobStar => {
					vec![(vec![SymbolicOutput::Wildcard], Vec::new())]
				},
			},
			&Self::Literal(ch) => match input {
				SymbolicChar::Literal(input_ch) if input_ch == ch => {
					vec![(vec![SymbolicOutput::Literal(ch)], Vec::new())]
				},
				SymbolicChar::Literal(_) => Vec::new(),
				SymbolicChar::GlobStar => {
					vec![(vec![SymbolicOutput::Literal(ch)], Vec::new())]
				},
			},
			Self::BracketedRanges { negated, items } => match input {
				SymbolicChar::Literal(ch) => {
					if *negated ^ items.iter().any(|&(lo, hi)| (lo <= ch) && (ch <= hi)) {
						vec![(vec![SymbolicOutput::Literal(ch)], Vec::new())]
					} else {
						Vec::new()
					}
				},
				SymbolicChar::GlobStar => {
					vec![(vec![SymbolicOutput::Wildcard], Vec::new())]
				},
			},
			Self::BoundedRepetition { min, max, item } => {
				let mut is_nullable: bool = item.is_nullable().is_some();
				todo!();
			},
			Self::KleeneClosure(item) => match input {
				SymbolicChar::Literal(ch) => {
					let mut ret: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = item.derivative_step(input);
					for (processed, to_process) in ret.iter_mut() {
						to_process.push(self.clone());
					}
					ret
				},
				SymbolicChar::GlobStar => {
					vec![(vec![SymbolicOutput::Wildcard], Vec::new())]
				},
			},
			Self::KleenePlus(item) => item.wrap_as_desugared_kleene_plus().derivative_step(input),
			Self::Sequence(items) => Self::process_sequence(items, input),
			Self::Alternation(items) => items
				.iter()
				.flat_map(|item| item.derivative_step(input))
				.collect::<Vec<_>>(),
			Self::Capture(sub_rule) => {
				let mut inner: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = sub_rule.regex.derivative_step(input);
				for (processed, to_process) in inner.iter_mut() {
					processed.insert(0, SymbolicOutput::StartCapture((**sub_rule).clone()));
					processed.push(SymbolicOutput::StartCapture((**sub_rule).clone()));
				}
				inner
			},
			Self::Placeholder { name, item } => item.derivative_step(input),
		}
	}

	fn process_sequence(items: &[Self], input: SymbolicChar) -> Vec<(Vec<SymbolicOutput>, Vec<Self>)> {
		let Some(first): Option<&Self> = items.first() else {
			match input {
				SymbolicChar::Literal(_) => {
					return Vec::new();
				},
				SymbolicChar::GlobStar => {
					return vec![(Vec::new(), Vec::new())];
				},
			}
		};
		let rest: &[Self] = &items[1..];

		let mut ret: Vec<(Vec<SymbolicOutput>, Vec<Self>)> = first.derivative_step(input);
		for (processed, to_process) in ret.iter_mut() {
			to_process.extend(rest.iter().cloned());
		}

		if first.is_nullable().is_some() {
			ret.extend(Self::Sequence(rest.to_vec()).derivative_step(input));
		}

		ret
	}
}

impl Regex {
	fn wildcard() -> Self {
		Self::KleeneClosure(Box::new(Self::AnyChar))
	}

	/*
	fn close_capture(&self) -> Self {
		let Self::Capture(sub_rule): &Self = self else {
			panic!();
		};
		let mut sub_rule: SubRule = (***sub_rule).clone();
		sub_rule.regex = Regex::NIL;
		Self::Capture(sub_rule)
	}

	fn is_close_capture(&self) -> bool {
		if let Self::Capture(sub_rule) = self {
			if sub_rule.regex == Regex::NIL {
				return true;
			}
		}
		false
	}
	*/

	fn concat(&self, other: &Self) -> Self {
		let Ok(lhs): Result<Self, ()> = self.is_valid() else {
			return Self::EPSILON;
		};
		let Ok(rhs): Result<Self, ()> = other.is_valid() else {
			return Self::EPSILON;
		};
		if lhs.is_nullable().is_some() {
			return rhs;
		}
		if rhs.is_nullable().is_some() {
			return lhs;
		}
		if let Self::Sequence(lhs) = &lhs {
			return if let Self::Sequence(rhs) = &rhs {
				Self::Sequence(lhs.iter().cloned().chain(rhs.iter().cloned()).collect::<Vec<_>>())
			} else {
				Self::Sequence(
					lhs.iter()
						.cloned()
						.chain(std::iter::once(rhs.clone()))
						.collect::<Vec<_>>(),
				)
			};
		}
		Self::Sequence(vec![lhs, rhs])
	}
}

impl Regex {
	fn epsilon_or_nil(&self) -> Self {
		if self.is_nullable().is_some() {
			Self::EPSILON
		} else {
			Self::NIL
		}
	}

	fn is_valid(&self) -> Result<Self, ()> {
		match self {
			Regex::AnyChar | Regex::Literal(..) | Regex::BracketedRanges { .. } => Ok(self.clone()),
			Regex::Capture(sub_rule) => {
				sub_rule.regex.is_valid()?;
				Ok(self.clone())
			},
			Regex::KleeneClosure(item)
			| Regex::KleenePlus(item)
			| Regex::BoundedRepetition { item, .. }
			| Regex::Placeholder { item, .. } => {
				item.is_valid()?;
				Ok(self.clone())
			},
			Regex::Sequence(items) => {
				for child in items.iter() {
					child.is_valid()?;
				}
				Ok(self.clone())
			},
			Regex::Alternation(items) => {
				let items: Vec<Self> = items.iter().filter_map(|item| item.is_valid().ok()).collect::<Vec<_>>();
				if !items.is_empty() {
					Ok(Self::Alternation(items))
				} else {
					Err(())
				}
			},
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn derivative_test() {
		let search: SearchString = SearchString::parse("hello*1234*world").unwrap();
		let regex: Regex = Regex::from_pattern("hello my \\d+th world").unwrap();
		let interpretations: Vec<Vec<SymbolicOutput>> = regex.derivative(search.as_slice());

		for interp in interpretations.iter() {
			print!("- ");
			for ch in interp.iter() {
				print!("{ch}");
			}
			println!();
		}
	}
}

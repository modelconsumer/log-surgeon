use std::num::NonZero;
use std::sync::Arc;

use crate::dfa::Tdfa;
use crate::regex::AnchoredRegex;
use crate::regex::Regex;

/// Index in the parsing specification, offset by/starting at 1.
/// `Option<RuleIdx>` is ABI equivalent to `u16` (for FFI);
/// `None`/`0` represents static text fragments.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(transparent)]
pub struct RuleIdx(NonZero<u16>);

#[derive(Debug, Clone)]
pub struct RootRule {
	pub idx: RuleIdx,
	pub name: Arc<str>,
	/// Priority level given by the user.
	pub priority: i32,

	pub regex: AnchoredRegex,
	pub rule_info: Vec<RuleInfo>,

	pub dfa: Tdfa,
}

impl Eq for RootRule {}

impl PartialEq for RootRule {
	fn eq(&self, other: &Self) -> bool {
		(self.idx, &self.name, self.priority, &self.regex)
			.cmp(&(other.idx, &other.name, other.priority, &other.regex))
			.is_eq()
	}
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct SubRule {
	pub name: Arc<str>,
	pub regex: Regex,

	pub root_rule_idx: RuleIdx,
	/// ID statically assigned left-to-right based on the regex pattern.
	/// For example, the pattern `(?<start>[a-z]+(?<rest>\.[a-z]+)*)|(?<start>[0-9]+)` has three non-zero capture IDs.
	/// When the pattern is actually matched,
	/// there may be multiple instances of capture ID 2 (corresponding to `"rest"`).
	/// The capture ID also differentiates between different captures with the same text name,
	/// e.g. the two instances of `"start"` in the pattern above.
	pub id: NonZero<u16>,
	/// ID of the parent capture, if any.
	pub parent_id: Option<NonZero<u16>>,
	/// Total number of nested captures (recursively/arbitrarily deep);
	/// `0` iff this is a "leaf" capture.
	pub descendents: usize,

	/// Qualified name w.r.t captures including the leading dot;
	/// a top-level capture is ".a", a second-level capture is ".a.b".
	pub qualified_name: Arc<str>,

	// TODO
	pub fully_qualified_name: Arc<str>,
}

/// Common info for both root rules and sub-rules.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuleInfo {
	pub root_idx: RuleIdx,
	pub root_name: Arc<str>,

	/// If this is not a root rule, additional sub-rule info.
	pub maybe_sub_rule: Option<Arc<SubRule>>,

	pub fully_qualified_name: Arc<str>,
}

impl std::ops::Index<Option<NonZero<u16>>> for RootRule {
	type Output = RuleInfo;

	fn index(&self, i: Option<NonZero<u16>>) -> &Self::Output {
		let i: usize = usize::from(i.map_or(0, NonZero::get));
		&self.rule_info[i]
	}
}

impl RuleIdx {
	/// cbindgen:ignore
	pub const NIL: Self = Self(NonZero::<u16>::MAX);

	pub const fn new(idx: NonZero<u16>) -> Self {
		Self(idx)
	}
}

impl From<RuleIdx> for NonZero<u16> {
	fn from(rule_idx: RuleIdx) -> Self {
		rule_idx.0
	}
}

impl From<RuleIdx> for u16 {
	fn from(rule_idx: RuleIdx) -> Self {
		rule_idx.0.get()
	}
}

impl From<NonZero<u16>> for RuleIdx {
	fn from(rule_idx: NonZero<u16>) -> Self {
		Self(rule_idx)
	}
}

impl std::fmt::Display for RuleIdx {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.0.fmt(fmt)
	}
}

impl RootRule {
	/// Construct a new root rule;
	/// initialize the [`RuleInfo`] for the root rule and any/all sub-rules.
	pub fn new(idx: RuleIdx, name: Arc<str>, priority: i32, regex: AnchoredRegex, dfa: Tdfa) -> Self {
		let mut rule_info: Vec<RuleInfo> = Vec::with_capacity(usize::from(regex.total_captures.get()));
		rule_info.push(RuleInfo {
			root_idx: idx,
			root_name: name.clone(),
			maybe_sub_rule: None,
			fully_qualified_name: name.clone(),
		});

		/*
		regex.regex.for_each_capture(&mut |sub_rule| {
			{
				sub_rule.fully_qualified_name = Arc::from(format!("{}{}", name, sub_rule.qualified_name));
			}
			// let i: usize = sub_rule.id_as_usize();
			// assert_eq!(rule_info.len(), i);
			// rule_info.push(RuleInfo {
			// 	root_idx: idx,
			// 	root_name: name.clone(),
			// 	maybe_sub_rule: Some(sub_rule.clone()),
			// 	fully_qualified_name: Arc::from(format!("{}{}", name, sub_rule.qualified_name)),
			// });
			Ok::<(), std::convert::Infallible>(())
		});
		*/

		let mut stack: Vec<&Regex> = vec![&regex.regex];
		while let Some(regex) = stack.pop() {
			match regex {
				Regex::AnyChar | Regex::Literal(..) | Regex::BracketedRanges { .. } => (),
				Regex::Capture(sub_rule) => {
					let i: usize = sub_rule.id_as_usize();
					assert_eq!(rule_info.len(), i);
					stack.push(&sub_rule.regex);
					rule_info.push(RuleInfo {
						root_idx: idx,
						root_name: name.clone(),
						// Note: `sub_rule` has type `&DeepClone<Arc<SubRule>>`;
						// `(*sub_rule)` has type `DeepClone<Arc<SubRule>>`,
						// and so `(**sub_rule)` has type `Arc<SubRule>`.
						maybe_sub_rule: Some((**sub_rule).clone()),
						fully_qualified_name: Arc::from(format!("{}{}", name, sub_rule.qualified_name)),
					});
				},
				Regex::KleeneClosure(item)
				| Regex::KleenePlus(item)
				| Regex::BoundedRepetition { item, .. }
				| Regex::Placeholder { item, .. } => {
					stack.push(item);
				},
				Regex::Sequence(items) | Regex::Alternation(items) => {
					// Push on to stack in reverse to mirror DFS.
					for sub_item in items.iter().rev() {
						stack.push(sub_item);
					}
				},
			}
		}

		Self {
			idx,
			name,
			priority,
			regex,
			rule_info,
			dfa,
		}
	}

	/// Find matching (nested) captures matching exactly (the fragments of) a fully qualified name.
	pub fn find_capture<'a>(
		&'a self,
		current_regex: &'a Regex,
		first: &str,
		rest: &[&str],
		collect: &mut Vec<(&'a RuleInfo, &'a Regex)>,
	) {
		match current_regex {
			Regex::AnyChar | Regex::Literal(..) | Regex::BracketedRanges { .. } => (),
			Regex::Capture(sub_rule) => {
				if &*sub_rule.name == first {
					if let Some(first) = rest.first().copied() {
						self.find_capture(&sub_rule.regex, first, &rest[1..], collect);
					} else {
						collect.push((&self[Some(sub_rule.id)], current_regex));
					}
				}
			},
			Regex::KleeneClosure(item)
			| Regex::KleenePlus(item)
			| Regex::BoundedRepetition { item, .. }
			| Regex::Placeholder { item, .. } => {
				self.find_capture(item, first, rest, collect);
			},
			Regex::Sequence(items) | Regex::Alternation(items) => {
				for item in items.iter() {
					self.find_capture(item, first, rest, collect);
				}
			},
		}
	}
}

impl RootRule {
	pub fn has_captures(&self) -> bool {
		// Always has itself as the $0$th `RuleInfo`.
		self.rule_info.len() > 1
	}
}

impl SubRule {
	/// Convenience conversion to `usize`.
	pub fn id_as_usize(&self) -> usize {
		usize::from(self.id.get())
	}

	pub fn is_leaf(&self) -> bool {
		self.descendents == 0
	}
}

impl RuleInfo {
	pub fn sub_rule_name(&self) -> &str {
		if let Some(sub_rule) = &self.maybe_sub_rule {
			&sub_rule.name
		} else {
			""
		}
	}

	pub fn is_root(&self) -> bool {
		self.maybe_sub_rule.is_none()
	}
}

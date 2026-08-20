//! Based on Angelo Borsotti and Ulya Trafimovich. 2022. A closer look at TDFA.
//! - <https://re2c.org/2022_borsotti_trofimovich_a_closer_look_at_tdfa.pdf>
//! - <https://arxiv.org/abs/2206.01398>
//!

// mod graph_dot_output;
mod regex_construction;
mod search_decomposition;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

pub use search_decomposition::Path;
pub use search_decomposition::PathComponent;

use crate::interval_tree::Interval;
use crate::interval_tree::IntervalTree;
use crate::interval_tree::PolicyUnique;
use crate::parsing_spec::Encoding;
use crate::parsing_spec::RuleIdx;
use crate::parsing_spec::SubRule;

#[derive(Debug, Clone)]
pub struct Tnfa {
	states: Vec<NfaState>,
	tags: BTreeSet<CaptureTag>,
}

#[derive(Debug, Clone)]
pub struct NfaState {
	/// ID and also an index into an [`Nfa`]'s list of states.
	pub idx: NfaIdx,
	pub transitions: Transitions,
	pub maybe_accepts_for_rule: Option<RuleIdx>,
	pub maybe_encoding: Option<Arc<Encoding>>,
	/// Not strictly needed, but useful for debugging (including DOT output).
	pub name: Cow<'static, str>,
}

/// Newtype wrapper around a `usize` index.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NfaIdx(usize);

/// By construction, an NFA state has homogenous transitions of one of these kinds.
///
/// Extra spontaneous transitions can be used to preserve this property,
/// even if they aren't otherwise necessary.
///
/// Furthermore, the predecessor states of an NFA state should,
/// by construction, also have the same kind of transitions.
///
/// See [`crate::dfa::Kernel`] for the "use" of this invariant.
#[derive(Debug, Clone)]
pub enum Transitions {
	/// Empty transitions should use this variant;
	/// see [`Tnfa::intersect`] for the reason.
	Interval(IntervalTree<u32, NfaIdx>),
	/// Priority-ordered, untagged epsilon transitions.
	Spontaneous(Vec<NfaIdx>),
	/// A single tagged transition;
	/// by construction, tagged transitions are created on "fresh" states
	/// (with no existing transitions),
	/// and such states will not have other transitions.
	Tagged {
		tag: CaptureTag,
		positive: bool,
		target: NfaIdx,
	},
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct CaptureTag {
	pub rule_idx: RuleIdx,
	pub sub_rule: Arc<SubRule>,
	pub maybe_encoding: Option<Arc<Encoding>>,
	/// "Open" before "close" in the derived ordering.
	pub is_close: bool,
}

#[derive(Debug, Clone, Copy)]
struct StatePair<'a> {
	nfas: (&'a Tnfa, &'a Tnfa),
	states: (NfaIdx, NfaIdx),
}

impl Eq for StatePair<'_> {}

impl Ord for StatePair<'_> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.states.cmp(&other.states)
	}
}

impl PartialEq for StatePair<'_> {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other).is_eq()
	}
}

impl PartialOrd for StatePair<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl<'a> StatePair<'a> {
	fn new(nfa1: &'a Tnfa, nfa2: &'a Tnfa, state1: NfaIdx, state2: NfaIdx) -> Self {
		Self {
			nfas: (nfa1, nfa2),
			states: (state1, state2),
		}
	}

	fn state1(&self) -> &NfaState {
		&self.nfas.0[self.states.0]
	}

	fn state2(&self) -> &NfaState {
		&self.nfas.1[self.states.1]
	}
}

impl Tnfa {
	pub const BLANK: Self = Self {
		states: Vec::new(),
		tags: BTreeSet::new(),
	};

	pub fn new() -> Self {
		Self {
			states: vec![NfaState {
				idx: NfaIdx::BEGIN,
				name: Cow::Borrowed("begin"),
				transitions: Transitions::Interval(IntervalTree::new()),
				maybe_accepts_for_rule: None,
				maybe_encoding: None,
			}],
			tags: BTreeSet::new(),
		}
	}

	pub fn tags(&self) -> &BTreeSet<CaptureTag> {
		&self.tags
	}

	fn new_state<LikeString>(&mut self, name: LikeString) -> NfaIdx
	where
		LikeString: Into<Cow<'static, str>>,
	{
		let idx: NfaIdx = NfaIdx(self.states.len());
		let state: NfaState = NfaState {
			idx,
			name: name.into(),
			transitions: Transitions::Interval(IntervalTree::new()),
			maybe_accepts_for_rule: None,
			maybe_encoding: None,
		};
		self.states.push(state);
		idx
	}
}

impl Tnfa {
	/// Return a new TNFA representing an intersection of `self` and `other`; conceptually:
	///
	/// - Each state in the intersection corresponds to a pair of states from `self` and `other`.
	/// - If `a` and `b` are states from `self`, `c` and `d` are states from `other`,
	///   with edges `a -> b` and `u -> v`:
	///   - If the two source edges are identical/overlapping symbol transitions,
	///     then there is an edge in the intersection `(a, u) -> (b, v)`
	///     for the corresponding input symbol(s).
	///   - If `a -> b` is a spontaneous transition,
	///     then there is an spontaneous transition edge in the intersection `(a, u) -> (b, u)`,
	///     and vice versa for `u -> v`.
	///   - If `a -> b` is a tagged transition,
	///     then there is a tagged transition edge in the intersection `(a, u) -> (b, u)`
	///     for the corresponding tag.
	///
	/// In particular, `other` should not contain tagged transitions,
	/// and this function will panic if so.
	///
	/// Only reachable state pairs are considered/created,
	/// and this function also filters out states/paths that cannot lead to an accepting state.
	///
	/// Furthermore, note that this function "exhausts" spontaneous transitions from `self`
	/// before considering/creating intersection states for spontaneous transitions from `other`;
	/// if `a -> b` and `u -> v` are as above and both spontaneous transitions,
	/// this intersect only contains `(a, u) -> (b, u)` and not `(a, u) -> (a, v)`.
	///
	/// Because of this asymmetry, empty [`NfaState::transitions`]s
	/// should be represented with an empty [`Transitions::Interval`]
	/// rather than an empty [`Transitions::Spontaneous`];
	/// otherwise, the case where both `a` and `u` as above "have" spontaneous transitions,
	/// but `a` actually has no outgoing transitions,
	/// needs to be handled separately, i.e. as `(a, u) -> (a, v)`.
	///
	/// When `FOR_SEARCH` set, non-literal interval transitions from `other`,
	/// i.e. those corresponding to a wildcard in the search,
	/// remain "maximal",
	/// in order to differentiate between literal edges that "come from"
	/// a parsing specification pattern (i.e. "would necessarily match"),
	/// as opposed to the search (i.e. "meaningful search value").
	pub fn intersect<const FOR_SEARCH: bool>(&self, other: &Self) -> Self {
		let begin: NfaIdx = NfaIdx::BEGIN;

		let mut stack: Vec<(StatePair<'_>, NfaIdx)> = vec![(StatePair::new(self, other, begin, begin), begin)];
		let mut seen: BTreeMap<StatePair<'_>, NfaIdx> = BTreeMap::from_iter(stack.iter().copied());

		let mut intersection: Self = Self {
			states: vec![NfaState {
				idx: NfaIdx::BEGIN,
				name: Cow::Borrowed("begin"),
				transitions: Transitions::Spontaneous(Vec::new()),
				maybe_accepts_for_rule: None,
				maybe_encoding: None,
			}],
			tags: BTreeSet::new(),
		};

		while let Some((pair, state)) = stack.pop() {
			if let Some(rule) = pair.state1().maybe_accepts_for_rule
				&& pair.state2().is_accepting()
			{
				intersection[state].maybe_accepts_for_rule = Some(rule);
				assert_eq!(intersection[state].transitions.len(), 0);
				continue;
			}

			let mut lookup_state = |target, combined: &mut Self| {
				*seen.entry(target).or_insert_with(|| {
					let next: NfaIdx = combined.new_state(format!("({}, {})", target.states.0, target.states.1));
					stack.push((target, next));
					next
				})
			};

			match (&pair.state1().transitions, &pair.state2().transitions) {
				(_, Transitions::Tagged { .. }) => {
					panic!("the right hand side of `Tnfa::intersect` should not contain tags");
				},
				(Transitions::Tagged { tag, positive, target }, _) => {
					let next: NfaIdx =
						lookup_state(StatePair::new(self, other, *target, pair.states.1), &mut intersection);
					intersection[state].transitions = Transitions::Tagged {
						tag: tag.clone(),
						positive: *positive,
						target: next,
					};
				},
				(Transitions::Spontaneous(transitions1), _) => {
					intersection[state].transitions = Transitions::Spontaneous(
						transitions1
							.iter()
							.map(|&target1| {
								let next: NfaIdx = lookup_state(
									StatePair::new(self, other, target1, pair.states.1),
									&mut intersection,
								);
								next
							})
							.collect::<Vec<_>>(),
					);
				},
				(_, Transitions::Spontaneous(transitions2)) => {
					intersection[state].transitions = Transitions::Spontaneous(
						transitions2
							.iter()
							.map(|&target2| {
								let next: NfaIdx = lookup_state(
									StatePair::new(self, other, pair.states.0, target2),
									&mut intersection,
								);
								next
							})
							.collect::<Vec<_>>(),
					);
				},
				(Transitions::Interval(transitions1), Transitions::Interval(transitions2)) => {
					let mut combined: IntervalTree<u32, NfaIdx> = IntervalTree::new();
					for (interval1, &target1) in transitions1.iter() {
						for (interval2, &target2) in transitions2.iter() {
							let Some(overlap): Option<Interval<u32>> = interval1.overlap(&interval2) else {
								continue;
							};
							if interval2.start() != interval2.end() {
								// Query wildcard.
								// TODO not true for arbitrary intersections - e.g. encodings
								// assert_eq!((interval2.start(), interval2.end()), (0, u32::from(char::MAX)));
								let next: NfaIdx =
									lookup_state(StatePair::new(self, other, target1, target2), &mut intersection);
								combined.insert(
									if FOR_SEARCH {
										Interval::new(0, u32::MAX)
									} else {
										overlap
									},
									next,
									PolicyUnique,
								);
							} else {
								// Query literal character.
								assert_eq!(interval2.start(), interval2.end());
								assert_eq!(overlap, interval2);
								let next: NfaIdx =
									lookup_state(StatePair::new(self, other, target1, target2), &mut intersection);
								combined.insert(overlap, next, PolicyUnique);
							}
						}
					}
					intersection[state].transitions = Transitions::Interval(combined);
				},
			}
		}

		let can_accept: Vec<bool> = intersection.compute_live_states();
		for state in intersection.states.iter_mut() {
			match &mut state.transitions {
				Transitions::Interval(transitions) => {
					transitions.retain(|&(_interval, target)| can_accept[target.0]);
				},
				Transitions::Spontaneous(transitions) => {
					transitions.retain(|&target| can_accept[target.0]);
				},
				Transitions::Tagged { target, .. } => {
					assert_eq!(can_accept[state.idx.0], can_accept[target.0]);
				},
			}
		}

		intersection
	}

	pub fn can_accept(&self) -> bool {
		self.compute_live_states()[0]
	}

	/// States that can reach an accepting state.
	fn compute_live_states(&self) -> Vec<bool> {
		let mut acceptable: Vec<bool> = vec![false; self.states.len()];

		for state in self.states.iter() {
			if state.is_accepting() {
				acceptable[state.idx.0] = true;
			}
		}

		let mut changed: bool = true;
		while changed {
			changed = false;
			for state in self.states.iter() {
				for target_idx in state.transitions.successors() {
					if acceptable[target_idx.0] {
						let old_can_accept: bool = std::mem::replace(&mut acceptable[state.idx.0], true);
						if !old_can_accept {
							changed = true;
						}
					}
				}
			}
		}

		acceptable
	}
}

impl Tnfa {
	pub fn concat(&self, other: &Self) -> Tnfa {
		let mut new_states: Vec<NfaState> = self
			.states
			.iter()
			.cloned()
			.chain(other.states.iter().map(|state| state.offset_idxes(self.states.len())))
			.collect::<Vec<_>>();

		for my_state in new_states[..self.states.len()].iter_mut() {
			if let Some(rule_idx) = my_state.maybe_accepts_for_rule {
				assert_eq!(rule_idx, RuleIdx::NIL);
				assert_eq!(my_state.transitions.len(), 0);
				my_state.maybe_accepts_for_rule = None;
				my_state.transitions = Transitions::Spontaneous(vec![NfaIdx(self.states.len())]);
			}
		}

		for other_state in new_states[self.states.len()..].iter_mut() {
			if let Some(rule_idx) = other_state.maybe_accepts_for_rule {
				assert_eq!(rule_idx, RuleIdx::NIL);
			}
		}

		Self {
			states: new_states,
			tags: &self.tags | &other.tags,
		}
	}

	/// TODO: name of this function
	pub fn or(&self, other: &Self) -> Self {
		let my_states: Vec<NfaState> = self
			.states
			.iter()
			.map(|state| state.offset_idxes(2))
			.collect::<Vec<_>>();
		let other_states: Vec<NfaState> = self
			.states
			.iter()
			.map(|state| state.offset_idxes(2 + self.states.len()))
			.collect::<Vec<_>>();

		let mut new_states: Vec<NfaState> = Vec::with_capacity(2 + self.states.len());
		let end_idx: NfaIdx = NfaIdx(1);

		new_states.push(NfaState {
			idx: NfaIdx::BEGIN,
			name: Cow::Borrowed("begin"),
			transitions: Transitions::Spontaneous(vec![NfaIdx(2), NfaIdx(2 + self.states.len())]),
			maybe_accepts_for_rule: None,
			maybe_encoding: None,
		});

		new_states.push(NfaState {
			idx: end_idx,
			name: Cow::Borrowed("end"),
			transitions: Transitions::Interval(IntervalTree::new()),
			maybe_accepts_for_rule: Some(RuleIdx::NIL),
			maybe_encoding: None,
		});

		for state in new_states[2..].iter_mut() {
			if let Some(rule_idx) = state.maybe_accepts_for_rule {
				assert_eq!(rule_idx, RuleIdx::NIL);
				assert_eq!(state.transitions.len(), 0);
				state.maybe_accepts_for_rule = None;
				state.transitions = Transitions::Spontaneous(vec![end_idx]);
			}
		}

		new_states.extend(my_states.into_iter());
		new_states.extend(other_states.into_iter());

		Self {
			states: new_states,
			tags: &self.tags | &other.tags,
		}
	}
}

impl NfaState {
	fn offset_idxes(&self, offset: usize) -> Self {
		Self {
			idx: NfaIdx(offset + self.idx.0),
			name: self.name.clone(),
			transitions: {
				let mut transitions: Transitions = self.transitions.clone();
				match &mut transitions {
					Transitions::Interval(transitions) => {
						transitions.iter_mut().for_each(|(_interval, target)| {
							target.0 += offset;
						});
					},
					Transitions::Spontaneous(transitions) => {
						transitions.iter_mut().for_each(|target| {
							target.0 += offset;
						});
					},
					Transitions::Tagged { target, .. } => {
						target.0 += offset;
					},
				};
				transitions
			},
			maybe_accepts_for_rule: self.maybe_accepts_for_rule,
			maybe_encoding: self.maybe_encoding.clone(),
		}
	}
}

impl std::ops::Index<NfaIdx> for Tnfa {
	type Output = NfaState;

	fn index(&self, i: NfaIdx) -> &Self::Output {
		&self.states[i.0]
	}
}

impl std::ops::IndexMut<NfaIdx> for Tnfa {
	fn index_mut(&mut self, i: NfaIdx) -> &mut Self::Output {
		&mut self.states[i.0]
	}
}

impl NfaState {
	pub fn is_accepting(&self) -> bool {
		self.maybe_accepts_for_rule.is_some()
	}
}

impl NfaIdx {
	pub const BEGIN: NfaIdx = Self(0);
}

impl std::fmt::Display for NfaIdx {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		fmt.write_fmt(format_args!("q{}", self.0))
	}
}

impl Transitions {
	pub fn len(&self) -> usize {
		match self {
			Self::Interval(transitions) => transitions.len(),
			Self::Spontaneous(transitions) => transitions.len(),
			Self::Tagged { .. } => 1,
		}
	}

	/// Returns an iterator of successor [`NfaIdx`]s.
	/// The elided `'_` lifetime in the return type refers to the lifetime of `&self`,
	/// and means that the returned `dyn Iterator` will/must be valid for at least the lifetime of `&self`.
	fn successors(&self) -> Box<dyn Iterator<Item = NfaIdx> + '_> {
		match self {
			Self::Interval(transitions) => Box::new(transitions.iter().map(|(_interval, target)| *target)),
			Self::Spontaneous(transitions) => Box::new(transitions.iter().copied()),
			Self::Tagged { target, .. } => Box::new(std::iter::once(*target)),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::dfa::Tdfa;
	use crate::regex::Regex;

	#[test]
	fn intersect_match() {
		let nfa1: Tnfa = Tnfa::for_regex(&Regex::from_pattern_with_placeholders(r"\w*\d\w*", &mut ()).unwrap());
		let nfa2: Tnfa = Tnfa::for_regex(&Regex::from_pattern_with_placeholders(r"\d+", &mut ()).unwrap());
		let nfa3: Tnfa = Tnfa::for_regex(&Regex::from_pattern_with_placeholders(r"\w+", &mut ()).unwrap());

		let intersection12: Tnfa = nfa1.intersect::<false>(&nfa2);
		let dfa: Tdfa = Tdfa::determinization(&intersection12);
		assert!(!dfa.execute("a1b"));

		let intersection13: Tnfa = nfa1.intersect::<false>(&nfa3);
		let dfa: Tdfa = Tdfa::determinization(&intersection13);
		assert!(dfa.execute("a1b"));
	}
}

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::interval_tree::Interval;
use crate::interval_tree::IntervalTree;
use crate::interval_tree::PolicyUnique;
use crate::nfa::CaptureTag;
use crate::nfa::NfaIdx;
use crate::nfa::Tnfa;
use crate::nfa::Transitions;
use crate::parsing_spec::Encoding;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleIdx;
use crate::parsing_spec::SubRule;
use crate::regex::Regex;

impl Tnfa {
	/// Create a capturing TNFA for an unanchored regex with accepting state for the given [`RuleIdx`].
	/// Used for per-rule TDFAs and for search.
	pub fn for_single_rule(rule_idx: RuleIdx, regex: &Regex, encodings: &[Arc<Encoding>]) -> Self {
		let mut nfa: Self = Self::new();

		let rule_start: NfaIdx = NfaIdx::BEGIN;
		let rule_end: NfaIdx = nfa.new_state("end");

		let tags: BTreeSet<CaptureTag> = nfa.build_regex_nfa::<true>(rule_idx, regex, encodings, rule_start, rule_end);
		nfa[rule_end].maybe_accepts_for_rule = Some(rule_idx);

		nfa.tags = tags;

		nfa
	}

	/// Create a non-capturing TNFA for an unanchored regex and [`RuleIdx::NIL`],
	/// for search, encodings, and TNFA intersections.
	///
	/// The code is basically duplicated from [`Tnfa::for_single_rule`],
	/// but we want to enforce that [`Tnfa::build_regex_nfa`] is called without captures.
	pub fn for_regex(regex: &Regex) -> Tnfa {
		let mut nfa: Self = Self::new();

		let rule_start: NfaIdx = NfaIdx::BEGIN;
		let rule_end: NfaIdx = nfa.new_state("end");

		let tags: BTreeSet<CaptureTag> = nfa.build_regex_nfa::<false>(RuleIdx::NIL, regex, &[], rule_start, rule_end);
		nfa[rule_end].maybe_accepts_for_rule = Some(RuleIdx::NIL);

		nfa.tags = tags;

		nfa
	}

	/// `WITH_CAPTURES` also controls "with(out) anchors"; with captures <=> without anchors.
	/// A `Tnfa` without captures additionally has transitions for "before" and "after" characters,
	/// used to match anchors/delimiters/any character (if the rule is unanchored).
	///
	/// See [`crate::dfa::Tdfa::execute_without_captures`] for more details.
	pub fn for_rules<'a, const WITH_CAPTURES: bool, Rules>(rules: Rules, delimiters: &str) -> Self
	where
		Rules: IntoIterator<Item = &'a RootRule>,
	{
		let mut nfa: Self = Self::new();

		let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();

		let mut spontaneous: Vec<NfaIdx> = Vec::new();

		for rule in rules.into_iter() {
			let anchored_rule_start: NfaIdx = nfa.new_state(format!(
				"anchored rule {} ('{}') start",
				rule.idx,
				rule.name.escape_default()
			));
			let anchored_rule_end: NfaIdx = nfa.new_state(format!(
				"anchored rule {} ('{}') end",
				rule.idx,
				rule.name.escape_default()
			));
			let rule_start: NfaIdx = nfa.new_state(format!("rule '{}' start", rule.name));
			let rule_end: NfaIdx = nfa.new_state(format!("rule '{}' end", rule.name));

			spontaneous.push(anchored_rule_start);

			if rule.regex.anchor_before {
				nfa[anchored_rule_start].transitions = Transitions::Interval(IntervalTree::from_iter(
					delimiters
						.chars()
						.map(|ch| (Interval::new(u32::from(ch), u32::from(ch)), rule_start, PolicyUnique)),
				));
			} else {
				nfa[anchored_rule_start].transitions = Transitions::Interval(IntervalTree::from_iter(std::iter::once(
					(Interval::new(0, u32::from(char::MAX)), rule_start, PolicyUnique),
				)));
			}

			if let Some(encoding) = &rule.maybe_encoding {
				let leaf_nfa: Tnfa = Tnfa::for_regex(&rule.regex.regex);
				let intersection: Tnfa = leaf_nfa.intersect::<false>(&encoding.nfa);

				if !intersection.can_accept() {
					continue;
				}

				nfa.splice(&intersection, rule_start, rule_end);
			} else {
				tags = &tags
					| &nfa.build_regex_nfa::<WITH_CAPTURES>(rule.idx, &rule.regex.regex, &[], rule_start, rule_end);
			}

			if rule.regex.anchor_after {
				nfa[rule_end].transitions =
					Transitions::Interval(IntervalTree::from_iter(delimiters.chars().map(|ch| {
						(
							Interval::new(u32::from(ch), u32::from(ch)),
							anchored_rule_end,
							PolicyUnique,
						)
					})));
			} else {
				nfa[rule_end].transitions = Transitions::Interval(IntervalTree::from_iter(std::iter::once((
					Interval::new(0, u32::from(char::MAX)),
					anchored_rule_end,
					PolicyUnique,
				))));
			}

			nfa[anchored_rule_end].maybe_accepts_for_rule = Some(rule.idx);
		}

		nfa[NfaIdx::BEGIN].transitions = Transitions::Spontaneous(spontaneous);

		nfa.tags = tags;

		nfa
	}

	/// Build the sub-TNFA for `regex` from `current` to `target` state.
	/// `rule_idx` is used to track the containing root rule,
	/// since regex capture [`SubRule`]s are agnostic to the external parsing specification.
	///
	/// This follows standard regex to NFA construction,
	/// augmented with tags as per the TDFA paper.
	///
	/// `target` should be a "new" state.
	///
	/// Invariants:
	/// - The transitions to a TNFA state from its successors are all of the same [`Transitions`] kind.
	///
	fn build_regex_nfa<const WITH_CAPTURES: bool>(
		&mut self,
		rule_idx: RuleIdx,
		regex: &Regex,
		encodings: &[Arc<Encoding>],
		mut current: NfaIdx,
		target: NfaIdx,
	) -> BTreeSet<CaptureTag> {
		assert_eq!(self[current].transitions.len(), 0);
		match regex {
			Regex::AnyChar => {
				self[current].transitions = Transitions::Interval(IntervalTree::from_iter(std::iter::once((
					Interval::new(0, u32::from(char::MAX)),
					target,
					PolicyUnique,
				))));
				BTreeSet::new()
			},
			&Regex::Literal(ch) => {
				self[current].transitions = Transitions::Interval(IntervalTree::from_iter(std::iter::once((
					Interval::new(u32::from(ch), u32::from(ch)),
					target,
					PolicyUnique,
				))));
				BTreeSet::new()
			},
			Regex::Capture(sub_rule) => {
				if WITH_CAPTURES {
					self.capture(rule_idx, sub_rule, encodings, current, target)
				} else {
					self.build_regex_nfa::<false>(rule_idx, &sub_rule.regex, encodings, current, target)
				}
			},
			Regex::BracketedRanges { negated, items } => {
				let intervals: Vec<Interval<u32>> = if *negated {
					let mut intervals: Vec<Interval<u32>> = Vec::with_capacity(items.len());
					for &(start, end) in items.iter() {
						// Should have been verified during regex pattern parsing.
						assert!(start <= end);

						intervals.push(Interval::new(u32::from(start), u32::from(end)));
					}
					Interval::complement(&mut intervals)
				} else {
					items
						.iter()
						.map(|&(start, end)| Interval::new(u32::from(start), u32::from(end)))
						.collect::<Vec<_>>()
				};
				self[current].transitions = Transitions::Interval(IntervalTree::from_iter(
					intervals.into_iter().map(|interval| (interval, target, PolicyUnique)),
				));
				BTreeSet::new()
			},
			Regex::KleeneClosure(item) => {
				let item_start: NfaIdx = self.new_state(format!("kleene item start ({item})"));
				let item_end: NfaIdx = self.new_state(format!("kleene item end ({item})"));
				let item_skip: NfaIdx = self.new_state(format!("kleene item skip ({item})"));

				self[current].transitions = Transitions::Spontaneous(vec![item_start, item_skip]);

				let tags: BTreeSet<CaptureTag> =
					self.build_regex_nfa::<WITH_CAPTURES>(rule_idx, item, encodings, item_start, item_end);

				self[item_end].transitions = Transitions::Spontaneous(vec![item_start, target]);

				let after_negative_tags: NfaIdx = self.negative_tags(tags.iter().cloned(), item_skip);
				self[after_negative_tags].transitions = Transitions::Spontaneous(vec![target]);

				tags
			},
			Regex::KleenePlus(item) => self.build_regex_nfa::<WITH_CAPTURES>(
				rule_idx,
				&item.wrap_as_desugared_kleene_plus(),
				encodings,
				current,
				target,
			),
			Regex::BoundedRepetition { min, max, item } => {
				// Should have been verified during regex pattern parsing.
				assert!(*max > 0);
				assert!(min <= max);

				// `tags` gets appended to (potentially) multiple times,
				// each time with the same values,
				// instead of explicitly handling
				// `{0,n}` (no first loop), `{m,n}` (both loops), and `{n,n}` (no second loop)
				// cases separately.
				let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();

				for i in 0..*min {
					let sub_target: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} ({item})"));

					tags.append(
						&mut self.build_regex_nfa::<WITH_CAPTURES>(rule_idx, item, encodings, current, sub_target),
					);

					current = sub_target;
				}

				for i in *min..*max {
					let mut sub_skip: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} break ({item})"));
					let sub_have: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} continue ({item})"));
					let sub_target: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} success ({item})"));

					self[current].transitions = Transitions::Spontaneous(vec![sub_have, sub_skip]);

					tags.append(
						&mut self.build_regex_nfa::<WITH_CAPTURES>(rule_idx, item, encodings, sub_have, sub_target),
					);

					if i == 0 {
						sub_skip = self.negative_tags(tags.iter().cloned(), sub_skip)
					}
					self[sub_skip].transitions = Transitions::Spontaneous(vec![target]);

					current = sub_target;
				}

				self[current].transitions = Transitions::Spontaneous(vec![target]);
				tags
			},
			Regex::Sequence(items) => {
				let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();
				// TODO this is for search... handle it better?
				if items.is_empty() {
					self[current].transitions = Transitions::Spontaneous(vec![target]);
				}
				for (i, sub_item) in items.iter().enumerate() {
					let sub_target: NfaIdx = if i + 1 < items.len() {
						self.new_state(format!("sequence sub target (of {sub_item})"))
					} else {
						target
					};
					tags.append(
						&mut self.build_regex_nfa::<WITH_CAPTURES>(rule_idx, sub_item, encodings, current, sub_target),
					);
					current = sub_target;
				}
				tags
			},
			Regex::Alternation(items) => self.alternate::<WITH_CAPTURES>(rule_idx, items, encodings, current, target),
			Regex::Placeholder { item, .. } => {
				self.build_regex_nfa::<WITH_CAPTURES>(rule_idx, item, encodings, current, target)
			},
		}
	}

	/// Build the sub-TNFA for a regex capture expression.
	fn capture(
		&mut self,
		rule: RuleIdx,
		sub_rule: &Arc<SubRule>,
		encodings: &[Arc<Encoding>],
		current: NfaIdx,
		target: NfaIdx,
	) -> BTreeSet<CaptureTag> {
		let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();

		let mut encoding_variants: Vec<NfaIdx> = Vec::new();
		let mut intermediate_states: Vec<(NfaIdx, Vec<CaptureTag>)> = Vec::new();

		if sub_rule.is_leaf() {
			let leaf_nfa: Self = Self::for_regex(&sub_rule.regex);

			for enc in encodings.iter() {
				let intersection: Tnfa = leaf_nfa.intersect::<false>(&enc.nfa);

				println!("intersecting {} with {}", sub_rule.name, enc.name);

				if !intersection.can_accept() {
					continue;
				}
				println!("- can accept");

				let sub_start: NfaIdx = self.new_state(format!(
					"capture {} (encoding {}) started",
					sub_rule.name.escape_default(),
					enc.name.escape_default()
				));
				let sub_start2: NfaIdx = self.new_state(format!(
					"capture {} (encoding {}) started2",
					sub_rule.name.escape_default(),
					enc.name.escape_default()
				));
				let sub_end: NfaIdx = self.new_state(format!(
					"capture {} (encoding {}) ended",
					sub_rule.name.escape_default(),
					enc.name.escape_default()
				));
				let sub_end2: NfaIdx = self.new_state(format!(
					"capture {} (encoding {}) ended2",
					sub_rule.name.escape_default(),
					enc.name.escape_default()
				));

				let start_tag: CaptureTag = CaptureTag {
					rule_idx: rule,
					sub_rule: sub_rule.clone(),
					maybe_encoding: Some(enc.clone()),
					is_close: false,
				};
				let end_tag = CaptureTag {
					is_close: true,
					..start_tag.clone()
				};

				self[sub_start].transitions = Transitions::Tagged {
					tag: start_tag.clone(),
					positive: true,
					target: sub_start2,
				};

				self.splice(&intersection, sub_start2, sub_end2);

				self[sub_end2].transitions = Transitions::Tagged {
					tag: end_tag.clone(),
					positive: true,
					target: sub_end,
				};

				encoding_variants.push(sub_start);
				intermediate_states.push((sub_end, vec![start_tag.clone(), end_tag.clone()]));

				tags.insert(start_tag);
				tags.insert(end_tag);
			}
		}

		let sub_start: NfaIdx =
			self.new_state(format!("capture {} (fallback) started", sub_rule.name.escape_default()));
		let sub_start2: NfaIdx = self.new_state(format!(
			"capture {} (fallback) started2",
			sub_rule.name.escape_default()
		));
		let sub_end: NfaIdx = self.new_state(format!("capture {} (fallback) ended", sub_rule.name.escape_default()));
		let sub_end2: NfaIdx = self.new_state(format!("capture {} (fallback) ended2", sub_rule.name.escape_default()));

		let start_tag: CaptureTag = CaptureTag {
			rule_idx: rule,
			sub_rule: sub_rule.clone(),
			maybe_encoding: None,
			is_close: false,
		};
		let end_tag = CaptureTag {
			is_close: true,
			..start_tag.clone()
		};

		self[sub_start].transitions = Transitions::Tagged {
			tag: start_tag.clone(),
			positive: true,
			target: sub_start2,
		};

		let inner_tags: BTreeSet<CaptureTag> =
			self.build_regex_nfa::<true>(rule, &sub_rule.regex, encodings, sub_start2, sub_end2);

		tags = &tags | &inner_tags;

		self[sub_end2].transitions = Transitions::Tagged {
			tag: end_tag.clone(),
			positive: true,
			target: sub_end,
		};

		encoding_variants.push(sub_start);
		intermediate_states.push((sub_end, vec![start_tag.clone(), end_tag.clone()]));

		tags.insert(start_tag);
		tags.insert(end_tag);

		self[current].transitions = Transitions::Spontaneous(encoding_variants);

		for (i, (sub_state, _)) in intermediate_states.iter().enumerate() {
			let mut sub_current: NfaIdx = *sub_state;
			for (other, (_, other_tags)) in intermediate_states.iter().enumerate() {
				if other == i {
					continue;
				}

				sub_current = self.negative_tags(other_tags.iter().cloned(), sub_current);
			}

			self[sub_current].transitions = Transitions::Spontaneous(vec![target]);
		}

		tags
	}

	/// Build the sub-TNFA for a regex alternation.
	fn alternate<const WITH_CAPTURE: bool>(
		&mut self,
		rule_idx: RuleIdx,
		items: &[Regex],
		encodings: &[Arc<Encoding>],
		current: NfaIdx,
		target: NfaIdx,
	) -> BTreeSet<CaptureTag> {
		let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();
		let mut intermediate_states: Vec<(NfaIdx, BTreeSet<CaptureTag>)> = Vec::new();

		let mut starts: Vec<NfaIdx> = Vec::new();
		for sub_item in items.iter() {
			let sub_start: NfaIdx = self.new_state("alternate sub start (of {sub_item})");
			let sub_target: NfaIdx = self.new_state("alternate sub target (of {sub_item})");

			starts.push(sub_start);

			intermediate_states.push((
				sub_target,
				self.build_regex_nfa::<WITH_CAPTURE>(rule_idx, sub_item, encodings, sub_start, sub_target),
			));
		}
		self[current].transitions = Transitions::Spontaneous(starts);

		for (i, (sub_state, sub_tags)) in intermediate_states.iter().enumerate() {
			let mut sub_current: NfaIdx = *sub_state;
			for (other, (_, other_tags)) in intermediate_states.iter().enumerate() {
				if other == i {
					continue;
				}

				sub_current = self.negative_tags(other_tags.iter().cloned(), sub_current);
			}

			self[sub_current].transitions = Transitions::Spontaneous(vec![target]);

			// `&BTreeSet<_>` implements `BitOr`, `BTreeSet<_>` does not.
			// `sub_tags` from the loop is already a `&BTreeSet<_>`.
			tags = &tags | sub_tags;
		}

		tags
	}

	/// Build the negative tag sequence, as per the TDFA paper.
	fn negative_tags(&mut self, tags: impl IntoIterator<Item = CaptureTag>, mut current: NfaIdx) -> NfaIdx {
		for t in tags {
			let next: NfaIdx = self.new_state("negative tag ({t:?})");
			self[current].transitions = Transitions::Tagged {
				tag: t,
				positive: false,
				target: next,
			};
			current = next;
		}
		current
	}
}

impl Tnfa {
	fn splice(&mut self, other: &Tnfa, current: NfaIdx, target: NfaIdx) {
		let my_states: Vec<NfaIdx> = other
			.states
			.iter()
			.map(|state| self.new_state(state.name.clone()))
			.collect::<Vec<_>>();

		self[current].transitions = Transitions::Spontaneous(vec![my_states[0]]);
		for (i, other_state) in other.states.iter().enumerate() {
			let idx: NfaIdx = my_states[i];
			if other_state.is_accepting() {
				assert_eq!(self[idx].transitions.len(), 0);
				self[idx].transitions = Transitions::Spontaneous(vec![target]);
			} else {
				match &other_state.transitions {
					Transitions::Interval(transitions) => {
						let mut transitions: IntervalTree<u32, NfaIdx> = transitions.clone();
						transitions.iter_mut().for_each(|(_, target)| {
							*target = my_states[target.0];
						});
						self[idx].transitions = Transitions::Interval(transitions);
					},
					Transitions::Spontaneous(transitions) => {
						self[idx].transitions = Transitions::Spontaneous(
							transitions.iter().map(|target| my_states[target.0]).collect::<Vec<_>>(),
						);
					},
					Transitions::Tagged { .. } => {
						unreachable!("encoding pattern should not have captures");
					},
				}
			}
		}
	}
}

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

		let tags: BTreeSet<CaptureTag> = nfa.build_regex_nfa(rule_idx, regex, encodings, rule_start, rule_end);
		nfa[rule_end].maybe_accepts_for_rule = Some(rule_idx);

		nfa.tags = tags;

		nfa
	}

	/// Create a TNFA for an unanchored regex with [`RuleIdx::NIL`],
	/// for search, encodings, and TNFA intersections.
	pub fn for_regex(regex: &Regex) -> Tnfa {
		Self::for_single_rule(RuleIdx::NIL, regex, &[])
	}

	/// Create an anchored TNFA for matching root rules,
	/// splitting leaf rules (including the root rules without sub-rules) by encodings.
	pub fn for_rules(rules: &[RootRule], delimiters: &str, encodings: &[Arc<Encoding>]) -> Self {
		let mut nfa: Self = Self::new();

		let mut tags: BTreeSet<CaptureTag> = BTreeSet::new();

		let mut anchored_rule_starts: Vec<NfaIdx> = Vec::new();

		for rule in rules.iter() {
			let rule_description: String = format!("rule {} ('{}')", rule.idx, rule.name.escape_default());

			let rule_start_pre_anchor: NfaIdx = nfa.new_state(format!("{rule_description} start (pre-anchor)"));
			let rule_start_post_anchor: NfaIdx = nfa.new_state(format!("{rule_description} start (post-anchor)"));

			anchored_rule_starts.push(rule_start_pre_anchor);

			nfa[rule_start_pre_anchor].transitions =
				lookaround_transitions(rule.regex.anchor_before, delimiters, rule_start_post_anchor);

			let mut encoded_rule_starts: Vec<NfaIdx> = Vec::new();

			if !rule.has_captures() {
				for enc in encodings.iter() {
					let leaf_nfa: Tnfa = Tnfa::for_regex(&rule.regex.regex);
					let intersection: Tnfa = leaf_nfa.intersect::<false>(&enc.nfa);

					if !intersection.can_accept() {
						continue;
					}

					let rule_description: String =
						format!("{} (encoding '{}')", rule_description, enc.name.escape_default());

					let encoded_rule_start: NfaIdx = nfa.new_state(format!("{rule_description} start"));
					let encoded_rule_end_pre_anchor: NfaIdx =
						nfa.new_state(format!("{rule_description} end (pre-anchor)"));
					let encoded_rule_end_post_anchor: NfaIdx =
						nfa.new_state(format!("{rule_description} end (post-anchor)"));

					encoded_rule_starts.push(encoded_rule_start);

					nfa.splice(&intersection, encoded_rule_start, encoded_rule_end_pre_anchor);

					nfa[encoded_rule_end_pre_anchor].transitions =
						lookaround_transitions(rule.regex.anchor_after, delimiters, encoded_rule_end_post_anchor);

					nfa[encoded_rule_end_post_anchor].maybe_accepts_for_rule = Some(rule.idx);
					nfa[encoded_rule_end_post_anchor].maybe_encoding = Some(enc.clone());
				}
			}

			let unencoded_rule_start: NfaIdx = nfa.new_state(format!("{rule_description} unencoded start"));
			let unencoded_rule_end_pre_anchor: NfaIdx =
				nfa.new_state(format!("{rule_description} unencoded end (pre-anchor)"));
			let unencoded_rule_end_post_anchor: NfaIdx =
				nfa.new_state(format!("{rule_description} unencoded end (post-anchor)"));

			encoded_rule_starts.push(unencoded_rule_start);
			nfa[rule_start_post_anchor].transitions = Transitions::Spontaneous(encoded_rule_starts);

			tags = &tags
				| &nfa.build_regex_nfa(
					rule.idx,
					&rule.regex.regex,
					encodings,
					unencoded_rule_start,
					unencoded_rule_end_pre_anchor,
				);

			nfa[unencoded_rule_end_pre_anchor].transitions =
				lookaround_transitions(rule.regex.anchor_after, delimiters, unencoded_rule_end_post_anchor);

			nfa[unencoded_rule_end_post_anchor].maybe_accepts_for_rule = Some(rule.idx);
			nfa[unencoded_rule_end_post_anchor].maybe_encoding = None;
		}

		nfa[NfaIdx::BEGIN].transitions = Transitions::Spontaneous(anchored_rule_starts);

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
	fn build_regex_nfa(
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
			Regex::Capture(sub_rule) => self.capture(rule_idx, sub_rule, encodings, current, target),
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

				let tags: BTreeSet<CaptureTag> = self.build_regex_nfa(rule_idx, item, encodings, item_start, item_end);

				self[item_end].transitions = Transitions::Spontaneous(vec![item_start, target]);

				let after_negative_tags: NfaIdx = self.negative_tags(tags.iter().cloned(), item_skip);
				self[after_negative_tags].transitions = Transitions::Spontaneous(vec![target]);

				tags
			},
			Regex::KleenePlus(item) => self.build_regex_nfa(
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

					tags.append(&mut self.build_regex_nfa(rule_idx, item, encodings, current, sub_target));

					current = sub_target;
				}

				for i in *min..*max {
					let mut sub_skip: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} break ({item})"));
					let sub_have: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} continue ({item})"));
					let sub_target: NfaIdx = self.new_state(format!("bounded {i} of {min}..={max} success ({item})"));

					self[current].transitions = Transitions::Spontaneous(vec![sub_have, sub_skip]);

					tags.append(&mut self.build_regex_nfa(rule_idx, item, encodings, sub_have, sub_target));

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
					tags.append(&mut self.build_regex_nfa(rule_idx, sub_item, encodings, current, sub_target));
					current = sub_target;
				}
				tags
			},
			Regex::Alternation(items) => self.alternate(rule_idx, items, encodings, current, target),
			Regex::Placeholder { item, .. } => self.build_regex_nfa(rule_idx, item, encodings, current, target),
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

		let description: String = format!("capture '{}'", sub_rule.qualified_name.escape_default());

		if sub_rule.is_leaf() {
			let leaf_nfa: Self = Self::for_regex(&sub_rule.regex);

			for enc in encodings.iter() {
				let intersection: Tnfa = leaf_nfa.intersect::<false>(&enc.nfa);

				if !intersection.can_accept() {
					continue;
				}

				let description: String = format!("{} (encoding '{}')", description, enc.name.escape_default());

				println!("{description} can match");

				let start_outside_capture: NfaIdx = self.new_state(format!("{description} start (outside capture)"));
				let start_inside_capture: NfaIdx = self.new_state(format!("{description} start (inside capture)"));
				let end_inside_capture: NfaIdx = self.new_state(format!("{description} end (inside capture)"));
				let end_outside_capture: NfaIdx = self.new_state(format!("{description} end (outside capture)"));

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

				self[start_outside_capture].transitions = Transitions::Tagged {
					tag: start_tag.clone(),
					positive: true,
					target: start_inside_capture,
				};

				self.splice(&intersection, start_inside_capture, end_inside_capture);

				self[end_inside_capture].transitions = Transitions::Tagged {
					tag: end_tag.clone(),
					positive: true,
					target: end_outside_capture,
				};

				encoding_variants.push(start_outside_capture);
				intermediate_states.push((end_outside_capture, vec![start_tag.clone(), end_tag.clone()]));

				tags.insert(start_tag);
				tags.insert(end_tag);
			}
		}

		// Fallback; no encoding match.
		{
			let description: String = format!("{description} (no encoding)");

			let start_outside_capture: NfaIdx = self.new_state(format!("{description} start (outside capture)"));
			let start_inside_capture: NfaIdx = self.new_state(format!("{description} start (inside capture)"));
			let end_inside_capture: NfaIdx = self.new_state(format!("{description} end (inside capture)"));
			let end_outside_capture: NfaIdx = self.new_state(format!("{description} end (outside capture)"));

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

			self[start_outside_capture].transitions = Transitions::Tagged {
				tag: start_tag.clone(),
				positive: true,
				target: start_inside_capture,
			};

			let inner_tags: BTreeSet<CaptureTag> = self.build_regex_nfa(
				rule,
				&sub_rule.regex,
				encodings,
				start_inside_capture,
				end_inside_capture,
			);

			tags = &tags | &inner_tags;

			self[end_inside_capture].transitions = Transitions::Tagged {
				tag: end_tag.clone(),
				positive: true,
				target: end_outside_capture,
			};

			encoding_variants.push(start_outside_capture);
			intermediate_states.push((end_outside_capture, vec![start_tag.clone(), end_tag.clone()]));

			tags.insert(start_tag);
			tags.insert(end_tag);
		}

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
	fn alternate(
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
				self.build_regex_nfa(rule_idx, sub_item, encodings, sub_start, sub_target),
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
			let next: NfaIdx = self.new_state(format!("negative tag ({t:?})"));
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

		assert_eq!(self[current].transitions.len(), 0);
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

fn lookaround_transitions(anchored: bool, delimiters: &str, target: NfaIdx) -> Transitions {
	assert!(!delimiters.is_empty());
	if anchored {
		Transitions::Interval(IntervalTree::from_iter(
			delimiters
				.chars()
				.map(|ch| (Interval::new(u32::from(ch), u32::from(ch)), target, PolicyUnique)),
		))
	} else {
		Transitions::Interval(IntervalTree::from_iter(std::iter::once((
			Interval::new(0, u32::from(char::MAX)),
			target,
			PolicyUnique,
		))))
	}
}

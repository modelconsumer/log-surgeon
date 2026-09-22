use crate::dfa::BackupState;
use crate::dfa::MatchedRule;
use crate::dfa::Tdfa;
use crate::interval_tree::Interval;
use crate::interval_tree::IntervalTree;
use crate::parsing_spec::EncodingIdx;
use crate::parsing_spec::RuleIdx;

/// If set in a transition value, the target state is accepting (has a non-`None`
/// [`accepts_for_rule`](CompressedDfa::accepts_for_rule) entry), and the low bits are the target state index.
///
/// This lets the hot loop test acceptance with a single load (the transition itself) instead of
/// a second dependent load into `accepts_for_rule` for every character.
const ACCEPTING_BIT: u16 = 0x8000;
const STATE_MASK: u16 = !ACCEPTING_BIT;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedDfa {
	pub intervals: Vec<Interval<u32>>,
	pub accepts_for_rule: Vec<Option<(RuleIdx, Option<EncodingIdx>)>>,
	/// Flattened `states.len() * 0x80` table of encoded transitions for ASCII bytes,
	/// indexed by `state * 0x80 + byte`. Encoded values: target state index in the low bits,
	/// plus [`ACCEPTING_BIT`] if the target state is accepting. `0` means "no transition".
	pub ascii_transitions: Vec<u16>,
	pub non_ascii_transitions: Vec<u16>,
}

impl Tdfa {
	pub fn compress(&self) -> CompressedDfa {
		use crate::interval_tree::PolicyNoop;

		let mut all_intervals: IntervalTree<u32, ()> = IntervalTree::new();
		for state in self.states.iter() {
			for (interval, _transition) in state.transitions.iter() {
				all_intervals.insert(interval, (), PolicyNoop);
			}
		}
		let mut all_intervals: Vec<Interval<u32>> = all_intervals
			.iter()
			.map(|(interval, _transition)| interval)
			.collect::<Vec<_>>();

		all_intervals.retain(|interval| interval.end() >= 0x80);
		if let Some(first) = all_intervals.first_mut() {
			if first.start() == 0x80 {
				*first = Interval::new(0x80, first.end());
			}
		}

		let mut accepts_for_rule: Vec<Option<(RuleIdx, Option<EncodingIdx>)>> = Vec::with_capacity(self.states.len());
		let mut ascii_transitions: Vec<u16> = vec![0; self.states.len() * 0x80];
		let mut non_ascii_transitions: Vec<u16> = Vec::with_capacity(self.states.len() * all_intervals.len());

		assert!(
			self.states.len() <= usize::from(STATE_MASK),
			"too many DFA states to encode"
		);

		let encode = |target: usize| -> u16 {
			let state: u16 = u16::try_from(target).unwrap();
			if self.states[target].accepting_rule.is_some() {
				state | ACCEPTING_BIT
			} else {
				state
			}
		};

		for (state_index, state) in self.states.iter().enumerate() {
			accepts_for_rule.push(state.accepting_rule);
			let base: usize = state_index * 0x80;
			for i in 0..0x80usize {
				if let Some(transition) = state.transitions.lookup(u32::try_from(i).unwrap()) {
					assert_ne!(transition.target, 0);
					ascii_transitions[base + i] = encode(transition.target);
				}
			}
			for &interval in all_intervals.iter() {
				if let Some(transition) = state.transitions.lookup(interval.start()) {
					assert_ne!(transition.target, 0);
					non_ascii_transitions.push(encode(transition.target));
				} else {
					non_ascii_transitions.push(0);
				}
			}
		}

		CompressedDfa {
			intervals: all_intervals,
			accepts_for_rule,
			ascii_transitions,
			non_ascii_transitions,
		}
	}
}

impl CompressedDfa {
	pub const BLANK: Self = Self {
		intervals: Vec::new(),
		accepts_for_rule: Vec::new(),
		ascii_transitions: Vec::new(),
		non_ascii_transitions: Vec::new(),
	};

	pub fn execute<'input>(&self, input: &'input str, last_was_delimited: u32) -> Option<MatchedRule<'input>> {
		// The anchor transition's target may itself be accepting, but (matching
		// [`Tdfa::execute_without_captures`]) acceptance is only recorded while consuming input.
		let mut current_state: u16 = self.lookup_next_state(0, last_was_delimited)? & STATE_MASK;

		let mut maybe_backup: Option<BackupState> = None;

		let bytes: &[u8] = input.as_bytes();
		let ascii_transitions: &[u16] = &self.ascii_transitions;
		let mut i: usize = 0;
		while i < bytes.len() {
			let b: u8 = bytes[i];
			// `consumed` is recorded *before* advancing `i`, so it holds the position of the
			// character that led to the accepting state (matching `Tdfa::execute_without_captures`).
			let (encoded, advance): (u16, usize) = if b < 0x80 {
				// Fast path: ASCII bytes map directly into the per-state table,
				// so we avoid UTF-8 decoding entirely. The table also encodes
				// whether the target is accepting, saving a second lookup.
				// SAFETY: `current_state` is always a valid state index (it comes from a masked
				// transition value, or the anchor transition at the top), and `b < 0x80`.
				let encoded: u16 =
					unsafe { *ascii_transitions.get_unchecked(usize::from(current_state) * 0x80 + usize::from(b)) };
				(encoded, 1)
			} else {
				let ch: char = input[i..].chars().next().unwrap();
				match self.lookup_next_state(current_state, u32::from(ch)) {
					Some(encoded) => (encoded, ch.len_utf8()),
					None => break,
				}
			};

			if encoded == 0 {
				break;
			}
			if encoded & ACCEPTING_BIT != 0 {
				let (rule_idx, maybe_encoding_idx) = self.accepts_for_rule[usize::from(encoded & STATE_MASK)].unwrap();
				maybe_backup = Some(BackupState {
					rule_idx,
					maybe_encoding_idx,
					consumed: i,
				});
			}
			current_state = encoded & STATE_MASK;
			i += advance;
		}

		// Treat end-of-input as if a newline followed.
		// (Note: skipped if the loop broke early on a failed transition,
		// matching the `chain`-based iteration in [`Tdfa::execute_without_captures`].)
		if i == bytes.len()
			&& let Some(encoded) = self.lookup_next_state(current_state, u32::from('\n'))
		{
			current_state = encoded & STATE_MASK;
			if encoded & ACCEPTING_BIT != 0 {
				let (rule_idx, maybe_encoding_idx) = self.accepts_for_rule[usize::from(current_state)].unwrap();
				maybe_backup = Some(BackupState {
					rule_idx,
					maybe_encoding_idx,
					consumed: input.len(),
				});
			}
		}

		let backup: BackupState = maybe_backup?;

		Some(MatchedRule {
			rule_idx: backup.rule_idx,
			maybe_encoding_idx: backup.maybe_encoding_idx,
			lexeme: &input[..backup.consumed],
		})
	}

	#[inline(always)]
	fn lookup_next_state(&self, current_state: u16, ch: u32) -> Option<u16> {
		let current_state: usize = usize::from(current_state);
		let encoded: u16 = if ch < 0x80 {
			self.ascii_transitions[current_state * 0x80 + ch as usize]
		} else {
			let char_class: usize = self.char_to_class(ch);
			if char_class == usize::MAX {
				return None;
			}
			self.non_ascii_transitions[(current_state * self.intervals.len()) + char_class]
		};
		// `0` means no transition. Valid transitions always have a nonzero target state.
		(encoded != 0).then_some(encoded)
	}

	fn char_to_class(&self, ch: u32) -> usize {
		// TODO refactor with interval tree
		let i: usize = self.intervals.partition_point(|interval| interval.end() < ch);
		if let Some(interval) = self.intervals.get(i) {
			if interval.start() <= ch {
				return i;
			}
		}
		usize::MAX
	}
}

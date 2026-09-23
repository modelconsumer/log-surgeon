use super::*;
use crate::regex::AnchoredRegex;

#[test]
fn big_pattern() {
	let dfa: Tdfa = for_pattern("0((?<foobar>1(2[a-zA-Z])*)*|(?<baz>xyz))*world");
	let b: bool = dfa.execute("012a2b2c12z12zxyzxyzxyzworld");
	assert!(b);
}

#[test]
fn bracketed_expression_with_overlapping_range() {
	let dfa: Tdfa = for_pattern("[aa]");
	let b: bool = dfa.execute("a");
	assert!(b);
}

#[test]
fn submatch_precedence() {
	let rule: RootRule = for_rule("(?<foo>a|aa)(?<bar>a|aa)");
	let dfa: &Tdfa = &rule.dfa;
	let mut data: TdfaExecution = dfa.execution_data();

	let b: bool = dfa.execute_with_captures("aaa", &mut data, &rule);
	assert!(b);

	assert_eq!(data.captures.len(), 2);
	assert_eq!(data.captures[0].range.start, 0);
	assert_eq!(data.captures[0].range.end, 1);
	assert_eq!(data.captures[1].range.start, 1);
	assert_eq!(data.captures[1].range.end, 3);
}

/// [`Tdfa::execute`] only checks that the whole input can be consumed;
/// it does not require the DFA to land on an accepting state.
#[test]
fn execute_accepts_viable_prefixes() {
	let dfa: Tdfa = for_pattern("abcdef");

	assert!(dfa.execute("abcdef"));
	assert!(dfa.execute("abc"));
	assert!(dfa.execute(""));

	assert!(!dfa.execute("abcdefg"));
	assert!(!dfa.execute("xyz"));
	assert!(!dfa.execute("abd"));
}

#[test]
fn literal_sequence() {
	let dfa: Tdfa = for_pattern("abcdef");

	assert!(full_match(&dfa, "abcdef"));

	assert!(!full_match(&dfa, ""));
	assert!(!full_match(&dfa, "abc"));
	assert!(!full_match(&dfa, "abcdefg"));
	assert!(!full_match(&dfa, "Abcdef"));
}

#[test]
fn alternation() {
	let dfa: Tdfa = for_pattern("abc|def|gh");

	assert!(full_match(&dfa, "abc"));
	assert!(full_match(&dfa, "def"));
	assert!(full_match(&dfa, "gh"));

	assert!(!full_match(&dfa, "abd"));
	assert!(!full_match(&dfa, "g"));
	assert!(!full_match(&dfa, "abcdef"));
}

#[test]
fn repetition_operators() {
	let kleene_plus: Tdfa = for_pattern("ab+c");
	assert!(full_match(&kleene_plus, "abc"));
	assert!(full_match(&kleene_plus, "abbbbc"));
	assert!(!full_match(&kleene_plus, "ac"));

	let kleene_closure: Tdfa = for_pattern("ab*c");
	assert!(full_match(&kleene_closure, "ac"));
	assert!(full_match(&kleene_closure, "abc"));
	assert!(full_match(&kleene_closure, "abbbbc"));
	assert!(!full_match(&kleene_closure, "abbbb"));

	let optional: Tdfa = for_pattern("ab?c");
	assert!(full_match(&optional, "ac"));
	assert!(full_match(&optional, "abc"));
	assert!(!full_match(&optional, "abbc"));

	let bounded: Tdfa = for_pattern("ab{2,3}c");
	assert!(!full_match(&bounded, "abc"));
	assert!(full_match(&bounded, "abbc"));
	assert!(full_match(&bounded, "abbbc"));
	assert!(!full_match(&bounded, "abbbbc"));

	let exact: Tdfa = for_pattern("ab{2}c");
	assert!(full_match(&exact, "abbc"));
	assert!(!full_match(&exact, "abc"));
	assert!(!full_match(&exact, "abbbc"));
}

#[test]
fn character_classes() {
	let ranges: Tdfa = for_pattern("[a-c1-3]+");
	assert!(full_match(&ranges, "abc123"));
	assert!(!full_match(&ranges, "abcd"));

	let negated: Tdfa = for_pattern("[^a-c]+");
	assert!(full_match(&negated, "xyz"));
	assert!(!full_match(&negated, "xbz"));

	let escape_classes: Tdfa = for_pattern(r"\w+\s\d+");
	assert!(full_match(&escape_classes, "abc 123"));
	assert!(full_match(&escape_classes, "aB9\t0"));
	assert!(!full_match(&escape_classes, "abc 12a"));
	assert!(!full_match(&escape_classes, "abc-123"));

	let any_char: Tdfa = for_pattern("a.c");
	assert!(full_match(&any_char, "abc"));
	assert!(full_match(&any_char, "a\nc"));
	// A multi-byte code point counts as a single character.
	assert!(full_match(&any_char, "a\u{e9}c"));
	assert!(!full_match(&any_char, "ac"));
}

#[test]
fn ip_address_like_pattern() {
	let dfa: Tdfa = for_pattern(r"\d+\.\d+\.\d+\.\d+");

	assert!(full_match(&dfa, "1.2.3.4"));
	assert!(full_match(&dfa, "127.0.0.1"));

	assert!(!full_match(&dfa, "1.2.3"));
	assert!(!full_match(&dfa, "1.2.3."));
	assert!(!full_match(&dfa, "1.2.3.4.5"));
	assert!(!full_match(&dfa, "1x2.3.4.5"));
}

#[test]
fn captures_in_sequence() {
	let rule: RootRule = for_rule("(?<a>x)(?<b>y)");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "xy");

	assert_eq!(captures, vec![(1, 0, 1), (2, 1, 2)]);
}

#[test]
fn captures_of_leaf_values() {
	let rule: RootRule = for_rule(r"(?<num>\d+)\.(?<frac>\d+)");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "12.345");

	assert_eq!(captures, vec![(1, 0, 2), (2, 3, 6)]);
}

/// Repeated captures are reported once per iteration, left to right.
#[test]
fn repeated_capture_reports_each_iteration() {
	let rule: RootRule = for_rule("(?<a>x)+");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "xxx");

	assert_eq!(captures, vec![(1, 0, 1), (1, 1, 2), (1, 2, 3)]);

	let bounded: RootRule = for_rule("(?<a>b){2,3}");
	assert_eq!(captures_of(&bounded, "bb"), vec![(1, 0, 1), (1, 1, 2)]);
	assert_eq!(captures_of(&bounded, "bbb"), vec![(1, 0, 1), (1, 1, 2), (1, 2, 3)]);
}

/// Captures are sorted left to right, then parent before child.
#[test]
fn nested_captures() {
	let rule: RootRule = for_rule("(?<outer>a(?<inner>b)c)");
	let dfa: &Tdfa = &rule.dfa;
	let mut data: TdfaExecution = dfa.execution_data();

	assert!(dfa.execute_with_captures("abc", &mut data, &rule));
	assert_eq!(data.captures.len(), 2);

	let outer: &MatchedCapture = &data.captures[0];
	assert_eq!(outer.capture_id.get(), 1);
	assert_eq!(outer.parent_id, None);
	assert_eq!(outer.parent_index, 0);
	assert!(!outer.is_leaf);
	assert_eq!((outer.range.start, outer.range.end), (0, 3));

	let inner: &MatchedCapture = &data.captures[1];
	assert_eq!(inner.capture_id.get(), 2);
	assert_eq!(inner.parent_id.map(NonZero::get), Some(1));
	// `parent_index` is 1-based; `0` means "the root rule".
	assert_eq!(inner.parent_index, 1);
	assert!(inner.is_leaf);
	assert_eq!((inner.range.start, inner.range.end), (1, 2));
}

#[test]
fn nested_repeated_captures() {
	let rule: RootRule = for_rule("(?<a>(?<b>x)+)");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "xxx");

	assert_eq!(captures, vec![(1, 0, 3), (2, 0, 1), (2, 1, 2), (2, 2, 3)]);

	let mut data: TdfaExecution = rule.dfa.execution_data();
	assert!(rule.dfa.execute_with_captures("xxx", &mut data, &rule));
	// All inner captures point at the single outer capture.
	assert_eq!(
		Vec::from_iter(data.captures.iter().map(|cap| cap.parent_index)),
		vec![0, 1, 1, 1],
	);
}

/// Each alternative gets a distinct capture ID, even with the same name.
#[test]
fn alternation_captures_have_distinct_ids() {
	let rule: RootRule = for_rule("(?<a>(?<b>x)|(?<c>y))+");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "xyx");

	assert_eq!(
		captures,
		vec![(1, 0, 1), (2, 0, 1), (1, 1, 2), (3, 1, 2), (1, 2, 3), (2, 2, 3)],
	);
}

/// Repetition operators are greedy; the first capture takes as much as it can.
#[test]
fn greedy_repetition_captures() {
	let rule: RootRule = for_rule("(?<a>a+)(?<b>a+)");
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, "aaaa");

	assert_eq!(captures, vec![(1, 0, 3), (2, 3, 4)]);
}

/// A capture that doesn't participate in the match isn't reported.
#[test]
fn skipped_capture_is_not_reported() {
	let optional: RootRule = for_rule("a(?<a>b)?c");
	assert_eq!(captures_of(&optional, "ac"), vec![]);
	assert_eq!(captures_of(&optional, "abc"), vec![(1, 1, 2)]);

	let kleene: RootRule = for_rule("x(?<a>y)*z");
	assert_eq!(captures_of(&kleene, "xz"), vec![]);
	assert_eq!(captures_of(&kleene, "xyyz"), vec![(1, 1, 2), (1, 2, 3)]);
}

/// Capture ranges are byte offsets into the input.
#[test]
fn capture_ranges_are_byte_offsets() {
	let rule: RootRule = for_rule("(?<a>.)(?<b>.)");
	let input: &str = "\u{e9}b";
	let captures: Vec<(u16, usize, usize)> = captures_of(&rule, input);

	assert_eq!(captures, vec![(1, 0, 2), (2, 2, 3)]);
	assert_eq!(&input[0..2], "\u{e9}");
}

#[test]
fn execution_data_is_reusable() {
	let rule: RootRule = for_rule(r"(?<a>\d+)-(?<b>\d+)");
	let dfa: &Tdfa = &rule.dfa;
	let mut data: TdfaExecution = dfa.execution_data();

	assert!(dfa.execute_with_captures("12-34", &mut data, &rule));
	let first: Vec<(usize, usize)> = Vec::from_iter(data.captures.iter().map(|c| (c.range.start, c.range.end)));

	// A failed execution shouldn't leave stale captures behind.
	assert!(!dfa.execute_with_captures("12+34", &mut data, &rule));
	assert_eq!(data.captures, vec![]);

	assert!(dfa.execute_with_captures("12-34", &mut data, &rule));
	let second: Vec<(usize, usize)> = Vec::from_iter(data.captures.iter().map(|c| (c.range.start, c.range.end)));

	assert_eq!(first, second);
}

#[test]
fn captures_carry_rule_idx() {
	let rule_idx: RuleIdx = RuleIdx::new(NonZero::new(7).unwrap());
	let rule: RootRule = for_rule_with_idx(rule_idx, "(?<a>x)");
	let dfa: &Tdfa = &rule.dfa;
	let mut data: TdfaExecution = dfa.execution_data();

	assert!(dfa.execute_with_captures("x", &mut data, &rule));
	assert_eq!(data.captures.len(), 1);
	assert_eq!(data.captures[0].rule_idx, rule_idx);
}

/// Run the DFA to completion and check it lands on an accepting state,
/// as opposed to [`Tdfa::execute`], which only checks the input can be consumed.
fn full_match(dfa: &Tdfa, input: &str) -> bool {
	let mut current_state: usize = 0;
	for ch in input.chars() {
		let Some(transition): Option<&Transition> = dfa.lookup_transition(current_state, u32::from(ch)) else {
			return false;
		};
		current_state = transition.target;
	}
	dfa.states[current_state].accepting_rule.is_some()
}

/// We need to go through [`AnchoredRegex::from_pattern_with_placeholders`]
/// which assigns capture IDs.
#[track_caller]
fn for_rule(pattern: &str) -> RootRule {
	for_rule_with_idx(RuleIdx::NIL, pattern)
}

#[track_caller]
fn for_rule_with_idx(rule_idx: RuleIdx, pattern: &str) -> RootRule {
	let anchored: AnchoredRegex =
		AnchoredRegex::from_pattern_with_placeholders(pattern, "test".into(), &mut ()).unwrap();
	RootRule::new(rule_idx, "test".into(), 0, anchored, &[])
}

#[track_caller]
fn for_pattern(pattern: &str) -> Tdfa {
	for_rule(pattern).dfa
}

/// Returns captures as `(capture ID, start, end)` triples.
/// Panics if no match.
#[track_caller]
fn captures_of(rule: &RootRule, input: &str) -> Vec<(u16, usize, usize)> {
	let mut data: TdfaExecution = rule.dfa.execution_data();
	assert!(rule.dfa.execute_with_captures(input, &mut data, rule));
	Vec::from_iter(
		data.captures
			.iter()
			.map(|cap| (cap.capture_id.get(), cap.range.start, cap.range.end)),
	)
}

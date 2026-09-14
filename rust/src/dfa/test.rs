use super::*;

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
	let dfa: Tdfa = for_pattern("(?<foo>a|aa)(?<bar>a|aa)");
	let mut data: TdfaExecution = dfa.execution_data();

	let b: bool = dfa.execute_with_captures("aaa", &mut data, RuleIdx::NIL);
	assert!(b);

	assert_eq!(data.captures.len(), 2);
	assert_eq!(data.captures[0].range.start, 0);
	assert_eq!(data.captures[0].range.end, 1);
	assert_eq!(data.captures[1].range.start, 1);
	assert_eq!(data.captures[1].range.end, 3);
}

fn for_pattern(pattern: &str) -> Tdfa {
	let regex: Regex = Regex::from_pattern(pattern).unwrap();
	Tdfa::for_single_rule(RuleIdx::NIL, &regex, &[])
}

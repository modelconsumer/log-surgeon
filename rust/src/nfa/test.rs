use super::*;
use crate::dfa::Tdfa;
use crate::regex::Regex;

#[test]
fn intersect_match() {
	let nfa1: Tnfa = for_pattern(r"\w*\d\w*");
	let nfa2: Tnfa = for_pattern(r"\d+");
	let nfa3: Tnfa = for_pattern(r"\w+");

	let intersection12: Tnfa = nfa1.intersect::<false, true>(&nfa2);
	let dfa: Tdfa = Tdfa::determinization::<false>(&intersection12);
	assert!(!dfa.execute("a1b"));

	let intersection13: Tnfa = nfa1.intersect::<false, true>(&nfa3);
	let dfa: Tdfa = Tdfa::determinization::<false>(&intersection13);
	assert!(dfa.execute("a1b"));
}

fn for_pattern(pattern: &str) -> Tnfa {
	let regex: Regex = Regex::from_pattern(pattern).unwrap();
	Tnfa::for_regex(&regex)
}

use super::*;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::ParsingSpecBuilder;
use crate::search::SearchString;

fn spec_with_rules(rules: &[(&str, &str)]) -> ParsingSpec {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	for &(name, pattern) in rules.iter() {
		builder.add_rule(name, pattern).unwrap();
	}
	builder.build()
}

fn test_spec() -> ParsingSpec {
	spec_with_rules(&[
		("level", "TRACE|DEBUG|INFO|WARN|ERROR"),
		("digits", "[0-9]+"),
		("word", "[a-zA-Z]+"),
	])
}

/// Parses `query` and drops a single trailing wildcard, as the engine does.
fn symbols_of(query: &str) -> Vec<SymbolicChar> {
	let parsed: SearchString = SearchString::parse(query).unwrap();
	let symbols: &[SymbolicChar] = parsed.as_slice();
	if Some(&SymbolicChar::GlobStar) == symbols.last() {
		symbols[..(symbols.len() - 1)].to_vec()
	} else {
		symbols.to_vec()
	}
}

/// Aligns with the engine's current (prefix-matching) semantics.
fn align_query(spec: &ParsingSpec, shape: &str, query: &str) -> Outcome {
	let model: ShapeModel = ShapeModel::new(spec, shape).unwrap();
	align(&model, &symbols_of(query), false, Budget::default())
}

/// Aligns with end anchoring, i.e. the query must consume the shape through to its end.
fn align_query_anchored(spec: &ParsingSpec, shape: &str, query: &str) -> Outcome {
	let model: ShapeModel = ShapeModel::new(spec, shape).unwrap();
	align(&model, &symbols_of(query), true, Budget::default())
}

fn is_rejected(outcome: &Outcome) -> bool {
	matches!(outcome, Outcome::Rejected)
}

/// Renders the alignments as compact strings, for readable assertions.
/// A capture shows as `name{contents}`, static text bare.
fn rendered(outcome: &Outcome) -> Vec<String> {
	let Outcome::Approximate(alignments) = outcome else {
		panic!("expected alignments, got {outcome:?}");
	};
	let mut out: Vec<String> = alignments
		.iter()
		.map(|alignment| {
			alignment
				.fragments
				.iter()
				.map(|fragment| match fragment {
					Fragment::Static(contents) => contents.iter().map(ToString::to_string).collect::<String>(),
					Fragment::Capture { sub_rule, contents } => format!(
						"{}{{{}}}",
						sub_rule.fully_qualified_name,
						contents.iter().map(ToString::to_string).collect::<String>()
					),
				})
				.collect::<String>()
		})
		.collect::<Vec<_>>();
	out.sort();
	out.dedup();
	out
}

#[test]
fn static_text_must_match_verbatim() {
	let spec: ParsingSpec = test_spec();
	assert!(!is_rejected(&align_query(&spec, "hello %word%", "*hello*")));
	// `hxllo` is not producible by the static text, but note `%word%` can emit `hxllo`'s letters, so
	// this is only rejected once the run has to straddle the static text. Use a character no part can
	// emit to isolate the static-text check.
	assert!(is_rejected(&align_query(&spec, "hello %word%", "*h#llo*")));
}

#[test]
fn placeholder_charset_rejects_foreign_characters() {
	let spec: ParsingSpec = test_spec();
	// A digit-only placeholder cannot produce letters, and no static text supplies them either.
	assert!(is_rejected(&align_query(&spec, "id=%digits%", "*id=abc*")));
	assert!(!is_rejected(&align_query(&spec, "id=%digits%", "*id=123*")));
}

#[test]
fn fixed_text_is_decomposed_into_static_and_capture() {
	let spec: ParsingSpec = test_spec();
	let outcome: Outcome = align_query(&spec, "id=%digits%", "id=123*");
	// The decomposition that matters: `id=` is the shape's static text, `123` a capture of `digits`.
	assert!(
		rendered(&outcome).contains(&"id=digits{123}".to_owned()),
		"got {:?}",
		rendered(&outcome)
	);
}

#[test]
fn capture_spanning_a_whole_placeholder() {
	let spec: ParsingSpec = test_spec();
	let outcome: Outcome = align_query(&spec, "%level% ok", "INFO ok");
	assert!(
		rendered(&outcome).contains(&"level{INFO} ok".to_owned()),
		"got {:?}",
		rendered(&outcome)
	);
}

#[test]
fn text_is_split_across_static_and_placeholder() {
	let spec: ParsingSpec = test_spec();
	// `abc` must be split: `a` from static text, `bc` captured by the rule.
	let outcome: Outcome = align_query(&spec, "a%word%", "abc*");
	assert!(
		rendered(&outcome).contains(&"aword{bc}".to_owned()),
		"got {:?}",
		rendered(&outcome)
	);
}

#[test]
fn contiguity_is_enforced_across_a_placeholder() {
	let spec: ParsingSpec = test_spec();
	// `a` and `b` are both producible, but `axb` requires the digit-only placeholder to emit `x`.
	assert!(is_rejected(&align_query(&spec, "a%digits%b", "*axb*")));
	// With a digit between, it aligns.
	assert!(!is_rejected(&align_query(&spec, "a%digits%b", "*a1b*")));
}

#[test]
fn runs_must_align_in_order() {
	let spec: ParsingSpec = test_spec();
	// Both runs exist in the shape, but in the opposite order, so no alignment consumes them in order.
	// This is the key improvement over checking each run independently.
	assert!(!is_rejected(&align_query(&spec, "aaa %word% zzz", "*aaa*zzz*")));
	assert!(is_rejected(&align_query(&spec, "aaa %digits% zzz", "*zzz*aaa*")));
}

#[test]
fn start_anchoring_is_honoured() {
	let spec: ParsingSpec = test_spec();
	// Anchored at the start: the query must begin where the shape begins.
	assert!(!is_rejected(&align_query(&spec, "hello %word%", "hello*")));
	// `ello` cannot start the shape, and `h` blocks it from starting mid-text.
	assert!(is_rejected(&align_query(&spec, "hello %word%", "ello*")));
	// Unanchored, the same text is fine.
	assert!(!is_rejected(&align_query(&spec, "hello %word%", "*ello*")));
}

#[test]
fn end_anchoring_requires_consuming_the_shape() {
	let spec: ParsingSpec = test_spec();
	// Anchored: trailing static text remains unconsumed, so the query cannot reach the end.
	assert!(is_rejected(&align_query_anchored(&spec, "%word% tail", "*xyz")));
	// Consuming through to the end is accepted.
	assert!(!is_rejected(&align_query_anchored(&spec, "%word% tail", "* tail")));
	// Unanchored (the engine's current semantics), the remaining text is unconstrained.
	assert!(!is_rejected(&align_query(&spec, "%word% tail", "*xyz")));
}

#[test]
fn wildcard_only_placeholder_is_not_reported_as_a_capture() {
	let spec: ParsingSpec = test_spec();
	let outcome: Outcome = align_query(&spec, "a%word%b", "a*b*");
	// A capture whose contents are only wildcards says nothing about the rule's value, so it must not
	// appear as a `digits{*}`-style vacuous result.
	for rendering in rendered(&outcome).iter() {
		assert!(!rendering.contains("word{*}"), "vacuous capture in {rendering:?}");
	}
}

#[test]
fn empty_query_is_unknown() {
	let spec: ParsingSpec = test_spec();
	// The engine panics on an empty query; never claim a verdict for it.
	assert!(matches!(align_query(&spec, "%word%", "*"), Outcome::Unknown));
}

#[test]
fn exhausted_budget_is_unknown_not_rejected() {
	let spec: ParsingSpec = test_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "a%word%b%word%c%word%d").unwrap();
	let budget: Budget = Budget {
		max_partial_alignments: 1,
		max_alignments: 1,
	};
	// Degrading must never look like a rejection.
	assert!(matches!(
		align(&model, &symbols_of("*abc*"), false, budget),
		Outcome::Unknown
	));
}

#[test]
fn adjacent_wildcards_do_not_duplicate_alignments() {
	let spec: ParsingSpec = test_spec();
	// Interior and leading runs of wildcards collapse, so they cannot enumerate the same decomposition
	// more than once. (Note the engine strips only a single *trailing* wildcard, so `*ab*` and `**ab**`
	// are genuinely different queries and are not compared here.)
	let one: Outcome = align_query(&spec, "a%word%b", "*ab*");
	let two: Outcome = align_query(&spec, "a%word%b", "**ab*");
	assert_eq!(rendered(&one), rendered(&two));

	let one: Outcome = align_query(&spec, "a%word%b", "*a*b*");
	let two: Outcome = align_query(&spec, "a%word%b", "*a***b*");
	assert_eq!(rendered(&one), rendered(&two));

	// No alignment contains a doubled wildcard.
	for rendering in rendered(&one).iter() {
		assert!(!rendering.contains("**"), "doubled wildcard in {rendering:?}");
	}
}

#[test]
fn can_match_agrees_with_align() {
	// `can_match` is the cheap reachability-only pass; it must never disagree with the full walk, since
	// the search path relies on it to skip shapes.
	let spec: ParsingSpec = test_spec();
	for shape in [
		"id=%digits%",
		"a%word%b",
		"%level% %word%",
		"hello %word% world",
		"plain text",
		"%digits%-%digits%",
	] {
		let model: ShapeModel = ShapeModel::new(&spec, shape).unwrap();
		for query in [
			"*a*", "*abc*", "id=1*", "*id=x*", "a*b", "*INFO*", "*zzz*", "hello*", "*world", "plain*", "*-*", "*1*2*",
			"*#*",
		] {
			let symbols: Vec<SymbolicChar> = symbols_of(query);
			for anchored_at_end in [false, true] {
				let cheap: bool = can_match(&model, &symbols, anchored_at_end);
				let full: bool = !matches!(
					align(&model, &symbols, anchored_at_end, Budget::default()),
					Outcome::Rejected
				);
				assert_eq!(
					full, cheap,
					"can_match disagreed with align: shape={shape:?} query={query:?} anchored={anchored_at_end}"
				);
			}
		}
	}
}

#[test]
fn every_alignment_preserves_the_querys_fixed_text() {
	// A structural invariant: concatenating an alignment's fragments must reproduce the query symbols,
	// modulo the wildcard collapsing the walk performs.
	let spec: ParsingSpec = test_spec();
	for (shape, query) in [
		("id=%digits% %level%", "*id=1*INFO*"),
		("a%word%b", "*ab*"),
		("%level% %word% %digits%", "*INFO*x*9*"),
		("hello %word% world", "hello*world"),
	] {
		let outcome: Outcome = align_query(&spec, shape, query);
		let Outcome::Approximate(alignments) = &outcome else {
			continue;
		};
		let expected: String = symbols_of(query)
			.iter()
			.map(ToString::to_string)
			.collect::<String>()
			.replace('*', "");
		for alignment in alignments.iter() {
			let actual: String = alignment
				.fragments
				.iter()
				.flat_map(|fragment| match fragment {
					Fragment::Static(contents) | Fragment::Capture { contents, .. } => contents.iter(),
				})
				.map(ToString::to_string)
				.collect::<String>()
				.replace('*', "");
			assert_eq!(expected, actual, "shape={shape:?} query={query:?}");
		}
	}
}

use super::*;
use crate::parsing_spec::ParsingSpecBuilder;

fn spec_with_rules(rules: &[(&str, &str)]) -> ParsingSpec {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	for &(name, pattern) in rules.iter() {
		builder.add_rule(name, pattern).unwrap();
	}
	builder.build()
}

fn test_spec() -> ParsingSpec {
	spec_with_rules(&[
		("digits", "[0-9]+"),
		("word", "[a-z]+"),
		("level", "INFO|WARN|ERROR"),
		("path", r"/[a-z]+"),
	])
}

/// The split points at which the rule can supply a leading part of the run.
fn suffix_splits(fit: &RunFit) -> Vec<usize> {
	Vec::from_iter(
		fit.suffixes
			.iter()
			.enumerate()
			.filter(|(_, fits)| **fits)
			.map(|(k, _)| k),
	)
}

/// The split points at which the rule can supply a trailing part of the run.
fn prefix_splits(fit: &RunFit) -> Vec<usize> {
	Vec::from_iter(
		fit.prefixes
			.iter()
			.enumerate()
			.filter(|(_, fits)| **fits)
			.map(|(k, _)| k),
	)
}

#[test]
fn whole_run_inside_a_rule() {
	let spec: ParsingSpec = test_spec();

	let fit: RunFit = RunFit::compute(&spec, "digits", "123");
	assert!(fit.fits_wholly(), "a digit run fits inside a digit rule");
	assert!(!fit.is_empty());

	// A letter run cannot sit inside a digits-only rule.
	let fit: RunFit = RunFit::compute(&spec, "digits", "abc");
	assert!(!fit.fits_wholly());
	assert!(fit.is_empty(), "no whole fit and no partial fit");
}

#[test]
fn whole_fit_carries_interpretations() {
	let spec: ParsingSpec = test_spec();
	let fit: RunFit = RunFit::compute(&spec, "digits", "123");
	// The interpretations are what Stage C will embed into the shape's decomposition, so they must name
	// the rule.
	assert!(!fit.whole.is_empty());
	assert!(
		fit.whole
			.iter()
			.flat_map(|interpretation| interpretation.sub_queries.iter())
			.any(|sub_query| &*sub_query.fully_qualified_name == "digits")
	);
}

#[test]
fn empty_contributions_are_always_available() {
	let spec: ParsingSpec = test_spec();
	let fit: RunFit = RunFit::compute(&spec, "digits", "abc");
	// Supplying *none* of the run is always possible, and is how a rule that is irrelevant to a run is
	// represented.
	assert!(fit.suffixes[0]);
	assert!(fit.prefixes[3]);
}

#[test]
fn straddle_splits_for_a_partially_matching_run() {
	let spec: ParsingSpec = test_spec();

	// Run `12x`: the digits rule can end with `1` or `12`, but not `12x`.
	let fit: RunFit = RunFit::compute(&spec, "digits", "12x");
	assert_eq!(vec![0, 1, 2], suffix_splits(&fit));
	assert!(!fit.fits_wholly(), "`12x` cannot sit wholly inside `[0-9]+`");
	assert!(fit.has_partial());

	// Run `x12`: the digits rule can begin with `12` (split at 1) or `2` (split at 2).
	let fit: RunFit = RunFit::compute(&spec, "digits", "x12");
	assert_eq!(vec![1, 2, 3], prefix_splits(&fit));
}

#[test]
fn straddle_is_bounded_by_the_rules_language() {
	let spec: ParsingSpec = test_spec();

	// `level` is a fixed alternation, so only genuine suffixes of a branch qualify.
	let fit: RunFit = RunFit::compute(&spec, "level", "INFOx");
	// The rule can end with `I`? No: it must end with a whole branch... but a *prefix* query `*I`
	// asks whether some match ends with `I`, which `INFO` does not. Only `INFO` ends a branch.
	assert_eq!(vec![0, 4], suffix_splits(&fit));
	assert!(!fit.fits_wholly());
}

#[test]
fn run_with_no_relationship_to_the_rule_is_empty() {
	let spec: ParsingSpec = test_spec();
	let fit: RunFit = RunFit::compute(&spec, "level", "zzz");
	assert!(fit.is_empty(), "`zzz` shares nothing with INFO|WARN|ERROR");
}

#[test]
fn special_characters_in_a_run_are_escaped() {
	let spec: ParsingSpec = spec_with_rules(&[("star", r"a\*b"), ("slash", r"a\\b")]);

	// A literal `*` in the run must not be read as a wildcard.
	let fit: RunFit = RunFit::compute(&spec, "star", "a*b");
	assert!(fit.fits_wholly());

	// A literal backslash likewise.
	let fit: RunFit = RunFit::compute(&spec, "slash", r"a\b");
	assert!(fit.fits_wholly());

	// And a `*` run must not match a rule that has no literal star.
	let fit: RunFit = RunFit::compute(&spec, "slash", "a*b");
	assert!(!fit.fits_wholly());
}

#[test]
fn unknown_rule_has_no_fit() {
	let spec: ParsingSpec = test_spec();
	// Callers must treat this as "cannot conclude", not as a proof of no match.
	let fit: RunFit = RunFit::compute(&spec, "nonexistent", "abc");
	assert!(!fit.fits_wholly());
}

#[test]
fn cache_returns_the_same_fit() {
	let spec: ParsingSpec = test_spec();
	let cache: RunFitCache = RunFitCache::new();
	assert!(cache.is_empty());

	let first: Arc<RunFit> = cache.get(&spec, "digits", "123");
	assert_eq!(1, cache.len());
	let second: Arc<RunFit> = cache.get(&spec, "digits", "123");
	assert!(Arc::ptr_eq(&first, &second), "a hit must not recompute");
	assert_eq!(1, cache.len());

	// Distinct keys are cached separately.
	let _ = cache.get(&spec, "digits", "456");
	let _ = cache.get(&spec, "word", "123");
	assert_eq!(3, cache.len());

	cache.clear();
	assert!(cache.is_empty());
}

#[test]
fn cache_clone_shares_fits() {
	let spec: ParsingSpec = test_spec();
	let cache: RunFitCache = RunFitCache::new();
	let original: Arc<RunFit> = cache.get(&spec, "digits", "123");

	let cloned: RunFitCache = cache.clone();
	assert_eq!(1, cloned.len());
	assert!(Arc::ptr_eq(&original, &cloned.get(&spec, "digits", "123")));
}

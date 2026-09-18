use super::*;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::ParsingSpecBuilder;
use crate::search::SearchString;
use crate::search::SymbolicChar;
use crate::search::decompose::Run;
use crate::search::decompose::RunFitCache;
use crate::search::decompose::runs_of;

fn spec_with_rules(rules: &[(&str, &str)]) -> ParsingSpec {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	for &(name, pattern) in rules.iter() {
		builder.add_rule(name, pattern).unwrap();
	}
	builder.build()
}

fn test_spec() -> ParsingSpec {
	spec_with_rules(&[("digits", "[0-9]+"), ("word", "[a-z]+")])
}

/// The query's symbols, exactly as written.
///
/// Runs are taken from the raw symbols: whether the query ends in a wildcard is precisely what tells
/// [`runs_of`] whether the last run is anchored at the end, so neither adding nor removing one here is
/// harmless.
fn symbols_of(query: &str) -> Vec<SymbolicChar> {
	SearchString::parse(query).unwrap().as_slice().to_vec()
}

/// The texts of `pieces`, comma separated.
fn texts(pieces: &[PieceRef]) -> String {
	pieces
		.iter()
		.map(|piece| piece.text.clone())
		.collect::<Vec<_>>()
		.join(",")
}

fn composed_for(spec: &ParsingSpec, shape: &str, query: &str) -> (ShapeModel, Composed) {
	let (model, composed, _) = composed_and_runs_for(spec, shape, query);
	(model, composed)
}

fn composed_and_runs_for(spec: &ParsingSpec, shape: &str, query: &str) -> (ShapeModel, Composed, Vec<Run>) {
	let model: ShapeModel = ShapeModel::new(spec, shape);
	let runs: Vec<Run> = runs_of(&symbols_of(query));
	let fits: RunFitCache = RunFitCache::new();
	let table: PlacementTable = PlacementTable::compute(spec, &model, &runs, &fits).unwrap();
	let composed: Composed = compose(spec, &model, &table, &runs, &fits, ComposeBudget::default());
	(model, composed, runs)
}

/// Renders a composition as sorted `part=text` entries, marking captures with `<>`.
fn rendered(composed: &Composed) -> Vec<String> {
	let Composed::Compositions(compositions) = composed else {
		panic!("expected compositions, got {composed:?}");
	};
	let mut out: Vec<String> = compositions
		.iter()
		.map(|composition| {
			let mut entries: Vec<String> = Vec::new();
			for capture in composition.captures.iter() {
				entries.push(format!(
					"{}:<{}={}>",
					capture.part,
					capture.name,
					texts(&capture.pieces)
				));
			}
			for literal in composition.literals.iter() {
				entries.push(format!("{}:'{}'", literal.part, texts(&literal.pieces)));
			}
			entries.sort();
			entries.join(" ")
		})
		.collect::<Vec<_>>();
	out.sort();
	out.dedup();
	out
}

#[test]
fn single_run_inside_a_rule() {
	let spec: ParsingSpec = test_spec();
	let (_, composed) = composed_for(&spec, "id=%digits%", "*123*");
	assert_eq!(vec!["1:<digits=123>"], rendered(&composed));
}

#[test]
fn single_run_in_static_text() {
	let spec: ParsingSpec = test_spec();
	let (_, composed) = composed_for(&spec, "id=%digits%", "*id=*");
	assert_eq!(vec!["0:'id='"], rendered(&composed));
}

#[test]
fn run_straddling_rule_and_text() {
	let spec: ParsingSpec = test_spec();
	// `12y`: `12` from the rule at part 1, `y` from static text at part 2.
	let (_, composed) = composed_for(&spec, "x%digits%y", "*12y*");
	assert!(
		rendered(&composed).contains(&"1:<digits=12> 2:'y'".to_owned()),
		"got {:?}",
		rendered(&composed)
	);
}

#[test]
fn positional_identity_is_recorded() {
	let spec: ParsingSpec = test_spec();
	// The requirement: two `word` instances must be distinguishable. The composition names the part
	// index, so the two placements yield visibly different results.
	let (_, composed) = composed_for(&spec, "A%word%B%word%C", "*qq*");
	assert_eq!(
		vec!["1:<word=qq>", "3:<word=qq>"],
		rendered(&composed),
		"each instance of `word` must be identified by its part index"
	);
}

#[test]
fn two_runs_in_distinct_rules() {
	let spec: ParsingSpec = test_spec();
	let (_, composed) = composed_for(&spec, "A%word%B%digits%C", "*qq*77*");
	assert!(
		rendered(&composed).contains(&"1:<word=qq> 3:<digits=77>".to_owned()),
		"got {:?}",
		rendered(&composed)
	);
}

#[test]
fn two_runs_sharing_one_rule_are_merged() {
	let spec: ParsingSpec = test_spec();
	// Both runs can sit inside the same rule, separated by text the rule also produces.
	let (_, composed) = composed_for(&spec, "id=%digits%", "*1*2*");
	assert!(
		rendered(&composed).contains(&"1:<digits=1,2>".to_owned()),
		"pieces landing in one part must merge in order; got {:?}",
		rendered(&composed)
	);
}

#[test]
fn ordering_is_enforced() {
	let spec: ParsingSpec = test_spec();
	// In-order composes.
	let (_, composed) = composed_for(&spec, "A%digits%B%digits%C", "*A*B*");
	assert!(matches!(composed, Composed::Compositions(_)));

	// Reversed does not: each run is placeable alone, but not left to right.
	let (_, composed) = composed_for(&spec, "A%digits%B%digits%C", "*B*A*");
	assert!(matches!(composed, Composed::Impossible), "got {composed:?}");
}

#[test]
fn run_that_fits_nowhere_is_impossible() {
	let spec: ParsingSpec = test_spec();
	let (_, composed) = composed_for(&spec, "id=%digits%", "*#*");
	assert!(matches!(composed, Composed::Impossible));
}

#[test]
fn wildcard_only_query_has_one_empty_composition() {
	let spec: ParsingSpec = test_spec();
	// `*` constrains nothing, so there is exactly one trivial decomposition.
	let (_, composed) = composed_for(&spec, "id=%digits%", "*");
	let Composed::Compositions(compositions) = &composed else {
		panic!("expected compositions, got {composed:?}");
	};
	assert_eq!(1, compositions.len());
	assert!(compositions[0].captures.is_empty());
	assert!(compositions[0].literals.is_empty());
}

#[test]
fn exhausted_budget_is_unknown_not_impossible() {
	let spec: ParsingSpec = test_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "A%word%B%word%C%word%D");
	let runs: Vec<Run> = runs_of(&symbols_of("*q*q*q*"));
	let fits: RunFitCache = RunFitCache::new();
	let table: PlacementTable = PlacementTable::compute(&spec, &model, &runs, &fits).unwrap();
	let composed: Composed = compose(
		&spec,
		&model,
		&table,
		&runs,
		&fits,
		ComposeBudget { max_compositions: 1 },
	);
	// Degrading must never look like a proof of impossibility.
	assert!(matches!(composed, Composed::Unknown), "got {composed:?}");
}

/// Renders an interpretation the way the engine's `Debug` does, but compactly.
fn render_interpretation(interpretation: &crate::search::Interpretation) -> String {
	interpretation
		.sub_queries
		.iter()
		.map(|sub_query| {
			if !sub_query.is_static_text() {
				format!("<{}={}>", sub_query.fully_qualified_name, sub_query.string_value)
			} else {
				format!("'{}'", sub_query.string_value)
			}
		})
		.collect::<Vec<_>>()
		.join(" ")
}

fn interpretations_of(spec: &ParsingSpec, shape: &str, query: &str) -> Vec<String> {
	let (model, composed, runs) = composed_and_runs_for(spec, shape, query);
	let Composed::Compositions(compositions) = &composed else {
		panic!("expected compositions, got {composed:?}");
	};
	let mut out: Vec<String> = compositions
		.iter()
		.map(|composition| render_interpretation(&composition.to_interpretation(&model, &runs)))
		.collect::<Vec<_>>();
	out.sort();
	out.dedup();
	out
}

/// The query text an interpretation accounts for, as runs of literal characters.
///
/// Concatenating sub-query values and splitting on wildcards recovers what the query's runs must have
/// been, which is what the invariant compares against.
fn runs_accounted_for(interpretation: &crate::search::Interpretation) -> Vec<String> {
	let mut runs: Vec<String> = vec![String::new()];
	for sub_query in interpretation.sub_queries.iter() {
		for symbol in sub_query.symbolic_value.iter() {
			match symbol {
				SymbolicChar::Literal(character) => runs.last_mut().expect("non-empty").push(*character),
				SymbolicChar::GlobStar => {
					if !runs.last().expect("non-empty").is_empty() {
						runs.push(String::new());
					}
				},
			}
		}
	}
	runs.retain(|run| !run.is_empty());
	runs
}

#[test]
fn every_literal_character_survives_rendering() {
	let spec: ParsingSpec = test_spec();
	// Shapes that exercise static text, single and repeated rules, and adjacent rules.
	const SHAPES: &[&str] = &[
		"id=%digits%",
		"A%word%B%word%C",
		"A%word%B%digits%C",
		"%word%%digits%",
		"x%digits%y%word%z%digits%w",
	];
	// Queries whose runs must reappear intact, including ones that straddle part boundaries.
	const QUERIES: &[&str] = &[
		"*qq*", "*77*", "*id=*", "*id=7*", "*1*2*", "*qq*77*", "*x*y*", "*ab7*", "*7cd*",
	];

	let mut checked: usize = 0;
	for shape in SHAPES.iter() {
		for query in QUERIES.iter() {
			let (model, composed, runs) = composed_and_runs_for(&spec, shape, query);
			let Composed::Compositions(compositions) = &composed else {
				continue;
			};
			// What the query itself asks for.
			let expected: Vec<String> = runs.iter().map(|run| run.text.clone()).collect::<Vec<_>>();

			for composition in compositions.iter() {
				let interpretation = composition.to_interpretation(&model, &runs);
				assert_eq!(
					expected,
					runs_accounted_for(&interpretation),
					"query {query:?} on shape {shape:?} rendered as {:?}",
					render_interpretation(&interpretation)
				);
				checked += 1;
			}
		}
	}
	// Guard against the assertions being vacuous.
	assert!(checked > 25, "only {checked} interpretations checked");
}

#[test]
fn vacuous_captures_encode_position() {
	let spec: ParsingSpec = test_spec();
	// Two `word` references: the first is identified by having no vacuous capture before it, the second
	// by having exactly one. The surrounding `'*'`s are the shape's static text (`A`, `B`, `C`), reported
	// unconstrained because the query's wildcards merely pass over them.
	assert_eq!(
		vec!["'*' <word=*> '*' <word=*qq*> '*'", "'*' <word=*qq*> '*'"],
		interpretations_of(&spec, "A%word%B%word%C", "*qq*")
	);
}

#[test]
fn trailing_unconstrained_rules_are_omitted() {
	let spec: ParsingSpec = test_spec();
	// Three references, only the first constrained: the trailing two are unconstrained and omitted.
	// The shape's static text is still reported, so each rendering ends with a `'*'`, never a vacuous
	// capture.
	let rendered: Vec<String> = interpretations_of(&spec, "A%word%B%word%C%word%D", "*qq*");
	assert!(rendered.contains(&"'*' <word=*qq*> '*'".to_owned()), "got {rendered:?}");
	for one in rendered.iter() {
		assert!(!one.ends_with("=*>"), "trailing vacuous capture in {one:?}");
	}
}

#[test]
fn text_matched_by_static_text_is_preserved() {
	let spec: ParsingSpec = test_spec();
	// The run is satisfied by the shape's static text: no rule is constrained, but the query's
	// characters must still appear. The run covers the whole of `id=`, so the value carries no
	// wildcard on either side — a static value must glob-match its part's text exactly, and `'*id='`
	// would not match `id=`.
	assert_eq!(vec!["'id='"], interpretations_of(&spec, "id=%digits%", "*id=*"));
}

/// A static sub-query's value must glob-match its shape part's text *exactly*, so a run landing
/// strictly inside a longer static part has to be padded on **both** sides.
///
/// Padding used to be allowed only for rules, on the grounds that static text "must match verbatim".
/// That reasoning is backwards: precisely because the text is reproduced verbatim, a value covering
/// only part of it must be free to skip the rest. Reporting `'*ab'` for a run `ab` inside `abcdef`
/// claims the text *ends* in `ab`, which is false and unsatisfiable.
#[test]
fn a_run_inside_a_longer_static_part_is_padded_on_both_sides() {
	let spec: ParsingSpec = test_spec();
	assert_eq!(vec!["'*bcd*'"], interpretations_of(&spec, "abcdef", "*bcd*"));
	// Flush against the start: no padding before, but the tail must still be skippable.
	assert_eq!(vec!["'abc*'"], interpretations_of(&spec, "abcdef", "*abc*"));
	// Flush against the end: the mirror image.
	assert_eq!(vec!["'*def'"], interpretations_of(&spec, "abcdef", "*def*"));
	// The whole part: no padding at all.
	assert_eq!(vec!["'abcdef'"], interpretations_of(&spec, "abcdef", "*abcdef*"));
}

#[test]
fn a_run_straddling_a_boundary_stays_contiguous() {
	let spec: ParsingSpec = test_spec();
	// `id=7` is one run, split across static text and the rule. The query has no wildcard inside it, so
	// the rendering must not introduce one between `id=` and `7`.
	assert_eq!(
		vec!["'id=' <digits=7*>"],
		interpretations_of(&spec, "id=%digits%", "*id=7*")
	);
}

#[test]
fn multiple_pieces_in_one_capture_are_wildcard_separated() {
	let spec: ParsingSpec = test_spec();
	// Both runs land in the one rule; the rule may emit text between them. The shape's trailing static
	// text is reported as `'*'`.
	assert_eq!(
		vec!["'*' <digits=*1*2*>"],
		interpretations_of(&spec, "id=%digits%", "*1*2*")
	);
}

#[test]
fn captures_in_distinct_rules_keep_their_positions() {
	let spec: ParsingSpec = test_spec();
	let rendered: Vec<String> = interpretations_of(&spec, "A%word%B%digits%C", "*qq*77*");
	assert!(
		rendered.contains(&"'*' <word=*qq*> '*' <digits=*77*> '*'".to_owned()),
		"got {rendered:?}"
	);
}

/// Whether the composed path finds any decomposition at all.
fn composes(spec: &ParsingSpec, shape: &str, query: &str) -> bool {
	let (_, composed) = composed_for(spec, shape, query);
	matches!(&composed, Composed::Compositions(compositions) if !compositions.is_empty())
}

/// A spec whose rules are alternations, so that "the rule contains X" and "the rule *is* X" differ.
fn level_spec() -> ParsingSpec {
	spec_with_rules(&[("level", "TRACE|DEBUG|INFO|WARN|ERROR|FATAL"), ("word", "[a-zA-Z]+")])
}

#[test]
fn several_runs_can_share_one_static_part() {
	let spec: ParsingSpec = test_spec();
	// Regression: placements recorded only *which* part a run landed in, not where within it. All three
	// runs belong in the single static part `abc `, at increasing offsets, which the part-only DP state
	// could not express — so this decomposition was missed entirely.
	assert!(composes(&spec, "abc %word%", "a*b*c"));
	// The offsets must be respected, not merely recorded: `c*b` is not in ascending order.
	assert!(!composes(&spec, "abc %word%", "c*b*a"));
}

#[test]
fn one_rule_must_produce_all_of_its_runs_together() {
	let spec: ParsingSpec = level_spec();
	// Regression: each run was checked against the rule *individually*, so both `INFO` and `WARN` were
	// placed in one `%level%`. The rule is an alternation: it matches either alone and neither pair.
	let rendered: Vec<String> = interpretations_of(&spec, "%level% %word%", "*INFO*WARN*");
	assert!(
		!rendered.iter().any(|one| one.contains("<level=*INFO*WARN*>")),
		"a single `level` cannot produce both runs: {rendered:?}"
	);
	// The decomposition that splits them across the two rules is still found.
	assert!(
		rendered.iter().any(|one| one.contains("<word=*INFO*WARN*>")),
		"got {rendered:?}"
	);
}

#[test]
fn an_anchored_run_must_start_the_rule_not_merely_occur_in_it() {
	let spec: ParsingSpec = level_spec();
	// Regression: containment (`fits_wholly`) was used where anchoring demands the rule *begin* with the
	// run. `WARN` contains `N`, but no level starts with it, so `N*` must not be placed in `%level%`.
	assert!(!composes(&spec, "%level% %word%", "N*"));
	// A level that really does start with `I` is still placed.
	assert!(composes(&spec, "%level% %word%", "I*"));
}

#[test]
fn an_anchored_straddle_must_match_the_rule_exactly() {
	let spec: ParsingSpec = level_spec();
	// Regression: the straddle path used `suffixes[split]` ("the rule can *end* with this"), but an
	// anchored run pins the rule's start too, so the rule must match the piece exactly. `WARN` ends with
	// `N`, which allowed `NIn*` to be split as `level=N` + `word=In`.
	assert!(!composes(&spec, "%level%%word%", "NIn*"));
	// The same shape still admits a split where the first piece really is a whole level.
	assert!(composes(&spec, "%level%%word%", "INFOxy*"));
}

#[test]
fn captures_and_literals_are_in_shape_order() {
	let spec: ParsingSpec = test_spec();
	let (_, composed) = composed_for(&spec, "a%word%b%digits%c", "*xx*99*");
	let Composed::Compositions(compositions) = &composed else {
		panic!("expected compositions");
	};
	for composition in compositions.iter() {
		let parts: Vec<usize> = composition.captures.iter().map(|c| c.part).collect::<Vec<_>>();
		let mut sorted: Vec<usize> = parts.clone();
		sorted.sort_unstable();
		assert_eq!(sorted, parts, "captures must be in shape order");
	}
}

#[test]
fn unconstrained_static_text_is_reported_as_a_wildcard() {
	let spec: ParsingSpec = test_spec();
	// The query constrains only the rule; the shape's static text is reported as `'*'`, matching the
	// engine and `search_by_name`'s output shape, rather than being dropped.
	assert_eq!(
		vec!["'*' <word=*qq*> '*'"],
		interpretations_of(&spec, "A%word%B", "*qq*")
	);
}

#[test]
fn a_run_split_between_text_and_a_rule_yields_two_sub_queries() {
	let spec: ParsingSpec = test_spec();
	// A run of `foobar` against `foo%word%`: `foo` is the shape's static text and `bar` is the rule's.
	// Both must appear, as two sub-queries.
	assert_eq!(
		vec!["'foo' <word=bar>"],
		interpretations_of(&spec, "foo%word%", "foobar")
	);
}

#[test]
fn a_static_gap_between_captures_is_one_wildcard() {
	let spec: ParsingSpec = test_spec();
	// Both captures are constrained, so the static text separating them is reported once as `'*'`.
	assert!(
		interpretations_of(&spec, "A%word%B%digits%C", "*qq*77*")
			.contains(&"'*' <word=*qq*> '*' <digits=*77*> '*'".to_owned()),
		"got {:?}",
		interpretations_of(&spec, "A%word%B%digits%C", "*qq*77*")
	);
}

#[test]
fn adjacent_static_parts_are_merged() {
	let spec: ParsingSpec = test_spec();
	// An escaped `%` tokenizes `a%%b` into two adjacent static parts. They must render as one sub-query,
	// because `Interpretation::invariants` forbids two static sub-queries in a row.
	for rendered in interpretations_of(&spec, "a%%b%word%", "*a*") {
		let static_count: usize = rendered.matches("'").count() / 2;
		assert!(static_count <= 1, "adjacent static parts not merged: {rendered:?}");
	}
}

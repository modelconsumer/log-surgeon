use super::*;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::ParsingSpecBuilder;
use crate::prefilter::Run;
use crate::prefilter::RunFitCache;
use crate::prefilter::runs_of;
use crate::search::SearchString;
use crate::search::SymbolicChar;

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

/// The query's symbols, with the engine's prefix semantics applied.
///
/// A query not ending in a wildcard still matches a longer message (`*END` behaves as `*END*`), so the
/// wildcard is made explicit. [`runs_of`] then reports no run as end-anchored, which is what lets a
/// capture keep the trailing `*` a rule needs to emit its own text.
fn symbols_of(query: &str) -> Vec<SymbolicChar> {
	let parsed: SearchString = SearchString::parse(query).unwrap();
	let mut symbols: Vec<SymbolicChar> = parsed.as_slice().to_vec();
	if Some(&SymbolicChar::GlobStar) != symbols.last() {
		symbols.push(SymbolicChar::GlobStar);
	}
	symbols
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
	let model: ShapeModel = ShapeModel::new(spec, shape).unwrap();
	let runs: Vec<Run> = runs_of(&symbols_of(query));
	let fits: RunFitCache = RunFitCache::new();
	let table: PlacementTable = PlacementTable::compute(spec, &model, &runs, &fits).unwrap();
	let composed: Composed = compose(&model, &table, ComposeBudget::default());
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
	let model: ShapeModel = ShapeModel::new(&spec, "A%word%B%word%C%word%D").unwrap();
	let runs: Vec<Run> = runs_of(&symbols_of("*q*q*q*"));
	let fits: RunFitCache = RunFitCache::new();
	let table: PlacementTable = PlacementTable::compute(&spec, &model, &runs, &fits).unwrap();
	let composed: Composed = compose(&model, &table, ComposeBudget { max_compositions: 1 });
	// Degrading must never look like a proof of impossibility.
	assert!(matches!(composed, Composed::Unknown), "got {composed:?}");
}

/// Renders an interpretation the way the engine's `Debug` does, but compactly.
fn render_interpretation(interpretation: &crate::search::Interpretation) -> String {
	interpretation
		.sub_queries
		.iter()
		.map(|sub_query| {
			if sub_query.rule_idx.is_some() {
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
	// by having exactly one.
	assert_eq!(
		vec!["<word=*> <word=*qq*>", "<word=*qq*>"],
		interpretations_of(&spec, "A%word%B%word%C", "*qq*")
	);
}

#[test]
fn trailing_unconstrained_rules_are_omitted() {
	let spec: ParsingSpec = test_spec();
	// Three references, only the first constrained: the trailing two add nothing, because the query's
	// implicit trailing wildcard leaves them unconstrained.
	let rendered: Vec<String> = interpretations_of(&spec, "A%word%B%word%C%word%D", "*qq*");
	assert!(rendered.contains(&"<word=*qq*>".to_owned()), "got {rendered:?}");
	// No rendering may end with a vacuous capture.
	for one in rendered.iter() {
		assert!(!one.ends_with("=*>"), "trailing vacuous capture in {one:?}");
	}
}

#[test]
fn text_matched_by_static_text_is_preserved() {
	let spec: ParsingSpec = test_spec();
	// The run is satisfied by the shape's static text: no rule is constrained, but the query's
	// characters must still appear. The leading `*` is the query's own.
	assert_eq!(vec!["'*id='"], interpretations_of(&spec, "id=%digits%", "*id=*"));
}

#[test]
fn a_run_straddling_a_boundary_stays_contiguous() {
	let spec: ParsingSpec = test_spec();
	// `id=7` is one run, split across static text and the rule. The query has no wildcard inside it, so
	// the rendering must not introduce one between `id=` and `7`.
	assert_eq!(
		vec!["'*id=' <digits=7*>"],
		interpretations_of(&spec, "id=%digits%", "*id=7*")
	);
}

#[test]
fn multiple_pieces_in_one_capture_are_wildcard_separated() {
	let spec: ParsingSpec = test_spec();
	// Both runs land in the one rule; the rule may emit text between them.
	assert_eq!(
		vec!["<digits=*1*2*>"],
		interpretations_of(&spec, "id=%digits%", "*1*2*")
	);
}

#[test]
fn captures_in_distinct_rules_keep_their_positions() {
	let spec: ParsingSpec = test_spec();
	let rendered: Vec<String> = interpretations_of(&spec, "A%word%B%digits%C", "*qq*77*");
	assert!(
		rendered.contains(&"<word=*qq*> <digits=*77*>".to_owned()),
		"got {rendered:?}"
	);
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

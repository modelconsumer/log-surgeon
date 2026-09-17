use super::*;
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
	spec_with_rules(&[("digits", "[0-9]+"), ("word", "[a-z]+"), ("level", "INFO|WARN|ERROR")])
}

fn symbols_of(query: &str) -> Vec<SymbolicChar> {
	let parsed: SearchString = SearchString::parse(query).unwrap();
	let symbols: &[SymbolicChar] = parsed.as_slice();
	if Some(&SymbolicChar::GlobStar) == symbols.last() {
		symbols[..(symbols.len() - 1)].to_vec()
	} else {
		symbols.to_vec()
	}
}

fn table_for(spec: &ParsingSpec, shape: &str, query: &str) -> (ShapeModel, Vec<Run>, PlacementTable) {
	let model: ShapeModel = ShapeModel::new(spec, shape).unwrap();
	let runs: Vec<Run> = runs_of(&symbols_of(query));
	let fits: RunFitCache = RunFitCache::new();
	let table: PlacementTable = PlacementTable::compute(spec, &model, &runs, &fits).unwrap();
	(model, runs, table)
}

/// Renders a placement as `part:text` pieces, marking rules with `<>`.
fn render(placement: &Placement) -> String {
	placement
		.pieces
		.iter()
		.map(|piece| {
			if piece.is_rule {
				format!("{}:<{}>", piece.part, piece.text)
			} else {
				format!("{}:'{}'", piece.part, piece.text)
			}
		})
		.collect::<Vec<_>>()
		.join("+")
}

fn rendered(table: &PlacementTable, run: usize) -> Vec<String> {
	let mut out: Vec<String> = table.placements[run].iter().map(render).collect::<Vec<_>>();
	out.sort();
	out.dedup();
	out
}

#[test]
fn splits_a_query_into_runs() {
	let runs: Vec<Run> = runs_of(&symbols_of("*abc*def*"));
	assert_eq!(2, runs.len());
	assert_eq!("abc", runs[0].text);
	assert_eq!("def", runs[1].text);
	assert!(!runs[0].anchored_start);

	// A query with no leading wildcard anchors its first run.
	let runs: Vec<Run> = runs_of(&symbols_of("abc*"));
	assert_eq!(1, runs.len());
	assert!(runs[0].anchored_start);

	// A query with no trailing wildcard anchors its last run. Note the engine strips one trailing
	// wildcard, so `abc` and `abc*` both arrive here as `abc`.
	let runs: Vec<Run> = runs_of(&symbols_of("*abc"));
	assert!(runs[0].anchored_end);

	// A wildcard-only query has no runs at all, so it constrains nothing.
	assert!(runs_of(&symbols_of("*")).is_empty());
}

#[test]
fn run_placed_in_static_text() {
	let spec: ParsingSpec = test_spec();
	// `id=` is static text in part 0.
	let (_, _, table) = table_for(&spec, "id=%digits%", "*id=*");
	assert_eq!(vec!["0:'id='"], rendered(&table, 0));
}

#[test]
fn run_placed_inside_a_rule() {
	let spec: ParsingSpec = test_spec();
	let (_, _, table) = table_for(&spec, "id=%digits%", "*123*");
	// Part 1 is the rule; the run sits wholly inside it.
	assert_eq!(vec!["1:<123>"], rendered(&table, 0));
}

#[test]
fn run_straddling_a_rule_and_following_text() {
	let spec: ParsingSpec = test_spec();
	// `12y`: `12` from the digits rule, `y` from the static text after it.
	let (_, _, table) = table_for(&spec, "x%digits%y", "*12y*");
	assert!(
		rendered(&table, 0).contains(&"1:<12>+2:'y'".to_owned()),
		"got {:?}",
		rendered(&table, 0)
	);
}

#[test]
fn run_straddling_text_then_rule_is_found_from_the_text_side() {
	let spec: ParsingSpec = test_spec();
	// `x12` has `x` in static text and `12` in the rule. The straddle search starts from a rule, so this
	// direction is represented by the *rule* supplying a trailing piece; check the run is placeable.
	let (model, _, table) = table_for(&spec, "x%digits%y", "*x12*");
	assert!(!table.is_impossible(), "`x12` must be placeable in `x%digits%y`");
	assert!(can_compose(&table, model.parts.len()));
}

#[test]
fn positional_identity_distinguishes_two_instances_of_one_rule() {
	let spec: ParsingSpec = test_spec();
	// The key requirement: `A%foo%B%foo%C` has two `foo` instances, and a placement must say *which*.
	let (_, _, table) = table_for(&spec, "A%word%B%word%C", "*qq*");
	let placements: Vec<String> = rendered(&table, 0);
	// Parts 1 and 3 are the two rule instances; both are valid placements and are reported distinctly.
	assert!(placements.contains(&"1:<qq>".to_owned()), "got {placements:?}");
	assert!(placements.contains(&"3:<qq>".to_owned()), "got {placements:?}");
	assert_eq!(2, placements.len());
}

#[test]
fn run_that_fits_nowhere_is_impossible() {
	let spec: ParsingSpec = test_spec();
	// `#` appears in no static text and no rule's language.
	let (_, _, table) = table_for(&spec, "id=%digits%", "*#*");
	assert!(table.is_impossible(), "a run that fits nowhere proves no match");
	assert!(!can_compose(&table, 2));
}

#[test]
fn runs_must_compose_in_order() {
	let spec: ParsingSpec = test_spec();

	// `A` then `B` is in order and composes.
	let (model, _, table) = table_for(&spec, "A%word%B%word%C", "*A*B*");
	assert!(!table.is_impossible());
	assert!(can_compose(&table, model.parts.len()));

	// `B` then `A` is not: each run is placeable alone, but not in that order.
	let (model, _, table) = table_for(&spec, "A%digits%B%digits%C", "*B*A*");
	assert!(!table.is_impossible(), "each run is individually placeable");
	assert!(
		!can_compose(&table, model.parts.len()),
		"but they cannot be placed left to right"
	);
}

#[test]
fn two_runs_may_share_one_rule() {
	let spec: ParsingSpec = test_spec();
	// `*1*2*` can both sit inside the same digits rule, since a rule can emit text between the runs.
	let (model, _, table) = table_for(&spec, "id=%digits%", "*1*2*");
	assert!(!table.is_impossible());
	assert!(can_compose(&table, model.parts.len()), "a rule can hold both runs");
}

#[test]
fn anchored_run_must_start_at_the_shape_start() {
	let spec: ParsingSpec = test_spec();

	// Anchored `id=` matches the shape's opening static text.
	let (_, _, table) = table_for(&spec, "id=%digits%", "id=*");
	assert!(!table.is_impossible());

	// Anchored `d=` does not, since it is not at offset 0.
	let (_, _, table) = table_for(&spec, "id=%digits%", "d=*");
	assert!(table.is_impossible(), "an anchored run cannot start mid-text");
}

#[test]
fn static_text_occurrence_is_required_verbatim() {
	let spec: ParsingSpec = test_spec();
	// `hello` is both a substring of the static text *and* a word the rule can emit, so both placements
	// are reported: the run does not "have" to be in the variable, but it may be.
	let (_, _, table) = table_for(&spec, "hello %word%", "*hello*");
	assert_eq!(vec!["0:'hello'", "1:<hello>"], rendered(&table, 0));

	// `hellp` is not in the static text, but `%word%` is `[a-z]+`, so only the rule placement remains.
	let (_, _, table) = table_for(&spec, "hello %word%", "*hellp*");
	assert_eq!(vec!["1:<hellp>"], rendered(&table, 0));

	// A run in neither is impossible.
	let (_, _, table) = table_for(&spec, "hello %word%", "*HELLO*");
	assert!(table.is_impossible());
}

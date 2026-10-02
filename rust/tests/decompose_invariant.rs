use log_surgeon::parsing_spec::ParsingSpec;
use log_surgeon::parsing_spec::ParsingSpecBuilder;
use log_surgeon::search::Interpretation;
use log_surgeon::search::SearchString;
use log_surgeon::search::SymbolicChar;
use log_surgeon::search::decompose::ComposeBudget;
use log_surgeon::search::decompose::Composed;
use log_surgeon::search::decompose::PlacementTable;
use log_surgeon::search::decompose::Run;
use log_surgeon::search::decompose::RunFitCache;
use log_surgeon::search::decompose::ShapeModel;
use log_surgeon::search::decompose::compose;
use log_surgeon::search::decompose::runs_of;

fn render(interpretation: &Interpretation) -> String {
	interpretation
		.sub_queries
		.iter()
		.map(|sq| {
			if !sq.is_static_text() {
				format!("<{}={}>", sq.fully_qualified_name, sq.string_value)
			} else {
				format!("'{}'", sq.string_value)
			}
		})
		.collect::<Vec<_>>()
		.join(" ")
}

fn runs_accounted_for(interpretation: &Interpretation) -> Vec<String> {
	let mut runs: Vec<String> = vec![String::new()];
	for sub_query in interpretation.sub_queries.iter() {
		for symbol in sub_query.symbolic_value.iter() {
			match symbol {
				SymbolicChar::Literal(c) => runs.last_mut().unwrap().push(*c),
				SymbolicChar::GlobStar => {
					if !runs.last().unwrap().is_empty() {
						runs.push(String::new());
					}
				},
			}
		}
	}
	runs.retain(|r| !r.is_empty());
	runs
}

/// The invariant must hold across the whole real corpus, not just synthetic shapes.
#[test]
fn invariant_holds_on_corpus() {
	let spec: String = std::fs::read_to_string("spec3.txt").unwrap();
	let spec: ParsingSpec = ParsingSpecBuilder::from_parsing_spec_definition(&spec)
		.unwrap()
		.build();
	const LOG_SHAPES: &[&str] = include!("../log_shapes.rs");

	const QUERIES: &[&str] = &[
		"*blk_1073746491_5667*",
		"*INFO*blk*",
		"*blk_-*",
		"*Receiving*",
		"*10.250.*",
		"*NameSystem.allocateBlock*",
		"*terminating*",
		// End-anchored: no trailing wildcard,
		// so the last run must be the last thing in the message.
		"*blk_1073746491_5667",
		"*terminating",
		"*Receiving",
	];

	let mut checked: usize = 0;
	let mut shapes_with_output: usize = 0;

	for query_text in QUERIES.iter() {
		let query: SearchString = SearchString::parse(query_text).unwrap();
		// Runs come from the raw symbols:
		// a trailing wildcard is exactly what marks the last run as unanchored,
		// so it must be neither added nor removed here.
		let runs: Vec<Run> = runs_of(query.as_slice());
		let expected: Vec<String> = runs.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
		let fits: RunFitCache = RunFitCache::new();

		for shape in LOG_SHAPES.iter() {
			let model: ShapeModel = ShapeModel::new(&spec, shape);
			// A cap was exceeded, so the table is incomplete; nothing to check for this shape.
			let Some(table) = PlacementTable::compute(&spec, &model, &runs, &fits) else {
				continue;
			};
			let composed: Composed = compose(
				&spec,
				&model,
				&table,
				&runs,
				query.anchored_end(),
				&fits,
				ComposeBudget::default(),
			);
			let Composed::Compositions(compositions) = composed else {
				continue;
			};
			if !compositions.is_empty() {
				shapes_with_output += 1;
			}
			for composition in compositions.iter() {
				let interpretation = composition.to_interpretation(&model, &runs);
				assert_eq!(
					expected,
					runs_accounted_for(&interpretation),
					"query {query_text:?} on shape {shape:?} rendered as {:?}",
					render(&interpretation)
				);
				checked += 1;
			}
		}
	}

	println!("checked {checked} interpretations over {shapes_with_output} (query, shape) pairs");
	assert!(checked > 1000, "only {checked} checked");
}

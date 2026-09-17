use super::*;
use crate::parsing_spec::ParsingSpecBuilder;

fn spec_with_rules(rules: &[(&str, &str)]) -> ParsingSpec {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	for &(name, pattern) in rules.iter() {
		builder.add_rule(name, pattern).unwrap();
	}
	builder.build()
}

fn leaf_spec() -> ParsingSpec {
	spec_with_rules(&[("level", "TRACE|DEBUG|INFO|WARN|ERROR"), ("digits", "[0-9]+")])
}

/// A compact rendering of the parts, for readable assertions.
fn describe(model: &ShapeModel) -> Vec<String> {
	model
		.parts
		.iter()
		.map(|part| match part {
			ShapePart::Static(text) => format!("text({text})"),
			ShapePart::Placeholder(placeholder) => format!("rule({})", placeholder.name),
		})
		.collect::<Vec<_>>()
}

#[test]
fn tokenizes_static_text_and_placeholders() {
	let spec: ParsingSpec = leaf_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "hello %level% world").unwrap();
	assert_eq!(vec!["text(hello )", "rule(level)", "text( world)"], describe(&model));
	assert_eq!(1, model.num_placeholders());
}

#[test]
fn tokenization_matches_the_engines() {
	// The model must agree with `automata_for_shape` about where placeholders are; both go through
	// `split_log_shape`, so this pins the shared behaviour (including the `%%` escape).
	let spec: ParsingSpec = leaf_spec();
	for shape in ["%level%", "a%level%b", "100%% done", "%level%%digits%", "plain"] {
		let model: ShapeModel = ShapeModel::new(&spec, shape).unwrap();
		let fragments: Vec<LogShapeFragment> = spec.split_log_shape(shape);
		assert_eq!(fragments.len(), model.parts.len(), "shape={shape:?}");
		for (fragment, part) in std::iter::zip(fragments.iter(), model.parts.iter()) {
			match (fragment, part) {
				(LogShapeFragment::Text(text), ShapePart::Static(modelled)) => assert_eq!(text, modelled),
				(LogShapeFragment::Rule(name), ShapePart::Placeholder(placeholder)) => {
					assert_eq!(name, &placeholder.name);
				},
				_ => panic!("mismatched fragment/part for shape={shape:?}"),
			}
		}
	}
}

#[test]
fn escaped_percent_is_static_text() {
	let spec: ParsingSpec = leaf_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "100%% done").unwrap();
	assert_eq!(0, model.num_placeholders());
	assert!(!model.can_decompose());
}

#[test]
fn placeholder_charset_comes_from_the_rule() {
	let spec: ParsingSpec = leaf_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "%digits%").unwrap();
	let placeholder: &Placeholder = model.placeholders().next().unwrap();
	assert!(placeholder.charset.contains('0'));
	assert!(placeholder.charset.contains('9'));
	assert!(!placeholder.charset.contains('a'));
}

#[test]
fn unknown_rule_yields_no_model() {
	let spec: ParsingSpec = leaf_spec();
	// The engine reports this as an error; the prefilter must not prune on it.
	assert!(ShapeModel::new(&spec, "%nonexistent%").is_none());
}

#[test]
fn leaf_root_rule_is_decomposable() {
	let spec: ParsingSpec = leaf_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "x %level% y").unwrap();
	assert!(model.all_placeholders_are_leaves());
	assert!(model.can_decompose());

	// A capture-less root rule is reported under its own name, matching `automata_for_shape`'s
	// implicit whole-rule capture.
	let placeholder: &Placeholder = model.placeholders().next().unwrap();
	assert_eq!(1, placeholder.alternatives.len());
	assert_eq!("level", &*placeholder.alternatives[0].fully_qualified_name);
}

#[test]
fn rule_with_nested_captures_is_not_decomposable() {
	// `blockID` has captures, so it is not a leaf; decomposing it would have to report `num`/`gen`.
	let spec: ParsingSpec = spec_with_rules(&[("blockID", r"blk_(?<num>[0-9]+)_(?<gen>[0-9]+)")]);
	let model: ShapeModel = ShapeModel::new(&spec, "%blockID%").unwrap();
	assert!(!model.all_placeholders_are_leaves());
	assert!(!model.can_decompose());
}

#[test]
fn leaf_sub_rule_reference_is_decomposable() {
	// Referencing the leaf capture directly *is* decomposable.
	let spec: ParsingSpec = spec_with_rules(&[("blockID", r"blk_(?<num>[0-9]+)_(?<gen>[0-9]+)")]);
	let model: ShapeModel = ShapeModel::new(&spec, "%blockID.num%").unwrap();
	assert!(model.can_decompose());
	let placeholder: &Placeholder = model.placeholders().next().unwrap();
	assert_eq!("blockID.num", &*placeholder.alternatives[0].fully_qualified_name);
	assert!(placeholder.charset.contains('7'));
	assert!(!placeholder.charset.contains('b'));
}

#[test]
fn placeholderless_shape_is_not_decomposable() {
	// The important soundness case: a literal-only shape has nothing to attribute query text to, so it
	// must go to the engine rather than being treated as decomposable (and yielding no matches).
	let spec: ParsingSpec = leaf_spec();
	let model: ShapeModel = ShapeModel::new(&spec, "plain literal text").unwrap();
	assert_eq!(0, model.num_placeholders());
	assert!(model.all_placeholders_are_leaves(), "vacuously true");
	assert!(!model.can_decompose());
}

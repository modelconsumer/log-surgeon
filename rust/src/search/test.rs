use super::*;
use crate::parsing_spec::ParsingSpecBuilder;

#[test]
fn search_email() {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	builder
		.add_rule("email", r"(?<user>\w+)@((?<parts>\w+)\.)+(?<tld>\w+)")
		.unwrap();

	let spec: ParsingSpec = builder.build();

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "*a*@*mail*example*", "email");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}
	}
}

#[test]
fn search_block_id() {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	builder
		.add_rule("block_id", r"blk_(?<blockNum>[0-9]+)_(?<genStamp>[0-9]+)")
		.unwrap();

	let spec: ParsingSpec = builder.build();

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "*blk*_566*", "block_id");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}

		assert_eq!(interpretations.len(), 2);
		assert_eq!(interpretations[0].sub_queries[0].string_value, "blk_");
		assert_eq!(interpretations[0].sub_queries[1].string_value, "566*");
		assert_eq!(
			&*interpretations[0].sub_queries[1].fully_qualified_name,
			"block_id.blockNum"
		);
		assert_eq!(interpretations[1].sub_queries[0].string_value, "blk*_");
		// assert_eq!(interpretations[1].sub_queries[1].string_value, "*");
		// assert_eq!(
		// 	&*interpretations[1].sub_queries[1].fully_qualified_name,
		// 	"block_id.blockNum"
		// );
		// assert_eq!(interpretations[0].sub_queries[2].string_value, "_");
		assert_eq!(interpretations[1].sub_queries[1].string_value, "566*");
		assert_eq!(
			&*interpretations[1].sub_queries[1].fully_qualified_name,
			"block_id.genStamp"
		);
	}
}

#[test]
fn search_nested_name_without_leaf_capture() {
	let spec: ParsingSpec = spec! {
		r#"
			foo: "_(?<bar>[a-z]+|(?<baz>[0-9]+))_"
			"#
	};

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "_a*b_", "foo");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}

		assert_eq!(interpretations.len(), 1);
		assert_eq!(interpretations[0].sub_queries[0].string_value, "_a*b_");
		assert_eq!(&*interpretations[0].sub_queries[0].fully_qualified_name, "foo");
	}

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "a*b", "foo.bar");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}

		assert_eq!(interpretations.len(), 2);
		assert_eq!(interpretations[0].sub_queries[0].string_value, "*a*b*");
		assert_eq!(&*interpretations[0].sub_queries[0].fully_qualified_name, "");
		assert_eq!(interpretations[1].sub_queries[0].string_value, "*a*b*");
		assert_eq!(&*interpretations[1].sub_queries[0].fully_qualified_name, "foo");
	}

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "0*1", "foo.bar.baz");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}

		assert_eq!(interpretations.len(), 1);
		assert_eq!(interpretations[0].sub_queries[0].string_value, "0*1");
		assert_eq!(&*interpretations[0].sub_queries[0].fully_qualified_name, "foo.bar.baz");
	}
}

#[test]
fn test_subsumes() {
	let a: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![SymbolicChar::Literal('a'), SymbolicChar::GlobStar],
		string_value: String::new(),
	};
	let b: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![SymbolicChar::Literal('a')],
		string_value: String::new(),
	};
	let c: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![SymbolicChar::GlobStar, SymbolicChar::Literal('a')],
		string_value: String::new(),
	};
	let d: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![SymbolicChar::Literal('a')],
		string_value: String::new(),
	};
	let e: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![SymbolicChar::GlobStar],
		string_value: String::new(),
	};
	let f: SubQuery = SubQuery {
		group: 0,
		rule_idx: None,
		fully_qualified_name: Arc::from(""),
		symbolic_value: vec![
			SymbolicChar::GlobStar,
			SymbolicChar::Literal('a'),
			SymbolicChar::GlobStar,
		],
		string_value: String::new(),
	};

	assert!(a.subsumes(&b));
	assert!(!b.subsumes(&a));

	assert!(c.subsumes(&d));
	assert!(!d.subsumes(&e));

	assert!(!a.subsumes(&c));
	assert!(!c.subsumes(&a));

	assert!(a.subsumes(&a));
	assert!(b.subsumes(&b));
	assert!(c.subsumes(&c));
	assert!(d.subsumes(&d));
	assert!(e.subsumes(&e));

	assert!(e.subsumes(&a));
	assert!(e.subsumes(&b));
	assert!(e.subsumes(&c));
	assert!(e.subsumes(&d));
	assert!(e.subsumes(&f));
}

#[test]
fn full_log_search() {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	builder
		.add_rule("email", r"(?<user>\w+)@((?<parts>\w+)\.)+(?<tld>\w+)")
		.unwrap();

	let spec: ParsingSpec = builder.build();

	let interpretations: Vec<Interpretation> = do_full_search(&spec, "a@com*", "hello a@com.example");

	println!("=== Interpretations");
	for interpretation in interpretations.iter() {
		println!("- {interpretation:?}");
	}
}

#[test]
fn kv_ip_pattern() {
	let spec: ParsingSpec = spec! {
		r#"
		kv: "(?<key>[\w_./\\-]+)([:=]|: )(?<ip_value>/?\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d{1,5})?)"
		"#
	};

	let interpretations: Vec<Interpretation> =
		do_full_search(&spec, "*172.31.17.135*", "hello %kv.key%: %kv.ip_value% world");

	println!("=== Interpretations");
	for interpretation in interpretations.iter() {
		println!("- {interpretation:?}");
	}

	// assert_eq!(interpretations.len(), 2);
}

fn do_full_search(spec: &ParsingSpec, query: &str, shape: &str) -> Vec<Interpretation> {
	let query: SearchString = SearchString::parse(query).unwrap();

	let mut interpretations: Vec<Vec<Interpretation>> = query.search_by_log_shapes(&spec, &[shape]);

	interpretations.pop().unwrap()
}

fn do_search_by_name(spec: &ParsingSpec, query: &str, name: &str) -> Vec<Interpretation> {
	let query: SearchString = SearchString::parse(query).unwrap();

	let interpretations: Vec<Interpretation> = query.search_by_name(&spec, name);

	interpretations
}

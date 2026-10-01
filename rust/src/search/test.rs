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
		let interpretations: Vec<Interpretation> =
			do_search_by_name(&spec, "*a*@*mail*example*", "email");
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
		let interpretations: Vec<Interpretation> =
			do_search_by_name(&spec, "*blk*_566*", "block_id");
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
		assert_eq!(interpretations[0].sub_queries[2].string_value, "*");
		assert_eq!(interpretations[0].sub_queries[3].string_value, "*");
		assert_eq!(
			&*interpretations[0].sub_queries[3].fully_qualified_name,
			"block_id.genStamp"
		);

		assert_eq!(interpretations[1].sub_queries[0].string_value, "blk*");
		assert_eq!(interpretations[1].sub_queries[1].string_value, "*");
		assert_eq!(
			&*interpretations[1].sub_queries[1].fully_qualified_name,
			"block_id.blockNum"
		);
		assert_eq!(interpretations[1].sub_queries[2].string_value, "_");
		assert_eq!(interpretations[1].sub_queries[3].string_value, "566*");
		assert_eq!(
			&*interpretations[1].sub_queries[3].fully_qualified_name,
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
		assert_eq!(
			&*interpretations[0].sub_queries[0].fully_qualified_name,
			"foo"
		);
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
		assert_eq!(
			&*interpretations[1].sub_queries[0].fully_qualified_name,
			"foo"
		);
	}

	{
		let interpretations: Vec<Interpretation> = do_search_by_name(&spec, "0*1", "foo.bar.baz");
		println!("===");

		for i in interpretations.iter() {
			println!("- {i:?}");
		}

		assert_eq!(interpretations.len(), 1);
		assert_eq!(interpretations[0].sub_queries[0].string_value, "0*1");
		assert_eq!(
			&*interpretations[0].sub_queries[0].fully_qualified_name,
			"foo.bar.baz"
		);
	}
}

#[test]
fn test_covers() {
	let a: SubQuery =
		SubQuery::new_static_text(vec![SymbolicChar::Literal('a'), SymbolicChar::GlobStar]);
	let b: SubQuery = SubQuery::new_static_text(vec![SymbolicChar::Literal('a')]);
	let c: SubQuery =
		SubQuery::new_static_text(vec![SymbolicChar::GlobStar, SymbolicChar::Literal('a')]);
	let d: SubQuery = SubQuery::new_static_text(vec![SymbolicChar::Literal('a')]);
	let e: SubQuery = SubQuery::new_static_text(vec![SymbolicChar::GlobStar]);
	let f: SubQuery = SubQuery::new_static_text(vec![
		SymbolicChar::GlobStar,
		SymbolicChar::Literal('a'),
		SymbolicChar::GlobStar,
	]);

	assert!(a.covers(&b));
	assert!(!b.covers(&a));

	assert!(c.covers(&d));
	assert!(!d.covers(&e));

	assert!(!a.covers(&c));
	assert!(!c.covers(&a));

	assert!(a.covers(&a));
	assert!(b.covers(&b));
	assert!(c.covers(&c));
	assert!(d.covers(&d));
	assert!(e.covers(&e));

	assert!(e.covers(&a));
	assert!(e.covers(&b));
	assert!(e.covers(&c));
	assert!(e.covers(&d));
	assert!(e.covers(&f));
}

/// Whether the glob `pattern` matches the whole of `text`.
fn glob_matches(pattern: &[SymbolicChar], text: &[char]) -> bool {
	match pattern.first() {
		None => text.is_empty(),
		Some(SymbolicChar::GlobStar) => {
			(0..=text.len()).any(|skip| glob_matches(&pattern[1..], &text[skip..]))
		},
		Some(&SymbolicChar::Literal(expected)) => {
			!text.is_empty() && (text[0] == expected) && glob_matches(&pattern[1..], &text[1..])
		},
	}
}

/// Every value with no adjacent wildcards, up to `length` symbols over `alphabet`.
fn values_in_normal_form(alphabet: &[char], length: usize) -> Vec<Vec<SymbolicChar>> {
	let mut found: Vec<Vec<SymbolicChar>> = Vec::new();
	let mut frontier: Vec<Vec<SymbolicChar>> = vec![Vec::new()];
	found.push(Vec::new());

	for _ in 0..length {
		let mut next: Vec<Vec<SymbolicChar>> = Vec::new();
		for value in frontier.iter() {
			// Adjacent wildcards are excluded: no producer emits them, and `covers` is not even
			// reflexive on them. See `Interpretation::invariants`.
			if !matches!(value.last(), Some(SymbolicChar::GlobStar)) {
				next.push([value.as_slice(), &[SymbolicChar::GlobStar]].concat());
			}
			for &character in alphabet.iter() {
				next.push([value.as_slice(), &[SymbolicChar::Literal(character)]].concat());
			}
		}
		found.extend(next.iter().cloned());
		frontier = next;
	}

	found
}

/// `covers` must never claim a containment that does not hold.
///
/// This is the *only* direction the implementation guarantees, and the one
/// `dedup_covered_interpretations` depends on: a spurious `true` deletes a real answer, whereas a
/// missed containment merely leaves a redundant one. The converse is deliberately not asserted --
/// `covers` is a positional test and misses e.g. `aa*` against `aaa*`.
#[test]
fn covers_never_claims_an_unsound_containment() {
	let alphabet: [char; 2] = ['a', 'b'];
	let values: Vec<Vec<SymbolicChar>> = values_in_normal_form(&alphabet, 4);

	// Every word the patterns could distinguish, up to a length exceeding the longest pattern.
	let mut words: Vec<Vec<char>> = vec![Vec::new()];
	let mut frontier: Vec<Vec<char>> = vec![Vec::new()];
	for _ in 0..6 {
		let mut next: Vec<Vec<char>> = Vec::new();
		for word in frontier.iter() {
			for &character in alphabet.iter() {
				next.push([word.as_slice(), &[character]].concat());
			}
		}
		words.extend(next.iter().cloned());
		frontier = next;
	}

	let languages: Vec<Vec<bool>> = values
		.iter()
		.map(|value| Vec::from_iter(words.iter().map(|word| glob_matches(value, word))))
		.collect::<Vec<_>>();

	let mut checked: usize = 0;
	for (general, general_language) in values.iter().zip(languages.iter()) {
		for (specific, specific_language) in values.iter().zip(languages.iter()) {
			if !SubQuery::new_static_text(general.clone())
				.covers(&SubQuery::new_static_text(specific.clone()))
			{
				continue;
			}
			checked += 1;
			let unsound: Option<usize> = specific_language
				.iter()
				.zip(general_language.iter())
				.position(|(&in_specific, &in_general)| in_specific && !in_general);
			assert!(
				unsound.is_none(),
				"{:?} claims to cover {:?}, but {:?} matches only the latter",
				String::from_iter(general.iter().map(ToString::to_string)),
				String::from_iter(specific.iter().map(ToString::to_string)),
				words[unsound.expect("just checked")]
					.iter()
					.collect::<String>(),
			);
		}
	}

	assert!(
		0 < checked,
		"expected some pair to be covered, or the test proves nothing"
	);
	println!("verified {checked} covering pairs against true glob containment");
}

/// `covers` is reflexive and transitive on values in normal form, which is what makes
/// `dedup_covered_interpretations` reach a fixpoint.
#[test]
fn covers_is_a_partial_order_in_normal_form() {
	let values: Vec<SubQuery> = values_in_normal_form(&['a', 'b'], 3)
		.into_iter()
		.map(SubQuery::new_static_text)
		.collect::<Vec<_>>();

	for value in values.iter() {
		assert!(
			value.covers(value),
			"not reflexive: {:?}",
			value.string_value
		);
	}

	for general in values.iter() {
		for middle in values.iter().filter(|middle| general.covers(middle)) {
			for specific in values.iter().filter(|specific| middle.covers(specific)) {
				assert!(
					general.covers(specific),
					"not transitive: {:?} > {:?} > {:?}",
					general.string_value,
					middle.string_value,
					specific.string_value,
				);
			}
		}
	}
}

#[test]
fn full_log_search() {
	let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
	builder
		.add_rule("email", r"(?<user>\w+)@((?<parts>\w+)\.)+(?<tld>\w+)")
		.unwrap();

	let spec: ParsingSpec = builder.build();

	let interpretations: Vec<Interpretation> =
		do_full_search(&spec, "a@com*", "hello a@com.example");

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

	let interpretations: Vec<Interpretation> = do_full_search(
		&spec,
		"*172.31.17.135*",
		"hello %kv.key%: %kv.ip_value% world",
	);

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

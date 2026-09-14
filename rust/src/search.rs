#[cfg(test)]
mod test;

use std::collections::BTreeMap;
use std::num::NonZero;
use std::sync::Arc;

use crate::nfa::Path;
use crate::nfa::PathComponent;
use crate::nfa::Tnfa;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleIdx;
use crate::parsing_spec::RuleInfo;
use crate::parsing_spec::SubRule;
use crate::regex::Regex;

#[derive(Debug)]
pub struct SearchString(Vec<SymbolicChar>);

impl std::fmt::Display for SearchString {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		for ch in self.0.iter() {
			ch.fmt(fmt)?;
		}
		Ok(())
	}
}

#[derive(Debug)]
pub enum SearchStringError<'input> {
	InvalidEscape { before: &'input str, after: &'input str },
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum SymbolicChar {
	Literal(char),
	GlobStar,
	GlobOne,
}

impl std::fmt::Debug for SymbolicChar {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(self, fmt)
	}
}

impl std::fmt::Display for SymbolicChar {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Literal(ch) => {
				match ch {
					'*' | '?' | '\\' => {
						fmt.write_str("\\")?;
					},
					_ => (),
				}
				ch.fmt(fmt)
			},
			Self::GlobStar => fmt.write_str("*"),
			Self::GlobOne => fmt.write_str("?"),
		}
	}
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Interpretation {
	pub sub_queries: Vec<SubQuery>,
}

impl std::fmt::Debug for Interpretation {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.sub_queries
			.iter()
			.fold(&mut fmt.debug_list(), |list, sub_query| list.entry(sub_query))
			.finish()
	}
}

#[derive(Clone)]
pub struct SubQuery {
	/// Used to associate leaf queries of the same root match together.
	pub group: usize,
	pub rule_idx: Option<RuleIdx>,
	pub fully_qualified_name: Arc<str>,
	pub symbolic_value: Vec<SymbolicChar>,
	pub string_value: String,
}

impl std::fmt::Debug for SubQuery {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if let Some(rule_idx) = self.rule_idx {
			fmt.write_fmt(format_args!(
				"({}:?<{}:{}>{})",
				self.group, rule_idx, self.fully_qualified_name, self.string_value,
			))
		} else {
			fmt.write_str(&self.string_value)
		}
	}
}

impl Eq for SubQuery {}

impl Ord for SubQuery {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		(
			&self.group,
			&self.rule_idx,
			&self.fully_qualified_name,
			&self.symbolic_value,
		)
			.cmp(&(
				&other.group,
				&other.rule_idx,
				&other.fully_qualified_name,
				&other.symbolic_value,
			))
	}
}

impl PartialOrd for SubQuery {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl PartialEq for SubQuery {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other).is_eq()
	}
}

#[derive(Debug, Clone)]
pub struct InterpretationPrefix {
	successors: BTreeMap<SubQuery, Self>,
}

#[derive(Clone, Copy)]
struct SearchStringView<'a> {
	full_string: &'a SearchString,
	start: usize,
	end: usize,
}

impl std::fmt::Debug for SearchStringView<'_> {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		fmt.debug_tuple("SearchStringView")
			.field(&self.as_str().iter().map(SymbolicChar::to_string).collect::<String>())
			.finish()
	}
}

impl std::fmt::Display for SearchStringView<'_> {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		for ch in self.iter() {
			ch.fmt(fmt)?;
		}
		Ok(())
	}
}

impl Interpretation {
	fn dedup_covered_interpretations(interpretations: &mut Vec<Self>) {
		interpretations.sort();
		interpretations.dedup();

		let mut i: usize = 0;
		while i < interpretations.len() {
			let mut j: usize = i + 1;
			while j < interpretations.len() {
				if interpretations[i].sub_queries.len() != interpretations[j].sub_queries.len() {
					j += 1;
					continue;
				}
				if std::iter::zip(
					interpretations[i].sub_queries.iter(),
					interpretations[j].sub_queries.iter(),
				)
				.all(|(query1, query2)| query1.subsumes(query2))
				{
					interpretations.remove(j);
					continue;
				}
				if std::iter::zip(
					interpretations[i].sub_queries.iter(),
					interpretations[j].sub_queries.iter(),
				)
				.all(|(query1, query2)| query2.subsumes(query1))
				{
					interpretations.swap(i, j);
					interpretations.remove(j);
					j = i + 1;
					continue;
				}
				j += 1;
			}
			i += 1;
		}
	}

	/// Re-numbers groups consecutively starting from `1`
	/// (static text always has group `0`).
	#[allow(unused)]
	fn canonicalize(&mut self, cache: &mut [Option<NonZero<usize>>]) {
		cache.fill(None);

		let mut n: NonZero<usize> = NonZero::<usize>::MIN;

		for sub_query in self.sub_queries.iter_mut() {
			if sub_query.group == 0 {
				continue;
			}
			sub_query.group = cache[sub_query.group]
				.get_or_insert_with(|| {
					let x: NonZero<usize> = n.checked_add(1).unwrap();
					std::mem::replace(&mut n, x)
				})
				.get();
		}
	}
}

impl SearchString {
	pub fn parse(input: &str) -> Result<Self, SearchStringError<'_>> {
		let mut chars: Vec<SymbolicChar> = Vec::new();
		let mut last_was_escape: bool = false;
		for (i, ch) in input.char_indices() {
			match ch {
				'*' | '?' | '\\' => {
					chars.push(if last_was_escape {
						SymbolicChar::Literal(ch)
					} else {
						match ch {
							'*' => SymbolicChar::GlobStar,
							'?' => SymbolicChar::GlobOne,
							'\\' => {
								last_was_escape = true;
								continue;
							},
							_ => {
								unreachable!();
							},
						}
					});
				},
				_ => {
					if last_was_escape {
						let (before, after): (&str, &str) = input.split_at(i);
						return Err(SearchStringError::InvalidEscape { before, after });
					} else {
						chars.push(SymbolicChar::Literal(ch));
					}
				},
			}
			last_was_escape = false;
		}
		if last_was_escape {
			return Err(SearchStringError::InvalidEscape {
				before: input,
				after: "",
			});
		}
		Ok(Self(chars))
	}

	pub fn as_slice(&self) -> &[SymbolicChar] {
		&self.0
	}

	pub fn search_by_name(&self, spec: &ParsingSpec, name: &str) -> Vec<Interpretation> {
		let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(name);

		if name.is_empty() {
			return Vec::new();
		}

		self.view(0, self.0.len()).interpretations_for_name(spec, &rows)
	}

	pub fn search_by_log_shapes(&self, spec: &ParsingSpec, log_shapes: &[&str]) -> Vec<Vec<Interpretation>> {
		let view: SearchStringView<'_> = if self.0.last().unwrap() == &SymbolicChar::GlobStar {
			self.view(0, self.0.len() - 1)
		} else {
			self.view(0, self.0.len())
		};

		log_shapes
			.iter()
			.map(|shape| {
				let shape: &str = shape.as_ref();
				// TODO unwrap
				let automata: Tnfa = spec.automata_for_shape(shape).unwrap();
				view.interpretations_for_shape(spec, &automata)
			})
			.collect::<Vec<_>>()
	}

	fn view(&self, start: usize, end: usize) -> SearchStringView<'_> {
		SearchStringView {
			full_string: self,
			start,
			end,
		}
	}
}

impl<'a> SearchStringView<'a> {
	fn as_str(&self) -> &[SymbolicChar] {
		&self.full_string.0[self.start..self.end]
	}

	fn to_regex(&self) -> Regex {
		Regex::Sequence(Vec::from_iter(self.as_str().iter().map(SymbolicChar::to_regex)))
	}

	fn interpretations_for_name(&self, spec: &ParsingSpec, rows: &[(&RuleInfo, &Regex)]) -> Vec<Interpretation> {
		let mut interpretations: Vec<Interpretation> = Vec::new();

		for &(rule_info, regex) in rows.iter() {
			let rule_nfa: Tnfa = Tnfa::for_single_rule(rule_info.root_idx, regex, &[]);

			let potential_interpretations: Vec<Interpretation> =
				self.interpretations_for_nfa(spec, &rule_nfa, 0, Some(rule_info));

			interpretations.extend(potential_interpretations.into_iter());
		}

		interpretations.sort();
		interpretations.dedup();

		interpretations
	}

	fn interpretations_for_shape(&self, _spec: &ParsingSpec, shape_nfa: &Tnfa) -> Vec<Interpretation> {
		assert_ne!(self.as_str(), [SymbolicChar::GlobStar]);

		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex());

		now!(t0);
		let intersection: Tnfa = shape_nfa.intersect::<true, false>(&search_nfa);
		now!(t1);
		debug!(
			"intersecting {} states with {} states took {}",
			shape_nfa.states.len(),
			search_nfa.states.len(),
			t1.duration_since(t0).as_millis()
		);
		if intersection.definitely_cannot_accept() {
			return Vec::new();
		}

		let paths: Vec<Path> = intersection.compute_paths::<true>();

		for path in paths.iter() {
			assert!(!path.components.is_empty());

			let mut sub_queries: Vec<SubQuery> = Vec::new();

			for token in path.components.iter() {
				match token {
					PathComponent::Literal(contents) => {
						sub_queries.push(SubQuery::new_static_text(contents.clone()));
					},
					PathComponent::Capture { sub_rule, contents } => {
						sub_queries.push(SubQuery::new(1, sub_rule, contents.clone()));
					},
				}
			}

			interpretations.push(Interpretation { sub_queries });
		}

		interpretations.iter().for_each(Interpretation::invariants);

		interpretations.sort();
		interpretations.dedup();

		Interpretation::dedup_covered_interpretations(&mut interpretations);

		interpretations
	}

	fn interpretations_for_nfa(
		&self,
		spec: &ParsingSpec,
		nfa: &Tnfa,
		group: usize,
		maybe_rule_info: Option<&RuleInfo>,
	) -> Vec<Interpretation> {
		assert_ne!(self.as_str(), [SymbolicChar::GlobStar]);

		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex());

		let intersection: Tnfa = nfa.intersect::<true, true>(&search_nfa);

		let paths: Vec<Path> = intersection.compute_paths::<false>();

		for path in paths.iter() {
			assert!(!path.components.is_empty());

			let mut sub_queries: Vec<SubQuery> = Vec::new();

			if let PathComponent::Literal(contents) = path.components.first().unwrap()
				&& (path.components.len() == 1)
			{
				let rule: &RootRule = &spec[path.rule_idx];
				let rule_info: &RuleInfo = if let Some(rule_info) = maybe_rule_info {
					assert_eq!(rule_info.root_idx, rule.idx);
					rule_info
				} else {
					&rule[None]
				};

				let mut implicit_capture: SubQuery = SubQuery::new(
					group,
					&Arc::new(SubRule {
						name: rule.name.clone(),
						regex: rule.regex.regex.clone(),
						root_rule_idx: rule.idx,
						// TODO
						id: NonZero::<u16>::MAX,
						parent_id: None,
						descendants: 0,
						qualified_name: rule.name.clone(),
						fully_qualified_name: rule.name.clone(),
					}),
					contents.clone(),
				);

				if let Some(sub_rule) = &rule_info.maybe_sub_rule {
					assert!(!sub_rule.is_leaf());

					let mut static_text: SubQuery = SubQuery::new_static_text(contents.clone());

					implicit_capture.surround_with_wildcards();
					static_text.surround_with_wildcards();

					interpretations.push(Interpretation {
						sub_queries: vec![implicit_capture],
					});
					interpretations.push(Interpretation {
						sub_queries: vec![static_text],
					});
				} else {
					interpretations.push(Interpretation {
						sub_queries: vec![implicit_capture],
					});
				}

				continue;
			}

			for token in path.components.iter() {
				match token {
					PathComponent::Literal(contents) => {
						sub_queries.push(SubQuery::new_static_text(contents.clone()));
					},
					PathComponent::Capture { sub_rule, contents } => {
						sub_queries.push(SubQuery::new(group, sub_rule, contents.clone()));
					},
				}
			}

			interpretations.push(Interpretation { sub_queries });
		}

		interpretations.iter().for_each(Interpretation::invariants);

		Interpretation::dedup_covered_interpretations(&mut interpretations);

		interpretations
	}
}

impl std::ops::Deref for SearchStringView<'_> {
	type Target = [SymbolicChar];

	fn deref(&self) -> &Self::Target {
		self.as_str()
	}
}

impl SymbolicChar {
	pub fn is_wildcard(&self) -> bool {
		matches!(self, Self::GlobStar | Self::GlobOne)
	}

	fn to_regex(&self) -> Regex {
		match *self {
			SymbolicChar::Literal(ch) => Regex::Literal(ch),
			SymbolicChar::GlobStar => Regex::KleeneClosure(Box::new(Regex::AnyChar)),
			SymbolicChar::GlobOne => Regex::BoundedRepetition {
				min: 0,
				max: 1,
				item: Box::new(Regex::AnyChar),
			},
		}
	}
}

impl Interpretation {
	fn invariants(&self) {
		let mut last_was_static_text: bool = false;
		for sub_query in self.sub_queries.iter() {
			if sub_query.is_static_text() {
				assert!(!last_was_static_text);
				last_was_static_text = true;
			} else {
				last_was_static_text = false;
			}
		}
	}
}

impl SubQuery {
	fn new_static_text(symbolic_value: Vec<SymbolicChar>) -> Self {
		let string_value: String = symbolic_value.iter().fold(String::new(), |mut accum, &ch| {
			accum.push_str(&ch.to_string());
			accum
		});
		Self {
			group: 0,
			rule_idx: None,
			fully_qualified_name: Arc::from(""),
			symbolic_value,
			string_value,
		}
	}

	fn new(group: usize, sub_rule: &SubRule, symbolic_value: Vec<SymbolicChar>) -> Self {
		// TODO duplicated above
		let string_value: String = symbolic_value.iter().fold(String::new(), |mut accum, &ch| {
			accum.push_str(&ch.to_string());
			accum
		});
		if sub_rule.fully_qualified_name.is_empty() {
			panic!("qualified name is {}", sub_rule.qualified_name);
		}
		assert!(!sub_rule.fully_qualified_name.is_empty());
		Self {
			group,
			rule_idx: Some(sub_rule.root_rule_idx),
			fully_qualified_name: sub_rule.fully_qualified_name.clone(),
			symbolic_value,
			string_value,
		}
	}

	fn is_static_text(&self) -> bool {
		self.rule_idx.is_none()
	}

	/// Returns `true` iff `self` is a "refinement" of `other`,
	/// or `self` is exactly a single wildcard (star).
	fn subsumes(&self, other: &Self) -> bool {
		if (self.group, self.rule_idx, &self.fully_qualified_name)
			!= (other.group, other.rule_idx, &other.fully_qualified_name)
		{
			return false;
		}
		if self.symbolic_value == [SymbolicChar::GlobStar] {
			return true;
		}
		// Remark: Always has 1 subslice.
		let mut other_parts: Vec<&[SymbolicChar]> = other
			.symbolic_value
			.split(|&ch| ch == SymbolicChar::GlobStar)
			.collect::<Vec<_>>();
		if *other.symbolic_value.last().unwrap() == SymbolicChar::GlobStar {
			if *self.symbolic_value.last().unwrap() != SymbolicChar::GlobStar {
				return false;
			}
			let last: &[SymbolicChar] = other_parts.pop().unwrap();
			assert_eq!(last, []);
		}
		let mut i: usize = 0;
		if *other.symbolic_value.first().unwrap() == SymbolicChar::GlobStar {
			if *self.symbolic_value.first().unwrap() != SymbolicChar::GlobStar {
				return false;
			}
			assert_eq!(other_parts[0], []);
			i += 1;
		}
		for my_part in self.symbolic_value.split(|&ch| ch == SymbolicChar::GlobStar) {
			if my_part.is_empty() {
				continue;
			}
			let Some(other_part): Option<&[SymbolicChar]> = other_parts.get(i).copied() else {
				return false;
			};
			if let Some(suffix) = other_part.strip_prefix(my_part) {
				if suffix.is_empty() {
					i += 1;
				} else {
					other_parts[i] = suffix;
				}
			} else {
				return false;
			}
		}
		i == other_parts.len()
	}

	fn surround_with_wildcards(&mut self) {
		if *self.symbolic_value.first().unwrap() != SymbolicChar::GlobStar {
			self.symbolic_value.insert(0, SymbolicChar::GlobStar);
			self.string_value.insert(0, '*');
		}
		if *self.symbolic_value.last().unwrap() != SymbolicChar::GlobStar {
			self.symbolic_value.push(SymbolicChar::GlobStar);
			self.string_value.push('*');
		}
	}
}

impl InterpretationPrefix {
	pub fn from_interpretations(interpretations: &[Interpretation]) -> Self {
		let mut this: Self = Self::new();
		for interpretation in interpretations.iter() {
			this.add_interpretation(&interpretation.sub_queries);
		}
		this
	}

	pub fn len(&self) -> usize {
		self.successors.len()
	}

	pub fn total_len(&self) -> usize {
		if self.successors.is_empty() {
			return 1;
		}
		self.successors
			.values()
			.fold(0, |accum, successor| accum + successor.total_len())
	}

	fn new() -> Self {
		Self {
			successors: BTreeMap::new(),
		}
	}

	fn add_interpretation(&mut self, sub_queries: &[SubQuery]) {
		let Some(first): Option<&SubQuery> = sub_queries.first() else {
			return;
		};
		self.successors
			.entry(first.clone())
			.or_insert_with(Self::new)
			.add_interpretation(&sub_queries[1..]);
	}

	pub fn print(&self, indent: usize) {
		for (sub_query, successors) in self.successors.iter() {
			println!(
				"{:\t>indent$}- {sub_query:?} ({} -> {})",
				"",
				successors.len(),
				successors.total_len()
			);
			successors.print(indent + 1);
		}
	}
}

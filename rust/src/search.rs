#[cfg(test)]
mod test;

// use std::collections::BTreeMap;
use std::num::NonZero;
use std::sync::Arc;

use crate::nfa::Path;
use crate::nfa::PathComponent;
use crate::nfa::Tnfa;
use crate::parsing_spec::LogShapeFragment;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleIdx;
use crate::parsing_spec::RuleInfo;
use crate::parsing_spec::SubRule;
use crate::regex::Regex;

#[derive(Debug)]
pub struct SearchString {
	symbols: Vec<SymbolicChar>,
	fragments: Vec<String>,
}

impl std::fmt::Display for SearchString {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		for ch in self.symbols.iter() {
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
				if matches!(ch, '*' | '\\') {
					fmt.write_str("\\")?;
				}
				ch.fmt(fmt)
			},
			Self::GlobStar => fmt.write_str("*"),
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
		let mut symbols: Vec<SymbolicChar> = Vec::new();
		let mut fragments: Vec<String> = Vec::new();
		let mut current_fragment: String = String::new();
		let mut last_was_escape: bool = false;
		for (i, ch) in input.char_indices() {
			match ch {
				'*' | '\\' => {
					symbols.push(if last_was_escape {
						current_fragment.push('*');
						SymbolicChar::Literal(ch)
					} else {
						match ch {
							'*' => {
								fragments.push(std::mem::replace(&mut current_fragment, String::new()));
								SymbolicChar::GlobStar
							},
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
						current_fragment.push(ch);
						symbols.push(SymbolicChar::Literal(ch));
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
		fragments.push(current_fragment);
		Ok(Self { symbols, fragments })
	}

	pub fn as_slice(&self) -> &[SymbolicChar] {
		self.symbols.as_slice()
	}

	pub fn search_by_name(&self, spec: &ParsingSpec, name: &str) -> Vec<Interpretation> {
		let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(name);

		if name.is_empty() {
			return Vec::new();
		}

		self.view(0, self.symbols.len()).interpretations_for_name(spec, &rows)
	}

	pub fn search_by_log_shapes(&self, spec: &ParsingSpec, log_shapes: &[&str]) -> Vec<Vec<Interpretation>> {
		let view: SearchStringView<'_> = if self.symbols.last().unwrap() == &SymbolicChar::GlobStar {
			self.view(0, self.symbols.len() - 1)
		} else {
			self.view(0, self.symbols.len())
		};

		/*
		let mut boundary_chars: Vec<char> = Vec::new();
		for (i, search_fragment) in self.fragments.iter().enumerate() {
			if (i > 0)
				&& let Some(ch) = search_fragment.chars().next()
			{
				boundary_chars.push(ch);
			}
			if ((i + 1) < self.fragments.len())
				&& let Some(ch) = search_fragment.chars().next_back()
			{
				boundary_chars.push(ch);
			}
		}
		boundary_chars.sort();
		boundary_chars.dedup();
		*/
		let mut search_characters: Vec<char> =
			Vec::from_iter(self.fragments.iter().flat_map(|fragment| fragment.chars()));
		search_characters.sort();
		search_characters.dedup();
		// println!("search characters is {:?}", search_characters);

		let mut interpretations_by_shape: Vec<Vec<Interpretation>> = Vec::with_capacity(log_shapes.len());
		for &shape in log_shapes.iter() {
			let shape_fragments: Vec<LogShapeFragment> = spec.split_log_shape(shape);
			let mut must_be_in_rule: bool = true;
			for (i, fragment) in shape_fragments.iter().enumerate() {
				let LogShapeFragment::Rule(_rule_name): &LogShapeFragment = fragment else {
					continue;
				};
				if i > 0 {
					let fragment_before: &LogShapeFragment = &shape_fragments[i - 1];
					match fragment_before {
						LogShapeFragment::Text(text) => {
							assert!(!text.is_empty());
							let boundary: char = text.chars().next_back().unwrap();
							if search_characters.binary_search(&boundary).is_ok() {
								must_be_in_rule = false;
								break;
							}
						},
						LogShapeFragment::Rule(_rule_name) => {
							must_be_in_rule = false;
							break;
						},
					}
				}
				if i + 1 < shape_fragments.len() {
					let fragment_after: &LogShapeFragment = &shape_fragments[i + 1];
					match fragment_after {
						LogShapeFragment::Text(text) => {
							assert!(!text.is_empty());
							let boundary: char = text.chars().next().unwrap();
							if search_characters.binary_search(&boundary).is_ok() {
								must_be_in_rule = false;
								break;
							}
						},
						LogShapeFragment::Rule(_rule_name) => {
							must_be_in_rule = false;
							break;
						},
					}
				}
			}
			if must_be_in_rule {
				now!(t0);
				trace!("must be in rule: {shape:.1024}");
				let mut fragment_interpretations_matrix: Vec<Vec<Interpretation>> =
					vec![Vec::new(); shape_fragments.len() * self.fragments.len()];

				let mut text_interpretations: Vec<Interpretation> = vec![
					Interpretation {
						sub_queries: Vec::new()
					};
					shape_fragments.len()
				];

				let mut rule_indices: Vec<usize> = Vec::new();

				for (i, fragment) in shape_fragments.iter().enumerate() {
					match fragment {
						LogShapeFragment::Text(text) => {
							text_interpretations[i] = Interpretation {
								sub_queries: vec![SubQuery::new_static_text(Vec::from_iter(
									text.chars().map(SymbolicChar::Literal),
								))],
							};
						},
						LogShapeFragment::Rule(rule_name) => {
							rule_indices.push(i);
							for (j, search_fragment) in self.fragments.iter().enumerate() {
								fragment_interpretations_matrix[(i * self.fragments.len()) + j] =
									if !search_fragment.is_empty() {
										let sub_search: SearchString =
											SearchString::parse(&format!("*{search_fragment}*")).unwrap();
										let interpretations: Vec<Interpretation> =
											sub_search.search_by_name(spec, rule_name);

										// if !interpretations.is_empty() {
										// 	trace!(
										// 		"interpretations for {search_fragment} and {rule_name} can match"
										// 	);
										// 	for interp in interpretations.iter() {
										// 		println!("- {interp:?}");
										// 	}
										// }

										interpretations
									} else {
										/*
										Vec::from_iter(spec.rules_for_name(rule_name).into_iter().map(
											|(rule_info, _regex)| {
												let sub_rule: Arc<SubRule> =
													if let Some(sub_rule) = rule_info.maybe_sub_rule.clone() {
														sub_rule
													} else {
														Arc::new(SubRule {
															name: rule_info.root_name.clone(),
															regex: Regex::NIL,
															root_rule_idx: rule_info.root_idx,
															// TODO
															id: NonZero::<u16>::MAX,
															parent_id: None,
															descendants: 0,
															qualified_name: rule_info.root_name.clone(),
															fully_qualified_name: rule_info.root_name.clone(),
														})
													};
												Interpretation {
													sub_queries: vec![SubQuery::new(
														0,
														&sub_rule,
														vec![SymbolicChar::GlobStar],
													)],
												}
											},
										))
										*/
										Vec::new()
									};
							}
						},
					}
				}

				if rule_indices.is_empty() {
					interpretations_by_shape.push(Vec::new());
					continue;
				}

				let mut finished: Vec<Interpretation> = Vec::new();

				let mut stack: Vec<(Interpretation, usize, usize)> = Vec::new();

				for i in 0..self.fragments.len() {
					// let mut no_match: bool = true;
					for interpretation in
						fragment_interpretations_matrix[(rule_indices[0] * self.fragments.len()) + i].iter()
					{
						stack.push((interpretation.clone(), 1, i + 1));
						// no_match = false;
					}
					// if no_match {
					// 	println!(
					// 		"fragment {:?} no matches among {:?}",
					// 		&shape_fragments[rule_indices[0]],
					// 		&self.fragments[..]
					// 	);
					// }
				}

				while let Some((interpretation, shape_fragment, search_fragment)) = stack.pop() {
					if shape_fragment == rule_indices.len() {
						finished.push(interpretation);
						continue;
					}
					if search_fragment == self.fragments.len() {
						continue;
					}
					let row: usize = rule_indices[shape_fragment];
					let search_fragment: usize = search_fragment.max(row + 1);
					for col in search_fragment..self.fragments.len() {
						// let mut no_match: bool = true;
						for interpretation in fragment_interpretations_matrix[(row * self.fragments.len()) + col].iter()
						{
							stack.push((interpretation.clone(), shape_fragment + 1, col + 1));
							// no_match = false;
						}
						// if no_match {
						// 	println!(
						// 		"fragment {:?} no matches among {:?}",
						// 		&shape_fragments[rule_indices[shape_fragment]],
						// 		&self.fragments[..]
						// 	);
						// }
					}
				}

				now!(t1);
				debug!("have {} interpretations, took {} ms", finished.len(), millis!(t0, t1));
				// for interp in finished.iter() {
				// 	println!("- {interp:?}");
				// }
				interpretations_by_shape.push(finished);
			} else {
				// TODO unwrap
				let automata: Tnfa = spec.automata_for_shape(shape).unwrap();
				interpretations_by_shape.push(view.interpretations_for_shape(spec, &automata));
			}
		}

		interpretations_by_shape
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
		&self.full_string.symbols[self.start..self.end]
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
		trace!(
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
		*self == Self::GlobStar
	}

	fn to_regex(&self) -> Regex {
		match *self {
			SymbolicChar::Literal(ch) => Regex::Literal(ch),
			SymbolicChar::GlobStar => Regex::KleeneClosure(Box::new(Regex::AnyChar)),
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

#[cfg(test)]
mod test;

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
use crate::prefilter;
use crate::prefilter::ComposeBudget;
use crate::prefilter::Composed;
use crate::prefilter::PlacementTable;
use crate::prefilter::Run;
use crate::prefilter::RunFitCache;
use crate::prefilter::ShapeModel;
use crate::prefilter::ShapeModelCache;
use crate::regex::Regex;

#[derive(Debug)]
pub struct SearchString {
	symbols: Vec<SymbolicChar>,
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
	pub rule_idx: Option<RuleIdx>,
	pub fully_qualified_name: Arc<str>,
	pub symbolic_value: Vec<SymbolicChar>,
	pub string_value: String,
}

impl std::fmt::Debug for SubQuery {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if let Some(rule_idx) = self.rule_idx {
			fmt.write_fmt(format_args!(
				"(?<{}:{}>{})",
				rule_idx, self.fully_qualified_name, self.string_value,
			))
		} else {
			fmt.write_str(&self.string_value)
		}
	}
}

impl Eq for SubQuery {}

impl Ord for SubQuery {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		(&self.rule_idx, &self.fully_qualified_name, &self.symbolic_value).cmp(&(
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

/// A query as the engine sees it, with its end-anchoring made explicit.
///
/// See [`SearchString::anchored`], which is the only place these two are derived, so that every caller
/// agrees about what `*foo` means.
#[derive(Clone, Copy)]
struct AnchoredQuery<'a> {
	/// The query with a single trailing wildcard removed, if it had one.
	view: SearchStringView<'a>,
	/// Whether a match must run to the end of the message.
	anchored_end: bool,
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
}

impl SearchString {
	pub fn parse(input: &str) -> Result<Self, SearchStringError<'_>> {
		let mut symbols: Vec<SymbolicChar> = Vec::new();
		let mut last_was_escape: bool = false;
		for (i, ch) in input.char_indices() {
			match ch {
				'*' | '\\' => {
					symbols.push(if last_was_escape {
						SymbolicChar::Literal(ch)
					} else {
						match ch {
							'*' => SymbolicChar::GlobStar,
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
		Ok(Self { symbols })
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
		self.search_by_log_shapes_with(spec, Some(&ShapeModelCache::new()), log_shapes)
	}

	/// As [`Self::search_by_log_shapes`], reusing `cache`'s prefilter models.
	///
	/// Prefer this when searching the same shapes more than once, e.g. via
	/// [`crate::parser::Parser::shape_models`]: building a shape's prefilter model is otherwise repeated
	/// for every query.
	pub fn search_by_log_shapes_cached(
		&self,
		spec: &ParsingSpec,
		cache: &ShapeModelCache,
		log_shapes: &[&str],
	) -> Vec<Vec<Interpretation>> {
		self.search_by_log_shapes_with(spec, Some(cache), log_shapes)
	}

	fn search_by_log_shapes_with(
		&self,
		spec: &ParsingSpec,
		cache: Option<&ShapeModelCache>,
		log_shapes: &[&str],
	) -> Vec<Vec<Interpretation>> {
		let anchored: AnchoredQuery<'_> = self.anchored();

		// The query's runs and their fits are shared across every shape: this is where the composition
		// path gets its leverage, since a corpus mentions the same few rules over and over, and a rule's
		// behaviour on a run does not depend on the shape referencing it.
		//
		// Note the runs come from the *raw* symbols, not the engine's view: dropping the trailing
		// wildcard is what tells [`prefilter::runs_of`] the last run is anchored, and re-adding one would
		// erase exactly the information anchoring depends on.
		let runs: Vec<Run> = prefilter::runs_of(&self.symbols);
		let fits: RunFitCache = RunFitCache::new();

		log_shapes
			.iter()
			.map(|&shape| {
				anchored
					.view
					.interpretations_for_log_shape(spec, cache, shape, &runs, &fits, anchored.anchored_end)
			})
			.collect::<Vec<_>>()
	}

	/// This query split into the engine's view of it, plus whether it is anchored at the end.
	///
	/// Anchoring is one uniform rule, read off both ends the same way: a query is anchored at a boundary
	/// exactly when it does not have a wildcard there. So `foo*` is anchored at the start, `*foo` at the
	/// end, `foo` at both (an exact match), and `*foo*` at neither.
	///
	/// Start anchoring needs no flag because it is already *structural*: an unanchored query literally
	/// begins with a [`SymbolicChar::GlobStar`], so the automaton built from it begins with `.*`. End
	/// anchoring cannot be structural in the same way — a trailing `.*` would have to be simulated
	/// through the whole rest of the shape — so it is carried as a flag, the wildcard is dropped from
	/// the view, and the automata are instead told not to require reaching the shape's end. That keeps
	/// the unanchored case exactly as cheap as it was: acceptance is decided the moment the query's own
	/// automaton accepts.
	fn anchored(&self) -> AnchoredQuery<'_> {
		match self.symbols.last() {
			Some(&SymbolicChar::GlobStar) => AnchoredQuery {
				view: self.view(0, self.symbols.len() - 1),
				anchored_end: false,
			},
			// Also covers the empty query, which anchors vacuously.
			_ => AnchoredQuery {
				view: self.view(0, self.symbols.len()),
				anchored_end: true,
			},
		}
	}

	/// Interpretations for `log_shape`, always via the automata, bypassing [`crate::prefilter`].
	///
	/// Exposed so that tests can pin the prefilter against the engine it is meant to agree with.
	pub fn interpretations_for_log_shape_via_engine(&self, spec: &ParsingSpec, log_shape: &str) -> Vec<Interpretation> {
		// TODO unwrap
		let automata: Tnfa = spec.automata_for_shape(log_shape).unwrap();
		self.interpretations_for_automata(spec, &automata)
	}

	/// Interpretations for an already-built shape automaton.
	///
	/// Exposed so that tests can pin a *truncated* automaton (see
	/// [`ParsingSpec::automata_for_fragments`]) against the full one it stands in for.
	pub fn interpretations_for_automata(&self, spec: &ParsingSpec, automata: &Tnfa) -> Vec<Interpretation> {
		let anchored: AnchoredQuery<'_> = self.anchored();
		anchored
			.view
			.interpretations_for_shape(spec, automata, anchored.anchored_end)
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

	/// Interpretations of this query for a single log shape.
	///
	/// Three paths, cheapest first:
	///
	/// 1. [`crate::prefilter::can_match`] on the coarse charset model. When it proves the shape cannot
	///    match, the shape's TNFA is never built and never intersected with the query.
	/// 2. [`crate::prefilter::compose`], which decomposes the query against the shape directly, reusing
	///    per-rule simulations across shapes. This answers the real question — which rule instance holds
	///    what text — without building any automata for the shape.
	/// 3. The engine, when composition declines to conclude (an unreasonable rule, or an exhausted
	///    budget). This is the slow path that the other two exist to avoid.
	///
	/// Note the [`crate::prefilter::align`] decompositions are *not* usable as a result: placeholders
	/// there are over-approximated (notably they may match the empty string, which a rule like `[a-z]+`
	/// cannot), so they form a superset of the engine's interpretations. Composition is exact because it
	/// simulates the rules themselves rather than their charsets.
	fn interpretations_for_log_shape(
		&self,
		spec: &ParsingSpec,
		cache: Option<&ShapeModelCache>,
		shape: &str,
		runs: &[Run],
		fits: &RunFitCache,
		anchored_end: bool,
	) -> Vec<Interpretation> {
		now!(t0);
		// `None` means a placeholder named an undefined rule; leave that to the engine, which reports it.
		let maybe_model: Option<Arc<ShapeModel>> = match cache {
			Some(cache) => cache.get(spec, shape),
			None => ShapeModel::new(spec, shape).map(Arc::new),
		};

		// Kept so the engine fallback can reuse the placements to narrow the shape it builds.
		let mut maybe_narrowed: Option<Tnfa> = None;

		if let Some(model) = maybe_model.as_ref() {
			if !prefilter::can_match(model, self.as_str(), anchored_end) {
				trace!("prefilter rejected shape {shape:.256}");
				return Vec::new();
			}

			// A shape of pure static text has nowhere to attribute the query's characters, and a non-leaf
			// placeholder is reported as the whole rule rather than its nested captures, so its capture
			// names would not match the engine's. Both belong to the engine — but their placements are
			// still valid, and still worth computing, because they narrow the automaton it builds.
			//
			// `None` means some rule could not be reasoned about, so nothing may be concluded at all.
			let maybe_table: Option<PlacementTable> = PlacementTable::compute(spec, model, runs, fits);

			if let Some(table) = maybe_table.as_ref()
				&& model.can_decompose()
				&& let Some(interpretations) = Self::composed_interpretations(spec, model, table, runs, fits)
			{
				now!(t1);
				trace!("composed shape in {} ms {shape:.256}", millis!(t0, t1));
				return interpretations;
			}

			// Composition declined to conclude, but its placements still bound where the query's literal
			// text can sit, which is enough to drop the shape's unreachable tail from the automaton.
			maybe_narrowed = maybe_table
				.as_ref()
				.and_then(|table| self.truncated_automata(spec, model, table, anchored_end));
		}

		let automata: Tnfa = match maybe_narrowed {
			Some(automata) => automata,
			// TODO unwrap
			None => spec.automata_for_shape(shape).unwrap(),
		};
		let interpretations: Vec<Interpretation> = self.interpretations_for_shape(spec, &automata, anchored_end);
		now!(t1);
		debug!("- took {} ms", millis!(t0, t1));
		interpretations
	}

	/// The shape's automaton, truncated to the parts the query can actually reach.
	///
	/// Real shapes carry very long tails of static text — tens of thousands of characters — while their
	/// placeholders cluster near the front, and one state is emitted per literal character. Under prefix
	/// matching the intersection accepts as soon as the *query's* automaton accepts, which happens at the
	/// end of its last literal run, so those trailing states are built and intersected only to be thrown
	/// away.
	///
	/// [`PlacementTable::window`] bounds every placement of every run, so no literal character of the
	/// query can land beyond `window.1`. Truncating there is exactly "do not simulate the query's
	/// trailing wildcard": the parts dropped are the ones it would have consumed.
	///
	/// Only the *tail* is dropped. The head must be kept verbatim — the query's leading wildcard still
	/// has to traverse it, and replacing it with `.*` would be a superset, letting runs straddle where
	/// the real static text forbids it and inventing interpretations the full shape does not have.
	///
	/// Returns `None` when there is nothing to truncate, so the caller builds the shape as before.
	fn truncated_automata(
		&self,
		spec: &ParsingSpec,
		model: &ShapeModel,
		table: &PlacementTable,
		anchored_end: bool,
	) -> Option<Tnfa> {
		// A query anchored at the end must consume the shape through to its end, so nothing may be
		// dropped: the truncated parts are precisely the ones it still has to match.
		if anchored_end {
			return None;
		}

		let (_, end): (usize, usize) = table.window()?;
		let last: usize = model.parts.len().checked_sub(1)?;

		if end >= last {
			return None;
		}

		let fragments: Vec<LogShapeFragment> = model.fragments_in(0, end);
		let truncated: Tnfa = spec.automata_for_fragments(&fragments).ok()?;

		trace!("truncated shape from {} parts to 0..={end}", model.parts.len());

		Some(truncated)
	}

	/// Interpretations for `model` via [`crate::prefilter::compose`].
	///
	/// `None` means composition declined to conclude and the caller must fall back to the engine. An
	/// empty vector is a real answer — the shape cannot match — and is not the same thing.
	fn composed_interpretations(
		spec: &ParsingSpec,
		model: &ShapeModel,
		table: &PlacementTable,
		runs: &[Run],
		fits: &RunFitCache,
	) -> Option<Vec<Interpretation>> {
		match prefilter::compose(spec, model, table, runs, fits, ComposeBudget::default()) {
			Composed::Impossible => Some(Vec::new()),
			Composed::Unknown => None,
			Composed::Compositions(compositions) => {
				let mut interpretations: Vec<Interpretation> = Vec::from_iter(
					compositions
						.iter()
						.map(|composition| composition.to_interpretation(model, runs)),
				);
				// The engine's callers expect a canonical, duplicate-free set; distinct compositions can
				// render identically once positions collapse to the same sub-queries.
				interpretations.sort();
				interpretations.dedup();
				Some(interpretations)
			},
		}
	}

	fn interpretations_for_name(&self, spec: &ParsingSpec, rows: &[(&RuleInfo, &Regex)]) -> Vec<Interpretation> {
		let mut interpretations: Vec<Interpretation> = Vec::new();

		for &(rule_info, regex) in rows.iter() {
			let rule_nfa: Tnfa = Tnfa::for_single_rule(rule_info.root_idx, regex, &[]);

			let potential_interpretations: Vec<Interpretation> =
				self.interpretations_for_nfa(spec, &rule_nfa, Some(rule_info));

			interpretations.extend(potential_interpretations.into_iter());
		}

		interpretations.sort();
		interpretations.dedup();

		interpretations
	}

	/// Interpretations of this query against a shape's automaton.
	///
	/// `anchored_end` decides how the intersection is closed off, and is the whole of end-anchoring on
	/// the engine side:
	///
	/// - `false` (`*foo*`, `foo*`): the intersection accepts as soon as the *query's* automaton accepts,
	///   whatever state the shape is in. The shape's remaining states are never explored, which is what
	///   makes an absent trailing wildcard cheap — it is never simulated, only assumed. Paths are then
	///   closed with a trailing wildcard to say "and then anything".
	/// - `true` (`*foo`, `foo`): both automata must accept together, so the match runs to the end of the
	///   message. Nothing is appended to the paths, and a rule pinned to producing nothing is reported
	///   as an empty capture.
	fn interpretations_for_shape(
		&self,
		_spec: &ParsingSpec,
		shape_nfa: &Tnfa,
		anchored_end: bool,
	) -> Vec<Interpretation> {
		assert_ne!(self.as_str(), [SymbolicChar::GlobStar]);

		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex());

		now!(t0);
		let intersection: Tnfa = if anchored_end {
			shape_nfa.intersect::<true, true>(&search_nfa)
		} else {
			shape_nfa.intersect::<true, false>(&search_nfa)
		};
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

		let paths: Vec<Path> = if anchored_end {
			intersection.compute_paths::<false>()
		} else {
			intersection.compute_paths::<true>()
		};

		for path in paths.iter() {
			assert!(!path.components.is_empty());

			let mut sub_queries: Vec<SubQuery> = Vec::new();

			for token in path.components.iter() {
				match token {
					PathComponent::Literal(contents) => {
						sub_queries.push(SubQuery::new_static_text(contents.clone()));
					},
					PathComponent::Capture { sub_rule, contents } => {
						sub_queries.push(SubQuery::new(sub_rule, contents.clone()));
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
						sub_queries.push(SubQuery::new(sub_rule, contents.clone()));
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
	pub(crate) fn new_static_text(symbolic_value: Vec<SymbolicChar>) -> Self {
		let string_value: String = symbolic_value.iter().fold(String::new(), |mut accum, &ch| {
			accum.push_str(&ch.to_string());
			accum
		});
		Self {
			rule_idx: None,
			fully_qualified_name: Arc::from(""),
			symbolic_value,
			string_value,
		}
	}

	pub(crate) fn new(sub_rule: &SubRule, symbolic_value: Vec<SymbolicChar>) -> Self {
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
		if (self.rule_idx, &self.fully_qualified_name) != (other.rule_idx, &other.fully_qualified_name) {
			return false;
		}
		if self.symbolic_value == [SymbolicChar::GlobStar] {
			return true;
		}
		// An empty value is a capture pinned to the empty string (an end-anchored query against a rule
		// such as `(?<leaf>[a-z]*)`). It is maximally specific: it subsumes only another empty value, and
		// nothing subsumes it but itself.
		if self.symbolic_value.is_empty() || other.symbolic_value.is_empty() {
			return self.symbolic_value == other.symbolic_value;
		}
		// Remark: Always has 1 subslice.
		let mut other_parts: Vec<&[SymbolicChar]> = other
			.symbolic_value
			.split(|&ch| ch == SymbolicChar::GlobStar)
			.collect::<Vec<_>>();
		if *other.symbolic_value.last().expect("non-empty") == SymbolicChar::GlobStar {
			if *self.symbolic_value.last().expect("non-empty") != SymbolicChar::GlobStar {
				return false;
			}
			let last: &[SymbolicChar] = other_parts.pop().expect("always has 1 subslice");
			assert_eq!(last, []);
		}
		let mut i: usize = 0;
		if *other.symbolic_value.first().expect("non-empty") == SymbolicChar::GlobStar {
			if *self.symbolic_value.first().expect("non-empty") != SymbolicChar::GlobStar {
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
		// An empty value means the rule is pinned to producing nothing; padding it with wildcards would
		// turn that into "anything", which is the opposite claim.
		if self.symbolic_value.is_empty() {
			return;
		}
		if *self.symbolic_value.first().expect("non-empty") != SymbolicChar::GlobStar {
			self.symbolic_value.insert(0, SymbolicChar::GlobStar);
			self.string_value.insert(0, '*');
		}
		if *self.symbolic_value.last().expect("non-empty") != SymbolicChar::GlobStar {
			self.symbolic_value.push(SymbolicChar::GlobStar);
			self.string_value.push('*');
		}
	}
}

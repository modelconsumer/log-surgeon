pub mod decompose;

#[cfg(test)]
mod test;

use std::sync::Arc;

use crate::nfa::Path;
use crate::nfa::PathComponent;
use crate::nfa::Tnfa;
use crate::parsing_spec::LogShapeFragment;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleInfo;
use crate::regex::Regex;
use crate::search::decompose::ComposeBudget;
use crate::search::decompose::Composed;
use crate::search::decompose::PlacementTable;
use crate::search::decompose::Query;
use crate::search::decompose::RunFitCache;
use crate::search::decompose::ShapeModel;
use crate::search::decompose::ShapeModelCache;

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
	InvalidEscape {
		before: &'input str,
		after: &'input str,
	},
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
	pub leaf_queries: Vec<LeafQuery>,
}

impl std::fmt::Debug for Interpretation {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.leaf_queries
			.iter()
			.fold(&mut fmt.debug_list(), |list, query| list.entry(query))
			.finish()
	}
}

#[derive(Clone)]
pub struct LeafQuery {
	pub fully_qualified_name: Arc<str>,
	pub symbolic_value: Vec<SymbolicChar>,
	pub string_value: String,
}

impl std::fmt::Debug for LeafQuery {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if !self.is_static_text() {
			fmt.write_fmt(format_args!(
				"(?<{}>{:?})",
				self.fully_qualified_name, self.string_value,
			))
		} else {
			fmt.write_str(&self.string_value)
		}
	}
}

impl Eq for LeafQuery {}

impl Ord for LeafQuery {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		(&self.fully_qualified_name, &self.symbolic_value)
			.cmp(&(&other.fully_qualified_name, &other.symbolic_value))
	}
}

impl PartialOrd for LeafQuery {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl PartialEq for LeafQuery {
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

/// A shape's automaton, possibly cut short, with what that cut left out.
///
/// See [`SearchStringView::truncated_automata`]. The tail is what lets the rendering stay
/// identical to the un-truncated shape's: dropped static text still has to appear as a
/// trailing `'*'`, because it is text the message contains even though the query does not
/// constrain it.
struct TruncatedShape {
	automata: Tnfa,
	/// What the dropped parts (if any) are made of; [`Tail::Empty`] for a complete shape.
	dropped: Tail,
}

/// What the parts after the last rendered one consist of.
///
/// An unanchored query's trailing wildcard absorbs them,
/// but the *rendering* still depends on what they are:
/// static text must be reported (as a `'*'` on the preceding static sub-query,
/// or one of its own), while variables are unconstrained captures, which are omitted.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Tail {
	/// Nothing follows.
	Empty,
	/// Only rule references follow.
	OnlyVariables,
	/// Some static text follows.
	HasStatic,
}

impl Tail {
	/// Classifies `parts`, a slice of everything after the last rendered part.
	fn of_parts(parts: &[decompose::ShapePart]) -> Self {
		if parts.is_empty() {
			Self::Empty
		} else if parts.iter().all(decompose::ShapePart::is_variable) {
			Self::OnlyVariables
		} else {
			Self::HasStatic
		}
	}

	/// Classifies a suffix of *rendered* sub-queries, the ones the engine's path dropped.
	fn of_leaves(leaf_queries: &[LeafQuery]) -> Self {
		if leaf_queries.is_empty() {
			Self::Empty
		} else if leaf_queries.iter().any(LeafQuery::is_static_text) {
			Self::HasStatic
		} else {
			Self::OnlyVariables
		}
	}

	/// Combines two tails over disjoint regions: static text in either dominates.
	fn or(self, other: Self) -> Self {
		std::cmp::max(self, other)
	}

	/// Accounts for `tail` in a rendered interpretation covering parts up to it,
	/// for an unanchored query. One rule, shared by composition and the engine:
	///
	/// - nothing rendered at all -> the whole query was unconstrained: a single `'*'`;
	/// - the last sub-query is static -> it must permit *some*thing after it,
	///   whenever anything follows (its bare text would assert the message ends there);
	/// - the last sub-query is a capture -> a `'*'` sub-query is added,
	///   but only to stand for dropped *static text*;
	///   dropped variables are unconstrained captures, which are omitted.
	fn finish_unanchored(self, leaf_queries: &mut Vec<LeafQuery>) {
		if leaf_queries.is_empty() {
			leaf_queries.push(LeafQuery::new_static_text(vec![SymbolicChar::GlobStar]));
			return;
		}
		let last: &mut LeafQuery = leaf_queries.last_mut().expect("non-empty");
		if last.is_static_text() {
			if Self::Empty != self {
				last.append_wildcard();
			}
		} else if Self::HasStatic == self {
			leaf_queries.push(LeafQuery::new_static_text(vec![SymbolicChar::GlobStar]));
		}
	}
}

impl std::fmt::Debug for SearchStringView<'_> {
	fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		fmt.debug_tuple("SearchStringView")
			.field(
				&self
					.as_str()
					.iter()
					.map(SymbolicChar::to_string)
					.collect::<String>(),
			)
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
	/// Whether every sub-query of `self` covers the corresponding one of `other`,
	/// so `other` describes nothing `self` does not already describe.
	///
	/// Positional, so it presumes both decompose the shape the same way;
	/// a difference in length means they are different decompositions and neither covers the other.
	fn covers(&self, other: &Self) -> bool {
		(self.leaf_queries.len() == other.leaf_queries.len())
			&& std::iter::zip(self.leaf_queries.iter(), other.leaf_queries.iter())
				.all(|(mine, theirs)| mine.covers(theirs))
	}

	/// Drops interpretations that another already covers.
	///
	/// [`LeafQuery::covers`] is exact glob containment,
	/// so the result is a minimal antichain up to interpretations with equal languages.
	///
	/// Order is not preserved. Callers sort beforehand only to `dedup` exact duplicates; nothing
	/// downstream depends on the order, and not preserving it keeps this a single pass over each
	/// candidate.
	fn dedup_covered_interpretations(interpretations: &mut Vec<Self>) {
		interpretations.sort();
		interpretations.dedup();

		let mut kept: Vec<Self> = Vec::with_capacity(interpretations.len());

		for candidate in interpretations.drain(..) {
			if kept.iter().any(|keeper| keeper.covers(&candidate)) {
				continue;
			}
			// The candidate survives, so anything it covers is now redundant. `swap_remove` is used
			// rather than `retain` only because the order is already known not to matter.
			let mut index: usize = 0;
			while index < kept.len() {
				if candidate.covers(&kept[index]) {
					kept.swap_remove(index);
				} else {
					index += 1;
				}
			}
			kept.push(candidate);
		}

		*interpretations = kept;
	}
}

impl SearchString {
	/// Parses a query.
	///
	/// `*` and `\` are the only metacharacters; `\*` and `\\` write them literally.
	///
	/// The result is canonical: adjacent wildcards are collapsed, since `**` says exactly what
	/// `*` does. Everything downstream relies on this -- anchoring is read off the first and
	/// last symbol, and a wildcard is a symbol like any other that the automata consume --
	/// so it is established once, here, rather than at every entry point.
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
							'*' => {
								if symbols.last().is_some_and(SymbolicChar::is_wildcard) {
									continue;
								}
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

	/// Whether a match must begin at the start of the message.
	///
	/// Anchoring is one uniform rule, read off both ends the same way:
	/// a query is anchored at a boundary exactly when it does not have a wildcard there.
	/// So `foo*` is anchored at the start, `*foo` at the end,
	/// `foo` at both (an exact match), and `*foo*` at neither.
	/// The empty query is anchored at both, and matches only an empty message.
	///
	/// Nothing is derived from this beyond the predicate itself:
	/// a wildcard is a symbol the automata consume like any other,
	/// so every match runs from the start of the message to its end,
	/// and "unanchored" just means a `*` is there to absorb the slack.
	#[must_use]
	pub fn anchored_start(&self) -> bool {
		!self.symbols.first().is_some_and(SymbolicChar::is_wildcard)
	}

	/// Whether a match must run through to the end of the message.
	///
	/// See [`Self::anchored_start`].
	#[must_use]
	pub fn anchored_end(&self) -> bool {
		!self.symbols.last().is_some_and(SymbolicChar::is_wildcard)
	}

	pub fn search_by_name(&self, spec: &ParsingSpec, name: &str) -> Vec<Interpretation> {
		let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(name);

		if name.is_empty() {
			return Vec::new();
		}

		self.view(0, self.symbols.len())
			.interpretations_for_name(spec, &rows)
	}

	/// Shape models are cached on `spec` (see [`ParsingSpec::shape_models`]), so searching the
	/// same shapes more than once reuses each built model instead of rebuilding it per query.
	pub fn search_by_log_shapes(
		&self,
		spec: &ParsingSpec,
		log_shapes: &[&str],
	) -> Vec<Vec<Interpretation>> {
		let cache: &ShapeModelCache = spec.shape_models();

		// The query's runs and their fits are shared across every shape:
		// this is where the composition path gets its leverage,
		// since a corpus mentions the same few rules over and over,
		// and a rule's behaviour on a run does not depend on the shape referencing it.
		let query: Query<'_> = Query::new(self);
		let fits: RunFitCache = RunFitCache::new();

		// Resolve every shape before searching any, so an unsupported shape fails the call outright
		// rather than after some shapes have already been processed. See [`ShapeModel::new`].
		let models: Vec<Arc<ShapeModel>> =
			Vec::from_iter(log_shapes.iter().map(|&shape| cache.get(spec, shape)));

		let view: SearchStringView<'_> = self.view(0, self.symbols.len());
		std::iter::zip(&models, log_shapes)
			.map(|(model, &shape)| {
				view.interpretations_for_log_shape(spec, model, shape, &query, &fits)
			})
			.collect::<Vec<_>>()
	}

	/// Interpretations for `log_shape`, always via the automata,
	/// bypassing [`crate::search::decompose`].
	///
	/// Exposed so that tests can pin [`decompose`] against the engine it is meant to agree with.
	pub fn interpretations_for_log_shape_via_engine(
		&self,
		spec: &ParsingSpec,
		log_shape: &str,
	) -> Vec<Interpretation> {
		let model: ShapeModel = ShapeModel::new(spec, log_shape);
		let automata: Tnfa = Self::automata_for_prefix(spec, &model, model.parts.len() - 1);
		self.interpretations_for_automata(spec, &automata, &[])
	}

	/// Interpretations for an already-built shape automaton.
	///
	/// Exposed so that tests can pin a *truncated* automaton (see
	/// [`ParsingSpec::automata_for_fragments`]) against the full one it stands in for.
	///
	/// `dropped` are the parts the automaton is missing from the shape's end, if any.
	/// It is needed because dropped static text is still reported -- as a trailing `'*'` --
	/// by the full shape, so a truncated one has to put it back to render identically.
	/// Pass an empty slice for a complete shape.
	pub fn interpretations_for_automata(
		&self,
		spec: &ParsingSpec,
		automata: &Tnfa,
		dropped: &[decompose::ShapePart],
	) -> Vec<Interpretation> {
		self.view(0, self.symbols.len()).interpretations_for_shape(
			spec,
			&TruncatedShape {
				// TODO: avoid this clone by borrowing the automaton instead.
				automata: automata.clone(),
				dropped: Tail::of_parts(dropped),
			},
		)
	}

	/// Builds the automaton for shape parts `0..=end`, round-tripping through the model's
	/// fragments so the parts built are exactly the parts placement reasoned about.
	///
	/// A `ShapeModel` already resolved every variable (see [`ShapeModel::new`]),
	/// so building from its fragments cannot fail.
	fn automata_for_prefix(spec: &ParsingSpec, model: &ShapeModel, end: usize) -> Tnfa {
		let fragments: Vec<LogShapeFragment> = model.fragments_in(0, end);
		spec.automata_for_fragments(&fragments)
			.expect("the model already resolved every rule in the shape")
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

	/// Whether a match must run through to the end of the message;
	/// see [`SearchString::anchored_end`].
	fn anchored_end(&self) -> bool {
		!self.as_str().last().is_some_and(SymbolicChar::is_wildcard)
	}

	/// This query as a regex, symbol for symbol: a wildcard is `.*`.
	///
	/// The intersection always runs to the end of the shape,
	/// so "and then anything" is something the query itself consumes.
	fn to_regex(&self) -> Regex {
		Regex::Sequence(Vec::from_iter(
			self.as_str().iter().map(SymbolicChar::to_regex),
		))
	}

	/// Interpretations of this query for a single log shape.
	///
	/// Three paths, cheapest first:
	///
	/// 1. [`crate::search::decompose::can_match`] on the coarse charset model.
	///    When it proves the shape cannot match,
	///    the shape's TNFA is never built and never intersected with the query.
	/// 2. [`crate::search::decompose::compose`],
	///    which decomposes the query against the shape directly,
	///    reusing per-rule simulations across shapes.
	///    This answers the real question -- which rule instance holds what text --
	///    without building any automata for the shape.
	/// 3. The engine, when composition declines to conclude (an unreasonable rule, or an exhausted
	///    budget). This is the slow path that the other two exist to avoid.
	///
	/// Composition is exact because it simulates the rules themselves,
	/// whereas the prefilter's `can_match` sees only their coarse charsets
	/// (and variables there may match the empty string, which a rule like `[a-z]+` cannot),
	/// so only the prefilter's "no" is a proof.
	fn interpretations_for_log_shape(
		&self,
		spec: &ParsingSpec,
		model: &ShapeModel,
		shape: &str,
		query: &Query<'_>,
		fits: &RunFitCache,
	) -> Vec<Interpretation> {
		now!(t0);

		if !decompose::can_match(model, query.symbols) {
			trace!("decompose rejected shape {shape:.256}");
			return Vec::new();
		}

		// `None` means a resource cap was exceeded, so the table is incomplete and nothing may be
		// concluded from it; fall back to the engine.
		let maybe_table: Option<PlacementTable> = PlacementTable::compute(spec, model, query, fits);

		if let Some(table) = maybe_table.as_ref()
			&& let Some(interpretations) =
				self.composed_interpretations(spec, model, table, query, fits)
		{
			now!(t1);
			trace!("composed shape in {} ms {shape:.256}", millis!(t0, t1));
			return interpretations;
		}

		// Composition declined to conclude, so build the shape's automaton and use the engine.
		// The table still bounds where the query's literal text can sit,
		// which is what decides how much of the shape has to be built;
		// see [`Self::truncated_automata`].
		let truncated: TruncatedShape = self.truncated_automata(spec, model, query, &maybe_table);
		let interpretations: Vec<Interpretation> = self.interpretations_for_shape(spec, &truncated);
		now!(t1);
		debug!("- took {} ms", millis!(t0, t1));
		interpretations
	}

	/// The shape's automaton, truncated to the parts the query can actually reach.
	///
	/// Real shapes carry very long tails of static text -- tens of thousands of characters --
	/// while their variables cluster near the front,
	/// and one state is emitted per literal character.
	/// A query that is not anchored at the end ends in a wildcard,
	/// and that wildcard consumes everything past its last literal run,
	/// so those trailing states are built and intersected only to be matched by `.*`.
	///
	/// [`PlacementTable::last_reachable_part`] bounds every placement of every run, so no literal
	/// character of the query can land beyond it. Truncating there is exactly "do not simulate the
	/// query's trailing wildcard": the parts dropped are the ones it would have consumed.
	/// Nothing is truncated when the query is anchored at the end (it must consume the shape
	/// through to its end), when the last part is reachable anyway, or when the table itself
	/// was incomplete and so does not bound the reach.
	///
	/// Only the *tail* is dropped. The head must be kept verbatim --
	/// the query's leading wildcard still has to traverse it,
	/// and replacing it with `.*` would be a superset,
	/// letting runs straddle where the real static text forbids it
	/// and inventing interpretations the full shape does not have.
	///
	/// The automaton for the kept prefix is built from the model's own fragments,
	/// never from the shape string,
	/// so the parts built are exactly the parts placement reasoned about
	/// (see [`SearchString::automata_for_prefix`]).
	fn truncated_automata(
		&self,
		spec: &ParsingSpec,
		model: &ShapeModel,
		query: &Query<'_>,
		maybe_table: &Option<PlacementTable>,
	) -> TruncatedShape {
		let last: usize = model.parts.len() - 1;

		let end: usize = if query.anchored_end {
			last
		} else {
			maybe_table
				.as_ref()
				.and_then(PlacementTable::last_reachable_part)
				.map_or(last, |end| end.min(last))
		};

		if end < last {
			trace!(
				"truncated shape from {} parts to 0..={end}",
				model.parts.len()
			);
		}

		TruncatedShape {
			automata: SearchString::automata_for_prefix(spec, model, end),
			// Dropped static text is still reported by the full shape, as a trailing `'*'` --
			// it is text the message contains even though the query says nothing about it --
			// so the truncated rendering has to put that back.
			// A tail of only rule references needs nothing:
			// those are unconstrained captures, which are omitted either way.
			dropped: Tail::of_parts(&model.parts[(end + 1)..]),
		}
	}

	/// Interpretations for `model` via [`crate::search::decompose::compose`].
	///
	/// `None` means composition declined to conclude and the caller must fall back to the engine.
	/// An empty vector is a real answer -- the shape cannot match -- and is not the same thing.
	fn composed_interpretations(
		&self,
		spec: &ParsingSpec,
		model: &ShapeModel,
		table: &PlacementTable,
		query: &Query<'_>,
		fits: &RunFitCache,
	) -> Option<Vec<Interpretation>> {
		match decompose::compose(spec, model, table, query, fits, ComposeBudget::default()) {
			Composed::Impossible => Some(Vec::new()),
			Composed::Unknown => None,
			Composed::Compositions(compositions) => {
				let mut interpretations: Vec<Interpretation> = Vec::from_iter(
					compositions
						.iter()
						.map(|composition| composition.to_interpretation(model, query)),
				);
				// The engine's callers expect a canonical, duplicate-free set;
				// distinct compositions can render identically once positions collapse
				// to the same sub-queries.
				interpretations.sort();
				interpretations.dedup();
				Some(interpretations)
			},
		}
	}

	fn interpretations_for_name(
		&self,
		spec: &ParsingSpec,
		rows: &[(&RuleInfo, &Regex)],
	) -> Vec<Interpretation> {
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
	/// The intersection always runs **to the end**:
	/// a state accepts only when the shape's automaton and the query's accept together.
	/// End-anchoring is therefore not a mode here --
	/// the query is used exactly as written,
	/// and a trailing wildcard (`*foo*`, `foo*`) is a real `.*` the intersection consumes.
	///
	/// That wildcard means the automaton must traverse whatever remains of the shape,
	/// which is why `shape_nfa` should already have been cut down to the part the query can reach;
	/// see [`Self::truncated_automata`].
	/// That division of labour is the point:
	/// deciding *how much shape matters* belongs to placement,
	/// which knows where the query's text can sit, not to the intersection,
	/// which would otherwise have to guess by stopping early.
	fn interpretations_for_shape(
		&self,
		spec: &ParsingSpec,
		shape: &TruncatedShape,
	) -> Vec<Interpretation> {
		let shape_nfa: &Tnfa = &shape.automata;
		let anchored_end: bool = self.anchored_end();

		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex());

		now!(t0);
		let intersection: Tnfa = shape_nfa.intersect::<true>(&search_nfa);
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

		let paths: Vec<Path> = intersection.compute_paths(spec);

		for path in paths.iter() {
			let mut leaf_queries: Vec<LeafQuery> = LeafQuery::from_path(path);

			if !anchored_end {
				// An unanchored query says nothing about the tail of the message,
				// so every rule reference after its last literal character is
				// unconstrained and is dropped (composition omits exactly these).
				let trimmed: Tail = Self::truncate_to_last_constrained(&mut leaf_queries);
				// What was trimmed, together with what truncation dropped from the
				// shape's automaton, is the tail the rendering must still account for.
				(trimmed.or(shape.dropped)).finish_unanchored(&mut leaf_queries);
			}

			interpretations.push(Interpretation { leaf_queries });
		}

		interpretations.iter().for_each(Interpretation::invariants);

		Interpretation::dedup_covered_interpretations(&mut interpretations);

		interpretations
	}

	/// Drops the sub-queries past the last one the query constrains, and classifies them.
	///
	/// A capture past the last constrained sub-query is vacuous (the query says nothing
	/// about it); a static one is kept as-is to the left of `last_constrained` only when
	/// it sits between two constrained parts. Trailing static text is reported by
	/// [`Tail::finish_unanchored`], so it is classified here rather than kept.
	///
	/// Positional identity is unharmed because it is read left to right --
	/// the first `n` sub-queries still correspond to the first `n` shape parts --
	/// so only trailing entries may go,
	/// and a *leading* or *interior* vacuous capture is always retained.
	fn truncate_to_last_constrained(leaf_queries: &mut Vec<LeafQuery>) -> Tail {
		let last_constrained: Option<usize> = leaf_queries.iter().rposition(|leaf_query| {
			leaf_query
				.symbolic_value
				.iter()
				.any(|symbol| !symbol.is_wildcard())
		});

		let Some(last) = last_constrained else {
			let tail: Tail = Tail::of_leaves(leaf_queries);
			// A path that constrains nothing describes a single unrestricted wildcard.
			leaf_queries.clear();
			leaf_queries.push(LeafQuery::new_static_text(vec![SymbolicChar::GlobStar]));
			return tail;
		};

		let tail: Tail = Tail::of_leaves(&leaf_queries[(last + 1)..]);
		leaf_queries.truncate(last + 1);
		tail
	}

	fn interpretations_for_nfa(
		&self,
		spec: &ParsingSpec,
		nfa: &Tnfa,
		maybe_rule_info: Option<&RuleInfo>,
	) -> Vec<Interpretation> {
		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex());

		let intersection: Tnfa = nfa.intersect::<true>(&search_nfa);

		let paths: Vec<Path> = intersection.compute_paths(spec);

		for path in paths.iter() {
			if let [PathComponent::Literal(contents)] = path.components.as_slice() {
				let rule: &RootRule = &spec[path.rule_idx];
				let rule_info: &RuleInfo = if let Some(rule_info) = maybe_rule_info {
					assert_eq!(rule_info.root_idx, rule.idx);
					rule_info
				} else {
					&rule[None]
				};

				let mut implicit_capture: LeafQuery =
					LeafQuery::new_rule(rule.name.clone(), contents.clone());

				if !rule_info.is_root() {
					assert!(!rule_info.is_leaf());
					let mut static_text: LeafQuery = LeafQuery::new_static_text(contents.clone());

					implicit_capture.surround_with_wildcards();
					static_text.surround_with_wildcards();

					interpretations.push(Interpretation {
						leaf_queries: vec![implicit_capture],
					});
					interpretations.push(Interpretation {
						leaf_queries: vec![static_text],
					});
				} else {
					interpretations.push(Interpretation {
						leaf_queries: vec![implicit_capture],
					});
				}

				continue;
			}

			interpretations.push(Interpretation {
				leaf_queries: LeafQuery::from_path(path),
			});
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
		for query in self.leaf_queries.iter() {
			if query.is_static_text() {
				assert!(!last_was_static_text);
				last_was_static_text = true;
			} else {
				last_was_static_text = false;
			}

			// No value carries adjacent wildcards:
			// `**` says exactly what `*` does, and [`LeafQuery::covers`]
			// is reflexive and transitive only on values without it --
			// its fast path recognises a lone `*` as universal,
			// but `**` falls through to the segment loop and fails even against itself.
			assert!(
				!query
					.symbolic_value
					.windows(2)
					.any(|pair| pair.iter().all(SymbolicChar::is_wildcard)),
				"adjacent wildcards in {:?}",
				query.string_value
			);
		}
	}
}

impl LeafQuery {
	pub(crate) fn new_static_text(symbolic_value: Vec<SymbolicChar>) -> Self {
		// `Arc::default()` special-cases ZSTs; no allocation needed.
		Self::new(Arc::<str>::default(), symbolic_value)
	}

	pub(crate) fn new_rule(
		fully_qualified_name: Arc<str>,
		symbolic_value: Vec<SymbolicChar>,
	) -> Self {
		assert!(!fully_qualified_name.is_empty());
		Self::new(fully_qualified_name, symbolic_value)
	}

	fn new(fully_qualified_name: Arc<str>, symbolic_value: Vec<SymbolicChar>) -> Self {
		let string_value: String =
			String::from_iter(symbolic_value.iter().map(SymbolicChar::to_string));
		Self {
			fully_qualified_name,
			symbolic_value,
			string_value,
		}
	}

	/// One sub-query per path component, in order.
	fn from_path(path: &Path) -> Vec<Self> {
		assert!(!path.components.is_empty());
		Vec::from_iter(path.components.iter().map(|token| match token {
			PathComponent::Literal(contents) => Self::new_static_text(contents.clone()),
			PathComponent::Capture { capture, contents } => {
				Self::new_rule(capture.fully_qualified_name.clone(), contents.clone())
			},
		}))
	}

	pub fn is_static_text(&self) -> bool {
		self.fully_qualified_name.is_empty()
	}

	/// Whether `self` describes everything `other` does:
	/// `true` iff every string matching `other`'s value also matches `self`'s,
	/// and both name the same capture (or are both static text).
	///
	/// This is exact glob containment; see [`glob_covers`].
	/// It is therefore reflexive and transitive on any values.
	fn covers(&self, other: &Self) -> bool {
		if self.fully_qualified_name != other.fully_qualified_name {
			return false;
		}
		glob_covers(&self.symbolic_value, &other.symbolic_value)
	}

	/// Appends a trailing wildcard, unless the value already ends in one.
	///
	/// Used where a value sits at the point the query's trailing wildcard applies,
	/// so it must permit anything after it.
	/// An empty value is left alone: it means the part is pinned to producing nothing,
	/// and padding it would claim the opposite.
	fn append_wildcard(&mut self) {
		if self.symbolic_value.is_empty()
			|| self
				.symbolic_value
				.last()
				.is_some_and(SymbolicChar::is_wildcard)
		{
			return;
		}
		self.symbolic_value.push(SymbolicChar::GlobStar);
		self.string_value.push('*');
	}

	fn surround_with_wildcards(&mut self) {
		// An empty value means the rule is pinned to producing nothing;
		// padding it with wildcards would turn that into "anything",
		// which is the opposite claim.
		if self.symbolic_value.is_empty() {
			return;
		}
		if *self.symbolic_value.first().expect("non-empty") != SymbolicChar::GlobStar {
			self.symbolic_value.insert(0, SymbolicChar::GlobStar);
			self.string_value.insert(0, '*');
		}
		self.append_wildcard();
	}
}

/// Whether every string matching the glob `other` also matches the glob `pattern`.
///
/// For globs whose only wildcard is `*`, containment holds iff `pattern` matches the
/// string made from `other` by replacing each `*` with a fresh symbol that no literal
/// equals:
///
/// - if containment holds, that string is itself in `other`'s language;
/// - conversely, a literal never matches a fresh symbol, so each one is absorbed by
///   some `*` of `pattern`, which could equally absorb any string in its place.
///
/// So this is an ordinary glob match in which a `*` of `other` is a single token that
/// only a `*` of `pattern` can consume. `O(n x m)` time, `O(m)` space.
fn glob_covers(pattern: &[SymbolicChar], other: &[SymbolicChar]) -> bool {
	// `matches[j]`: whether `pattern[i..]` matches `other[j..]`, for the current `i`.
	let mut matches: Vec<bool> = vec![false; other.len() + 1];
	matches[other.len()] = true;
	for symbol in pattern.iter().rev() {
		match symbol {
			SymbolicChar::GlobStar => {
				// Absorb nothing (`matches[j]` as is) or `other[j]` and continue.
				for j in (0..other.len()).rev() {
					matches[j] = matches[j] || matches[j + 1];
				}
			},
			SymbolicChar::Literal(_) => {
				for j in 0..other.len() {
					matches[j] = (*symbol == other[j]) && matches[j + 1];
				}
				matches[other.len()] = false;
			},
		}
	}
	matches[0]
}

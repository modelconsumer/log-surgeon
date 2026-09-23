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
use crate::search::decompose::Run;
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
	pub fully_qualified_name: Arc<str>,
	pub symbolic_value: Vec<SymbolicChar>,
	pub string_value: String,
}

impl std::fmt::Debug for SubQuery {
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

impl Eq for SubQuery {}

impl Ord for SubQuery {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		(&self.fully_qualified_name, &self.symbolic_value).cmp(&(&other.fully_qualified_name, &other.symbolic_value))
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

/// A shape's automaton, possibly cut short, with what that cut left out.
///
/// See [`SearchStringView::truncated_automata`]. The flag is what lets the rendering stay identical to
/// the full shape's: dropped static text still has to appear as a trailing `'*'`, because it is text
/// the message contains even though the query does not constrain it.
struct TruncatedShape {
	automata: Tnfa,
	/// Whether the parts dropped from the end included any static text.
	dropped_static: bool,
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
	/// Whether every sub-query of `self` covers the corresponding one of `other`, so `other` describes
	/// nothing `self` does not already describe.
	///
	/// Positional, so it presumes both decompose the shape the same way; a difference in length means
	/// they are different decompositions and neither covers the other.
	fn covers(&self, other: &Self) -> bool {
		(self.sub_queries.len() == other.sub_queries.len())
			&& std::iter::zip(self.sub_queries.iter(), other.sub_queries.iter())
				.all(|(mine, theirs)| mine.covers(theirs))
	}

	/// Drops interpretations that another already covers.
	///
	/// [`SubQuery::covers`] is a *conservative* test, so the result is not guaranteed to be a minimal
	/// antichain: a redundant interpretation the test cannot see through is kept. That costs an extra
	/// result, never a wrong one, which is why the cheap test is preferred to deciding glob containment.
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

	/// Shape models are cached on `spec` (see [`ParsingSpec::shape_models`]), so searching the
	/// same shapes more than once reuses each built model instead of rebuilding it per query.
	pub fn search_by_log_shapes(&self, spec: &ParsingSpec, log_shapes: &[&str]) -> Vec<Vec<Interpretation>> {
		let anchored: AnchoredQuery<'_> = self.anchored();
		let cache: &ShapeModelCache = spec.shape_models();

		// The query's runs and their fits are shared across every shape: this is where the composition
		// path gets its leverage, since a corpus mentions the same few rules over and over, and a rule's
		// behaviour on a run does not depend on the shape referencing it.
		//
		// Note the runs come from the *raw* symbols, not the engine's view: dropping the trailing
		// wildcard is what tells [`decompose::runs_of`] the last run is anchored, and re-adding one would
		// erase exactly the information anchoring depends on.
		let runs: Vec<Run> = decompose::runs_of(&self.symbols);
		let fits: RunFitCache = RunFitCache::new();

		// Resolve every shape before searching any, so an unsupported shape fails the call outright
		// rather than after some shapes have already been processed. See [`ShapeModel::new`].
		let models: Vec<Arc<ShapeModel>> = Vec::from_iter(log_shapes.iter().map(|&shape| cache.get(spec, shape)));

		std::iter::zip(&models, log_shapes)
			.map(|(model, &shape)| {
				anchored
					.view
					.interpretations_for_log_shape(spec, model, shape, &runs, &fits, anchored.anchored_end)
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
	/// anchoring cannot be structural in the same way -- a trailing `.*` would have to be simulated
	/// through the whole rest of the shape -- so it is carried as a flag, the wildcard is dropped from
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

	/// Interpretations for `log_shape`, always via the automata, bypassing [`crate::search::decompose`].
	///
	/// Exposed so that tests can pin [`decompose`] against the engine it is meant to agree with.
	pub fn interpretations_for_log_shape_via_engine(&self, spec: &ParsingSpec, log_shape: &str) -> Vec<Interpretation> {
		// TODO unwrap
		let automata: Tnfa = spec.automata_for_shape(log_shape).unwrap();
		self.interpretations_for_automata(spec, &automata, false)
	}

	/// Interpretations for an already-built shape automaton.
	///
	/// Exposed so that tests can pin a *truncated* automaton (see
	/// [`ParsingSpec::automata_for_fragments`]) against the full one it stands in for.
	///
	/// `dropped_static` says whether the parts cut from the end of the shape included any static text;
	/// pass `false` for a complete shape. It is needed because such text is still reported -- as a
	/// trailing `'*'` -- by the full shape, so a truncated one has to put it back to render identically.
	pub fn interpretations_for_automata(
		&self,
		spec: &ParsingSpec,
		automata: &Tnfa,
		dropped_static: bool,
	) -> Vec<Interpretation> {
		let anchored: AnchoredQuery<'_> = self.anchored();
		anchored.view.interpretations_for_shape(
			spec,
			&TruncatedShape {
				// TODO: avoid this clone by borrowing the automaton instead.
				automata: automata.clone(),
				dropped_static,
			},
			anchored.anchored_end,
		)
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
		self.to_regex_ending(false)
	}

	/// This query as a regex, optionally with a trailing `.*`.
	///
	/// [`SearchString::anchored`] strips a query's trailing wildcard so that the rest of the pipeline
	/// sees its runs correctly unanchored. `trailing_wildcard` puts it back for the automaton, which
	/// needs it explicitly: the intersection always runs to the end, so "and then anything" has to be
	/// something the query can actually consume.
	fn to_regex_ending(&self, trailing_wildcard: bool) -> Regex {
		let symbols = self.as_str().iter().map(SymbolicChar::to_regex);
		Regex::Sequence(if trailing_wildcard {
			Vec::from_iter(symbols.chain(std::iter::once(SymbolicChar::GlobStar.to_regex())))
		} else {
			Vec::from_iter(symbols)
		})
	}

	/// Interpretations of this query for a single log shape.
	///
	/// Three paths, cheapest first:
	///
	/// 1. [`crate::search::decompose::can_match`] on the coarse charset model. When it proves the shape cannot
	///    match, the shape's TNFA is never built and never intersected with the query.
	/// 2. [`crate::search::decompose::compose`], which decomposes the query against the shape directly, reusing
	///    per-rule simulations across shapes. This answers the real question -- which rule instance holds
	///    what text -- without building any automata for the shape.
	/// 3. The engine, when composition declines to conclude (an unreasonable rule, or an exhausted
	///    budget). This is the slow path that the other two exist to avoid.
	///
	/// Note the [`crate::search::decompose::align`] decompositions are *not* usable as a result: placeholders
	/// there are over-approximated (notably they may match the empty string, which a rule like `[a-z]+`
	/// cannot), so they form a superset of the engine's interpretations. Composition is exact because it
	/// simulates the rules themselves rather than their charsets.
	fn interpretations_for_log_shape(
		&self,
		spec: &ParsingSpec,
		model: &ShapeModel,
		shape: &str,
		runs: &[Run],
		fits: &RunFitCache,
		anchored_end: bool,
	) -> Vec<Interpretation> {
		now!(t0);

		if !decompose::can_match(model, self.as_str(), anchored_end) {
			trace!("decompose rejected shape {shape:.256}");
			return Vec::new();
		}

		// `None` means a resource cap was exceeded, so the table is incomplete and nothing may be
		// concluded from it; fall back to the engine.
		let maybe_table: Option<PlacementTable> = PlacementTable::compute(spec, model, runs, fits);

		if let Some(table) = maybe_table.as_ref()
			&& let Some(interpretations) = Self::composed_interpretations(spec, model, table, runs, fits)
		{
			now!(t1);
			trace!("composed shape in {} ms {shape:.256}", millis!(t0, t1));
			return interpretations;
		}

		// Composition declined to conclude, but its placements still bound where the query's literal
		// text can sit, which is enough to drop the shape's unreachable tail from the automaton.
		let maybe_truncated: Option<TruncatedShape> = maybe_table
			.as_ref()
			.and_then(|table| self.truncated_automata(spec, model, table, anchored_end));

		let truncated: TruncatedShape = match maybe_truncated {
			Some(truncated) => truncated,
			// TODO unwrap
			None => TruncatedShape {
				automata: spec.automata_for_shape(shape).unwrap(),
				dropped_static: false,
			},
		};
		let interpretations: Vec<Interpretation> = self.interpretations_for_shape(spec, &truncated, anchored_end);
		now!(t1);
		debug!("- took {} ms", millis!(t0, t1));
		interpretations
	}

	/// The shape's automaton, truncated to the parts the query can actually reach.
	///
	/// Real shapes carry very long tails of static text -- tens of thousands of characters -- while their
	/// placeholders cluster near the front, and one state is emitted per literal character. A query that
	/// is not anchored at the end ends in a wildcard, and that wildcard consumes everything past its
	/// last literal run, so those trailing states are built and intersected only to be matched by `.*`.
	///
	/// [`PlacementTable::last_reachable_part`] bounds every placement of every run, so no literal
	/// character of the query can land beyond it. Truncating there is exactly "do not simulate the
	/// query's trailing wildcard": the parts dropped are the ones it would have consumed.
	///
	/// Only the *tail* is dropped. The head must be kept verbatim -- the query's leading wildcard still
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
	) -> Option<TruncatedShape> {
		// A query anchored at the end must consume the shape through to its end, so nothing may be
		// dropped: the truncated parts are precisely the ones it still has to match.
		if anchored_end {
			return None;
		}

		let end: usize = table.last_reachable_part()?;
		let last: usize = model.parts.len().checked_sub(1)?;

		if end >= last {
			return None;
		}

		let fragments: Vec<LogShapeFragment> = model.fragments_in(0, end);
		let truncated: Tnfa = spec.automata_for_fragments(&fragments).ok()?;

		trace!("truncated shape from {} parts to 0..={end}", model.parts.len());

		// Whether the dropped tail contains any static text. If it does, the full shape would have
		// reported it as a trailing `'*'` -- it is text the message still contains, even though the query
		// says nothing about it -- so the truncated rendering has to put that back. A tail of only rule
		// references needs nothing: those are unconstrained captures, which are omitted either way.
		let dropped_static: bool = model.parts[(end + 1)..].iter().any(|part| !part.is_placeholder());

		Some(TruncatedShape {
			automata: truncated,
			dropped_static,
		})
	}

	/// Interpretations for `model` via [`crate::search::decompose::compose`].
	///
	/// `None` means composition declined to conclude and the caller must fall back to the engine. An
	/// empty vector is a real answer -- the shape cannot match -- and is not the same thing.
	fn composed_interpretations(
		spec: &ParsingSpec,
		model: &ShapeModel,
		table: &PlacementTable,
		runs: &[Run],
		fits: &RunFitCache,
	) -> Option<Vec<Interpretation>> {
		match decompose::compose(spec, model, table, runs, fits, ComposeBudget::default()) {
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
	/// The intersection always runs **to the end**: a state accepts only when the shape's automaton and
	/// the query's accept together. End-anchoring is therefore not a mode here -- it is simply whether
	/// the query ends in a wildcard, which `anchored_end` reports and which decides what is fed in:
	///
	/// - `true` (`*foo`, `foo`): the query is used as written, so the match must reach the shape's end.
	/// - `false` (`*foo*`, `foo*`): the trailing wildcard, dropped by [`SearchString::anchored`] so the
	///   rest of the pipeline sees the query's runs unanchored, is put back -- as a real `.*` the
	///   intersection consumes.
	///
	/// Restoring the wildcard means the automaton must traverse whatever remains of the shape, which is
	/// why `shape_nfa` should already have been cut down to the part the query can reach; see
	/// [`Self::truncated_automata`]. That division of labour is the point: deciding *how much shape
	/// matters* belongs to placement, which knows where the query's text can sit, not to the
	/// intersection, which would otherwise have to guess by stopping early.
	fn interpretations_for_shape(
		&self,
		_spec: &ParsingSpec,
		shape: &TruncatedShape,
		anchored_end: bool,
	) -> Vec<Interpretation> {
		let shape_nfa: &Tnfa = &shape.automata;
		assert_ne!(self.as_str(), [SymbolicChar::GlobStar]);

		let mut interpretations: Vec<Interpretation> = Vec::new();

		let search_nfa: Tnfa = Tnfa::for_regex(&self.to_regex_ending(!anchored_end));

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

		let paths: Vec<Path> = intersection.compute_paths(_spec);

		for path in paths.iter() {
			assert!(!path.components.is_empty());

			let mut sub_queries: Vec<SubQuery> = Vec::new();

			for token in path.components.iter() {
				match token {
					PathComponent::Literal(contents) => {
						sub_queries.push(SubQuery::new_static_text(contents.clone()));
					},
					PathComponent::Capture { capture, contents } => {
						sub_queries.push(SubQuery::new_rule(
							capture.fully_qualified_name.clone(),
							contents.clone(),
						));
					},
				}
			}

			if !anchored_end {
				// A truncated tail containing static text was reported by the full shape as a trailing
				// `'*'`; restore it so both render the same. It is added before trimming so that it is
				// treated exactly like the static part it stands for.
				//
				// If the path already ends in static text the two are adjacent, which
				// [`Interpretation::invariants`] forbids, so the wildcard joins that value instead of
				// becoming a sub-query of its own -- the same merge composition performs.
				if shape.dropped_static {
					match sub_queries.last_mut() {
						Some(last) if last.is_static_text() => last.append_wildcard(),
						_ => sub_queries.push(SubQuery::new_static_text(vec![SymbolicChar::GlobStar])),
					}
				}
				Self::drop_trailing_unconstrained(&mut sub_queries);
			}

			interpretations.push(Interpretation { sub_queries });
		}

		interpretations.iter().for_each(Interpretation::invariants);

		interpretations.sort();
		interpretations.dedup();

		Interpretation::dedup_covered_interpretations(&mut interpretations);

		interpretations
	}

	/// Drops the *captures* past the last sub-query the query constrains.
	///
	/// An unanchored query says nothing about the tail of the message, so every rule reference after its
	/// last literal character is unconstrained: each would be reported as a bare `*`, carrying no
	/// information. Composition omits exactly these, so dropping them here is what keeps the two paths
	/// reporting the same thing. Positional identity is unharmed because it is read left to right -- the
	/// first `n` sub-queries still correspond to the first `n` shape parts -- so only trailing entries may
	/// go, and a *leading* or *interior* vacuous capture is always retained.
	///
	/// Trailing **static text** is kept, and given a trailing `*` if it does not already end in one. A
	/// static value has to glob-match its part's text exactly, so a bare `'a'` where the shape continues
	/// `a%word%b...` would assert the message *ends* at `a` -- false, and matching nothing. The wildcard
	/// is what the query's own trailing wildcard means at that position, and it is added here rather
	/// than in the automaton because that is where the shape's remaining text stops being reported.
	fn drop_trailing_unconstrained(sub_queries: &mut Vec<SubQuery>) {
		let last_constrained: Option<usize> = sub_queries
			.iter()
			.rposition(|sub_query| sub_query.symbolic_value.iter().any(|symbol| !symbol.is_wildcard()));

		let Some(last) = last_constrained else {
			// Nothing is constrained at all; a single `*` says exactly that.
			sub_queries.clear();
			sub_queries.push(SubQuery::new_static_text(vec![SymbolicChar::GlobStar]));
			return;
		};

		// Keep a trailing static sub-query: unlike a capture, it is not vacuous -- it names text the
		// message must still contain, and the query's trailing wildcard covers only what follows *it*.
		let keep: usize = match sub_queries.get(last + 1) {
			Some(next) if next.is_static_text() => last + 2,
			_ => last + 1,
		};
		sub_queries.truncate(keep);

		// The last sub-query is now where the query's trailing wildcard applies, so it must permit
		// anything after it. A capture is already padded by the path itself; static text may not be.
		if let Some(final_sub_query) = sub_queries.last_mut()
			&& final_sub_query.is_static_text()
		{
			final_sub_query.append_wildcard();
		}
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

		let intersection: Tnfa = nfa.intersect::<true>(&search_nfa);

		let paths: Vec<Path> = intersection.compute_paths(spec);

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

				let mut implicit_capture: SubQuery = SubQuery::new_rule(rule.name.clone(), contents.clone());

				if !rule_info.is_root() {
					assert!(!rule_info.is_leaf());
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
					PathComponent::Capture { capture, contents } => {
						sub_queries.push(SubQuery::new_rule(
							capture.fully_qualified_name.clone(),
							contents.clone(),
						));
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

			// No value carries adjacent wildcards: `**` says exactly what `*` does, and [`SubQuery::covers`]
			// is reflexive and transitive only on values without it -- its fast path recognises a lone `*`
			// as universal, but `**` falls through to the segment loop and fails even against itself.
			assert!(
				!sub_query
					.symbolic_value
					.windows(2)
					.any(|pair| pair.iter().all(SymbolicChar::is_wildcard)),
				"adjacent wildcards in {:?}",
				sub_query.string_value
			);
		}
	}
}

impl SubQuery {
	pub(crate) fn new_static_text(symbolic_value: Vec<SymbolicChar>) -> Self {
		// `Arc::default()` special-cases ZSTs; no allocation needed.
		Self::new(Arc::<str>::default(), symbolic_value)
	}

	pub(crate) fn new_rule(fully_qualified_name: Arc<str>, symbolic_value: Vec<SymbolicChar>) -> Self {
		assert!(!fully_qualified_name.is_empty());
		Self::new(fully_qualified_name, symbolic_value)
	}

	fn new(fully_qualified_name: Arc<str>, symbolic_value: Vec<SymbolicChar>) -> Self {
		let string_value: String = symbolic_value.iter().fold(String::new(), |mut accum, &ch| {
			accum.push_str(&ch.to_string());
			accum
		});
		Self {
			fully_qualified_name,
			symbolic_value,
			string_value,
		}
	}

	pub fn is_static_text(&self) -> bool {
		self.fully_qualified_name.is_empty()
	}

	/// Whether `self` describes everything `other` does: `true` implies every string matching `other`'s
	/// value also matches `self`'s.
	///
	/// This is a **conservative syntactic test, not glob containment**. It compares the two values
	/// wildcard-segment by wildcard-segment, so it only sees a containment when the wildcards line up
	/// positionally: it reports `false` for `aa*` against `aaa*`, even though every string matching the
	/// latter matches the former. Only the stated implication holds; the converse does not.
	///
	/// That is enough for its one caller, [`Interpretation::dedup_covered_interpretations`], where a
	/// missed containment leaves a redundant interpretation and a spurious one would delete a real
	/// answer. Deciding true containment would need a quadratic match over the two patterns, which is
	/// not worth it to tidy the output.
	///
	/// Note this is reflexive and transitive only for values with no adjacent wildcards, which is what
	/// every producer emits; see [`Interpretation::invariants`].
	fn covers(&self, other: &Self) -> bool {
		if self.fully_qualified_name != other.fully_qualified_name {
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
			let Some(other_part): Option<&&[SymbolicChar]> = other_parts.get(i) else {
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

	/// Appends a trailing wildcard, unless the value already ends in one.
	///
	/// Used where a value sits at the point the query's trailing wildcard applies, so it must permit
	/// anything after it. An empty value is left alone: it means the part is pinned to producing
	/// nothing, and padding it would claim the opposite.
	fn append_wildcard(&mut self) {
		if self.symbolic_value.is_empty() || self.symbolic_value.last().is_some_and(SymbolicChar::is_wildcard) {
			return;
		}
		self.symbolic_value.push(SymbolicChar::GlobStar);
		self.string_value.push('*');
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

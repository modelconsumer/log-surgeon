//! Composes [`Placement`]s into whole-shape decompositions.
//!
//! # The DP
//!
//! A [`Composition`] chooses one placement per run such that the chosen placements run left to right
//! and do not overlap. The state is `(run index, earliest usable part)` and every placement advances
//! both, so the state space is a DAG and is solved by a reverse sweep — the same structure as
//! [`crate::prefilter::align`], and the reason feasibility can be decided in
//! `O(runs × parts × placements)` even when the number of compositions is large.
//!
//! Enumeration is separated from feasibility ([`crate::prefilter::can_compose`]) so that a shape can be
//! *rejected* cheaply, and only a shape that survives pays for materializing its decompositions. A
//! [`ComposeBudget`] caps enumeration so a pathological shape degrades to "no conclusion" instead of
//! exploding.
//!
//! # Positional identity
//!
//! Each [`Capture`] names the **index of the shape part** that produced it, so two references to the
//! same rule in one shape are distinguishable even when they share a name.
//!
//! [`Composition::to_interpretation`] renders that positionally: every rule reference up to the last
//! constrained one contributes a sub-query, with unconstrained references appearing as a bare `*`. The
//! position of a capture among those placeholders is what identifies which reference it is, so
//! `A%foo%B%foo%C` distinguishes its two `foo`s by whether a vacuous `foo=*` precedes the constrained
//! one.
//!
//! References *after* the last constrained capture are omitted: the query ends in an implicit `*`
//! (a single trailing wildcard is redundant under prefix matching), so trailing placeholders are
//! unconstrained and add nothing.

#[cfg(test)]
mod test;

use crate::parsing_spec::ParsingSpec;
use crate::prefilter::Placement;
use crate::prefilter::PlacementTable;
use crate::prefilter::Position;
use crate::prefilter::Run;
use crate::prefilter::RunFitCache;
use crate::prefilter::ShapeModel;
use crate::prefilter::ShapePart;
use crate::prefilter::placement::index_of;
use crate::prefilter::placement::positions_of;
use crate::search::Interpretation;
use crate::search::SubQuery;
use crate::search::SymbolicChar;

/// One complete way the query's literal text maps onto a shape.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Composition {
	/// The text attributed to rules, in shape order.
	pub captures: Vec<Capture>,
	/// The text attributed to the shape's static text, in shape order.
	pub literals: Vec<Literal>,
}

/// Query text produced by one rule reference in the shape.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Capture {
	/// Index into [`ShapeModel::parts`]: *which* reference, not merely which rule.
	pub part: usize,
	/// The rule's name as written in the shape.
	pub name: String,
	/// The characters of the query this rule must produce.
	///
	/// Several runs can land in one rule, so this may hold more than one piece; they are recorded in
	/// order and are separated by unconstrained text.
	pub pieces: Vec<PieceRef>,
}

/// Query text produced by the shape's static text.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Literal {
	/// Index into [`ShapeModel::parts`].
	pub part: usize,
	/// The characters of the query this static text supplies.
	pub pieces: Vec<PieceRef>,
}

/// A piece of query text, and the run it came from.
///
/// The run index is what makes wildcard placement recoverable: characters within one run are
/// contiguous in the query and must stay contiguous in the rendering, while a run *boundary* is exactly
/// where the query had a wildcard.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PieceRef {
	/// Index into the query's runs.
	pub run: usize,
	/// The characters supplied.
	pub text: String,
}

/// One shape part's contribution to a rendered interpretation.
struct RenderedPart {
	/// Index into [`ShapeModel::parts`].
	part: usize,
	/// The value this part's sub-query will carry.
	value: Vec<SymbolicChar>,
	/// How many pieces were merged into it; more than one needs a feasibility check.
	piece_count: usize,
}

impl Composition {
	/// Renders this composition as an [`Interpretation`].
	///
	/// Shape parts are emitted in order up to and including the last *constrained* one: static text as a
	/// static sub-query, rule references as captures. A reference with no query text attributed to it
	/// becomes a vacuous `*` capture, so a capture's position among the placeholders identifies which
	/// reference it is — this is how two references to one rule name are told apart.
	///
	/// Parts after the last constrained one are dropped, since the query's implicit trailing wildcard
	/// leaves them unconstrained.
	///
	/// # Invariant
	///
	/// Every literal character of the query appears in the result, in order, however it was split
	/// between static text and rules. A wildcard appears exactly where the query had one, i.e. between
	/// characters of different runs; characters of the *same* run stay adjacent, with no wildcard
	/// introduced between them even when they straddle a part boundary.
	#[must_use]
	pub fn to_interpretation(&self, model: &ShapeModel, runs: &[Run]) -> Interpretation {
		let rendered: Vec<RenderedPart> = self.rendered_parts(model, runs);

		// Nothing is constrained, so the query is satisfied without attributing text anywhere.
		if rendered.is_empty() {
			return Interpretation {
				sub_queries: vec![SubQuery::new_static_text(vec![SymbolicChar::GlobStar])],
			};
		}

		let mut sub_queries: Vec<SubQuery> = Vec::new();

		for part in rendered.into_iter() {
			match &model.parts[part.part] {
				// A name can resolve to several rules, but they occupy the same position, so emitting more
				// than one would duplicate the slot and break the positional correspondence.
				ShapePart::Placeholder(placeholder) => {
					if let Some(sub_rule) = placeholder.alternatives.first() {
						sub_queries.push(SubQuery::new(sub_rule, part.value));
					}
				},
				ShapePart::Static(_) => sub_queries.push(SubQuery::new_static_text(part.value)),
			}
		}

		Interpretation { sub_queries }
	}

	/// The shape parts that contribute a sub-query, with their rendered values.
	///
	/// Factored out so that rendering and multi-piece verification agree by construction: the value
	/// checked against a rule is the very one that will be reported.
	fn rendered_parts(&self, model: &ShapeModel, runs: &[Run]) -> Vec<RenderedPart> {
		// The last part holding any query text; everything after it is unconstrained.
		let last_constrained: Option<usize> = self
			.captures
			.iter()
			.map(|capture| capture.part)
			.chain(self.literals.iter().map(|literal| literal.part))
			.max();

		let Some(last_constrained) = last_constrained else {
			return Vec::new();
		};

		// The parts that will produce a sub-query, in order. Static text the query says nothing about is
		// skipped: the neighbouring wildcards already cover it. Collecting first makes the *next* part's
		// pieces available, which is what decides whether a run continues past this part.
		let emissions: Vec<(usize, &[PieceRef])> = (0..=last_constrained)
			.map(|part| (part, self.pieces_for(part)))
			.filter(|&(part, pieces)| !pieces.is_empty() || model.parts[part].is_placeholder())
			.collect::<Vec<_>>();

		let mut rendered: Vec<RenderedPart> = Vec::with_capacity(emissions.len());

		for (index, &(part, pieces)) in emissions.iter().enumerate() {
			let is_rule: bool = model.parts[part].is_placeholder();

			// The run holding the character emitted just before this part, if any.
			let preceding_run: Option<usize> = emissions[..index]
				.iter()
				.rev()
				.find_map(|(_, pieces)| pieces.last())
				.map(|piece| piece.run);
			// The run holding the character emitted just after this part, if any.
			let following_run: Option<usize> = emissions[(index + 1)..]
				.iter()
				.find_map(|(_, pieces)| pieces.first())
				.map(|piece| piece.run);

			rendered.push(RenderedPart {
				part,
				value: symbolic_value_of(pieces, runs, preceding_run, following_run, is_rule),
				piece_count: pieces.len(),
			});
		}

		rendered
	}

	/// The captures that need a multi-piece feasibility check, as `(rule name, value)`.
	///
	/// A capture holding pieces from *several* runs was validated only one run at a time: each run can
	/// sit in the rule, but that does not mean the rule can produce them all together. An alternation
	/// such as `INFO|WARN` admits either alone and neither pair. Single-piece captures are already
	/// exact, so they are skipped.
	///
	/// Values come from the same rendering used by [`Self::to_interpretation`], so the check asks about
	/// exactly the string that will be reported — including whether it is padded, which decides whether
	/// the rule may emit anything around it.
	#[must_use]
	pub fn captures_to_verify(&self, model: &ShapeModel, runs: &[Run]) -> Vec<(String, String)> {
		Vec::from_iter(self.rendered_parts(model, runs).into_iter().filter_map(|rendered| {
			match &model.parts[rendered.part] {
				ShapePart::Placeholder(placeholder) if rendered.piece_count > 1 => {
					// `Display` escapes the characters a query treats specially, so the value round-trips
					// through `SearchString::parse` as the literal text it stands for.
					Some((
						placeholder.name.clone(),
						String::from_iter(rendered.value.iter().map(ToString::to_string)),
					))
				},
				_ => None,
			}
		}))
	}

	/// The query text attributed to shape part `part`.
	fn pieces_for(&self, part: usize) -> &[PieceRef] {
		if let Some(capture) = self.captures.iter().find(|capture| capture.part == part) {
			return &capture.pieces;
		}
		if let Some(literal) = self.literals.iter().find(|literal| literal.part == part) {
			return &literal.pieces;
		}
		&[]
	}
}

/// The symbolic value for one part's pieces.
///
/// A wildcard is emitted exactly where the query had one — at a run boundary — and nowhere else. In
/// particular a run that crosses from one part into the next stays contiguous: `preceding_run` and
/// `following_run` name the runs adjacent to this part, so a boundary can be told apart from a mere
/// part boundary.
///
/// `is_rule` allows the padding a capture needs, since a rule may emit text of its own around the
/// query's characters. That padding is suppressed where the run continues into a neighbouring part, and
/// where the query anchors the run to the start or end of the message. Static text is never padded: it
/// must match verbatim.
fn symbolic_value_of(
	pieces: &[PieceRef],
	runs: &[Run],
	preceding_run: Option<usize>,
	following_run: Option<usize>,
	is_rule: bool,
) -> Vec<SymbolicChar> {
	// A rule with no attributed text is a bare `*`, present only to mark its position.
	if pieces.is_empty() {
		return vec![SymbolicChar::GlobStar];
	}

	let first: &PieceRef = pieces.first().expect("non-empty");
	let last: &PieceRef = pieces.last().expect("non-empty");
	// The run continues across the part boundary, so no wildcard may separate the characters.
	let continues_before: bool = preceding_run == Some(first.run);
	let continues_after: bool = following_run == Some(last.run);

	let mut value: Vec<SymbolicChar> = Vec::new();

	// The query had a wildcard before this text, unless the run continues from the previous part or is
	// pinned to the start of the message.
	if !continues_before && !(preceding_run.is_none() && runs[first.run].anchored_start) {
		value.push(SymbolicChar::GlobStar);
	}

	// Seeded with the first piece's own run: any boundary *before* this part has already been accounted
	// for above, and comparing against `preceding_run` here would emit a second wildcard for it.
	let mut previous_run: Option<usize> = Some(first.run);
	for piece in pieces.iter() {
		if previous_run.is_some_and(|previous| previous != piece.run) {
			value.push(SymbolicChar::GlobStar);
		}
		value.extend(piece.text.chars().map(SymbolicChar::Literal));
		previous_run = Some(piece.run);
	}

	// Likewise after: a rule may emit more text, but only where the query permits it. The condition
	// mirrors the leading one exactly — the run must be the last thing emitted, not merely end-anchored.
	if is_rule && !continues_after && !(following_run.is_none() && runs[last.run].anchored_end) {
		value.push(SymbolicChar::GlobStar);
	}

	value
}

/// Caps composition enumeration.
#[derive(Clone, Copy, Debug)]
pub struct ComposeBudget {
	pub max_compositions: usize,
}

impl Default for ComposeBudget {
	fn default() -> Self {
		Self {
			max_compositions: 4_096,
		}
	}
}

/// The result of composing a shape's placements.
#[derive(Clone, Debug)]
pub enum Composed {
	/// No assignment of runs to positions exists: the shape cannot match.
	Impossible,
	/// The complete set of decompositions.
	Compositions(Vec<Composition>),
	/// The budget was exhausted; the caller must not draw a conclusion.
	Unknown,
}

/// Enumerates every composition of `table`'s placements.
///
/// Compositions that assign several runs to one rule are verified against that rule before being
/// returned: [`PlacementTable`] places a run at a time, and a rule that admits each run separately need
/// not admit them together. Verification lives here, rather than in the caller, so that an infeasible
/// composition cannot escape.
#[must_use]
pub fn compose(
	spec: &ParsingSpec,
	model: &ShapeModel,
	table: &PlacementTable,
	runs: &[Run],
	fits: &RunFitCache,
	budget: ComposeBudget,
) -> Composed {
	if table.is_impossible() {
		return Composed::Impossible;
	}

	// An end-anchored query pins the shape's trailing parts to producing *nothing*, which is a real
	// constraint this module cannot express: rendering is truncated at the last constrained part (see
	// `rendered_parts`), justified by the query's implicit trailing wildcard leaving the rest
	// unconstrained. With no such wildcard those parts are constrained — to the empty string — and the
	// engine reports that precisely, as an empty capture. Defer to it rather than render a `*` that
	// claims the opposite.
	if runs.last().is_some_and(|run| run.anchored_end) && model.parts.last().is_some_and(ShapePart::can_be_empty) {
		return Composed::Unknown;
	}

	let num_runs: usize = table.placements.len();
	if 0 == num_runs {
		// A query of only wildcards constrains nothing; there is exactly one (empty) decomposition.
		return Composed::Compositions(vec![Composition {
			captures: Vec::new(),
			literals: Vec::new(),
		}]);
	}

	let num_parts: usize = model.parts.len();
	// States are *positions* (part plus character offset), not bare part indices: several runs can sit in
	// one static part, distinguished only by where in it they begin. See [`crate::prefilter::placement`].
	let positions: Vec<Position> = positions_of(table, num_parts);

	// Feasibility first, as *bits*: `reachable[(run * width) + position]` is true when runs `run..` can
	// all be placed without starting before `position`.
	//
	// Keeping this separate from enumeration is what bounds memory. Materializing the compositions at
	// every state instead would cost `states × compositions`, and a shape can have tens of thousands of
	// parts, so that is not viable.
	let width: usize = positions.len();
	let mut reachable: Vec<bool> = vec![false; (num_runs + 1) * width];
	for position in 0..width {
		reachable[(num_runs * width) + position] = true;
	}
	for run in (0..num_runs).rev() {
		for position in 0..width {
			reachable[(run * width) + position] = table.placements[run].iter().any(|placement| {
				((placement.start_part, placement.start_offset) >= positions[position])
					&& reachable[((run + 1) * width)
						+ index_of(
							&positions,
							(placement.next_available_part(), placement.next_available_offset()),
						)]
			});
		}
	}

	if !reachable[0] {
		return Composed::Impossible;
	}

	// Then enumerate, walking only states already known to lead to a complete assignment. Pruning on
	// `reachable` means every branch entered yields at least one composition, so the work is proportional
	// to the number of compositions rather than to the search space.
	let mut compositions: Vec<Composition> = Vec::new();
	let mut choices: Vec<usize> = Vec::with_capacity(num_runs);
	if !enumerate(
		model,
		table,
		&reachable,
		width,
		&positions,
		0,
		0,
		&mut choices,
		&mut compositions,
		budget,
	) {
		return Composed::Unknown;
	}

	// A rule holding pieces of several runs was validated one run at a time; check it against all of
	// them together. An alternation such as `INFO|WARN` admits either run alone but never both.
	compositions.retain(|composition| {
		composition
			.captures_to_verify(model, runs)
			.iter()
			.all(|(name, value)| fits.can_produce_all_text(spec, name, value))
	});

	compositions.sort();
	compositions.dedup();

	if compositions.is_empty() {
		return Composed::Impossible;
	}

	Composed::Compositions(compositions)
}

/// Depth-first enumeration of compositions, pruned by `reachable`.
///
/// Returns `false` if the budget was exceeded.
#[allow(clippy::too_many_arguments)]
fn enumerate(
	model: &ShapeModel,
	table: &PlacementTable,
	reachable: &[bool],
	width: usize,
	positions: &[Position],
	run: usize,
	position: usize,
	choices: &mut Vec<usize>,
	compositions: &mut Vec<Composition>,
	budget: ComposeBudget,
) -> bool {
	if run == table.placements.len() {
		if compositions.len() >= budget.max_compositions {
			return false;
		}
		compositions.push(build_composition(model, table, choices));
		return true;
	}

	for (choice, placement) in table.placements[run].iter().enumerate() {
		if (placement.start_part, placement.start_offset) < positions[position] {
			continue;
		}
		let next: usize = index_of(
			positions,
			(placement.next_available_part(), placement.next_available_offset()),
		);
		if !reachable[((run + 1) * width) + next] {
			continue;
		}

		choices.push(choice);
		let ok: bool = enumerate(
			model,
			table,
			reachable,
			width,
			positions,
			run + 1,
			next,
			choices,
			compositions,
			budget,
		);
		choices.pop();

		if !ok {
			return false;
		}
	}

	true
}

/// Turns one set of per-run placement choices into a [`Composition`].
///
/// Pieces landing in the same shape part are merged, which is what allows several runs to share a rule
/// (they are separated by text the rule also produces).
fn build_composition(model: &ShapeModel, table: &PlacementTable, choices: &[usize]) -> Composition {
	// Keyed by part index to merge pieces, then flattened in shape order.
	let mut by_part: std::collections::BTreeMap<usize, Vec<PieceRef>> = std::collections::BTreeMap::new();

	for (run, &choice) in choices.iter().enumerate() {
		let placement: &Placement = &table.placements[run][choice];
		for piece in placement.pieces.iter() {
			by_part.entry(piece.part).or_default().push(PieceRef {
				run,
				text: piece.text.clone(),
			});
		}
	}

	let mut captures: Vec<Capture> = Vec::new();
	let mut literals: Vec<Literal> = Vec::new();

	for (part, pieces) in by_part.into_iter() {
		match &model.parts[part] {
			ShapePart::Placeholder(placeholder) => captures.push(Capture {
				part,
				name: placeholder.name.clone(),
				pieces,
			}),
			ShapePart::Static(_) => literals.push(Literal { part, pieces }),
		}
	}

	Composition { captures, literals }
}

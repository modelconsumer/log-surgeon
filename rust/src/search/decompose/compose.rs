//! Composes [`Placement`]s into whole-shape decompositions.
//!
//! # The DP
//!
//! A [`Composition`] chooses one placement per run such that the chosen placements run
//! left to right and do not overlap. The state is `(run index, earliest usable part)`
//! and every placement advances both, so the state space is a DAG and is solved
//! by a reverse sweep -- the same structure as [`crate::search::decompose::align`],
//! and the reason feasibility can be decided in `O(runs x parts x placements)`
//! even when the number of compositions is large.
//!
//! Enumeration is separated from feasibility ([`crate::search::decompose::can_compose`]),
//! so that a shape can be *rejected* cheaply, and only a shape that survives pays for
//! materializing its decompositions. A [`ComposeBudget`] caps enumeration,
//! so a pathological shape degrades to "no conclusion" instead of exploding.
//!
//! # Positional identity
//!
//! Each [`Capture`] names the **index of the shape part** that produced it,
//! so two references to the same rule in one shape are distinguishable
//! even when they share a name.
//!
//! [`Composition::to_interpretation`] renders that positionally:
//! every rule reference up to the last constrained one contributes a sub-query,
//! with unconstrained references appearing as a bare `*`. The position of a capture
//! among those variables is what identifies which reference it is,
//! so `A%foo%B%foo%C` distinguishes its two `foo`s by whether a vacuous `foo=*`
//! precedes the constrained one.
//!
//! Static text is reported even where the query does not constrain it, as a `'*'`
//! sub-query, so the output has the same shape as the engine's
//! and [`crate::search::SearchString::search_by_name`]'s: a run split across static text
//! and a rule yields one sub-query for each. This is pure rendering -- the parts and
//! their attribution are already known, so no automaton or simulation is involved.
//!
//! A static sub-query's value must glob-match its part's text *exactly*,
//! so a run covering only part of a stretch is padded with `*` on both sides;
//! see [`symbolic_value_of`], which decides this from the piece offsets
//! rather than from whether the part is a rule.
//!
//! References *after* the last constrained capture are omitted when the query is not
//! anchored at the end: its trailing wildcard leaves them unconstrained,
//! so they add nothing. Their static text is still reported,
//! since that is where the trailing wildcard applies.

#[cfg(test)]
mod test;

use std::collections::BTreeMap;

use crate::parsing_spec::ParsingSpec;
use crate::search::Interpretation;
use crate::search::LeafQuery;
use crate::search::SymbolicChar;
use crate::search::decompose::Placement;
use crate::search::decompose::PlacementTable;
use crate::search::decompose::Run;
use crate::search::decompose::RunFitCache;
use crate::search::decompose::ShapeModel;
use crate::search::decompose::ShapePart;
use crate::search::decompose::placement::Reachability;

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
	/// Several runs can land in one rule, so this may hold more than one piece;
	/// they are recorded in order and are separated by unconstrained text.
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
/// contiguous in the query and must stay contiguous in the rendering,
/// while a run *boundary* is exactly where the query had a wildcard.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PieceRef {
	/// Index into the query's runs.
	pub run: usize,
	/// Where in the part's static text this piece begins, in characters; zero inside a rule.
	pub offset: usize,
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
	/// Shape parts are emitted in order: static text as a static sub-query,
	/// rule references as captures. A reference with no query text attributed to it
	/// becomes a vacuous `*` capture, so a capture's position among the variables
	/// identifies which reference it is -- this is how two references to
	/// one rule name are told apart.
	///
	/// Static text is reported whether or not the query constrains it,
	/// matching the engine and the shape of [`crate::search::SearchString::search_by_name`]'s
	/// output: a stretch the query's wildcard merely passes over becomes `'*'`.
	/// Consecutive static parts are merged into one sub-query,
	/// since [`Interpretation::invariants`] forbids two static sub-queries in a row.
	///
	/// Rule references after the last constrained one are **omitted**:
	/// the query's trailing wildcard leaves them unconstrained,
	/// and reporting each as a vacuous capture would add no information.
	///
	/// # Invariant
	///
	/// Every literal character of the query appears in the result, in order,
	/// however it was split between static text and rules. A wildcard appears exactly
	/// where the query had one, i.e. between characters of different runs;
	/// characters of the *same* run stay adjacent, with no wildcard introduced
	/// between them even when they straddle a part boundary.
	#[must_use]
	pub fn to_interpretation(&self, model: &ShapeModel, runs: &[Run]) -> Interpretation {
		let rendered: Vec<RenderedPart> = self.rendered_parts(model, runs);

		// Nothing is constrained, so the query is satisfied without attributing text anywhere.
		if rendered.is_empty() {
			return Interpretation {
				leaf_queries: vec![LeafQuery::new_static_text(vec![SymbolicChar::GlobStar])],
			};
		}

		let mut leaf_queries: Vec<LeafQuery> = Vec::new();

		for part in rendered.into_iter() {
			match &model.parts[part.part] {
				// A name can resolve to several rules, but they occupy the same position,
				// so emitting more than one would duplicate the slot
				// and break the positional correspondence.
				ShapePart::Variable(variable) => {
					if let Some(capture) = variable.alternatives.first() {
						leaf_queries.push(LeafQuery::new_rule(
							capture.fully_qualified_name.clone(),
							part.value,
						));
					}
				},
				ShapePart::Static(_) => leaf_queries.push(LeafQuery::new_static_text(part.value)),
			}
		}

		Interpretation { leaf_queries }
	}

	/// The shape parts that contribute a sub-query, with their rendered values.
	///
	/// Factored out so that rendering and multi-piece verification agree by construction:
	/// the value checked against a rule is the very one that will be reported.
	fn rendered_parts(&self, model: &ShapeModel, runs: &[Run]) -> Vec<RenderedPart> {
		// The last part holding any query text. Rule references after it are unconstrained,
		// so they are omitted rather than reported as vacuous captures.
		let last_constrained: Option<usize> = self
			.captures
			.iter()
			.map(|capture| capture.part)
			.chain(self.literals.iter().map(|literal| literal.part))
			.max();

		// The last static part. Trailing static text is still reported, as `*`,
		// because that is where the query's trailing wildcard applies.
		let last_static: Option<usize> = model.parts.iter().rposition(|part| !part.is_variable());

		let Some(last_emitted) = last_constrained.max(last_static) else {
			return Vec::new();
		};

		// Every static part is reported, whether or not the query constrains it --
		// an unconstrained one becomes the value `*`, matching the engine
		// and `search_by_name`'s output shape. Variables are reported only
		// up to the last constrained part; beyond that they are unconstrained and omitted.
		//
		// Collecting first makes the *next* part's pieces available,
		// which decides whether a run continues past this part.
		let emissions: Vec<(usize, &[PieceRef])> = (0..=last_emitted)
			.map(|part| (part, self.pieces_for(part)))
			.filter(|&(part, pieces)| {
				!pieces.is_empty()
					|| !model.parts[part].is_variable()
					|| Some(part) <= last_constrained
			})
			.collect::<Vec<_>>();

		let mut rendered: Vec<RenderedPart> = Vec::with_capacity(emissions.len());

		for (index, &(part, pieces)) in emissions.iter().enumerate() {
			// `Some(length)` for static text, whose value must glob-match that text exactly;
			// `None` for a rule, which is padded by what the query permits instead.
			// See `symbolic_value_of`.
			let static_len: Option<usize> = match &model.parts[part] {
				ShapePart::Static(text) => Some(text.chars().count()),
				ShapePart::Variable(_) => None,
			};

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
				value: symbolic_value_of(pieces, runs, preceding_run, following_run, static_len),
				piece_count: pieces.len(),
			});
		}

		// Merge consecutive static parts into one, since [`Interpretation::invariants`]
		// forbids two static sub-queries in a row. A capture is never merged:
		// its position is what identifies which rule reference it is.
		//
		// Concatenation is sound in both ways two static parts can end up adjacent:
		//
		// - genuinely adjacent in the shape (`a%%b` escapes a `%` as text):
		//   each value glob-matches its own text exactly,
		//   so the concatenation glob-matches the concatenated text exactly;
		// - separated by an omitted variable,
		//   which happens only past the last constrained part -- so the later part
		//   has no attributed text and its value is a bare `*`,
		//   which is exactly the wildcard the omitted variable's output requires.
		let mut merged: Vec<RenderedPart> = Vec::with_capacity(rendered.len());
		for part in rendered.into_iter() {
			if !model.parts[part.part].is_variable()
				&& let Some(last) = merged.last_mut()
				&& !model.parts[last.part].is_variable()
			{
				last.value =
					condense_wildcards(last.value.iter().chain(part.value.iter()).cloned());
				last.piece_count += part.piece_count;
				continue;
			}
			merged.push(part);
		}

		merged
	}

	/// The captures that need a multi-piece feasibility check, as `(rule name, value)`.
	///
	/// A capture holding pieces from *several* runs was validated only one run at a time:
	/// each run can sit in the rule, but that does not mean the rule can produce
	/// them all together. An alternation such as `INFO|WARN` admits either alone
	/// and neither pair. Single-piece captures are already exact, so they are skipped.
	///
	/// Values come from the same rendering used by [`Self::to_interpretation`],
	/// so the check asks about exactly the string that will be reported --
	/// including whether it is padded, which decides whether the rule may emit
	/// anything around it.
	#[must_use]
	pub fn captures_to_verify(&self, model: &ShapeModel, runs: &[Run]) -> Vec<(String, String)> {
		Vec::from_iter(
			self.rendered_parts(model, runs)
				.into_iter()
				.filter_map(|rendered| {
					match &model.parts[rendered.part] {
						ShapePart::Variable(variable) if rendered.piece_count > 1 => {
							// `Display` escapes the characters a query treats specially,
							// so the value round-trips through `SearchString::parse`
							// as the literal text it stands for.
							Some((
								variable.name.clone(),
								String::from_iter(rendered.value.iter().map(ToString::to_string)),
							))
						},
						_ => None,
					}
				}),
		)
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

/// Concatenates values, collapsing the `**` that adjoining wildcards would otherwise produce.
///
/// [`Interpretation::invariants`] forbids a doubled wildcard, and merging two static parts
/// (each of which may itself begin or end with `*`) can create one.
fn condense_wildcards(symbols: impl Iterator<Item = SymbolicChar>) -> Vec<SymbolicChar> {
	let mut value: Vec<SymbolicChar> = Vec::new();
	for symbol in symbols {
		if symbol.is_wildcard() && value.last().is_some_and(SymbolicChar::is_wildcard) {
			continue;
		}
		value.push(symbol);
	}
	value
}

/// The symbolic value for one part's pieces.
///
/// A wildcard is emitted exactly where the query had one -- at a run boundary --
/// and nowhere else. In particular a run that crosses from one part into the next
/// stays contiguous: `preceding_run` and `following_run` name the runs adjacent
/// to this part, so a boundary can be told apart from a mere part boundary.
///
/// The two kinds of part are padded on different grounds, so `static_len` distinguishes them.
///
/// A **rule** may emit text of its own around the query's characters,
/// so it is padded wherever the query permits: suppressed where the run continues
/// into a neighbouring part, and where the query anchors the run to the start
/// or end of the message.
///
/// **Static text** is reproduced verbatim, so its value must be a glob matching
/// the part's text *exactly*. The padding is therefore decided by the piece offsets
/// alone -- a wildcard stands for the characters of the part the run does not cover,
/// and appears if and only if there are some. Anchoring and run continuation need
/// no special case here: a run flowing in from the previous part necessarily
/// begins at offset zero, and one flowing out necessarily reaches the text's end,
/// so both fall out of the offsets.
fn symbolic_value_of(
	pieces: &[PieceRef],
	runs: &[Run],
	preceding_run: Option<usize>,
	following_run: Option<usize>,
	static_len: Option<usize>,
) -> Vec<SymbolicChar> {
	// A part with no attributed text is a bare `*`, present only to mark its position.
	if pieces.is_empty() {
		return vec![SymbolicChar::GlobStar];
	}

	let first: &PieceRef = pieces.first().expect("non-empty");
	let last: &PieceRef = pieces.last().expect("non-empty");
	// The run continues across the part boundary, so no wildcard may separate the characters.
	let continues_before: bool = preceding_run == Some(first.run);
	let continues_after: bool = following_run == Some(last.run);

	let mut value: Vec<SymbolicChar> = Vec::new();

	// Before: for static text, exactly the characters preceding the first piece;
	// for a rule, whatever the query's wildcard allows it to emit.
	let pad_before: bool = match static_len {
		Some(_) => 0 != first.offset,
		None => !continues_before && !(preceding_run.is_none() && runs[first.run].anchored_start),
	};
	if pad_before {
		value.push(SymbolicChar::GlobStar);
	}

	// Seeded with the first piece's own run: any boundary *before* this part has already been
	// accounted for above, and comparing against `preceding_run` here would emit
	// a second wildcard for it.
	let mut previous_run: Option<usize> = Some(first.run);
	for piece in pieces.iter() {
		if previous_run.is_some_and(|previous| previous != piece.run) {
			value.push(SymbolicChar::GlobStar);
		}
		value.extend(piece.text.chars().map(SymbolicChar::Literal));
		previous_run = Some(piece.run);
	}

	// And after, by the mirror image of the same reasoning.
	let pad_after: bool = match static_len {
		Some(length) => (last.offset + last.text.chars().count()) < length,
		None => !continues_after && !(following_run.is_none() && runs[last.run].anchored_end),
	};
	if pad_after {
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
			// Sized so that a shape which genuinely *has* many decompositions
			// is still answered here rather than deferred.
			// The engine is not a cheaper way to enumerate the same
			// answers -- it derives them by walking paths through an intersection,
			// which on such a shape is far more expensive and can hit its own path timeout.
			// Falling back is only worthwhile when the engine would do something different,
			// not when it would do the same thing slowly.
			//
			// The HDFS corpus motivates the size: `*INFO*blk*` against the
			// repeated-classpath shapes yields ~18 000 distinct interpretations,
			// enumerated in ~2s, where the engine times out.
			max_compositions: 65_536,
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
/// Compositions that assign several runs to one rule are verified against that rule
/// before being returned: [`PlacementTable`] places a run at a time, and a rule that
/// admits each run separately need not admit them together. Verification lives here,
/// rather than in the caller, so that an infeasible composition cannot escape.
///
/// `anchored_end` is [`crate::search::SearchString::anchored_end`] for the query the runs
/// came from. It is passed separately because the runs alone cannot tell the empty query,
/// which is anchored, from `*`, which is not: neither has any runs.
#[must_use]
pub fn compose(
	spec: &ParsingSpec,
	model: &ShapeModel,
	table: &PlacementTable,
	runs: &[Run],
	anchored_end: bool,
	fits: &RunFitCache,
	budget: ComposeBudget,
) -> Composed {
	debug_assert!(
		runs.last()
			.is_none_or(|run| run.anchored_end == anchored_end),
		"runs disagree with the query about end anchoring"
	);
	if table.is_impossible() {
		return Composed::Impossible;
	}

	// An end-anchored query pins the shape's trailing parts to producing *nothing*,
	// which is a real constraint this module cannot express: rendering is truncated
	// at the last constrained part (see `rendered_parts`), justified by the query's
	// trailing wildcard leaving the rest unconstrained. With no such wildcard
	// those parts are constrained -- to the empty string -- and the engine reports
	// that precisely, as an empty capture. Defer to it rather than render a `*` that
	// claims the opposite.
	//
	// Read off the query rather than the last run, so the empty query --
	// anchored at both ends, with no runs at all -- is deferred too.
	if anchored_end && model.parts.last().is_some_and(ShapePart::can_be_empty) {
		return Composed::Unknown;
	}

	let num_runs: usize = table.placements.len();
	if 0 == num_runs {
		// With no runs the query is either `*`, which constrains nothing and has exactly one
		// (empty) decomposition, or the empty query, which pins *every* part to producing
		// nothing -- a constraint only the engine can express (as above).
		if anchored_end {
			return Composed::Unknown;
		}
		return Composed::Compositions(vec![Composition {
			captures: Vec::new(),
			literals: Vec::new(),
		}]);
	}

	// Feasibility first, as *bits*. Keeping this separate from enumeration is what bounds
	// memory: materializing the compositions at every state instead would cost
	// `states x compositions`, and a shape can have tens of thousands of parts.
	// See [`Reachability`].
	let reachability: Reachability = Reachability::compute(table, model.parts.len());

	if !reachability.is_reachable(0, 0) {
		return Composed::Impossible;
	}

	// Then enumerate, walking only states already known to lead to a complete assignment.
	// Pruning on `reachability` means every branch entered yields at least one composition,
	// so the work is proportional to the number of compositions
	// rather than to the search space.
	let mut compositions: Vec<Composition> = Vec::new();
	let mut choices: Vec<usize> = Vec::with_capacity(num_runs);
	if !enumerate(
		model,
		table,
		&reachability,
		0,
		0,
		&mut choices,
		&mut compositions,
		budget,
	) {
		return Composed::Unknown;
	}

	// A rule holding pieces of several runs was validated one run at a time;
	// check it against all of them together. An alternation such as `INFO|WARN`
	// admits either run alone but never both.
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

/// Depth-first enumeration of compositions, pruned by `reachability`.
///
/// Returns `false` if the budget was exceeded.
#[allow(clippy::too_many_arguments)]
fn enumerate(
	model: &ShapeModel,
	table: &PlacementTable,
	reachability: &Reachability,
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
		if placement.start() < reachability.positions()[position] {
			continue;
		}
		let next: usize = reachability.index_of(placement.next_available());
		if !reachability.is_reachable(run + 1, next) {
			continue;
		}

		choices.push(choice);
		let ok: bool = enumerate(
			model,
			table,
			reachability,
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
/// Pieces landing in the same shape part are merged,
/// which is what allows several runs to share a rule
/// (they are separated by text the rule also produces).
fn build_composition(model: &ShapeModel, table: &PlacementTable, choices: &[usize]) -> Composition {
	// Keyed by part index to merge pieces, then flattened in shape order.
	let mut by_part: BTreeMap<usize, Vec<PieceRef>> = BTreeMap::new();

	for (run, &choice) in choices.iter().enumerate() {
		let placement: &Placement = &table.placements[run][choice];
		for piece in placement.pieces.iter() {
			by_part.entry(piece.part).or_default().push(PieceRef {
				run,
				offset: piece.offset,
				text: piece.text.clone(),
			});
		}
	}

	let mut captures: Vec<Capture> = Vec::new();
	let mut literals: Vec<Literal> = Vec::new();

	for (part, pieces) in by_part.into_iter() {
		match &model.parts[part] {
			ShapePart::Variable(variable) => captures.push(Capture {
				part,
				name: variable.name.clone(),
				pieces,
			}),
			ShapePart::Static(_) => literals.push(Literal { part, pieces }),
		}
	}

	Composition { captures, literals }
}

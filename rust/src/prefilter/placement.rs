//! Places a query's literal runs into a log shape, and composes those placements into decompositions.
//!
//! # The two stages
//!
//! **Placement** (per run, per position): where can this run go? A run must be produced in full, and
//! there are only three possibilities — inside one rule, inside the shape's static text, or straddling
//! a boundary between a rule and what sits beside it. Each possibility is decided from a cached
//! [`RunFit`] (rule side) or a substring search (static-text side), so the cost is independent of how
//! much static text the shape contains.
//!
//! If any run has no placement anywhere in the shape, the shape **cannot** match and is rejected
//! immediately, without touching an automaton.
//!
//! **Composition**: choose one placement per run such that the placements occur left to right and do
//! not overlap. This is a reachability problem over `(run index, shape position)` — every placement
//! advances both — so it is solved by the same kind of reverse DP sweep as
//! [`crate::prefilter::align`], in `O(runs × positions × placements)`, rather than by enumerating
//! choices.
//!
//! # Positional identity
//!
//! A decomposition records the **index of the shape part** that produced each piece, not just the rule
//! name. Two references to the same rule in one shape (`A%foo%B%foo%C`) are therefore distinguishable,
//! which the engine's own output is not: it reports both as `foo` with the same group, leaving the
//! instance to be inferred from position among vacuous captures.

#[cfg(test)]
mod test;

use std::sync::Arc;

use crate::parsing_spec::ParsingSpec;
use crate::prefilter::RunFit;
use crate::prefilter::RunFitCache;
use crate::prefilter::ShapeModel;
use crate::prefilter::ShapePart;
use crate::search::SymbolicChar;

/// A maximal stretch of literal characters from a query, with no wildcards.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Run {
	/// The literal text.
	pub text: String,
	/// Whether the run is pinned to the start of the message (no preceding wildcard).
	pub anchored_start: bool,
	/// Whether the run is pinned to the end of the message (no following wildcard).
	pub anchored_end: bool,
}

/// Splits a query into its literal runs.
///
/// Wildcards are separators and are not themselves runs; `anchored_start`/`anchored_end` record
/// whether a run abuts the query's boundary rather than a wildcard.
#[must_use]
pub fn runs_of(symbols: &[SymbolicChar]) -> Vec<Run> {
	let mut runs: Vec<Run> = Vec::new();
	let mut current: String = String::new();
	let mut anchored_start: bool = true;

	for &symbol in symbols.iter() {
		match symbol {
			SymbolicChar::Literal(c) => current.push(c),
			SymbolicChar::GlobStar => {
				if !current.is_empty() {
					runs.push(Run {
						text: std::mem::take(&mut current),
						anchored_start,
						anchored_end: false,
					});
				}
				anchored_start = false;
			},
		}
	}

	if !current.is_empty() {
		runs.push(Run {
			text: current,
			anchored_start,
			anchored_end: true,
		});
	}

	runs
}

/// Where one run sits in a shape.
///
/// Positions are indices into [`ShapeModel::parts`]. A placement spans `start_part..=end_part`; the
/// common cases are a single part, or a rule plus one neighbour.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
	/// The first shape part this run touches.
	pub start_part: usize,
	/// The last shape part this run touches.
	pub end_part: usize,
	/// Where in `start_part`'s static text this run begins, in characters.
	///
	/// Zero for a run beginning inside a rule, where an offset is meaningless: a rule's output is not
	/// fixed, so there is no position within it to speak of.
	pub start_offset: usize,
	/// Where in `end_part`'s static text this run ends, in characters, counted just past the last
	/// character consumed. Zero when the run ends inside a rule.
	///
	/// This is what lets several runs share one static part: `a*b*c` against the text `abc` places
	/// three runs in the same part, distinguished only by their offsets.
	pub end_offset: usize,
	/// How the run is divided among the parts it touches.
	pub pieces: Vec<Piece>,
}

/// One part's contribution to a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Piece {
	/// Index into [`ShapeModel::parts`].
	pub part: usize,
	/// The characters of the run this part supplies.
	pub text: String,
	/// Whether `part` is a rule (so this is a capture) rather than static text.
	pub is_rule: bool,
}

impl Placement {
	/// The part index a following run must start at or after.
	///
	/// A run that ends inside a rule leaves that rule available to a later run (a rule can produce more
	/// text after the run), and so does a run ending part-way through static text — the rest of that
	/// text is still to come. See [`Self::next_available_offset`], which separates the two.
	#[must_use]
	pub fn next_available_part(&self) -> usize {
		match self.pieces.last() {
			// Still inside the rule; a later run may continue within it.
			Some(piece) if piece.is_rule => self.end_part,
			// Static text with more characters left over is likewise still available.
			_ => self.end_part,
		}
	}

	/// The offset within [`Self::next_available_part`] a following run must start at or after.
	///
	/// Only meaningful where the placement ends in static text; a run ending inside a rule imposes no
	/// offset, since the rule may emit arbitrarily much more text before the next run begins.
	#[must_use]
	pub fn next_available_offset(&self) -> usize {
		match self.pieces.last() {
			Some(piece) if piece.is_rule => 0,
			_ => self.end_offset,
		}
	}

	/// Whether this placement ends inside a rule, leaving no fixed position within the part.
	#[must_use]
	pub fn ends_in_rule(&self) -> bool {
		self.pieces.last().is_some_and(|piece| piece.is_rule)
	}
}

/// The placements available to each run of a query, for one shape.
#[derive(Clone, Debug)]
pub struct PlacementTable {
	/// `placements[i]` are the ways run `i` can sit in the shape.
	pub placements: Vec<Vec<Placement>>,
}

/// Cap on placements recorded per run.
///
/// A shape with many rule instances can offer very many placements for one run. Exceeding this means
/// the table is incomplete, so no conclusion may be drawn; [`PlacementTable::compute`] returns `None`
/// and the caller falls back to the engine. A cap is necessary because placement enumeration is not
/// polynomial in general: a chain of back-to-back rules can split a run in exponentially many ways.
const MAX_PLACEMENTS_PER_RUN: usize = 2_048;

impl PlacementTable {
	/// Whether some run cannot be placed anywhere, proving the shape cannot match.
	#[must_use]
	pub fn is_impossible(&self) -> bool {
		self.placements.iter().any(Vec::is_empty)
	}

	/// The inclusive range of shape parts outside which no run can be placed.
	///
	/// Every way this query's literal text can sit in the shape lies within `start..=end`, so the parts
	/// outside it can only ever be traversed by a wildcard. In particular nothing beyond `end` can hold
	/// a literal character of the query, which is what lets the shape's automaton be built as a *prefix*
	/// ending there: a long tail of static text that no run can reach would otherwise contribute
	/// thousands of states to the intersection while constraining nothing. See
	/// [`crate::search::SearchString::search_by_log_shapes`].
	///
	/// Returns `None` when some run has no placement at all (the shape cannot match, so there is no
	/// window to speak of) or when there are no runs (the query is all wildcards and constrains
	/// nothing).
	#[must_use]
	pub fn window(&self) -> Option<(usize, usize)> {
		let mut start: usize = usize::MAX;
		let mut end: usize = 0;

		for placements in self.placements.iter() {
			// A run with nowhere to go means the shape cannot match; the caller must not narrow.
			let first: &Placement = placements.first()?;
			let mut lo: usize = first.start_part;
			let mut hi: usize = first.end_part;
			for placement in placements.iter() {
				lo = lo.min(placement.start_part);
				hi = hi.max(placement.end_part);
			}
			start = start.min(lo);
			end = end.max(hi);
		}

		(usize::MAX != start).then_some((start, end))
	}

	/// Computes the placements for every run of a query against `model`.
	///
	/// Returns `None` if any rule in the shape could not be reasoned about, in which case the caller
	/// must not draw a conclusion and should fall back to the engine.
	#[must_use]
	pub fn compute(spec: &ParsingSpec, model: &ShapeModel, runs: &[Run], fits: &RunFitCache) -> Option<Self> {
		let mut placements: Vec<Vec<Placement>> = Vec::with_capacity(runs.len());

		for run in runs.iter() {
			let mut for_run: Vec<Placement> = Vec::new();

			for (index, part) in model.parts.iter().enumerate() {
				match part {
					ShapePart::Static(text) => {
						// The run must appear verbatim in this static text.
						for (offset, _) in text.match_indices(&run.text) {
							// Offsets are in characters, not bytes, so that they can be compared against
							// character counts elsewhere.
							let start_offset: usize = text[..offset].chars().count();
							let end_offset: usize = start_offset + run.text.chars().count();

							// A start-anchored run must be the first thing the message emits: nothing may
							// precede it in this part, nor in any earlier part.
							if run.anchored_start && ((0 != start_offset) || !model.can_start_at(index)) {
								continue;
							}
							// And symmetrically for the end.
							if run.anchored_end && ((end_offset != text.chars().count()) || !model.can_end_at(index)) {
								continue;
							}

							for_run.push(Placement {
								start_part: index,
								end_part: index,
								start_offset,
								end_offset,
								pieces: vec![Piece {
									part: index,
									text: run.text.clone(),
									is_rule: false,
								}],
							});
						}

						// Straddle starting in static text: this text supplies a trailing piece of itself as
						// the run's leading piece, and the following parts supply the rest.
						for_run.extend(Self::straddles_from_text(spec, model, run, index, text, fits));
					},
					ShapePart::Placeholder(placeholder) => {
						let fit: Arc<RunFit> = fits.get(spec, &placeholder.name, &run.text);

						// Anchoring demands more than containment. A start-anchored run must be the first
						// thing the message emits, so every earlier part must be able to vanish *and* the
						// rule must *begin* with the run — `prefixes[0]`, not `fits_wholly` ("contains it
						// somewhere"). Without this a query of `N*` would be placed in a rule matching
						// `WARN`. The end is the mirror image, via `suffixes[len]`; anchored at both ends
						// the rule must match the run exactly, with nothing around it.
						let fits_here: bool = match (run.anchored_start, run.anchored_end) {
							(false, false) => fit.fits_wholly(),
							(true, false) => {
								model.can_start_at(index) && fit.prefixes.first().copied().unwrap_or(false)
							},
							(false, true) => model.can_end_at(index) && fit.suffixes.last().copied().unwrap_or(false),
							(true, true) => {
								model.can_start_at(index)
									&& model.can_end_at(index) && fits.matches_exactly(spec, &placeholder.name, &run.text)
							},
						};
						if fits_here {
							for_run.push(Placement {
								start_part: index,
								end_part: index,
								// Wholly inside a rule: no fixed position within the part.
								start_offset: 0,
								end_offset: 0,
								pieces: vec![Piece {
									part: index,
									text: run.text.clone(),
									is_rule: true,
								}],
							});
						}

						// Straddle: this rule supplies a leading piece of the run, and the following parts
						// supply the rest.
						for_run.extend(Self::straddles_from(spec, model, run, index, &fit, fits));
					},
				}

				if for_run.len() > MAX_PLACEMENTS_PER_RUN {
					// The table would be incomplete; refuse to answer rather than answer wrongly.
					return None;
				}
			}

			placements.push(for_run);
		}

		Some(Self { placements })
	}

	/// Placements where the static text at `index` supplies a *leading* piece of `run` and the parts
	/// after it supply the remainder.
	///
	/// The leading piece must be a *suffix* of the static text: the run continues past the end of the
	/// text into whatever follows, so the text's own tail is what it can contribute.
	fn straddles_from_text(
		spec: &ParsingSpec,
		model: &ShapeModel,
		run: &Run,
		index: usize,
		text: &str,
		fits: &RunFitCache,
	) -> Vec<Placement> {
		let characters: Vec<char> = run.text.chars().collect::<Vec<_>>();
		let mut results: Vec<Placement> = Vec::new();

		// `split` is how many leading characters of the run the static text supplies; a proper non-empty
		// prefix, since the whole-run case is handled by the substring search.
		for split in 1..characters.len() {
			let head: String = characters[..split].iter().collect::<String>();
			if !text.ends_with(&head) {
				continue;
			}
			// A start-anchored run must be the first thing the message emits, so nothing may precede it:
			// every earlier part must be able to vanish, and the head must be the whole of this text.
			if run.anchored_start && (!model.can_start_at(index) || (head.chars().count() != text.chars().count())) {
				continue;
			}

			// The head is a suffix of this text, so the run begins where that suffix begins.
			let start_offset: usize = text.chars().count() - head.chars().count();
			let pieces: Vec<Piece> = vec![Piece {
				part: index,
				text: head,
				is_rule: false,
			}];
			results.extend(Self::extend_straddle(
				spec,
				model,
				run,
				index,
				start_offset,
				index + 1,
				split,
				pieces,
				fits,
			));
		}

		results
	}

	/// Placements where the rule at `index` supplies a *leading* piece of `run` and the parts after it
	/// supply the remainder.
	///
	/// The remainder is matched greedily against the following parts: static text must match verbatim
	/// from its start, and a following rule must be able to *begin* with its piece. This is the
	/// "inside/outside" split, and it is bounded by the surrounding literal text.
	fn straddles_from(
		spec: &ParsingSpec,
		model: &ShapeModel,
		run: &Run,
		index: usize,
		fit: &RunFit,
		fits: &RunFitCache,
	) -> Vec<Placement> {
		let characters: Vec<char> = run.text.chars().collect::<Vec<_>>();
		let mut results: Vec<Placement> = Vec::new();

		// `split` is how many leading characters the rule supplies; it must be a non-empty proper prefix,
		// since `split == 0` and `split == len` are the non-straddling cases handled elsewhere.
		for split in 1..characters.len() {
			// A start-anchored run cannot begin part-way through a rule's output unless nothing can
			// precede that rule.
			if run.anchored_start && !model.can_start_at(index) {
				continue;
			}

			let head: String = characters[..split].iter().collect::<String>();
			// `suffixes[split]` only says the rule can *end* with this text. An anchored run additionally
			// pins the rule's start, so the rule must match the piece exactly; otherwise `NIn*` would be
			// split as `level=N` even though no level is just `N`.
			let fits_here: bool = if run.anchored_start {
				model.parts[index]
					.placeholder_name()
					.is_some_and(|name| fits.matches_exactly(spec, name, &head))
			} else {
				fit.suffixes.get(split).copied().unwrap_or(false)
			};
			if !fits_here {
				continue;
			}

			let pieces: Vec<Piece> = vec![Piece {
				part: index,
				text: head,
				is_rule: true,
			}];
			results.extend(Self::extend_straddle(
				spec,
				model,
				run,
				index,
				// Begins inside a rule: no position within the part.
				0,
				index + 1,
				split,
				pieces,
				fits,
			));
		}

		results
	}

	/// How many characters a rule at `part` may contribute as a *middle* piece of a run.
	///
	/// See the call site for why the following shape part pins these candidates.
	fn candidate_middle_lengths(model: &ShapeModel, part: usize, characters: &[char], consumed: usize) -> Vec<usize> {
		/// Cap on candidates when nothing in the shape pins the split, i.e. between back-to-back rules.
		/// Without a bound, a chain of adjacent rules multiplies candidates per link.
		const MAX_UNPINNED_SPLITS: usize = 8;

		let available: usize = characters.len() - consumed;
		// A middle piece is non-empty and must leave something for the following parts.
		if available < 2 {
			return Vec::new();
		}

		match model.parts.get(part + 1) {
			Some(ShapePart::Static(text)) => {
				// The run must continue with `text`, so the rule's piece ends where `text` begins.
				let Some(boundary) = text.chars().next() else {
					return Vec::new();
				};
				Vec::from_iter((1..available).filter(|&take| characters[consumed + take] == boundary))
			},
			// Nothing pins the boundary; try every split, bounded.
			Some(ShapePart::Placeholder(_)) => Vec::from_iter(1..available.min(MAX_UNPINNED_SPLITS + 1)),
			// The rule is the last part, so it cannot be a *middle* piece.
			None => Vec::new(),
		}
	}

	/// Completes a straddle: given `consumed` characters of `run` already supplied by parts up to
	/// `part - 1`, matches the remainder against `part` onwards.
	///
	/// Shared by both straddle directions (starting from static text or from a rule). Each following part
	/// must supply the run's next characters *contiguously*, since a run has no wildcards inside it:
	/// static text from its very start, and a rule either by finishing the run (it can begin with what
	/// remains) or by producing exactly a middle piece and handing off to the next part.
	///
	/// Returns every completion, because a rule in the middle of a run can take any number of characters.
	/// This is what lets a run be traced through an alternating sequence such as
	/// `blk_%blockNum%_%genStamp%`, where the run spans four parts.
	#[allow(clippy::too_many_arguments)]
	fn extend_straddle(
		spec: &ParsingSpec,
		model: &ShapeModel,
		run: &Run,
		start_part: usize,
		start_offset: usize,
		part: usize,
		consumed: usize,
		pieces: Vec<Piece>,
		fits: &RunFitCache,
	) -> Vec<Placement> {
		let characters: Vec<char> = run.text.chars().collect::<Vec<_>>();

		// The run is fully produced: this is a complete placement.
		if consumed == characters.len() {
			let last: Option<&Piece> = pieces.last();
			let end_part: usize = last.map_or(start_part, |piece| piece.part);
			// A straddle's final piece starts at the beginning of its part (the run flows into it), so the
			// end offset is just that piece's length — and is meaningless if the piece is a rule.
			let end_offset: usize = match last {
				Some(piece) if !piece.is_rule => piece.text.chars().count(),
				_ => 0,
			};

			// An end-anchored run must be the last thing the message emits. Every later part must be able
			// to vanish, and the run must reach the end of the part it finishes in. (A piece that ends
			// inside a *rule* is handled where that piece is produced, which is the only place that knows
			// whether the rule may emit more after it.)
			if run.anchored_end {
				let ends_cleanly: bool = match (last, &model.parts[end_part]) {
					(Some(piece), ShapePart::Static(text)) if !piece.is_rule => end_offset == text.chars().count(),
					_ => true,
				};
				if !ends_cleanly || !model.can_end_at(end_part) {
					return Vec::new();
				}
			}

			return vec![Placement {
				start_part,
				end_part,
				start_offset,
				end_offset,
				pieces,
			}];
		}

		// Ran out of shape before the run was fully produced.
		let Some(next) = model.parts.get(part) else {
			return Vec::new();
		};

		let remaining: String = characters[consumed..].iter().collect::<String>();

		match next {
			ShapePart::Static(text) => {
				// The static text must supply the next characters from its very start.
				let available: usize = text.chars().count();
				let take: usize = available.min(remaining.chars().count());
				let head: String = remaining.chars().take(take).collect::<String>();
				if !text.starts_with(&head) {
					return Vec::new();
				}
				// If the run continues past this part, it must have consumed all of the text; otherwise
				// there would be leftover literal text between the run's pieces.
				if ((consumed + take) < characters.len()) && (take < available) {
					return Vec::new();
				}

				let mut pieces: Vec<Piece> = pieces;
				pieces.push(Piece {
					part,
					text: head,
					is_rule: false,
				});
				Self::extend_straddle(
					spec,
					model,
					run,
					start_part,
					start_offset,
					part + 1,
					consumed + take,
					pieces,
					fits,
				)
			},
			ShapePart::Placeholder(placeholder) => {
				let mut results: Vec<Placement> = Vec::new();

				// The rule finishes the run: it can begin with everything that remains.
				//
				// `prefixes[consumed]` only says the rule can *begin* with the remainder, leaving it free
				// to emit more afterwards. An end-anchored run forbids that, so the rule must match the
				// remainder exactly and nothing may follow it.
				let fit: Arc<RunFit> = fits.get(spec, &placeholder.name, &run.text);
				let finishes_here: bool = if run.anchored_end {
					model.can_end_at(part) && fits.matches_exactly(spec, &placeholder.name, &remaining)
				} else {
					fit.prefixes.get(consumed).copied().unwrap_or(false)
				};
				if finishes_here {
					let mut pieces: Vec<Piece> = pieces.clone();
					pieces.push(Piece {
						part,
						text: remaining.clone(),
						is_rule: true,
					});
					results.push(Placement {
						start_part,
						end_part: part,
						start_offset,
						// Ends inside a rule, so there is no position within the part.
						end_offset: 0,
						pieces,
					});
				}

				// Or the rule produces exactly a middle piece, and the run continues into the next part. The
				// piece must be matched *exactly*: a run has no wildcards, so the rule cannot emit anything
				// beyond it.
				//
				// The candidate split points are not arbitrary. Whatever follows this rule in the shape
				// pins them:
				//
				// - static text: the run must continue with that text, so the rule's piece ends exactly
				//   where the text's first character next occurs in the run — a handful of candidates found
				//   by substring search, not one per length;
				// - another rule (back-to-back, no literal boundary): nothing pins the split, so every
				//   length must be tried. Bounded by `MAX_UNPINNED_SPLITS` to keep this from compounding
				//   across a chain of adjacent rules.
				for take in Self::candidate_middle_lengths(model, part, &characters, consumed) {
					let middle: String = characters[consumed..(consumed + take)].iter().collect::<String>();
					if !fits.matches_exactly(spec, &placeholder.name, &middle) {
						continue;
					}
					let mut pieces: Vec<Piece> = pieces.clone();
					pieces.push(Piece {
						part,
						text: middle,
						is_rule: true,
					});
					results.extend(Self::extend_straddle(
						spec,
						model,
						run,
						start_part,
						start_offset,
						part + 1,
						consumed + take,
						pieces,
						fits,
					));
				}

				results
			},
		}
	}
}

/// A position in the shape: a part, and a character offset within it.
///
/// Ordered lexicographically, which is exactly "no earlier in the shape than". The offset is what
/// allows several runs to occupy one static part; it is always zero for a position inside a rule, where
/// no fixed position exists.
pub type Position = (usize, usize);

/// The distinct positions a composition can be in, sorted.
///
/// The DP state is a position rather than a part index, so the state space must be discretised: only
/// positions where some placement starts, or where some placement leaves off, can ever be visited.
#[must_use]
pub fn positions_of(table: &PlacementTable, num_parts: usize) -> Vec<Position> {
	let mut positions: Vec<Position> = vec![(0, 0), (num_parts, 0)];

	for placements in table.placements.iter() {
		for placement in placements.iter() {
			positions.push((placement.start_part, placement.start_offset));
			positions.push((placement.next_available_part(), placement.next_available_offset()));
		}
	}

	positions.sort_unstable();
	positions.dedup();
	positions
}

/// The index of `position` in `positions`.
///
/// Panics if absent, which would mean [`positions_of`] and the DP disagree about the state space.
#[must_use]
pub fn index_of(positions: &[Position], position: Position) -> usize {
	positions.binary_search(&position).expect("position was collected")
}

/// Whether the runs can be placed left to right without overlapping.
///
/// This is the composition feasibility question, answered by a reverse DP sweep over
/// `(run index, earliest available position)`: `reachable[i][p]` is true when runs `i..` can all be
/// placed without starting before position `p`. Answering it separately from *enumerating* the
/// compositions means a shape can be rejected in polynomial time even when the number of compositions is
/// large.
#[must_use]
pub fn can_compose(table: &PlacementTable, num_parts: usize) -> bool {
	if table.is_impossible() {
		return false;
	}

	let positions: Vec<Position> = positions_of(table, num_parts);
	let num_runs: usize = table.placements.len();
	let width: usize = positions.len();
	let mut reachable: Vec<bool> = vec![false; (num_runs + 1) * width];

	// With no runs left, every position is fine.
	for position in 0..width {
		reachable[(num_runs * width) + position] = true;
	}

	for run in (0..num_runs).rev() {
		for position in 0..width {
			reachable[(run * width) + position] = table.placements[run].iter().any(|placement| {
				((placement.start_part, placement.start_offset) >= positions[position]) && {
					let next: usize = index_of(
						&positions,
						(placement.next_available_part(), placement.next_available_offset()),
					);
					reachable[((run + 1) * width) + next]
				}
			});
		}
	}

	reachable[0]
}

//! Aligns a query's fixed text against a [`ShapeModel`], decomposing it into the shape's static text
//! and captures of the shape's placeholders.
//!
//! # The model
//!
//! A message of a given shape is produced by walking the shape's parts left to right and emitting,
//! for each part, either its static text verbatim or some string the part's rule can match. A query
//! matches when its symbols consume such a message (a *prefix* of it, unless the query is anchored at
//! the end).
//!
//! The shape is flattened into [`Atom`]s — one per static character, one per placeholder — so a
//! position in the walk is just a pair of indices: how much of the query has been consumed, and how
//! much of the shape has been produced.
//!
//! Start anchoring needs no special handling: a query that is not anchored at the start simply
//! *begins* with a [`SymbolicChar::GlobStar`], which is what lets the shape emit unconsumed text. End
//! anchoring is passed explicitly, because the engine expresses it by *not* simulating a trailing
//! wildcard rather than by carrying one in the symbols — there is nothing in `symbols` to read it off.
//!
//! # Structure and termination
//!
//! Every transition advances the query cursor, the shape cursor, or both; neither moves backwards, so
//! the state space is a DAG and can be solved in one reverse sweep. The work happens in two passes:
//!
//! 1. *Reachability* — which states can still consume the rest of the query. This alone answers the
//!    prefilter's main question (can this shape match at all?) in `O(states)`, with no allocation per
//!    state, so rejecting a shape is cheap.
//! 2. *Enumeration* — only over reachable states, collecting the decompositions themselves. The number
//!    of decompositions can be exponential in principle, so this pass is capped by a [`Budget`];
//!    exceeding it yields [`Outcome::Unknown`] rather than a wrong answer.
//!
//! # Soundness
//!
//! Placeholders are approximated by a [`crate::prefilter::Charset`], and are permitted to match the
//! empty string (the model does not know a rule's minimum length). Both widen the set of accepted
//! alignments, so the result is a **superset** of the true decompositions: [`Outcome::Rejected`]
//! proves no match is possible, while [`Outcome::Approximate`] must still be confirmed by the engine.

#[cfg(test)]
mod test;

use std::sync::Arc;

use crate::parsing_spec::SubRule;
use crate::prefilter::Placeholder;
use crate::prefilter::ShapeModel;
use crate::prefilter::ShapePart;
use crate::search::SymbolicChar;

/// One way the query's fixed text lines up with a shape.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Alignment {
	pub fragments: Vec<Fragment>,
}

/// A piece of a query, attributed to the part of the shape that produced it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Fragment {
	/// Query text matched against the shape's static text, and the wildcards between.
	Static(Vec<SymbolicChar>),
	/// Query text matched against a placeholder, i.e. a capture of that rule.
	Capture {
		sub_rule: Arc<SubRule>,
		contents: Vec<SymbolicChar>,
	},
}

/// The result of aligning a query against a shape.
#[derive(Clone, Debug)]
pub enum Outcome {
	/// Proved that no message of this shape can match the query.
	Rejected,
	/// A superset of the true decompositions; the engine must confirm.
	Approximate(Vec<Alignment>),
	/// No conclusion: the budget was exhausted. The caller must use the engine.
	Unknown,
}

/// Caps the work the enumeration pass may do, so a pathological shape cannot blow up.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
	/// Maximum decompositions to materialize across all states.
	pub max_partial_alignments: usize,
	/// Maximum decompositions to return.
	pub max_alignments: usize,
}

impl Default for Budget {
	fn default() -> Self {
		// Generous enough for realistic shapes (thousands of atoms, a handful of query runs), small
		// enough that the fallback stays far cheaper than the intersection it is trying to avoid.
		Self {
			max_partial_alignments: 500_000,
			max_alignments: 4_096,
		}
	}
}

/// A shape flattened to one symbol-producing unit.
///
/// Flattening removes the need to track a character offset inside a static run, and makes indexed
/// access `O(1)` (indexing a `String` by character position is not).
#[derive(Clone, Copy, Debug)]
enum Atom<'a> {
	/// A single static character, which a match must reproduce verbatim.
	Char(char),
	/// A rule reference, standing for any string the rule can match.
	Placeholder(&'a Placeholder),
}

impl ShapeModel {
	/// Flattens this shape into [`Atom`]s.
	fn atoms(&self) -> Vec<Atom<'_>> {
		let mut atoms: Vec<Atom<'_>> = Vec::new();
		for part in self.parts.iter() {
			match part {
				ShapePart::Static(text) => atoms.extend(text.chars().map(Atom::Char)),
				ShapePart::Placeholder(placeholder) => atoms.push(Atom::Placeholder(placeholder)),
			}
		}
		atoms
	}
}

/// Whether any message of this shape could match the query.
///
/// This is the prefilter proper, and runs only the reachability pass: `false` **proves** no message of
/// the shape can match, while `true` means "not ruled out". It allocates one bit per state and nothing
/// per alignment, so it is much cheaper than [`align`] — use it whenever the decompositions themselves
/// are not needed.
///
/// `symbols` is the query as the engine sees it, i.e. with any single trailing wildcard already
/// stripped. `anchored_at_end` states whether the query must consume the shape through to its end,
/// which is exactly "the query had no trailing wildcard to strip".
#[must_use]
pub fn can_match(model: &ShapeModel, symbols: &[SymbolicChar], anchored_at_end: bool) -> bool {
	// An empty query constrains nothing; never claim it cannot match.
	if symbols.is_empty() {
		return true;
	}

	let symbols: Vec<SymbolicChar> = collapse_wildcards(symbols);

	if is_obviously_not_ruled_out(model, &symbols, anchored_at_end) {
		return true;
	}

	let atoms: Vec<Atom<'_>> = model.atoms();
	let solver: Solver<'_> = Solver {
		symbols: &symbols,
		atoms: &atoms,
		anchored_at_end,
	};

	solver.can_reach_acceptance()
}

/// A cheap *sufficient* condition for "not ruled out", used to skip the full walk.
///
/// If the query may float freely (it starts with a wildcard) and some single placeholder's charset
/// admits every literal the query contains, then as far as this model knows that placeholder alone
/// could emit the entire query text, so the shape cannot be rejected.
///
/// When the query is anchored at the end the placeholder must additionally be able to be *last*, since
/// the query has to consume the message through to its end; a placeholder with only nullable parts
/// after it qualifies.
///
/// This only ever returns `true`, i.e. only ever causes a shape to be *kept*, so it cannot make the
/// prefilter unsound. It matters because it is `O(placeholders + query)` against the walk's
/// `O(atoms × query)`: real log shapes are long (thousands of characters) and usually contain a
/// permissive placeholder, so this is the common case, and paying for the full table there is what
/// made the prefilter cost more than it saved.
fn is_obviously_not_ruled_out(model: &ShapeModel, symbols: &[SymbolicChar], anchored_at_end: bool) -> bool {
	if Some(&SymbolicChar::GlobStar) != symbols.first() {
		return false;
	}

	model.parts.iter().enumerate().any(|(index, part)| {
		let ShapePart::Placeholder(placeholder) = part else {
			return false;
		};
		// The query must be able to finish here, or it could not reach the end of the message.
		if anchored_at_end && !model.can_end_at(index) {
			return false;
		}
		placeholder.charset.is_universal()
			|| symbols.iter().all(|symbol| match symbol {
				SymbolicChar::Literal(c) => placeholder.charset.contains(*c),
				SymbolicChar::GlobStar => true,
			})
	})
}

/// Aligns `symbols` against `model`, decomposing the query's fixed text.
///
/// Prefer [`can_match`] when only the yes/no answer is needed; this additionally enumerates the
/// decompositions, which costs proportionally to how many there are.
///
/// `symbols` is the query as the engine sees it, i.e. with any single trailing wildcard already
/// stripped. `anchored_at_end` states whether the query must consume the shape through to its end;
/// pass `false` to mirror the engine's current prefix matching.
#[must_use]
pub fn align(model: &ShapeModel, symbols: &[SymbolicChar], anchored_at_end: bool, budget: Budget) -> Outcome {
	// An empty query constrains nothing, and the engine rejects it outright; do not claim otherwise.
	if symbols.is_empty() {
		return Outcome::Unknown;
	}

	// Collapsing runs of wildcards keeps alignments canonical: `**` constrains no more than `*`, so
	// leaving both would enumerate the same decomposition twice. It also guarantees no two adjacent
	// symbols are both wildcards, which the transitions below rely on.
	let symbols: Vec<SymbolicChar> = collapse_wildcards(symbols);
	let symbols: &[SymbolicChar] = &symbols;

	let atoms: Vec<Atom<'_>> = model.atoms();
	let solver: Solver<'_> = Solver {
		symbols,
		atoms: &atoms,
		anchored_at_end,
	};

	let reachable: Vec<bool> = solver.reachability();
	if !reachable[solver.index(0, 0)] {
		return Outcome::Rejected;
	}

	solver.enumerate(&reachable, budget)
}

/// The alignment problem for one (query, shape) pair.
struct Solver<'a> {
	symbols: &'a [SymbolicChar],
	atoms: &'a [Atom<'a>],
	anchored_at_end: bool,
}

impl<'a> Solver<'a> {
	/// The number of query cursor positions (`0..=len`).
	fn num_query_positions(&self) -> usize {
		self.symbols.len() + 1
	}

	/// Flattens a `(query, atom)` cursor to a single index.
	fn index(&self, query: usize, atom: usize) -> usize {
		(atom * self.num_query_positions()) + query
	}

	/// Whether the shape, having produced `atom..`, can stop without emitting anything more.
	///
	/// Only relevant when anchored at the end. Remaining static characters must be produced, so they
	/// block acceptance; a placeholder is conservatively assumed to be able to match the empty string.
	fn can_stop(&self, atom: usize) -> bool {
		self.atoms[atom..]
			.iter()
			.all(|atom| matches!(atom, Atom::Placeholder(_)))
	}

	/// Whether the query is fully consumed and the shape may stop here.
	fn is_accepting(&self, query: usize, atom: usize) -> bool {
		(query == self.symbols.len()) && (!self.anchored_at_end || self.can_stop(atom))
	}

	/// The longest block of query symbols, starting at `query`, that `placeholder` could emit.
	///
	/// A wildcard stands for arbitrary rule output, so it never bounds the block; a literal does unless
	/// the rule's charset admits it.
	fn max_capture_length(&self, placeholder: &Placeholder, query: usize) -> usize {
		let mut length: usize = 0;
		while let Some(&symbol) = self.symbols.get(query + length) {
			match symbol {
				SymbolicChar::Literal(c) => {
					if !placeholder.charset.contains(c) {
						break;
					}
				},
				SymbolicChar::GlobStar => (),
			}
			length += 1;
		}
		length
	}

	/// Whether the start state can reach acceptance.
	///
	/// Same recurrence as [`Self::reachability`], but keeps only the two atom columns it needs (the
	/// current one and its successor) instead of the full table, since no later pass reads the rest.
	fn can_reach_acceptance(&self) -> bool {
		let width: usize = self.num_query_positions();
		// `next` is the column for `atom + 1`; `current` is the column being filled.
		let mut next: Vec<bool> = vec![false; width];
		let mut current: Vec<bool> = vec![false; width];

		for atom in (0..=self.atoms.len()).rev() {
			for query in (0..width).rev() {
				current[query] = if self.is_accepting(query, atom) {
					true
				} else if query == self.symbols.len() {
					// Query consumed, but the shape must still produce static text (end-anchored only).
					false
				} else {
					let symbol: SymbolicChar = self.symbols[query];
					match self.atoms.get(atom) {
						// Past the end of the shape: only a wildcard can remain, matching nothing.
						None => (SymbolicChar::GlobStar == symbol) && current[query + 1],
						Some(Atom::Char(expected)) => match symbol {
							SymbolicChar::Literal(c) => (c == *expected) && next[query + 1],
							SymbolicChar::GlobStar => next[query] || current[query + 1],
						},
						Some(Atom::Placeholder(placeholder)) => {
							let longest: usize = self.max_capture_length(placeholder, query);
							(0..=longest).any(|length| next[query + length])
						},
					}
				};
			}

			if 0 == atom {
				return current[0];
			}
			std::mem::swap(&mut next, &mut current);
		}

		// Unreachable: the loop returns at `atom == 0`.
		false
	}

	/// Which states can consume the rest of the query and accept.
	///
	/// Solved in one reverse sweep: every transition either advances the atom cursor, or advances the
	/// query cursor while leaving the atom cursor alone, so visiting atoms descending (and, within an
	/// atom, query positions descending) always visits successors first.
	fn reachability(&self) -> Vec<bool> {
		let num_states: usize = self.num_query_positions() * (self.atoms.len() + 1);
		let mut reachable: Vec<bool> = vec![false; num_states];

		for atom in (0..=self.atoms.len()).rev() {
			for query in (0..self.num_query_positions()).rev() {
				let state: usize = self.index(query, atom);

				if self.is_accepting(query, atom) {
					reachable[state] = true;
					continue;
				}
				if query == self.symbols.len() {
					// Query consumed, but the shape must still produce static text (end-anchored only).
					continue;
				}

				let symbol: SymbolicChar = self.symbols[query];

				let Some(&current) = self.atoms.get(atom) else {
					// Past the end of the shape: there is no more message to consume, so only a
					// wildcard can remain, and only by matching nothing.
					reachable[state] = (SymbolicChar::GlobStar == symbol) && reachable[self.index(query + 1, atom)];
					continue;
				};

				reachable[state] = match current {
					Atom::Char(expected) => match symbol {
						// Static text must be reproduced verbatim.
						SymbolicChar::Literal(c) => (c == expected) && reachable[self.index(query + 1, atom + 1)],
						// The wildcard absorbs this character, or stops and leaves it to the next symbol.
						SymbolicChar::GlobStar => {
							reachable[self.index(query, atom + 1)] || reachable[self.index(query + 1, atom)]
						},
					},
					Atom::Placeholder(placeholder) => {
						let longest: usize = self.max_capture_length(placeholder, query);
						// `length == 0` means the placeholder's output is not described by the query.
						(0..=longest).any(|length| reachable[self.index(query + length, atom + 1)])
					},
				};
			}
		}

		reachable
	}

	/// Collects the decompositions, considering only reachable states.
	///
	/// Uses the same reverse sweep as [`Self::reachability`], so each state's decompositions are built
	/// from its successors' — already computed, and shared rather than re-explored. This is what keeps
	/// the pass proportional to the number of *distinct* decompositions instead of the number of paths.
	fn enumerate(&self, reachable: &[bool], budget: Budget) -> Outcome {
		let num_states: usize = self.num_query_positions() * (self.atoms.len() + 1);
		// `None` for unreachable states, which are never read.
		let mut suffixes: Vec<Option<Vec<Vec<Fragment>>>> = vec![None; num_states];
		let mut materialized: usize = 0;

		for atom in (0..=self.atoms.len()).rev() {
			for query in (0..self.num_query_positions()).rev() {
				let state: usize = self.index(query, atom);
				if !reachable[state] {
					continue;
				}

				let mut collected: Vec<Vec<Fragment>> = Vec::new();

				if self.is_accepting(query, atom) {
					// One decomposition: the empty suffix.
					collected.push(Vec::new());
				} else {
					let symbol: SymbolicChar = self.symbols[query];

					match self.atoms.get(atom) {
						None => {
							// Only a trailing wildcard, matching nothing.
							debug_assert_eq!(SymbolicChar::GlobStar, symbol);
							self.extend_from(
								&mut collected,
								&suffixes,
								reachable,
								query + 1,
								atom,
								Prepend::Symbol(SymbolicChar::GlobStar),
							);
						},
						Some(Atom::Char(expected)) => match symbol {
							SymbolicChar::Literal(c) => {
								debug_assert_eq!(c, *expected);
								self.extend_from(
									&mut collected,
									&suffixes,
									reachable,
									query + 1,
									atom + 1,
									Prepend::Symbol(SymbolicChar::Literal(c)),
								);
							},
							SymbolicChar::GlobStar => {
								// The wildcard absorbs this character...
								self.extend_from(
									&mut collected,
									&suffixes,
									reachable,
									query,
									atom + 1,
									Prepend::Symbol(SymbolicChar::GlobStar),
								);
								// ...or stops here, leaving it to the next query symbol.
								self.extend_from(
									&mut collected,
									&suffixes,
									reachable,
									query + 1,
									atom,
									Prepend::Nothing,
								);
							},
						},
						Some(Atom::Placeholder(placeholder)) => {
							let longest: usize = self.max_capture_length(placeholder, query);
							for length in 0..=longest {
								let consumed: &[SymbolicChar] = &self.symbols[query..(query + length)];
								let prepend: Prepend<'_> = if consumed.is_empty() {
									// Nothing attributed to this placeholder, so no capture is emitted.
									Prepend::Nothing
								} else if consumed.iter().all(SymbolicChar::is_wildcard) {
									// A capture of only wildcards says nothing about the placeholder's
									// value; record an unconstrained gap rather than a vacuous
									// "this rule matched" result.
									Prepend::Symbol(SymbolicChar::GlobStar)
								} else {
									Prepend::Capture(placeholder, consumed)
								};
								self.extend_from(
									&mut collected,
									&suffixes,
									reachable,
									query + length,
									atom + 1,
									prepend,
								);
							}
						},
					}
				}

				collected.sort();
				collected.dedup();

				materialized += collected.len();
				if (materialized > budget.max_partial_alignments) || (collected.len() > budget.max_alignments) {
					return Outcome::Unknown;
				}

				suffixes[state] = Some(collected);
			}
		}

		let Some(alignments) = suffixes[self.index(0, 0)].take() else {
			// Unreachable: the caller checked the start state is reachable.
			return Outcome::Rejected;
		};
		if alignments.is_empty() {
			return Outcome::Rejected;
		}
		if alignments.len() > budget.max_alignments {
			return Outcome::Unknown;
		}

		Outcome::Approximate(Vec::from_iter(
			alignments.into_iter().map(|fragments| Alignment { fragments }),
		))
	}

	/// Extends `collected` with each of the successor state's decompositions, prefixed by `prepend`.
	fn extend_from(
		&self,
		collected: &mut Vec<Vec<Fragment>>,
		suffixes: &[Option<Vec<Vec<Fragment>>>],
		reachable: &[bool],
		query: usize,
		atom: usize,
		prepend: Prepend<'_>,
	) {
		let successor: usize = self.index(query, atom);
		if !reachable[successor] {
			return;
		}
		let Some(tails) = suffixes[successor].as_ref() else {
			return;
		};

		for tail in tails.iter() {
			match prepend {
				Prepend::Nothing => collected.push(tail.clone()),
				Prepend::Symbol(symbol) => {
					let mut fragments: Vec<Fragment> = tail.clone();
					prepend_symbol(&mut fragments, symbol);
					collected.push(fragments);
				},
				Prepend::Capture(placeholder, contents) => {
					// One decomposition per alternative this name resolves to.
					for sub_rule in placeholder.alternatives.iter() {
						let mut fragments: Vec<Fragment> = Vec::with_capacity(tail.len() + 1);
						fragments.push(Fragment::Capture {
							sub_rule: sub_rule.clone(),
							contents: contents.to_vec(),
						});
						fragments.extend(tail.iter().cloned());
						collected.push(fragments);
					}
				},
			}
		}
	}
}

/// What to prepend to a successor's decompositions.
#[derive(Clone, Copy)]
enum Prepend<'a> {
	/// The transition consumed no query text.
	Nothing,
	/// A single symbol of static text (or an unconstrained gap).
	Symbol(SymbolicChar),
	/// A capture of `contents` by one of the placeholder's alternatives.
	Capture(&'a Placeholder, &'a [SymbolicChar]),
}

/// Prepends `symbol` to `fragments`, merging into a leading static fragment.
///
/// Merging keeps decompositions canonical, so that structurally identical results compare equal and
/// deduplicate. Adjacent wildcards collapse for the same reason.
fn prepend_symbol(fragments: &mut Vec<Fragment>, symbol: SymbolicChar) {
	if let Some(Fragment::Static(contents)) = fragments.first_mut() {
		if (SymbolicChar::GlobStar == symbol) && (Some(&SymbolicChar::GlobStar) == contents.first()) {
			return;
		}
		contents.insert(0, symbol);
	} else {
		fragments.insert(0, Fragment::Static(vec![symbol]));
	}
}

/// Removes redundant consecutive wildcards.
fn collapse_wildcards(symbols: &[SymbolicChar]) -> Vec<SymbolicChar> {
	let mut out: Vec<SymbolicChar> = Vec::with_capacity(symbols.len());
	for &symbol in symbols.iter() {
		if (SymbolicChar::GlobStar == symbol) && (Some(&SymbolicChar::GlobStar) == out.last()) {
			continue;
		}
		out.push(symbol);
	}
	out
}

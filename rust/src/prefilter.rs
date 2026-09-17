//! Cheap, sound over-approximations of log shapes, for [`crate::search::SearchString`].
//!
//! Searching a log shape is expensive: it builds the shape's TNFA, intersects it with the query's
//! TNFA, and enumerates paths through the result. This module models a shape with a much coarser
//! object — a sequence of static text and per-placeholder *character sets* — so that a shape which
//! provably cannot match can be discarded before any of that work happens.
//!
//! Soundness rests on [`Charset`] being a **superset** of the characters a rule can emit: every
//! regex construct either contributes its exact alphabet or widens the set to "everything". A shape
//! rejected on this model therefore could never have matched, while a shape that is retained is only
//! "not ruled out".

pub mod align;
pub mod cache;
pub mod compose;
pub mod placement;
pub mod run_fit;
pub mod shape;

#[cfg(test)]
mod test;

pub use align::Alignment;
pub use align::Budget;
pub use align::Fragment;
pub use align::Outcome;
pub use align::align;
pub use align::can_match;
pub use cache::ShapeModelCache;
pub use compose::Capture;
pub use compose::ComposeBudget;
pub use compose::Composed;
pub use compose::Composition;
pub use compose::compose;
pub use placement::Placement;
pub use placement::PlacementTable;
pub use placement::Position;
pub use placement::Run;
pub use placement::can_compose;
pub use placement::runs_of;
pub use run_fit::RunFit;
pub use run_fit::RunFitCache;
pub use shape::Dispatch;
pub use shape::Placeholder;
pub use shape::ShapeModel;
pub use shape::ShapePart;

use crate::interval_tree::Interval;
use crate::interval_tree::IntervalTree;
use crate::interval_tree::PolicyNoop;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RuleInfo;
use crate::regex::Regex;

/// A set of characters, stored as disjoint intervals of Unicode scalar values.
///
/// Backed by [`IntervalTree`] rather than a bitmap for two reasons:
/// a bracketed range costs `O(1)` per range instead of one insert per character
/// (a range such as `[\x00-\x{10FFFF}]` would otherwise be over a million inserts),
/// and a *negated* range can be represented exactly via [`Interval::complement`]
/// instead of being widened to "every character".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Charset(IntervalTree<u32, ()>);

impl Default for Charset {
	fn default() -> Self {
		Self::empty()
	}
}

impl Charset {
	/// The empty set.
	#[must_use]
	pub const fn empty() -> Self {
		Self(IntervalTree::new())
	}

	/// The set of every character.
	#[must_use]
	pub fn all() -> Self {
		let mut this: Self = Self::empty();
		this.insert_interval(Self::universe());
		this
	}

	/// Whether the set is empty.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Whether every character is in the set.
	///
	/// A fast path for callers that can then skip per-character checks entirely.
	#[must_use]
	pub fn is_universal(&self) -> bool {
		// Note: [`IntervalTree`] does not coalesce adjacent intervals that happen to hold equal values,
		// so a universal set is not necessarily a single entry; check for contiguous coverage instead.
		// Entries are disjoint and ascending, so one pass suffices.
		let mut cursor: u32 = 0;
		for (interval, _) in self.0.iter() {
			if interval.start() > cursor {
				// A gap before `cursor` is never closed by a later (higher) interval.
				return false;
			}
			if interval.end() >= u32::from(char::MAX) {
				return true;
			}
			cursor = interval.end() + 1;
		}
		false
	}

	/// Adds `c` to the set.
	pub fn insert(&mut self, c: char) {
		self.insert_interval(Interval::new(u32::from(c), u32::from(c)));
	}

	/// Adds every scalar value in `interval` to the set.
	pub fn insert_interval(&mut self, interval: Interval<u32>) {
		self.0.insert(interval, (), PolicyNoop);
	}

	/// Adds every character of `other` to the set.
	pub fn union(&mut self, other: &Self) {
		for (interval, _) in other.0.iter() {
			self.insert_interval(interval);
		}
	}

	/// Whether `c` might be in the set.
	#[must_use]
	pub fn contains(&self, c: char) -> bool {
		self.0.lookup(u32::from(c)).is_some()
	}

	/// Every Unicode scalar value, as a single interval.
	///
	/// Matches the bound [`crate::nfa::Tnfa`] construction uses for [`Regex::AnyChar`],
	/// so that a universal charset and a wildcard transition agree.
	fn universe() -> Interval<u32> {
		Interval::new(0, u32::from(char::MAX))
	}
}

impl Charset {
	/// Adds every character that `regex` can emit.
	///
	/// Mirrors the structure of TNFA construction for each [`Regex`] variant, so that the result is
	/// exactly the alphabet of the symbol transitions the engine would build. Since only the
	/// *alphabet* matters, the repetition and grouping variants simply recurse: how many times an item
	/// repeats cannot introduce a character the item itself could not emit.
	pub fn add_regex(&mut self, regex: &Regex) {
		match regex {
			Regex::AnyChar => self.insert_interval(Self::universe()),
			Regex::Literal(c) => self.insert(*c),
			Regex::Capture(sub_rule) => self.add_regex(&sub_rule.regex),
			Regex::BracketedRanges { negated, items } => {
				let mut intervals: Vec<Interval<u32>> = items
					.iter()
					.map(|&(low, high)| Interval::new(u32::from(low), u32::from(high)))
					.collect::<Vec<_>>();
				if *negated {
					// The complement is taken over `u32`, so it also admits surrogates and values above
					// `char::MAX`, which the engine's own complement admits too. That only widens the
					// set (sound), and `contains` is only ever asked about a `char`.
					intervals = Interval::complement(&mut intervals);
				}
				for interval in intervals.into_iter() {
					self.insert_interval(interval);
				}
			},
			Regex::KleeneClosure(item) | Regex::KleenePlus(item) => self.add_regex(item),
			Regex::BoundedRepetition { item, .. } => self.add_regex(item),
			Regex::Sequence(items) | Regex::Alternation(items) => {
				for item in items.iter() {
					self.add_regex(item);
				}
			},
			Regex::Placeholder { item, .. } => self.add_regex(item),
		}
	}
}

/// A superset of the characters that any match of the (sub)rule(s) named `name` can contain.
///
/// An unresolved name yields the universal set, so that callers never prune on an unknown rule.
#[must_use]
pub fn charset_for_name(spec: &ParsingSpec, name: &str) -> Charset {
	let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(name);
	if rows.is_empty() {
		return Charset::all();
	}

	let mut charset: Charset = Charset::empty();
	for &(_, regex) in rows.iter() {
		charset.add_regex(regex);
		if charset.is_universal() {
			// Cannot be widened further; skip the remaining alternatives.
			break;
		}
	}
	charset
}

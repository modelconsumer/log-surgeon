//! Answers "can the rule(s) named `name` satisfy this piece of a query?" for one
//! `(name, piece, pinning)` triple, memoized across shapes.
//!
//! # Why this exists
//!
//! Searching a log shape by intersecting automata costs the size of the *whole shape*.
//! Real shapes are dominated by static text --
//! in the HDFS corpus, 403 shapes are 22KB of banner text wrapped around two small rules --
//! so that cost is paid on material that a plain substring search could handle.
//!
//! Placement instead asks the question *per rule*, per piece of the query:
//! can the rule begin with, end with, contain, or exactly match this piece?
//! That cost is the size of the rule (tens of states), not the shape (tens of thousands),
//! and the answer is **independent of the shape**,
//! so it is cached and reused by every shape that mentions the rule.
//! In the same corpus there are 14,501 rule references but only 111 distinct rule names.
//!
//! Each question is a search query against the rule: `*piece*`, `piece*`, `*piece`,
//! or `piece` -- so it is answered by
//! [`crate::search::SearchString::search_by_name`],
//! and the same four-pinning generalisation in [`Pinned`] is what every placement case asks.

#[cfg(test)]
mod test;

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::parsing_spec::ParsingSpec;
use crate::search::SearchString;

/// Which ends of the rule's match the piece pins.
///
/// A piece of a query pins an end it abuts:
/// the start when the query is anchored there or the preceding part runs
/// into this one, the end when the query is anchored there or the piece
/// straddles into what follows. A run wholly inside a rule pins neither.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pinned {
	/// The rule's match must *begin* with the piece.
	pub start: bool,
	/// The rule's match must *end* with the piece.
	pub end: bool,
}

impl Pinned {
	/// Wraps the *literal* piece in the wildcards that express the pinning.
	///
	/// The piece is escaped, so it round-trips through
	/// [`SearchString::parse`] as literal text even when it contains `*` or `\`.
	pub fn wrap(&self, piece: &str) -> String {
		let mut query: String = String::with_capacity(piece.len() + 2);
		if !self.start {
			query.push('*');
		}
		for c in piece.chars() {
			if matches!(c, '*' | '\\') {
				query.push('\\');
			}
			query.push(c);
		}
		if !self.end {
			query.push('*');
		}
		query
	}

	/// Whether both ends are pinned, i.e. the rule must match `piece` exactly.
	pub fn exact(&self) -> bool {
		self.start && self.end
	}
}

/// A memoized answer for one `(name, query)` pair, on the same cache as [`matches_piece`].
///
/// The query string is the cache key: escaped literals and wildcard-bearing
/// values cannot collide, because an escaped `\*` is never bare `*`.
#[derive(Debug, Default)]
pub struct RunFitCache {
	matched: Mutex<BTreeMap<(Box<str>, Box<str>), bool>>,
}

impl RunFitCache {
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Whether the rule(s) named `name` can match `query`, which may contain wildcards.
	///
	/// Used where one rule reference holds pieces of *several* runs.
	/// Placement validates a run at a time,
	/// but a rule that admits each run separately need not admit them together:
	/// an alternation such as `INFO|WARN` matches either alone and neither pair.
	/// `query` is the value that will be reported for the capture,
	/// so this asks precisely the question the reported answer claims.
	#[must_use]
	pub fn matches_query(&self, spec: &ParsingSpec, name: &str, query: &str) -> bool {
		self.get(spec, name, query)
	}

	/// Whether the rule(s) named `name` can match `piece`, pinned as `pinned`.
	///
	/// `piece` is literal text; it is escaped before being wrapped.
	#[must_use]
	pub fn matches_piece(
		&self,
		spec: &ParsingSpec,
		name: &str,
		piece: &str,
		pinned: Pinned,
	) -> bool {
		self.get(spec, name, &pinned.wrap(piece))
	}

	/// The memoized answer for `(name, query)`, computing on a miss.
	fn get(&self, spec: &ParsingSpec, name: &str, query: &str) -> bool {
		let key: (Box<str>, Box<str>) = (Box::from(name), Box::from(query));

		if let Some(&cached) = self.matched.lock().unwrap().get(&key) {
			return cached;
		}

		// The lock is released while computing, so a slow simulation does not block other keys.
		// Two threads racing on the same key may both compute it; that is wasted work,
		// not a correctness problem, and is cheaper than holding the lock.
		let matches: bool = SearchString::parse(query)
			.map(|parsed| !parsed.search_by_name(spec, name).is_empty())
			.unwrap_or(false);

		self.matched.lock().unwrap().insert(key, matches);

		matches
	}

	/// The number of cached `(name, query)` pairs.
	#[must_use]
	pub fn len(&self) -> usize {
		self.matched.lock().unwrap().len()
	}

	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.matched.lock().unwrap().is_empty()
	}

	pub fn clear(&self) {
		self.matched.lock().unwrap().clear();
	}
}

impl Clone for RunFitCache {
	fn clone(&self) -> Self {
		Self {
			matched: Mutex::new(self.matched.lock().unwrap().clone()),
		}
	}
}

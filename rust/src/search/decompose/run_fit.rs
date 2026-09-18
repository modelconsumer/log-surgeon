//! Simulates a run of query text against a single rule, independently of any log shape.
//!
//! # Why this exists
//!
//! Searching a log shape by intersecting automata costs the size of the *whole shape*. Real shapes are
//! dominated by static text — in the HDFS corpus, 403 shapes are 22KB of banner text wrapped around two
//! small rules — so that cost is paid on material that a plain substring search could handle.
//!
//! This module instead asks the question *per rule*: "can this run of literal text sit inside this
//! rule, and if so how?" That cost is the size of the rule (tens of states), not the shape (tens of
//! thousands), and the answer is **independent of the shape**, so it can be cached and reused by every
//! shape that mentions the rule. In the same corpus there are 14,501 rule references but only 111
//! distinct rule names: simulating a run against all 111 takes ~4ms, against the shapes ~8s.
//!
//! # What a run can do
//!
//! A query is a sequence of literal runs separated by wildcards. Given a shape, each run must be
//! produced somewhere, and there are only three possibilities:
//!
//! 1. wholly inside one rule ([`RunFit::whole`]);
//! 2. wholly inside the shape's static text (a substring search, so not this module's concern);
//! 3. straddling a boundary, split between a rule and the text (or rule) beside it
//!    ([`RunFit::suffixes`] and [`RunFit::prefixes`]).
//!
//! For (3) the rule contributes either a *trailing* part of the run (the rule ends where the run
//! continues into what follows) or a *leading* part (the run runs into the start of the rule), so this
//! module records, for every split point, whether the rule can end with / begin with that piece.

#[cfg(test)]
mod test;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::parsing_spec::ParsingSpec;
use crate::search::Interpretation;
use crate::search::SearchString;

/// How a single run of literal text can be placed inside one rule.
///
/// "Run" here means a maximal stretch of literal characters from the query, with no wildcards.
#[derive(Clone, Debug)]
pub struct RunFit {
	/// Interpretations for the run sitting wholly inside the rule, as `*run*`.
	///
	/// Empty iff the rule cannot contain the run anywhere.
	pub whole: Vec<Interpretation>,
	/// `suffixes[k]` is set iff the rule can *end* with `run[..k]`.
	///
	/// Used where the run starts inside the rule and continues past its end, so the rule supplies the
	/// run's first `k` characters. Indexed by character count, `0..=run.len()`; index `0` means the rule
	/// contributes nothing, which is always possible.
	pub suffixes: Vec<bool>,
	/// `prefixes[k]` is set iff the rule can *begin* with `run[k..]`.
	///
	/// Used where the run ends inside the rule, so the rule supplies the run's last `len - k`
	/// characters. Indexed the same way; index `run.len()` means the rule contributes nothing.
	pub prefixes: Vec<bool>,
}

impl RunFit {
	/// Whether the run cannot interact with this rule at all.
	///
	/// When true, the rule can neither contain the run nor supply any non-empty part of it, so no
	/// placement of the run can involve this rule.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.whole.is_empty() && !self.has_partial()
	}

	/// Whether the rule can supply a non-empty proper part of the run.
	#[must_use]
	pub fn has_partial(&self) -> bool {
		// Index `0` of `suffixes` and the last index of `prefixes` are the trivial "contributes nothing"
		// cases, so they do not count as a partial fit.
		self.suffixes.iter().skip(1).any(|&fits| fits) || self.prefixes.iter().rev().skip(1).any(|&fits| fits)
	}

	/// Whether the run fits wholly inside the rule.
	#[must_use]
	pub fn fits_wholly(&self) -> bool {
		!self.whole.is_empty()
	}

	/// Computes how `run` can be placed inside the rule(s) named `name`.
	#[must_use]
	pub fn compute(spec: &ParsingSpec, name: &str, run: &str) -> Self {
		let characters: Vec<char> = run.chars().collect::<Vec<_>>();

		// The run wholly inside the rule.
		let whole: Vec<Interpretation> = match SearchString::parse(&escape_for_query(run)) {
			// `*run*`: the run may sit anywhere within the rule's match.
			Ok(_) => Self::interpretations_for(spec, name, &format!("*{}*", escape_for_query(run))),
			Err(_) => Vec::new(),
		};

		// Partial fits. `run[..k]` as a suffix of the rule means a query of `*run[..k]`, which (with the
		// engine's prefix semantics) asks for a match *ending* with that text. Symmetrically `run[k..]`
		// as a prefix of the rule is `run[k..]*`.
		let mut suffixes: Vec<bool> = vec![false; characters.len() + 1];
		let mut prefixes: Vec<bool> = vec![false; characters.len() + 1];
		// The empty contribution is always available.
		suffixes[0] = true;
		prefixes[characters.len()] = true;

		for k in 1..=characters.len() {
			let head: String = characters[..k].iter().collect::<String>();
			suffixes[k] = !Self::interpretations_for(spec, name, &format!("*{}", escape_for_query(&head))).is_empty();
		}
		for k in 0..characters.len() {
			let tail: String = characters[k..].iter().collect::<String>();
			prefixes[k] = !Self::interpretations_for(spec, name, &format!("{}*", escape_for_query(&tail))).is_empty();
		}

		Self {
			whole,
			suffixes,
			prefixes,
		}
	}

	pub(crate) fn interpretations_for(spec: &ParsingSpec, name: &str, query: &str) -> Vec<Interpretation> {
		match SearchString::parse(query) {
			Ok(parsed) => parsed.search_by_name(spec, name),
			// A run that cannot be expressed as a query cannot be reasoned about; treat as no fit, which
			// callers must then treat as "unknown" rather than a rejection.
			Err(_) => Vec::new(),
		}
	}
}

/// Escapes the characters that [`SearchString::parse`] treats specially, so `run` is matched literally.
fn escape_for_query(run: &str) -> String {
	let mut escaped: String = String::with_capacity(run.len());
	for c in run.chars() {
		if matches!(c, '*' | '\\') {
			escaped.push('\\');
		}
		escaped.push(c);
	}
	escaped
}

/// A cache of [`RunFit`]s, keyed by `(rule name, run)`.
///
/// This is where the algorithm's leverage comes from: a shape corpus mentions few distinct rules
/// relative to the number of rule *references*, and a query has few runs, so the number of distinct
/// simulations is tiny compared to the number of (shape, rule) pairs.
#[derive(Debug, Default)]
pub struct RunFitCache {
	fits: Mutex<BTreeMap<(Box<str>, Box<str>), Arc<RunFit>>>,
	/// Cache for [`Self::matches_exactly`].
	///
	/// Kept separate from `fits` because an exact-match question needs a *single* simulation, whereas a
	/// full [`RunFit`] needs one per split point. Middle-of-run placement asks the exact question for
	/// many short substrings, so conflating the two would multiply the cost by the run length.
	exact: Mutex<BTreeMap<(Box<str>, Box<str>), bool>>,
}

impl RunFitCache {
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// The fit for `run` against the rule(s) named `name`, computing and caching it on a miss.
	#[must_use]
	pub fn get(&self, spec: &ParsingSpec, name: &str, run: &str) -> Arc<RunFit> {
		let key: (Box<str>, Box<str>) = (Box::from(name), Box::from(run));

		if let Some(cached) = self.fits.lock().unwrap().get(&key) {
			return cached.clone();
		}

		// Note the lock is released while computing, so a slow simulation does not block other keys. Two
		// threads racing on the same key may both compute it; that is wasted work, not a correctness
		// problem, and is cheaper than holding the lock across the simulation.
		let fit: Arc<RunFit> = Arc::new(RunFit::compute(spec, name, run));

		self.fits.lock().unwrap().insert(key, fit.clone());

		fit
	}

	/// Whether the rule(s) named `name` can match `text` exactly, with nothing before or after.
	///
	/// Cheaper than [`Self::get`] — one simulation rather than one per split point — and cached
	/// separately, because placing a rule in the *middle* of a run asks this for many short substrings.
	#[must_use]
	pub fn matches_exactly(&self, spec: &ParsingSpec, name: &str, text: &str) -> bool {
		let key: (Box<str>, Box<str>) = (Box::from(name), Box::from(text));

		if let Some(&cached) = self.exact.lock().unwrap().get(&key) {
			return cached;
		}

		let matches: bool = !RunFit::interpretations_for(spec, name, &escape_for_query(text)).is_empty();

		self.exact.lock().unwrap().insert(key, matches);

		matches
	}

	/// Whether the rule(s) named `name` can match `query`, which may contain wildcards.
	///
	/// Used where one rule reference holds pieces of *several* runs. Placement validates a run at a time,
	/// but a rule that admits each run separately need not admit them together: an alternation such as
	/// `INFO|WARN` matches either alone and neither pair. `query` is the value that will be reported for
	/// the capture, so this asks precisely the question the reported answer claims.
	///
	/// Shares the [`Self::matches_exactly`] cache, which is keyed by the query string; a wildcard-bearing
	/// query cannot collide with the escaped literals stored there.
	#[must_use]
	pub fn can_produce_all_text(&self, spec: &ParsingSpec, name: &str, query: &str) -> bool {
		let key: (Box<str>, Box<str>) = (Box::from(name), Box::from(query));

		if let Some(&cached) = self.exact.lock().unwrap().get(&key) {
			return cached;
		}

		let matches: bool = !RunFit::interpretations_for(spec, name, query).is_empty();

		self.exact.lock().unwrap().insert(key, matches);

		matches
	}

	/// The number of cached `(rule, run)` pairs.
	#[must_use]
	pub fn len(&self) -> usize {
		self.fits.lock().unwrap().len()
	}

	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.fits.lock().unwrap().is_empty()
	}

	pub fn clear(&self) {
		self.fits.lock().unwrap().clear();
		self.exact.lock().unwrap().clear();
	}
}

impl Clone for RunFitCache {
	fn clone(&self) -> Self {
		Self {
			fits: Mutex::new(self.fits.lock().unwrap().clone()),
			exact: Mutex::new(self.exact.lock().unwrap().clone()),
		}
	}
}

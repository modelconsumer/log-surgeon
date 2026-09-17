//! A coarse model of a log shape.
//!
//! A log shape is a sequence of static text and references to rules (placeholders). [`ShapeModel`]
//! keeps that structure, pairing each placeholder with a [`Charset`] bounding the characters the rule
//! can emit, plus the sub-rule(s) needed to report a capture.
//!
//! The model is built from [`ParsingSpec::split_log_shape`], the same tokenizer
//! [`ParsingSpec::automata_for_shape`] uses, so the model and the automaton can never disagree about
//! where the placeholders are.
//!
//! # Preconditions
//!
//! A shape is expected to be *decomposable*: every placeholder must name a rule the spec defines, and
//! every such rule must be a **leaf** (no nested captures). Both are asserted when the model is built,
//! so callers never have to ask whether a shape can be decomposed — the type says it can. Violations
//! are programming errors in the shape, not conditions to recover from; see [`ShapeModel::new`].

#[cfg(test)]
mod test;

use std::num::NonZero;
use std::sync::Arc;

use crate::parsing_spec::LogShapeFragment;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::RuleInfo;
use crate::parsing_spec::SubRule;
use crate::prefilter::Charset;
use crate::regex::Regex;

/// One token of a log shape.
#[derive(Clone, Debug)]
pub enum ShapePart {
	/// Fixed text, which a match must reproduce verbatim.
	Static(String),
	/// A reference to a rule, standing for any string that rule can match.
	Placeholder(Placeholder),
}

impl ShapePart {
	/// Whether this part is a rule reference.
	#[must_use]
	pub fn is_placeholder(&self) -> bool {
		matches!(self, Self::Placeholder(_))
	}

	/// Whether this part can emit nothing at all.
	///
	/// Static text is never empty (the tokenizer does not emit empty fragments), so only a nullable
	/// placeholder can.
	#[must_use]
	pub fn can_be_empty(&self) -> bool {
		match self {
			Self::Placeholder(placeholder) => placeholder.can_match_empty,
			Self::Static(text) => text.is_empty(),
		}
	}

	/// The rule name this part references, if it is a reference at all.
	#[must_use]
	pub fn placeholder_name(&self) -> Option<&str> {
		match self {
			Self::Placeholder(placeholder) => Some(&placeholder.name),
			Self::Static(_) => None,
		}
	}
}

/// A rule reference in a log shape.
#[derive(Clone, Debug)]
pub struct Placeholder {
	/// The name as written in the shape, e.g. `kv.key`.
	pub name: String,
	/// A superset of the characters any match of this placeholder can contain.
	pub charset: Charset,
	/// The alternatives this name resolves to, each with the sub-rule to attribute a capture to.
	///
	/// A name can resolve to several rules (same name, different definitions), so a single
	/// placeholder may be reported as any one of them.
	pub alternatives: Vec<Arc<SubRule>>,
	/// Whether some alternative can match the empty string.
	///
	/// A nullable placeholder can stand entirely aside, so a part *before* it can still be the first
	/// thing a message emits, and a part *after* it can still be the last. Anchoring therefore cannot
	/// simply demand the first or last shape part; see [`ShapeModel::can_start_at`].
	pub can_match_empty: bool,
}

/// A log shape, modelled as a sequence of [`ShapePart`]s.
///
/// Every placeholder has been resolved and checked to be a leaf rule; see [`Self::new`].
#[derive(Clone, Debug)]
pub struct ShapeModel {
	pub parts: Vec<ShapePart>,
}

impl ShapeModel {
	/// Builds the model for `shape`.
	///
	/// # Panics
	///
	/// Panics if a placeholder names a rule the spec does not define, or names a rule with nested
	/// captures (i.e. one that is not a leaf). Both make the shape unsupported for direct
	/// decomposition, and continuing past them would silently produce a different answer than the
	/// engine — a non-leaf rule must be reported as its nested captures, which this model does not
	/// carry — so they are treated as errors in the shape rather than as conditions to recover from.
	#[must_use]
	pub fn new(spec: &ParsingSpec, shape: &str) -> Self {
		let mut parts: Vec<ShapePart> = Vec::new();

		for fragment in spec.split_log_shape(shape).into_iter() {
			match fragment {
				LogShapeFragment::Text(text) => {
					parts.push(ShapePart::Static(text));
				},
				LogShapeFragment::Rule(name) => {
					parts.push(ShapePart::Placeholder(Placeholder::new(spec, name, shape)));
				},
			}
		}

		Self { parts }
	}

	/// The fragments for parts `start..=end`, for rebuilding a *slice* of the shape's automaton.
	///
	/// Round-trips through [`LogShapeFragment`] rather than re-tokenizing the shape string, so the parts
	/// the automaton is built from are exactly the parts the placement reasoned about.
	#[must_use]
	pub fn fragments_in(&self, start: usize, end: usize) -> Vec<LogShapeFragment> {
		Vec::from_iter(self.parts[start..=end].iter().map(|part| match part {
			ShapePart::Static(text) => LogShapeFragment::Text(text.clone()),
			ShapePart::Placeholder(placeholder) => LogShapeFragment::Rule(placeholder.name.clone()),
		}))
	}

	/// Whether a message of this shape can *begin* with the text part `part` emits.
	///
	/// True when every earlier part can emit nothing at all. Static text is never empty — the tokenizer
	/// does not produce empty fragments — so only nullable placeholders can stand aside. This is what a
	/// start-anchored run needs: it must be the first thing in the message, which does not require it to
	/// be in the first *part* if the parts before it can vanish.
	#[must_use]
	pub fn can_start_at(&self, part: usize) -> bool {
		self.parts[..part].iter().all(ShapePart::can_be_empty)
	}

	/// Whether a message of this shape can *end* with the text part `part` emits.
	///
	/// The mirror of [`Self::can_start_at`]: every later part must be able to emit nothing.
	#[must_use]
	pub fn can_end_at(&self, part: usize) -> bool {
		self.parts[(part + 1)..].iter().all(ShapePart::can_be_empty)
	}

	/// The placeholders of this shape, in order.
	pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
		self.parts.iter().filter_map(|part| match part {
			ShapePart::Placeholder(placeholder) => Some(placeholder),
			ShapePart::Static(_) => None,
		})
	}

	/// The number of placeholders.
	#[must_use]
	pub fn num_placeholders(&self) -> usize {
		self.placeholders().count()
	}
}

impl Placeholder {
	/// Resolves `name` against `spec`.
	///
	/// # Panics
	///
	/// Panics if `name` resolves to no rule, or to a rule with nested captures. See
	/// [`ShapeModel::new`].
	#[must_use]
	fn new(spec: &ParsingSpec, name: String, shape: &str) -> Self {
		let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(&name);
		assert!(
			!rows.is_empty(),
			"log shape {shape:?} references undefined rule {name:?}"
		);

		let mut charset: Charset = Charset::empty();
		let mut alternatives: Vec<Arc<SubRule>> = Vec::with_capacity(rows.len());
		let mut can_match_empty: bool = false;

		for &(info, regex) in rows.iter() {
			charset.add_regex(regex);
			// Conservative in the direction that keeps anchoring sound: if *any* alternative is nullable,
			// the placeholder is treated as able to stand aside.
			can_match_empty |= regex.is_nullable().is_some();

			if let Some(sub_rule) = &info.maybe_sub_rule {
				assert!(
					sub_rule.is_leaf(),
					"log shape {shape:?} references non-leaf rule {name:?}: a rule with nested \
					 captures cannot be decomposed and must be referenced by one of its leaf captures \
					 instead (e.g. `%{name}.<capture>%`). Note this applies to log shapes; \
					 `search_by_name` accepts non-leaf names"
				);
				alternatives.push(sub_rule.clone());
			} else {
				// A root rule reference. `automata_for_shape` wraps a capture-less root rule in an
				// implicit capture of the whole rule; mirror that here so a decomposition can name it.
				let root: &crate::parsing_spec::RootRule = &spec[info.root_idx];
				assert!(
					!root.has_captures(),
					"log shape {shape:?} references non-leaf rule {name:?}: a rule with nested \
					 captures cannot be decomposed and must be referenced by one of its leaf captures \
					 instead. Note this applies to log shapes; `search_by_name` accepts non-leaf names"
				);
				alternatives.push(Arc::new(SubRule {
					name: info.root_name.clone(),
					regex: regex.clone(),
					root_rule_idx: info.root_idx,
					// Matches the placeholder `automata_for_shape` uses for an implicit root capture.
					id: NonZero::<u16>::MAX,
					parent_id: None,
					descendants: 0,
					qualified_name: info.root_name.clone(),
					fully_qualified_name: info.root_name.clone(),
				}));
			}
		}

		Self {
			name,
			charset,
			alternatives,
			can_match_empty,
		}
	}
}

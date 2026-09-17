//! A coarse model of a log shape, and the decision of how to search it.
//!
//! A log shape is a sequence of static text and references to rules (placeholders). [`ShapeModel`]
//! keeps that structure, pairing each placeholder with a [`Charset`] bounding the characters the rule
//! can emit, plus the sub-rule(s) needed to report a capture.
//!
//! The model is built from [`ParsingSpec::split_log_shape`], the same tokenizer
//! [`ParsingSpec::automata_for_shape`] uses, so the model and the automaton can never disagree about
//! where the placeholders are.

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
	/// Whether every alternative is a "leaf": a rule with no nested captures.
	///
	/// Only leaf placeholders can be decomposed by this module; a nested capture would require
	/// reporting the inner structure too, which is left to the engine.
	pub is_leaf: bool,
}

/// A log shape, modelled as a sequence of [`ShapePart`]s.
#[derive(Clone, Debug)]
pub struct ShapeModel {
	pub parts: Vec<ShapePart>,
}

/// How a shape should be searched.
///
/// Computing this is cheap; it decides whether the expensive engine path is needed at all.
#[derive(Clone, Debug)]
pub enum Dispatch {
	/// Proved that no message of this shape can match the query. No further work is needed.
	Rejected,
	/// The shape can be decomposed directly against the query's fixed text.
	Decompose(ShapeModel),
	/// Fall back to building the shape's TNFA and intersecting it with the query.
	Engine,
}

impl ShapeModel {
	/// Builds the model for `shape`.
	///
	/// Returns `None` if a placeholder names a rule the spec does not define, mirroring
	/// [`ParsingSpec::automata_for_shape`]'s failure so callers defer to the engine (which reports the
	/// error) rather than pruning on an unknown rule.
	#[must_use]
	pub fn new(spec: &ParsingSpec, shape: &str) -> Option<Self> {
		let mut parts: Vec<ShapePart> = Vec::new();

		for fragment in spec.split_log_shape(shape).into_iter() {
			match fragment {
				LogShapeFragment::Text(text) => {
					parts.push(ShapePart::Static(text));
				},
				LogShapeFragment::Rule(name) => {
					parts.push(ShapePart::Placeholder(Placeholder::new(spec, name)?));
				},
			}
		}

		Some(Self { parts })
	}

	/// The placeholders of this shape, in order.
	pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
		self.parts.iter().filter_map(|part| match part {
			ShapePart::Placeholder(placeholder) => Some(placeholder),
			ShapePart::Static(_) => None,
		})
	}

	/// Whether every placeholder resolves only to leaf rules.
	#[must_use]
	pub fn all_placeholders_are_leaves(&self) -> bool {
		self.placeholders().all(|placeholder| placeholder.is_leaf)
	}

	/// The number of placeholders.
	#[must_use]
	pub fn num_placeholders(&self) -> usize {
		self.placeholders().count()
	}

	/// Whether this model supports decomposition.
	///
	/// A shape with no placeholders is pure static text: there is nothing to attribute the query's
	/// fixed text to, so such a shape belongs to the engine. (Treating it as decomposable is what made
	/// the previous implementation silently drop matches for literal-only shapes.)
	#[must_use]
	pub fn can_decompose(&self) -> bool {
		(self.num_placeholders() > 0) && self.all_placeholders_are_leaves()
	}
}

impl Placeholder {
	/// Resolves `name` against `spec`.
	///
	/// Returns `None` if the name resolves to no rule at all.
	#[must_use]
	fn new(spec: &ParsingSpec, name: String) -> Option<Self> {
		let rows: Vec<(&RuleInfo, &Regex)> = spec.rules_for_name(&name);
		if rows.is_empty() {
			return None;
		}

		let mut charset: Charset = Charset::empty();
		let mut alternatives: Vec<Arc<SubRule>> = Vec::with_capacity(rows.len());
		let mut is_leaf: bool = true;

		for &(info, regex) in rows.iter() {
			charset.add_regex(regex);

			if let Some(sub_rule) = &info.maybe_sub_rule {
				is_leaf &= sub_rule.is_leaf();
				alternatives.push(sub_rule.clone());
			} else {
				// A root rule reference. `automata_for_shape` wraps a capture-less root rule in an
				// implicit capture of the whole rule; mirror that here so a decomposition can name it.
				// A root rule that *has* captures is not a leaf, and is left to the engine.
				let root: &crate::parsing_spec::RootRule = &spec[info.root_idx];
				is_leaf &= !root.has_captures();
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

		Some(Self {
			name,
			charset,
			alternatives,
			is_leaf,
		})
	}
}

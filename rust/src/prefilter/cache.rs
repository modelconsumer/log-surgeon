//! A cache of [`ShapeModel`]s, keyed by log shape.
//!
//! Building a model walks the whole shape and resolves every placeholder against the spec. Shapes are
//! long (thousands of characters is normal) and a caller typically searches the same set of shapes
//! repeatedly, so rebuilding per query dominates the prefilter's cost. The cache makes it a
//! once-per-shape cost instead.
//!
//! Uses interior mutability so it can sit behind a shared reference on a long-lived owner such as
//! [`crate::parser::Parser`], and so that filling it does not require `&mut` on the search path.

#[cfg(test)]
mod test;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::parsing_spec::ParsingSpec;
use crate::prefilter::ShapeModel;

/// A shape-keyed cache of prefilter models.
///
/// A cached entry of `None` records that the shape has no model (it names an undefined rule), so that
/// a failing shape is not re-resolved on every query either.
#[derive(Debug, Default)]
pub struct ShapeModelCache {
	models: Mutex<BTreeMap<Box<str>, Option<Arc<ShapeModel>>>>,
}

impl ShapeModelCache {
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// The model for `shape`, building and caching it on a miss.
	///
	/// `None` means the shape names a rule the spec does not define; see [`ShapeModel::new`].
	#[must_use]
	pub fn get(&self, spec: &ParsingSpec, shape: &str) -> Option<Arc<ShapeModel>> {
		// Note the lock is released before building, so a slow build does not block other shapes. Two
		// threads racing on the same missing shape may both build it; that is wasted work but not a
		// correctness problem, and is cheaper than holding the lock across the build.
		if let Some(cached) = self.models.lock().unwrap().get(shape) {
			return cached.clone();
		}

		let model: Option<Arc<ShapeModel>> = ShapeModel::new(spec, shape).map(Arc::new);

		self.models.lock().unwrap().insert(Box::from(shape), model.clone());

		model
	}

	/// The number of cached shapes.
	#[must_use]
	pub fn len(&self) -> usize {
		self.models.lock().unwrap().len()
	}

	/// Whether nothing is cached yet.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.models.lock().unwrap().is_empty()
	}

	/// Drops every cached model.
	pub fn clear(&self) {
		self.models.lock().unwrap().clear();
	}
}

impl Clone for ShapeModelCache {
	/// Clones the cached entries.
	///
	/// [`crate::parser::Parser`] is [`Clone`], and the models are behind [`Arc`], so this shares the
	/// built models rather than rebuilding them.
	fn clone(&self) -> Self {
		Self {
			models: Mutex::new(self.models.lock().unwrap().clone()),
		}
	}
}

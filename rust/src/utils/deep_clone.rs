use std::sync::Arc;

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeepClone<T>(T);

impl<T> DeepClone<T> {
	pub const fn new(inner: T) -> Self {
		Self(inner)
	}

	/// Forsaken orphan rules prevent `impl<T> From<DeepClone<T>> for T`.
	pub fn into_inner(self) -> T {
		self.0
	}
}

impl<T> From<T> for DeepClone<T> {
	fn from(inner: T) -> Self {
		Self::new(inner)
	}
}

impl<T> std::ops::Deref for DeepClone<T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl<T> std::ops::DerefMut for DeepClone<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

impl<U> Clone for DeepClone<Arc<U>>
where
	U: Clone,
{
	fn clone(&self) -> Self {
		Self(Arc::new((*self.0).clone()))
	}
}

#[cfg(test)]
mod test;

/// A naive interval "tree" implementation;
/// only constructed when building NFAs,
/// so we optimize for simplicity (correctness) and lookups.
///
/// Internally, just an ordered list of non-overlapping intervals;
/// lookups are `O(log(n))` with binary search.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntervalTree<T: Number, V: Clone> {
	intervals: Vec<(Interval<T>, V)>,
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Interval<T: Number> {
	start: T,
	end: T,
}

// XXX: use [`std::iter::Step`] when stable?
pub trait Number: Ord + std::fmt::Debug + Copy {
	const MIN: Self;
	const MAX: Self;

	fn up(&self) -> Self;
	fn down(&self) -> Self;
}

/// When inserting a value with an overlapping interval into an [`IntervalTree`],
/// a `Policy` determines how the existing and new values are combined.
pub trait Policy<T> {
	fn combine(&mut self, existing: &mut T, new: T);
}

/// Combine values using the [`Extend`] trait; e.g. for containers.
#[derive(Debug)]
pub struct PolicyExtend;

/// Combine values using the [`Add`] trait; e.g. for numbers.
#[derive(Debug)]
pub struct PolicyAdd;

/// Do nothing; for the unit type/object `()`.
#[derive(Debug)]
pub struct PolicyNoop;

/// Panics if overlapping intervals are inserted.
#[derive(Debug)]
pub struct PolicyUnique;

/// Use an arbitrary function to combine values.
#[derive(Debug)]
pub struct PolicyFunction<T>(T);

impl<T: Number, V: Clone> IntervalTree<T, V> {
	pub const fn new() -> Self {
		Self { intervals: Vec::new() }
	}

	pub fn len(&self) -> usize {
		self.intervals.len()
	}

	pub fn is_empty(&self) -> bool {
		self.intervals.is_empty()
	}
}

impl<T: Number, V: Clone> IntervalTree<T, V> {
	pub fn iter(&self) -> impl Iterator<Item = (Interval<T>, &V)> {
		self.intervals.iter().map(|(interval, value)| (*interval, value))
	}

	pub fn iter_mut(&mut self) -> impl Iterator<Item = (Interval<T>, &mut V)> {
		self.intervals.iter_mut().map(|(interval, value)| (*interval, value))
	}
}

impl<T: Number, V: Clone, P> FromIterator<(Interval<T>, V, P)> for IntervalTree<T, V>
where
	P: Policy<V>,
{
	fn from_iter<I>(iter: I) -> Self
	where
		I: IntoIterator<Item = (Interval<T>, V, P)>,
	{
		let mut this: Self = Self::new();
		for (interval, value, policy) in iter.into_iter() {
			this.insert(interval, value, policy);
		}
		this
	}
}

impl<T: Number, V: Clone> IntervalTree<T, V> {
	/// Lookup the value associated with the interval containing `pos` (if any).
	pub fn lookup(&self, pos: T) -> Option<&V> {
		self.lookup_entry(pos).map(|(_, value)| value)
	}

	/// Lookup the exact interval containing `pos` (if any).
	pub fn lookup_interval(&self, pos: T) -> Option<Interval<T>> {
		self.lookup_entry(pos).map(|(interval, _)| interval)
	}

	/// Insert a new value for the given interval.
	/// The `policy` determines how to merge values where the new interval overlaps with existing intervals.
	pub fn insert<P>(&mut self, new: Interval<T>, new_value: V, mut policy: P)
	where
		P: Policy<V>,
	{
		// This is the same as `self.partition_point(new.start)`,
		// but we write it out to make the symmetry clear with `first_disjoint_after`.
		let first_overlap_before: usize = self.intervals.partition_point(|(interval, _)| interval.end < new.start);
		let first_disjoint_after: usize = self
			.intervals
			.partition_point(|(interval, _)| interval.start <= new.end);

		let mut replacement: Vec<(Interval<T>, V)> =
			Vec::with_capacity((3 * (first_disjoint_after - first_overlap_before)) + 1);
		let mut cursor: T = new.start;
		let mut covered: bool = false;

		for (existing, existing_value) in self.intervals.drain(first_overlap_before..first_disjoint_after) {
			if cursor < existing.start {
				replacement.push((Interval::new(cursor, existing.start.down()), new_value.clone()));
				cursor = existing.start;
			} else if existing.start < cursor {
				replacement.push((Interval::new(existing.start, cursor.down()), existing_value.clone()));
			}

			let overlap_end: T = std::cmp::min(existing.end, new.end);
			let mut merged: V = existing_value.clone();
			policy.combine(&mut merged, new_value.clone());
			replacement.push((Interval::new(cursor, overlap_end), merged));

			if overlap_end < existing.end {
				replacement.push((Interval::new(overlap_end.up(), existing.end), existing_value));
			}

			if overlap_end == new.end {
				covered = true;
			} else {
				cursor = overlap_end.up();
			}
		}

		if !covered {
			replacement.push((Interval::new(cursor, new.end), new_value));
		}

		self.intervals
			.splice(first_overlap_before..first_overlap_before, replacement);
		#[cfg(debug_assertions)]
		self.check_invariants();
	}

	/// Retain entries satisfying `predicate`.
	pub fn retain<P>(&mut self, predicate: P)
	where
		P: FnMut(&(Interval<T>, V)) -> bool,
	{
		self.intervals.retain(predicate);
	}
}

impl<T: Number, V: Clone> IntervalTree<T, V> {
	/// Lookup the entry for the interval containing `pos`.
	fn lookup_entry(&self, pos: T) -> Option<(Interval<T>, &V)> {
		let index: usize = self.partition_point(pos);
		let (interval, value): &(Interval<T>, V) = self.intervals.get(index)?;
		assert!(pos <= interval.end);
		if interval.start <= pos {
			Some((*interval, value))
		} else {
			None
		}
	}

	/// Informally, returns the location of the first interval that "goes past" `pos`.
	/// If `pos` is "past" every interval, returns `self.intervals.len()`.
	/// Otherwise, `self.intervals[index].end >= pos`.
	fn partition_point(&self, pos: T) -> usize {
		#[cfg(debug_assertions)]
		self.check_invariants();
		// `partition_point` assumes partitioning as `[true, ..., false]` and returns the index of the first `false`.
		self.intervals.partition_point(|(interval, _)| interval.end < pos)
	}

	/// Checks that intervals are non-overlapping.
	#[cfg_attr(feature = "debug_assertions", allow(unused))]
	fn check_invariants(&self) {
		let mut maybe_previous: Option<T> = None;
		for (interval, _) in self.intervals.iter() {
			assert!(interval.start() <= interval.end());
			if let Some(previous) = maybe_previous {
				assert!(interval.start() > previous);
			}
			maybe_previous = Some(interval.end());
		}
	}
}

impl<T: Number> Interval<T> {
	pub fn new(start: T, end: T) -> Self {
		assert!(start <= end);
		Self { start, end }
	}
}

impl<T: Number> Interval<T> {
	pub fn start(&self) -> T {
		self.start
	}

	pub fn end(&self) -> T {
		self.end
	}

	pub fn contains(&self, other: &Interval<T>) -> bool {
		(self.start <= other.start) && (other.end <= self.end)
	}

	pub fn complement(intervals: &mut [Interval<T>]) -> Vec<Interval<T>> {
		intervals.sort_unstable();

		let mut complement: Vec<Interval<T>> = Vec::new();

		let mut cursor: T = T::MIN;
		for &Interval { start, end } in intervals.iter() {
			if end < cursor {
				continue;
			}
			if cursor < start {
				complement.push(Interval::new(cursor, start.down()));
			}
			if end == T::MAX {
				return complement;
			}
			cursor = end.up();
		}
		complement.push(Interval::new(cursor, T::MAX));

		complement
	}

	pub fn overlap(&self, other: &Self) -> Option<Self> {
		let start: T = std::cmp::max(self.start, other.start);
		let end: T = std::cmp::min(self.end, other.end);
		if start <= end {
			Some(Interval::new(start, end))
		} else {
			None
		}
	}
}

macro_rules! number_impl {
	($ty:ty, $($tt:tt)*) => {
		number_impl!($ty);
		number_impl!($($tt)*);
	};
	($ty:ty) => {
		impl Number for $ty {
			const MIN: Self = <$ty>::MIN;
			const MAX: Self = <$ty>::MAX;

			#[track_caller]
			fn up(&self) -> Self {
				self.checked_add(1).unwrap()
			}

			#[track_caller]
			fn down(&self) -> Self {
				self.checked_sub(1).unwrap()
			}
		}
	};
}

number_impl!(u8, u16, u32, u64, usize);
number_impl!(i8, i16, i32, i64, isize);

impl<T> Policy<T> for PolicyExtend
where
	T: IntoIterator + Extend<<T as IntoIterator>::Item>,
{
	fn combine(&mut self, existing: &mut T, new: T) {
		existing.extend(new);
	}
}

impl<T> Policy<T> for PolicyAdd
where
	T: std::ops::Add<Output = T> + Copy,
{
	fn combine(&mut self, existing: &mut T, new: T) {
		*existing = *existing + new;
	}
}

impl Policy<()> for PolicyNoop {
	fn combine(&mut self, _existing: &mut (), _new: ()) {}
}

impl<T> Policy<T> for PolicyUnique
where
	T: Eq + std::fmt::Debug,
{
	fn combine(&mut self, existing: &mut T, new: T) {
		assert_eq!(&new, existing);
	}
}

/// Ideally, we could implement `Policy<T>` on `F: FnMut(&mut T, T)` directly,
/// but we can't due to a limitation on Rust's trait solver.
/// Further, we need this helper function to make the trait bounds resolve properly.
impl<F> PolicyFunction<F> {
	pub fn new<T>(f: F) -> Self
	where
		F: for<'a> FnMut(&'a mut T, T),
	{
		Self(f)
	}
}

impl<T, F> Policy<T> for PolicyFunction<F>
where
	F: FnMut(&mut T, T),
{
	fn combine(&mut self, existing: &mut T, new: T) {
		self.0(existing, new);
	}
}

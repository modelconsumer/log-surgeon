use super::*;

#[test]
fn complement_of_nothing_is_everything() {
	let complement: Vec<Interval<u32>> = Interval::complement(&mut []);
	assert_eq!(complement, vec![Interval::new(0, u32::MAX)]);
}

/// A nested interval must not move the cursor backwards.
#[test]
fn complement_nested_intervals() {
	let intervals: &mut [Interval<u32>] = &mut [Interval::new(20, 40), Interval::new(25, 30)];
	let complement: Vec<Interval<u32>> = Interval::complement(intervals);
	assert_eq!(complement, vec![Interval::new(0, 19), Interval::new(41, u32::MAX)]);
}

#[test]
fn complement_duplicate_full_range() {
	let intervals: &mut [Interval<u8>] = &mut [Interval::new(0, u8::MAX), Interval::new(0, u8::MAX)];
	let complement: Vec<Interval<u8>> = Interval::complement(intervals);
	assert!(complement.is_empty());
}

#[test]
fn complement_signed() {
	let intervals: &mut [Interval<i8>] = &mut [Interval::new(0, 0)];
	let complement: Vec<Interval<i8>> = Interval::complement(intervals);
	assert_eq!(complement, vec![Interval::new(i8::MIN, -1), Interval::new(1, i8::MAX)]);
}

#[test]
fn overlap_basic() {
	assert_eq!(
		Interval::new(0, 10).overlap(&Interval::new(5, 20)),
		Some(Interval::new(5, 10))
	);
	assert_eq!(
		Interval::new(5, 20).overlap(&Interval::new(0, 10)),
		Some(Interval::new(5, 10))
	);
	assert_eq!(
		Interval::new(0, 10).overlap(&Interval::new(3, 4)),
		Some(Interval::new(3, 4))
	);
	assert_eq!(
		Interval::new(0, 10).overlap(&Interval::new(10, 10)),
		Some(Interval::new(10, 10))
	);
	// Adjacent but disjoint.
	assert_eq!(Interval::new(0u32, 10).overlap(&Interval::new(11, 20)), None);
	assert_eq!(Interval::new(11u32, 20).overlap(&Interval::new(0, 10)), None);
}

/// `overlap` must not overflow on intervals touching the representable bounds.
#[test]
fn overlap_at_bounds_does_not_overflow() {
	assert_eq!(
		Interval::new(0u8, u8::MAX).overlap(&Interval::new(0, u8::MAX)),
		Some(Interval::new(0, u8::MAX))
	);
	assert_eq!(
		Interval::new(0u8, 0).overlap(&Interval::new(0, u8::MAX)),
		Some(Interval::new(0, 0))
	);
	assert_eq!(Interval::new(0u8, 0).overlap(&Interval::new(u8::MAX, u8::MAX)), None);
}

#[test]
fn contains() {
	assert!(Interval::new(0u32, 10).contains(&Interval::new(2, 8)));
	assert!(Interval::new(0u32, 10).contains(&Interval::new(0, 10)));
	assert!(!Interval::new(0u32, 10).contains(&Interval::new(0, 11)));
	assert!(!Interval::new(2u32, 10).contains(&Interval::new(1, 5)));
}

#[test]
fn empty_tree_lookups() {
	let tree: IntervalTree<u32, u64> = IntervalTree::new();
	assert!(tree.is_empty());
	assert_eq!(tree.len(), 0);
	assert_eq!(tree.lookup(0), None);
	assert_eq!(tree.lookup(u32::MAX), None);
	assert_eq!(tree.lookup_interval(7), None);
}

#[test]
fn lookup_interval_returns_containing_interval() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(10, 20), 1, PolicyAdd);
	assert_eq!(tree.lookup_interval(10), Some(Interval::new(10, 20)));
	assert_eq!(tree.lookup_interval(20), Some(Interval::new(10, 20)));
	assert_eq!(tree.lookup_interval(9), None);
	assert_eq!(tree.lookup_interval(21), None);
}

/// A new interval strictly inside an existing one splits it into three.
#[test]
fn insert_strictly_contained() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, 100), 1, PolicyAdd);
	tree.insert(Interval::new(40, 50), 2, PolicyAdd);
	assert_eq!(tree.len(), 3);
	assert_eq!(tree.intervals[0], (Interval::new(0, 39), 1));
	assert_eq!(tree.intervals[1], (Interval::new(40, 50), 3));
	assert_eq!(tree.intervals[2], (Interval::new(51, 100), 1));
}

/// An existing interval strictly inside the new one.
#[test]
fn insert_strictly_containing() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(40, 50), 1, PolicyAdd);
	tree.insert(Interval::new(0, 100), 2, PolicyAdd);
	assert_eq!(tree.len(), 3);
	assert_eq!(tree.intervals[0], (Interval::new(0, 39), 2));
	assert_eq!(tree.intervals[1], (Interval::new(40, 50), 3));
	assert_eq!(tree.intervals[2], (Interval::new(51, 100), 2));
}

#[test]
fn insert_exactly_equal_interval() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(5, 10), 1, PolicyAdd);
	tree.insert(Interval::new(5, 10), 2, PolicyAdd);
	assert_eq!(tree.len(), 1);
	assert_eq!(tree.intervals[0], (Interval::new(5, 10), 3));
}

/// Adjacent (touching but non-overlapping) intervals must not be merged into one another.
#[test]
fn insert_adjacent_intervals_stay_separate() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, 5), 1, PolicyAdd);
	tree.insert(Interval::new(6, 10), 2, PolicyAdd);
	assert_eq!(tree.len(), 2);
	assert_eq!(tree.lookup(5), Some(&1));
	assert_eq!(tree.lookup(6), Some(&2));
}

/// Inserting in descending order exercises the `first_overlap_before > 0` splice path.
#[test]
fn insert_descending_order() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	for i in (0..10u32).rev() {
		tree.insert(Interval::new(i * 10, (i * 10) + 5), u64::from(i), PolicyAdd);
	}
	assert_eq!(tree.len(), 10);
	for i in 0..10u32 {
		assert_eq!(tree.lookup(i * 10), Some(&u64::from(i)));
		assert_eq!(tree.lookup((i * 10) + 6), None);
	}
}

/// The new interval spans several existing intervals *and* the gaps between them.
#[test]
fn insert_spanning_intervals_and_gaps() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(10, 19), 1, PolicyAdd);
	tree.insert(Interval::new(30, 39), 2, PolicyAdd);
	tree.insert(Interval::new(0, 49), 10, PolicyAdd);
	assert_eq!(
		tree.intervals,
		vec![
			(Interval::new(0, 9), 10),
			(Interval::new(10, 19), 11),
			(Interval::new(20, 29), 10),
			(Interval::new(30, 39), 12),
			(Interval::new(40, 49), 10),
		]
	);
}

#[test]
fn insert_at_min_and_max_bounds() {
	let mut tree: IntervalTree<u8, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, u8::MAX), 1, PolicyAdd);
	tree.insert(Interval::new(0, 0), 2, PolicyAdd);
	tree.insert(Interval::new(u8::MAX, u8::MAX), 4, PolicyAdd);
	assert_eq!(tree.lookup(0), Some(&3));
	assert_eq!(tree.lookup(1), Some(&1));
	assert_eq!(tree.lookup(u8::MAX - 1), Some(&1));
	assert_eq!(tree.lookup(u8::MAX), Some(&5));
}

#[test]
fn insert_full_range_over_full_range() {
	let mut tree: IntervalTree<u8, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, u8::MAX), 1, PolicyAdd);
	tree.insert(Interval::new(0, u8::MAX), 2, PolicyAdd);
	assert_eq!(tree.len(), 1);
	assert_eq!(tree.intervals[0], (Interval::new(0, u8::MAX), 3));
}

#[test]
fn insert_single_points_covering_whole_range() {
	let mut tree: IntervalTree<u8, u64> = IntervalTree::new();
	for pos in 0..=u8::MAX {
		tree.insert(Interval::new(pos, pos), u64::from(pos), PolicyAdd);
	}
	assert_eq!(tree.len(), 256);
	for pos in 0..=u8::MAX {
		assert_eq!(tree.lookup(pos), Some(&u64::from(pos)));
	}
}

#[test]
fn insert_signed_across_zero() {
	let mut tree: IntervalTree<i8, u64> = IntervalTree::new();
	tree.insert(Interval::new(i8::MIN, i8::MAX), 1, PolicyAdd);
	tree.insert(Interval::new(-1, 0), 2, PolicyAdd);
	assert_eq!(tree.lookup(i8::MIN), Some(&1));
	assert_eq!(tree.lookup(-2), Some(&1));
	assert_eq!(tree.lookup(-1), Some(&3));
	assert_eq!(tree.lookup(0), Some(&3));
	assert_eq!(tree.lookup(1), Some(&1));
	assert_eq!(tree.lookup(i8::MAX), Some(&1));
}

/// The policy must only be invoked where intervals actually overlap.
#[test]
fn policy_not_invoked_for_disjoint_inserts() {
	let mut combines: usize = 0;
	{
		let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
		tree.insert(Interval::new(0, 5), 1, PolicyAdd);
		tree.insert(
			Interval::new(10, 15),
			2,
			PolicyFunction::new(|_existing: &mut u64, _new: u64| combines += 1),
		);
	}
	assert_eq!(combines, 0);
}

#[test]
fn policy_unique_accepts_equal_values() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(1, 5), 7, PolicyUnique);
	tree.insert(Interval::new(1, 5), 7, PolicyUnique);
	assert_eq!(tree.intervals, vec![(Interval::new(1, 5), 7)]);
}

#[test]
#[should_panic]
fn policy_unique_rejects_overlapping_values() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(1, 5), 7, PolicyUnique);
	tree.insert(Interval::new(3, 9), 8, PolicyUnique);
}

#[test]
fn policy_extend_merges_containers() {
	let mut tree: IntervalTree<u32, Vec<u8>> = IntervalTree::new();
	tree.insert(Interval::new(0, 10), vec![1], PolicyExtend);
	tree.insert(Interval::new(5, 15), vec![2], PolicyExtend);
	assert_eq!(tree.lookup(0), Some(&vec![1]));
	assert_eq!(tree.lookup(5), Some(&vec![1, 2]));
	assert_eq!(tree.lookup(15), Some(&vec![2]));
}

#[test]
fn from_iter_matches_repeated_insert() {
	let entries: [(Interval<u32>, u64); 3] = [
		(Interval::new(0, 10), 1),
		(Interval::new(5, 15), 2),
		(Interval::new(15, 15), 3),
	];

	let collected: IntervalTree<u32, u64> = entries
		.iter()
		.map(|&(interval, value)| (interval, value, PolicyAdd))
		.collect();

	let mut inserted: IntervalTree<u32, u64> = IntervalTree::new();
	for (interval, value) in entries {
		inserted.insert(interval, value, PolicyAdd);
	}

	assert_eq!(collected, inserted);
}

#[test]
fn retain_removes_entries() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, 5), 1, PolicyAdd);
	tree.insert(Interval::new(10, 15), 2, PolicyAdd);
	tree.retain(|(_, value)| *value != 1);
	assert_eq!(tree.len(), 1);
	assert_eq!(tree.lookup(3), None);
	assert_eq!(tree.lookup(12), Some(&2));
}

#[test]
fn iter_and_iter_mut() {
	let mut tree: IntervalTree<u32, u64> = IntervalTree::new();
	tree.insert(Interval::new(0, 5), 1, PolicyAdd);
	tree.insert(Interval::new(10, 15), 2, PolicyAdd);

	assert_eq!(
		tree.iter().map(|(interval, _)| interval).collect::<Vec<_>>(),
		vec![Interval::new(0, 5), Interval::new(10, 15)]
	);

	for (_, value) in tree.iter_mut() {
		*value *= 10;
	}
	assert_eq!(tree.lookup(0), Some(&10));
	assert_eq!(tree.lookup(10), Some(&20));
}

//! Tarjan's SCC algorithm.
//! See <https://en.wikipedia.org/wiki/Tarjan%27s_strongly_connected_components_algorithm>.

/// Results of Tarjan's SCC algorithm.
/// See [`TarjanSccs::tarjan_scc`].
#[derive(Debug, Clone)]
pub struct TarjanSccs {
	/// List of SCCs, where an SCC is a list of vertex indices.
	///
	/// **Unlike** the standard Tarjan's SCC algorithm,
	/// which identifies SCCs in a reverse topological sort (of the DAG of the SCCs),
	/// the output [`TarjanSccs::sccs`] is an (forward) topological sort.
	/// In other words, if `vertices[0]` is an "entry" or "root",
	/// its SCC will be the first in `TarjanSccs::sccs`.
	///
	/// Further, vertices within an SCC are sorted by [`TarjanVertex::encountered_at`].
	pub sccs: Vec<Vec<usize>>,
	/// Additional info associated with each vertex.
	pub vertices: Vec<TarjanVertex>,
	/// Mapping [`TarjanVertex::encountered_at`] to original vertex indices.
	original_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct TarjanVertex {
	/// Order in which vertices are visited by the algorithm.
	/// Unvisited vertices are initialized with `usize::MAX`,
	/// which is not a valid index.
	pub encountered_at: usize,
	/// Minimal `encountered_at` among vertices of the corresponding SCC.
	pub low_link: usize,
	/// SCC index (in [`TarjanSccs::sccs`]).
	pub scc: usize,
	/// Working data for the algorithm.
	on_stack: bool,
}

#[derive(Debug)]
pub struct Frame<I> {
	index: usize,
	successors: I,
}

impl TarjanSccs {
	/// Compute [`TarjanSccs`] data from a list of vertices;
	/// the data refers to the vertices by their original index in the input slice.
	///
	/// See [`TarjanSccs`] for more details on the output.
	pub fn tarjan_scc<'a, T, F, I>(vertices: &'a [T], successors: F) -> Self
	where
		T: 'a,
		F: Fn(&'a T) -> I + 'a,
		I: Iterator<Item = usize>,
	{
		let mut this: Self = Self {
			sccs: Vec::new(),
			vertices: vec![
				TarjanVertex {
					encountered_at: usize::MAX,
					low_link: usize::MAX,
					scc: usize::MAX,
					on_stack: false,
				};
				vertices.len()
			],
			original_indices: Vec::new(),
		};

		{
			// For disjoint graphs, `strong_connect` needs to be called on
			// (at least one vertex of) each connected subgraph.

			let mut stack: Vec<usize> = Vec::new();
			for i in 0..vertices.len() {
				this.strong_connect(vertices, i, &successors, &mut stack);
			}
		}

		// Tarjan's SCC algorithm identifies SCCs in a reverse topological sort of the DAG of the SCCs;
		// i.e., the SCC containing the entry state of an NFA/DFA would be ordered last.
		this.sccs.reverse();

		for vertex in this.vertices.iter_mut() {
			vertex.scc = this.sccs.len() - vertex.scc - 1;
		}

		this
	}

	/// "Iterative" implementation of the recursive `strong_connect` subprocedure in Tarjan's SCC algorithm:
	/// visits all vertices that are reachable from `start`.
	///
	/// The recursion call-stack is implemented as a heap-allocated `Vec`/stack of frames;
	/// unlike iterative versions of tail-recursive functions,
	/// more bookkeeping is necessary to perform the "after-recursion" work.
	fn strong_connect<'a, T, F, I>(&mut self, vertices: &'a [T], start: usize, successors: &F, stack: &mut Vec<usize>)
	where
		T: 'a,
		F: Fn(&'a T) -> I + 'a,
		I: Iterator<Item = usize>,
	{
		if self.vertices[start].encountered_at != usize::MAX {
			return;
		}

		let encountered_at: usize = self.original_indices.len();
		self.vertices[start] = TarjanVertex {
			encountered_at,
			low_link: encountered_at,
			on_stack: true,
			scc: usize::MAX,
		};
		self.original_indices.push(start);

		assert_eq!(stack, &([] as [usize; 0])[..]);
		stack.push(start);

		let mut frames: Vec<Frame<I>> = vec![Frame {
			index: start,
			successors: successors(&vertices[start]),
		}];

		while let Some(frame) = frames.last_mut() {
			let i: usize = frame.index;
			if let Some(j) = frame.successors.next() {
				if self.vertices[j].encountered_at != usize::MAX {
					if self.vertices[j].on_stack {
						self.vertices[i].low_link =
							std::cmp::min(self.vertices[i].low_link, self.vertices[j].encountered_at);
					}
				} else {
					let encountered_at: usize = self.original_indices.len();
					self.vertices[j] = TarjanVertex {
						encountered_at,
						low_link: encountered_at,
						on_stack: true,
						scc: usize::MAX,
					};
					self.original_indices.push(j);
					stack.push(j);
					frames.push(Frame {
						index: j,
						successors: successors(&vertices[j]),
					});
				}
			} else {
				let frame: Frame<I> = frames.pop().unwrap();
				let i: usize = frame.index;

				if let Some(parent) = frames.last() {
					self.vertices[parent.index].low_link =
						std::cmp::min(self.vertices[parent.index].low_link, self.vertices[i].low_link);
				}

				if self.vertices[i].low_link == self.vertices[i].encountered_at {
					let mut scc: Vec<usize> = Vec::new();
					loop {
						let j: usize = stack.pop().unwrap();
						self.vertices[j].on_stack = false;
						self.vertices[j].scc = self.sccs.len();
						scc.push(j);
						if j == i {
							break;
						}
					}
					scc.reverse();
					assert!(scc.is_sorted_by_key(|&i| self.vertices[i].encountered_at));
					assert_eq!(scc[0], i);
					self.sccs.push(scc);
				}
			}
		}
	}

	#[allow(unused)]
	fn strong_connect_recursive<'a, T, F, I>(
		&mut self,
		vertices: &'a [T],
		i: usize,
		successors: &F,
		stack: &mut Vec<usize>,
	) where
		T: 'a,
		F: Fn(&'a T) -> I + 'a,
		I: Iterator<Item = usize>,
	{
		if self.vertices[i].encountered_at != usize::MAX {
			return;
		}

		let encountered_at: usize = self.original_indices.len();
		self.vertices[i] = TarjanVertex {
			encountered_at,
			low_link: encountered_at,
			on_stack: true,
			scc: usize::MAX,
		};
		self.original_indices.push(i);
		stack.push(i);

		for j in successors(&vertices[i]) {
			if self.vertices[j].encountered_at != usize::MAX {
				if self.vertices[j].on_stack {
					self.vertices[i].low_link =
						std::cmp::min(self.vertices[i].low_link, self.vertices[j].encountered_at);
				}
			} else {
				self.strong_connect_recursive(vertices, j, successors, stack);
				self.vertices[i].low_link = std::cmp::min(self.vertices[i].low_link, self.vertices[j].low_link);
			}
		}

		if self.vertices[i].low_link == self.vertices[i].encountered_at {
			let mut scc: Vec<usize> = Vec::new();
			loop {
				let j: usize = stack.pop().unwrap();
				self.vertices[j].on_stack = false;
				self.vertices[j].scc = self.sccs.len();
				scc.push(j);
				if j == i {
					break;
				}
			}
			scc.reverse();
			assert!(scc.is_sorted_by_key(|&i| self.vertices[i].encountered_at));
			assert_eq!(scc[0], i);
			self.sccs.push(scc);
		}
	}
}

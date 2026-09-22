## Searching Log Shapes

A *log shape* is a template for a set of log messages: static text with rule references in it, e.g.
`" %prefix.logLevel% %prefix.class%: "`. A *search string* is a query the user writes to find messages,
using literal text and `*` wildcards, e.g. `*blk_1073746491_5667*`.

[`SearchString::search_by_log_shapes`][search-by-log-shapes] answers: for each shape, **which
interpretations of the query does that shape admit?** An interpretation attributes each piece of the
query's literal text to either a rule reference or a piece of static text, with the wildcards covering
everything in between. It is the same decomposition a user would perform by hand to understand why
their query matched.

This document is the design of that function: the query semantics, the three paths it can take, the
data structures, the caching, and the correctness invariants. It is intended for someone modifying the
code, not for someone using it -- see the [parsing specification][parsing-spec] for the user-facing
concepts.

### Index
- Public API
- Query Semantics
- Architecture
- Shape Models
- Tier 1: Rejection
- Tier 2: Composition
- Tier 3: The Engine
  - Covering
- Shape Truncation
- Correctness
- Testing
- Complexity
- Known Limitations
- Related Code

### Public API

```rust
pub fn search_by_log_shapes(&self, spec: &ParsingSpec, log_shapes: &[&str]) -> Vec<Vec<Interpretation>>
pub fn search_by_log_shapes_cached(
    &self, spec: &ParsingSpec, cache: &ShapeModelCache, log_shapes: &[&str],
) -> Vec<Vec<Interpretation>>
```

The result is **positional**: `result[i]` holds the interpretations for `log_shapes[i]`, so a caller
can zip the two. Each inner vector is a canonical, duplicate-free set.

`_cached` takes a long-lived [`ShapeModelCache`][shape-model-cache]; it exists because building a
shape's model walks and resolves the whole shape, and callers such as
[`Parser`][parser] search the same shapes for many queries. `search_by_log_shapes` is the convenience
form that allocates a throwaway cache. Both funnel into the private `search_by_log_shapes_with`.

Two further methods expose the engine directly and exist for tests, not users:

- `interpretations_for_log_shape_via_engine` -- bypasses `decompose` entirely, so the
  [differential test][differential] can pin the fast paths against the engine they stand in for.
- `interpretations_for_automata` -- takes an already-built automaton plus a `dropped_static` flag
  saying whether that automaton is a truncation of a longer shape, so the
  [truncation test][narrowing] can compare a truncated shape against the full one.

### Query Semantics

A query is parsed by [`SearchString::parse`][search-parse] into a `Vec<SymbolicChar>`, where a symbol is
either a literal character or `GlobStar` (`*`). `*` and `\` are the only metacharacters; `\*` and `\\`
are the way to write them literally.

#### Anchoring

Anchoring follows **one rule**, read off both ends the same way:

> A query is anchored at a boundary exactly when it does not have a wildcard there.

| Query | Start anchored | End anchored | Meaning |
| --- | --- | --- | --- |
| `*foo*` | no | no | contains `foo` |
| `foo*` | yes | no | starts with `foo` |
| `*foo` | no | yes | ends with `foo` |
| `foo` | yes | yes | is exactly `foo` |

This is derived in exactly one place, [`SearchString::anchored`][search-anchored], which returns an
`AnchoredQuery { view, anchored_end }`:

- **Start anchoring is structural.** An unanchored query literally begins with a `GlobStar`, so the
  automaton built from it begins with `.*`. Nothing else is needed.
- **End anchoring is structural too**, but the trailing wildcard is carried *out of band*. It is
  dropped from the view -- so that `runs_of` sees the last run correctly unanchored -- and `anchored_end`
  records that it was there. Tier 3 puts it back as a real `.*` when it builds the query's automaton.

Both ends therefore mean the same kind of thing: a wildcard the automaton actually consumes. Nothing
stops the intersection early and nothing is appended to a path afterwards.

The cost of that trailing `.*` -- which must traverse whatever remains of the shape -- is controlled by
**not building the rest of the shape**, rather than by short-circuiting the automaton. See
[Shape Truncation](#shape-truncation). This is the "don't fully simulate a trailing `*`" principle,
relocated: deciding *how much shape matters* belongs to placement, which knows where the query's text
can sit, not to the intersection, which would otherwise have to guess.

> **History.** Two hacks lived here. First, the engine treated a missing trailing wildcard as if it
> were present, making `foo` and `foo*` equivalent; the uniform rule above replaced that. Second, the
> trailing wildcard was *simulated* by `Tnfa::intersect`'s `TO_END` const parameter, which made the
> intersection accept as soon as the query's automaton did, and `compute_paths`'s `WILDCARD_END`, which
> then glued a `*` onto the end of each path. Both are gone: the parameters were removed and the
> wildcard made real. See `tests/search_anchoring.rs`.

#### Empty captures

An end-anchored query can pin a rule to producing *nothing*. If a shape references a nullable sub-rule
via `%foo.leaf%` and the query is `x` against shape `x%foo.leaf%`, the correct interpretation contains
`(?<leaf>)` -- an empty capture. This is more precise than reporting `*`, which would claim the rule
could have produced anything.

The engine reports empty captures directly. Note that the spec builder *rejects a whole rule that can
match empty* (`RegexErrorKind::NullableExpression`), so this arises from nullable **captures inside** a
rule, not from nullable root rules.

### Architecture

Tiers 1 and 2 live in [`search::decompose`][decompose], a submodule of `search` because it is an
implementation detail of one function and has no other caller. It was once called `prefilter`, which
described only tier 1: on the HDFS corpus tier 2 *answers* 1205 shapes while tier 1 rejects 38 and the
engine never runs, so the module is primarily a decomposer that happens to begin with a filter.

`interpretations_for_log_shape` is a cascade, cheapest path first. A shape is answered by the first tier
that can conclude.

```
                      +-----------------------+
   per query -------->|  runs_of(raw symbols) |  once, reused for every shape
                      |  RunFitCache          |
                      +-----------+-----------+
                                  |
    per shape ---------------------v-----------------------------------------
                       +-----------------------+
                       | ShapeModel            |  parts + charsets + nullable flags
                       |  (cached; asserts     |  panics if a placeholder is
                       |   supported shape)    |  undefined or non-leaf
                       +-----------+-----------+
                                   |
                 +----------------v----------------+
        Tier 1   | can_match?  ---- no --> REJECT  |  O(atoms x query), usually O(placeholders)
                 +----------------+----------------+
                                  | yes
                 +----------------v----------------+
        Tier 2   | PlacementTable + compose        |
                 |  Impossible ------------------> |  answer: no match
                 |  Compositions ----------------> |  answer: interpretations
                 |  Unknown -----------+           |
                 +---------------------+-----------+
                                       |
                 +---------------------v-----------+
        Tier 3   | engine: intersect TNFA          |  full or truncated shape
                 +---------------------------------+
```

Two pieces of per-query state are computed once and shared across all shapes:

- **Runs** -- `decompose::runs_of(&self.symbols)`. A *run* is a maximal stretch of literal characters,
  carrying `anchored_start`/`anchored_end`. Runs come from the **raw** symbols, never from the engine's
  view: dropping the trailing wildcard is exactly what marks the last run as end-anchored, so
  re-adding one would erase the information anchoring depends on.
- **RunFitCache** -- see below.

### Shape Models

[`ShapeModel`][shape-model] is a coarse, allocation-light model of a shape, built by
[`ParsingSpec::split_log_shape`][split-log-shape] -- the same tokenizer the automaton builder uses, so
the model and the automaton can never disagree about where the placeholders are. It is a sequence of
[`ShapePart`][shape-part]s:

- `Static(String)` -- text a match must reproduce verbatim. Never empty.
- `Placeholder(Placeholder)` -- a rule reference, carrying:
  - `name` -- the rule name as written in the shape, used to label the captures it produces.
  - `charset` -- a **superset** of the characters any match of the rule can contain (see
    [`Charset`][charset]).
  - `alternatives` -- the sub-rules a capture could name, for positional identity.
  - `can_match_empty` -- whether some alternative is nullable.

#### Supported Shapes

Building a model **asserts** that every placeholder names a rule the spec defines and that the rule is a
leaf. Both failures are treated as errors in the shape, not conditions to recover from, so
`search_by_log_shapes` panics with a message naming the shape and rule:

- An **undefined rule** has no regex, so there is nothing to place a run into.
- A **non-leaf rule** (one with nested captures) would have to be reported as its nested captures, which
  the model does not carry; reporting the whole rule instead would silently differ from the engine. The
  shape must reference one of the leaf captures (`%blockID.num%`) instead.

A shape with **no placeholders** (pure static text) *is* supported: its text is exactly what a query
must reproduce, which placement and composition handle like any other part. This was previously
mistaken for "nothing to attribute query text to" and deferred to the engine.

Because validation happens when the model is built -- and all models are built before any shape is
searched -- an unsupported shape fails the whole call up front rather than part-way through the list.

> This restriction is specific to **log shapes**. [`SearchString::search_by_name`] searches a rule by
> name and reports the nested captures the engine finds, so it accepts non-leaf names (`"blockID"`)
> without issue. The two entry points share the query type but not this constraint.

The two anchoring questions are answered on the model:

```rust
model.can_start_at(part)         // every earlier part can emit nothing
model.can_end_at(part)           // every later part can emit nothing
```

They are what make anchoring correct in the presence of nullable placeholders: a start-anchored run
must be the first thing the *message* emits, which does not require it to be in the first shape *part*
if the parts before it can vanish.

#### RunFit

[`RunFit`][run-fit] answers "how can this run sit inside this rule?" for one `(rule name, run)` pair,
independent of any shape. This is the source of the algorithm's leverage: a corpus mentions few
distinct rule names relative to the number of rule *references*, and a query has few runs, so the
number of distinct simulations is tiny.

It records:

- `whole` -- interpretations for the run sitting wholly inside, as `*run*`.
- `suffixes[k]` -- the rule can *end* with `run[..k]`.
- `prefixes[k]` -- the rule can *begin* with `run[k..]`.

[`RunFitCache`][run-fit-cache] memoizes these under a mutex, releasing the lock while computing so a
slow simulation does not block other keys. Two threads racing on one key duplicate work but not
correctness. `matches_exactly` and `can_produce_all_text` share a second, separately-keyed cache,
because they need a single simulation rather than one per split point.

### Tier 1: Rejection

```rust
decompose::can_match(model: &ShapeModel, symbols: &[SymbolicChar], anchored_at_end: bool) -> bool
```

`false` **proves** no message of the shape can match. It runs the reachability pass of
[`decompose::align`][align], a DP over `(query cursor, shape cursor)` where every transition advances
one or the other, so the state space is a DAG solved in one reverse sweep. It allocates one bit per
state and nothing per alignment.

Placeholders are approximated by their charset and permitted to match empty. Both **widen** the set of
accepted alignments, so the result is a superset: a rejection is sound.

There is a cheap sufficient fast path, `is_obviously_not_ruled_out`: if the query starts with a
wildcard and some single placeholder's charset admits every literal in the query, that placeholder
alone could emit the whole query, so the shape cannot be rejected. It is `O(placeholders + query)`
against the walk's `O(atoms x query)`, and it is the common case on real shapes -- paying for the full
table there once made the rejection tier cost more than it saved. When the query is end-anchored, the
placeholder must additionally satisfy `can_end_at`.

> The `align` **decompositions** are deliberately not usable as the result. Because placeholders are
> over-approximated, they form a superset of the engine's answers. Only the yes/no answer is taken from
> this tier.

### Tier 2: Composition

This is the path that actually answers the question, and it never builds a shape automaton.

#### Placement

[`PlacementTable::compute`][placement-compute] asks, for each run, where it could go. A run must be
produced in full, and there are only three possibilities:

1. **wholly inside a rule** -- `RunFit::fits_wholly`;
2. **wholly inside static text** -- a `match_indices` substring search;
3. **straddling** a boundary -- split between a rule and its neighbour, recorded via `suffixes` /
   `prefixes`.

Anchoring constrains each in both directions:

- A start-anchored run must satisfy `can_start_at(index)` *and* begin at offset 0 of its part; inside a
  rule it must additionally be a **prefix** of the rule (`prefixes[0]`), not merely contained --
  otherwise `N*` would be placed in a rule matching `WARN`.
- The end is the mirror image, via `suffixes[len]` and `can_end_at`.
- Anchored at **both** ends, a rule must match the run *exactly*, with nothing around it.

A cap, `MAX_PLACEMENTS_PER_RUN = 2048`, bounds the table. Exceeding it returns `None` and the caller
falls back to the engine rather than answer wrongly -- placement enumeration is not polynomial in
general (a chain of back-to-back rules can split a run exponentially many ways).

#### Composition

[`compose`][compose] chooses one placement per run such that they run left to right and do not overlap.
The state is `(run index, earliest usable position)`; every placement advances both, so this is another
DAG solved by a reverse sweep. The position is a `(part, char offset)` pair, not a bare part index,
which is what lets several runs share one static part.

Feasibility is decided first, as bits; only a shape that survives pays to materialize its
compositions. A multi-piece capture (one rule holding pieces of several runs) is re-verified against
the rule as a whole, since a rule admitting each run alone need not admit them together -- an
alternation such as `INFO|WARN` admits either and neither pair.

#### Rendering

[`Composition::to_interpretation`][to-interpretation] turns a composition into the reported result. A
wildcard separating two runs is emitted exactly where the query had one, so a run crossing a part
boundary stays contiguous.

The `*` *padding* around a value is decided differently for the two kinds of part, because they mean
different things -- this is `symbolic_value_of`'s `static_len` parameter:

- A **rule** may emit text of its own around the query's characters, so it is padded wherever the query
  permits: suppressed where the run continues into a neighbouring part, and where the query anchors the
  run to that end of the message.
- **Static text** is reproduced verbatim, so its value must glob-match the part's text *exactly*.
  Padding is decided by the piece offsets recorded in `Placement` -- a `*` stands for the characters of
  the part the run does not cover, and appears if and only if there are some. Anchoring and run
  continuation need no special case: a run flowing in from the previous part necessarily begins at
  offset 0, and one flowing out necessarily reaches the text's end.

  This is the opposite of the intuition that "static text must match verbatim, so it is never padded".
  Precisely *because* it is verbatim, a value covering only part of it must be free to skip the rest;
  see [Correctness](#correctness).

Static text is reported whether or not the query constrains it, matching the engine and
`search_by_name`'s output shape: a stretch the query's wildcard merely passes over becomes a `'*'`
sub-query. So a run `foobar` against `foo%rule%` yields two sub-queries, `'foo'` and `(?<rule>bar)`.
Consecutive static parts are merged into one sub-query (an escaped `%%` tokenizes into two adjacent
parts, and `Interpretation::invariants` forbids two static sub-queries in a row). This is pure
rendering: the parts and their attribution are already known, so it needs no automaton or simulation.

Rule references after the last *constrained* one are omitted -- the query's implicit trailing wildcard
leaves them unconstrained, and each would only be a vacuous capture carrying no information. Their
static text still appears, as `'*'`, since that is where the trailing wildcard applies.

> When the query is end-anchored those trailing references are *not* unconstrained but pinned to the
> empty string. Composition cannot render that, so it returns `Composed::Unknown` and defers to the
> engine, which reports the exact empty captures.

The rendering deliberately does **not** reproduce the engine's wildcard *placement* inside a value
(the engine may write `INFO*` where composition writes `*INFO*`) nor the engine's synthetic trailing
`*`. Both describe the same decomposition; the differential test compares a normal form that strips
wildcards, so this stays pinned.

#### Budgets

| Constant | Value | On exhaustion |
| --- | --- | --- |
| `Budget::max_partial_alignments` | 500 000 | `Outcome::Unknown` |
| `Budget::max_alignments` | 4 096 | `Outcome::Unknown` |
| `MAX_PLACEMENTS_PER_RUN` | 2 048 | `PlacementTable::compute` -> `None` (+ `info!`) |
| `ComposeBudget::max_compositions` | 65 536 | `Composed::Unknown` |
| `MAX_UNPINNED_SPLITS` | 8 | `PlacementTable::compute` -> `None` (+ `info!`) |

Every budget degrades to "no conclusion", never to a wrong answer: reaching one returns
`None`/`Unknown` and the caller falls through to a more expensive but complete tier. The two placement
caps also emit a `tracing::info!` naming the run and the cap, so a silent performance cliff is
visible. Note this is a different treatment from unsupported shapes, which panic: a cap is a resource
limit an adversarial shape can reach, whereas an unsupported shape is a mistake in the shape itself.

`max_compositions` is deliberately generous, because falling back is only worthwhile when the engine
would do something *different* -- not when it would derive the same answers more slowly. A shape that
genuinely has many decompositions has them on either path, and enumerating them here is far cheaper
than walking paths through an intersection. On the HDFS corpus, `*INFO*blk*` against the repeated
classpath shapes (377 parts, ~190 placements per run) yields ~18 000 distinct interpretations: tier 2
enumerates them in ~2 s, whereas the engine exceeds `PATH_TIMEOUT_MILLIS` and aborts. At the old cap of
4 096 those four shapes deferred, and the query could not be answered at all.

`MAX_UNPINNED_SPLITS` caps the candidate split points tried when a run must pass through a rule that
has *another rule* immediately after it in the shape, so nothing in the static text pins where the
split falls. When the run is longer than the cap, **not every split can be tried** -- and a dropped
candidate would be a false rejection, so `candidate_middle_lengths` returns `None` and poisons the
whole table. This propagation is what makes the cap safe: the incomplete table is treated exactly like
an unreasonable rule, and the caller falls back to the engine.

> This was previously a real bug: the cap silently truncated the candidate list, so a shape whose
> correct split exceeded 8 characters (e.g. `%r1%%r2%%r3%` with `r2 = [a-z]{9}`, query
> `1aaaaaaaaa2`) was rejected even though the engine matched. Pinned by
> `middle_split_longer_than_the_candidate_cap_still_matches` and
> `adjacent_rule_chains_are_never_falsely_rejected` in the differential test.

### Tier 3: The Engine

Reached only when composition declines to conclude. It builds the shape's TNFA and intersects it with
the query's, then enumerates paths. The intersection **always runs to the end** -- a state accepts only
when both sides accept -- so `anchored_end` changes only what goes in, not how it is combined:

```rust
// `true` (`*foo`): used as written, so the match must reach the shape's end.
// `false` (`*foo*`): the dropped trailing wildcard is restored as a real `.*`.
let search_nfa = Tnfa::for_regex(&self.to_regex_ending(!anchored_end));
let intersection = shape_nfa.intersect::<true>(&search_nfa);
let paths = intersection.compute_paths();
```

The `FOR_SEARCH = true` parameter tells the intersection to keep non-literal (wildcard) transitions
maximal, so a literal edge that "came from" the shape is distinguishable from one that is a meaningful
search value. It is the only const parameter left.

Because a path now always reaches an accepting state, every capture it opened was explicitly closed.
`PartialPath::finish` asserts that, and reports a capture's contents exactly as the rule produced
them -- including *empty*, where the query pins the rule to producing nothing.

Two renderings then reconcile the engine's output with composition's, both applied only when the query
is unanchored (`drop_trailing_unconstrained`):

- **Trailing captures are dropped.** Past the query's last literal character every rule reference is
  unconstrained and would be reported as a bare `*`. Composition omits exactly these. Positional
  identity is read left to right, so trimming the tail is safe -- a *leading* or *interior* vacuous
  capture is always kept, since its position is what tells two references to one rule apart.
- **Trailing static text is kept, and given a `*`.** Unlike a capture it is not vacuous: it names text
  the message still contains. A bare `'a'` for a shape continuing `a%word%b...` would assert the message
  *ends* at `a`, which matches nothing; see
  [Satisfiability of static values](#correctness).

The result is sorted, deduplicated, and `dedup_covered_interpretations` drops interpretations another
already covers.

#### Covering

[`SubQuery::covers`][covers] decides whether one sub-query's value describes everything another's does.
It is a **conservative syntactic test, not glob containment**: it compares the two values
wildcard-segment by wildcard-segment, so it only sees a containment when the wildcards line up
positionally. It reports `false` for `aa*` against `aaa*`, even though every string matching the latter
matches the former.

Only one direction is guaranteed -- `covers` implies containment, never the converse -- and that is the
direction `dedup_covered_interpretations` needs: a missed containment leaves a redundant
interpretation, whereas a spurious one would delete a real answer. Deciding true containment would
need a quadratic match over the two patterns, which is not worth it merely to tidy the output. The
result is therefore **not guaranteed to be a minimal antichain**.

`covers` is reflexive and transitive only on values with **no adjacent wildcards**, which is what makes
the dedup loop reach a fixpoint. Its fast path recognises a lone `*` as universal, but `**` falls
through to the segment loop and fails even against itself. Every producer emits values in that normal
form -- `symbolic_value_of` emits at most one leading and one trailing `*`, and `condense_wildcards`
collapses any doubling introduced by merging -- and `Interpretation::invariants` asserts it.

The loop itself builds an antichain in one pass per candidate: a candidate covered by a survivor is
dropped, otherwise anything it covers is removed (by `swap_remove`) and it is kept. Order is not
preserved, which is why callers `sort` beforehand only to `dedup` exact duplicates.

### Shape Truncation

Real shapes carry very long tails of static text -- in the HDFS corpus, hundreds of shapes are over
20 KB of banner text wrapped around two small rules -- and the automaton builder emits **one state, and
one formatted name, per literal character**. Intersecting such a shape costs far more than the query
warrants.

This is what makes the restored trailing `.*` affordable, and it is the load-bearing half of the
"don't fully simulate a trailing `*`" principle: the wildcard is real, but there is little left for it
to traverse.

[`PlacementTable::last_reachable_part`][last-reachable-part] returns the last shape part the query's literal text
can reach, so the shape is built as a **prefix** ending there:

```rust
let end = table.last_reachable_part()?;
let fragments = model.fragments_in(0, end);
let truncated = spec.automata_for_fragments(&fragments)?;
```

On a 20 000-character shape this takes the automaton from ~20 000 states to ~56, with identical
results.

Four subtleties, all learned the hard way:

- **It is the *last* run that bounds the reach**, not the union over all runs. A composition lays its
  runs down left to right, so an earlier run that *could* sit late in the shape never does in a
  composition that also places the runs after it. Taking the union truncates far less -- on the HDFS
  corpus it trimmed 377 parts to 375, i.e. not at all.
- **Only the tail may be dropped.** The head must be kept verbatim: the query's leading wildcard still
  has to traverse it. Replacing the elided region with `.*` is **not** equivalent -- `.*` is a superset
  of the text it stands for, so it lets runs straddle where the real static text forbids it and invents
  interpretations the full shape does not have.
- **Never truncate when end-anchored.** The truncated parts are precisely the ones an end-anchored
  query still has to match. `truncated_automata` returns `None` in that case.
- **A dropped tail containing static text must still be reported**, as a trailing `'*'`, because the
  full shape would have reported it. `TruncatedShape::dropped_static` carries that fact to the
  rendering, where the wildcard is merged into the final static sub-query if there already is one.

`PlacementTable` is therefore computed even for shapes composition will not answer, purely so the
engine fallback can truncate. This is why `composed_interpretations` takes the table as an argument
rather than computing it.

### Correctness

The properties the implementation must preserve, and where they are pinned:

- **Soundness of rejection.** A shape the rejection tier discards must produce no engine match. A rejection
  must never drop a real result. This is why every candidate cap that cannot be exhausted completely
  must return "no conclusion" rather than an empty placement set; see [Budgets](#budgets).
- **Containment.** Where a decomposition is produced, it must cover every capture the engine reports
  -- a superset. It is deliberately not an equality, because placeholders are over-approximated.
- **Transparency.** `search_by_log_shapes` must return exactly what the engine alone would. This is
  what the differential test asserts, comparing rendered structures as sets.
- **Truncation transparency.** A truncated automaton must give the same interpretations as the full
  one.
- **Invariant.** Every literal character of the query appears in the result, in order, however it was
  split between static text and rules; a wildcard appears exactly where the query had one.
- **Normal form.** No value carries adjacent wildcards, and no two static sub-queries are adjacent.
  Both are asserted by `Interpretation::invariants`. The first is not cosmetic: [`SubQuery::covers`] is
  reflexive and transitive only without `**`, so dedup would silently misbehave on a value carrying it.
  See [Covering](#covering).
- **Satisfiability of static values.** A static sub-query's value must glob-match its shape part's text
  exactly. Reporting `'*ab'` for the run `ab` inside `abcdef` names the right characters but asserts
  the text *ends* in `ab`, which is false and matches nothing. A run covering only part of a stretch is
  therefore padded on both sides; see [Rendering](#rendering) for how the offsets decide it. Pinned by
  `static_sub_query_values_are_satisfiable` -- the structural comparison cannot see this, because
  stripping wildcards is blind to where they sit.
- **Support.** A shape either is supported -- every placeholder defined and leaf -- or the call fails up
  front. It never silently falls back to the engine for an unsupported shape, because that would make
  the same query return answers in a different form depending on the shape.

### Testing

| Test | What it pins |
| --- | --- |
| `tests/decompose_differential.rs` | Transparency and containment over 447 queries x 18 shapes, including end-anchored forms, nullable placeholders, and pure-static shapes. The primary safety net. Also pins satisfiability of static values (`static_sub_query_values_are_satisfiable`), which the structural comparison cannot see, and the two `MAX_UNPINNED_SPLITS` regressions. |
| `tests/search_anchoring.rs` | The anchoring semantics through the public entry point, plus `decompose`/engine agreement. |
| `tests/search_shape_support.rs` | Which shapes are supported, pure-static handling, and the panic contract for unsupported shapes. |
| `tests/shape_narrowing.rs` | Truncation is transparent, and actually reduces state count. |
| `tests/decompose_invariant.rs` | The invariant holds over the real HDFS corpus (thousands of shapes). |
| `src/search/test.rs` | `covers` soundness against true glob containment (`covers_never_claims_an_unsound_containment`) and its partial-order properties in normal form (`covers_is_a_partial_order_in_normal_form`). |
| `src/search/decompose/*/test.rs` | Unit tests for placement, composition, run fit, align, shape, and cache. |
| `tests/local_search.rs::blk_id_full_log_message` | End-to-end corpus run; also a performance baseline. |

The differential test is the one to update when semantics change: it runs both sides (public entry
point vs. `interpretations_for_log_shape_via_engine`) over a query sweep that mixes literal and
wildcard interleavings plus a deterministic pseudo-random sweep, and asserts they agree.

### Complexity

With `Q` = query length, `R` = number of runs, `P` = shape parts, `L` = shape literal characters:

| Tier | Cost | Notes |
| --- | --- | --- |
| Runs | `O(Q)` | once per query |
| RunFit | `O(rule size)` per distinct `(rule, run)` | cached, shape-independent |
| 1 (`can_match`) | `O(placeholders + Q)` fast, `O(P x Q)` worst | bits only |
| 2 (`compose`) | `O(R x positions x placements)` | plus enumeration, budgeted |
| 3 (engine) | `O(shape NFA x query NFA)` | shape NFA truncated to the last reachable part when unanchored |

### Known Limitations

- **`RunFit` re-simulates the rule per split point.** For each split `k` of a run it asks whether the
  rule can end with `run[..k]` / begin with `run[k..]`, which is one automaton intersection per `k`
  (`RunFit::compute`, `src/search/decompose/run_fit.rs`). A single dynamic program over all
  substrings of the run would answer every split in one pass. This is the largest remaining constant
  factor in tier 2; correctness does not depend on it.
- **The engine fallback can abort on pathological input.** `compute_paths` enforces
  `PATH_TIMEOUT_MILLIS` (2 s) and then calls `todo!()` (`src/nfa/search_decomposition.rs`), i.e. it
  panics rather than returning an error or degrading. This predates the decomposition work and is
  only reachable when tier 1 and tier 2 both decline, which the tier-2 composition budget makes
  rare; it is still a live crash for an input that gets there.

### Related Code

- `src/search.rs` -- the public API, `anchored`, the tier cascade, `interpretations_for_shape`,
  `SubQuery::covers` and `dedup_covered_interpretations`.
- `src/search/decompose/shape.rs` -- `ShapeModel`, `Placeholder`, anchoring helpers.
- `src/search/decompose/align.rs` -- the reachability DP and its fast path.
- `src/search/decompose/placement.rs` -- `Run`, `Placement`, `PlacementTable`, `last_reachable_part`,
  composition DP.
- `src/search/decompose/compose.rs` -- composition enumeration and rendering.
- `src/search/decompose/run_fit.rs` -- per-rule run simulation and caches.
- `src/search/decompose/cache.rs` -- `ShapeModelCache`.
- `src/parsing_spec.rs` -- `automata_for_shape` / `automata_for_fragments`, `split_log_shape`.
- `src/nfa.rs`, `src/nfa/search_decomposition.rs` -- the engine.

Tests: `tests/search_anchoring.rs`, `tests/search_shape_support.rs`, `tests/shape_narrowing.rs`,
`tests/decompose_differential.rs`, `tests/decompose_invariant.rs`.

[search-by-log-shapes]: ../src/search.rs
[search-parse]: ../src/search.rs
[search-anchored]: ../src/search.rs
[shape-model]: ../src/search/decompose/shape.rs
[shape-part]: ../src/search/decompose/shape.rs
[shape-model-cache]: ../src/search/decompose/cache.rs
[charset]: ../src/search/decompose.rs
[decompose]: ../src/search/decompose.rs
[placement-compute]: ../src/search/decompose/placement.rs
[compose]: ../src/search/decompose/compose.rs
[to-interpretation]: ../src/search/decompose/compose.rs
[covers]: ../src/search.rs
[run-fit]: ../src/search/decompose/run_fit.rs
[run-fit-cache]: ../src/search/decompose/run_fit.rs
[align]: ../src/search/decompose/align.rs
[last-reachable-part]: ../src/search/decompose/placement.rs
[split-log-shape]: ../src/parsing_spec.rs
[intersect]: ../src/nfa.rs
[parser]: ../src/parser.rs
[differential]: https://github.com/y-scope/log-surgeon/blob/log-mechanic/rust/tests/decompose_differential.rs
[narrowing]: https://github.com/y-scope/log-surgeon/blob/log-mechanic/rust/tests/shape_narrowing.rs
[parsing-spec]: parsing-specification.md

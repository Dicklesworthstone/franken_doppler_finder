# Changelog

This is the agent-facing history of `franken_doppler_finder` (`fdop`). It is synthesized from the evidence
listed below.

**Scope window:** from project inception on 2026-11-03 to the initial public import on 2026-11-03.

**Evidence and method.** The repository has no git history before its first commit. The whole tree was written
in one day by one Claude Code session and two of its subagents, then imported in three commits: the code, one
5-line evidence-calibration change, and the docs and CI. This history is reconstructed from four sources:
- the per-file write timestamps in that session's transcript;
- the session's own progress notes;
- the `result.json` of every reference-case run (`ny1`–`ny9`, `nassau1`–`nassau3`, `md1`, `md2`);
- the code at import.

All times are UTC on 2026-11-03. Version `0.1.0` is the workspace version in `Cargo.toml`. There is no git tag
and no GitHub Release yet. Because the code arrived in one import commit, each section below links its key files
at that commit, not per-change commits.

## Unreleased

### Recover independently supported flight-motion components

- Keep valid horizontal turn motion when a corrupt vertical-rate report rejects the full vector,
  and keep a position-supported vertical rate when speed or heading is missing or rejected.
  Unsupported components retain the existing finite-difference fallback.
- Preserve the original full-vector acceptance path and its exact arithmetic/report credit.
  Partial recovery requires actual position support at the existing 25 m/s tolerance, keeps
  the turn-neighbour requirements, and adds no report credit. It cannot combine two individually
  acceptable residuals into a newly accepted vector that exceeded the original joint limit.
- Add analytic circle/cubic-climb regressions through raw track construction, interpolation and
  emission/Doppler prediction, plus unchanged-vector and joint-limit controls. Strengthen the
  existing corruption tests to require both rejection of the bad component and retention of the
  supported one. No cleaning, gap, physical, confidence or comparison-budget threshold changes.
- **Validation:** the same native 34-test track selection changes from 30 passes / 4 failures on
  the original production code to 34 passes / 0 failures. Sparse-turn maximum position error is
  below 30 m (old fallback above 100 m), with less than one quarter of its maximum Doppler error;
  cubic-climb errors are below 0.1 m and 0.01 Hz. README names the oracles and their limits.
  Independent review passed. CLI selftest and recording replays remain unrun; no new
  real-recording accuracy is claimed.

### Score joined seeds before one complete event pair fills memory

- Deliver completed representatives directly to the quick trace check in groups of at most
  4,096. A large pair no longer retains a complete output vector of seeds that the quick check
  will reject; only its survivors carry forward. The deduplication keys, active worker chunks
  and hypothesis clouds still consume memory, so this is not a total-memory cap.
- Preserve every representative, output order and existing scoring arithmetic. The joined
  budget still counts rejected seeds and stops after the complete pair, with the same warnings
  and margin-gate consequence. No candidate cap or confidence threshold changes.
- Add a native regression that consumes all 32,820 distinct flight pairs without collecting
  their outputs, across three input batches and multiple output groups, under one and four
  workers. The existing exhaustive ordered-output regression also passes in the full core binary.
- **Validation:** the pinned-nightly core binary also passes the rejected-seed budget and
  two-flyover localization/refinement regressions. The latter retains correct A/B identities,
  0.2 m observer error and a 75 m reported radius on its synthetic scene. This follow-up has
  no new whole-recording memory or runtime measurement.

### Reuse time interpolation throughout each ridge profile

- Prepare the fixed time coordinate of each integration frame once instead of recomputing it
  for every frequency offset. Reuse it for normalized peak selection and the existing three-bin
  raw-background correction; retain fractional weights, endpoint duplication, frequency arithmetic,
  summation order, peak rules and measurement thresholds.
- Add two bit-for-bit scalar interpolation regressions spanning fractional/large-origin grids,
  boundaries, invalid coordinates and nonfinite values. Both pass in the actual native core crate,
  alongside all six existing trace tests and both existing spectrogram tests.
- The optimized native `ridge_profile_validation` component compared 384 complete original/current
  profiles exactly and recorded seven alternating 2,400-call trials: median paired speedup 1.69x,
  range 1.56-1.79x. README records hardware and build/workload details. This is a synthetic component
  result, not a recording runtime or accuracy result; CLI selftest and field replays remain unrun.

### Restore only the three-bin correction: no climb to a raw peak several bins away

- Remove the raw-peak climb added in 82f26a9. When the three-bin raw correction is unavailable, the
  normalized vertex stands again, as before 82f26a9. 5300665's three-bin-first correction, its
  frequency-band check and its basin-boundary test stay. The climb's own synthetic test goes with it.
- Why: on the 40 real Livermore clips (set D, development), main `0abfbe9` with this change scores
  34 right at 25 %/50 % and 37 at 30 %/60 %, against 33 and 36 without it (`sd_ncl1`, `sd_m0ab`). It
  beats every rival at both tolerances (dopplertrack wide 30 and 36). N1387U_R44 is right again
  (41 m/s, 476 m; truth 33 m/s, 421 m; 44 m/s, 544 m with the climb). Of the 31 clips whose distance
  moved, 23 moved closer to the ADS-B truth. Median distance error is 164 m (11.5 %) against 170 m
  (12.3 %).
- Wrong-date runs had gained evidence from the climb and lose it: `c_ref_0909` 12.95 nats against
  16.54, `c_sapsucker_1101` 5.10 against 6.16. Both stay low. `c_sapsucker_1101`'s search now finds no
  distinct rival place, so its margin check no longer fails, as before 82f26a9; it still fails core
  matches, an unheard close pass and its radius. Accepting only climbs of at least two bins, the shift
  82f26a9 was written for, still left 36 at 30 %/60 % on set D and made one of `c_sapsucker_1101`'s
  matches core, so the climb goes.
- Validation (`ncl1` = `0abfbe9` with this change, `m0ab` = `0abfbe9`; built on the benchmark machine,
  run locally): selftest passes, the six trace tests pass and clippy is clean. The workspace suite
  has the same seven failures on `0abfbe9` with and without this change, with identical messages:
  459079e's edge-pass test, two measure tests and four curve-match tests. On the seven development
  recordings detection keeps every event; three fewer pass quality on the reference recording (14 to
  11), one fewer on the osprey cam's 10-31 and on Duke Farms, and one more on the feeder. `locate`
  with the benchmark's R2 inputs, offline on one cache (`ab_m0ab_ncl1`): all five wrong-date runs stay
  low (`c_ref_0907`, `c_ref_0909`, `c_sapsucker_1101`, `c_osprey_1031`, `c_duke_1102`), and no
  development answer loses a grade. The reference recording stays high (300 m), the osprey cam's 11-01
  and 11-02 high (110 m, 220 m), Duke Farms medium (970 m), the feeder gives no location, and the
  osprey cam's 10-31 rises from medium to high (80 m), its second match core again. The eagle cam
  stays low (290 m off; core matches, radius).

### Preserve complete cross-day flights when an adjoining day supplies their type

- Apply the data API's strict `keep_unknown = false` policy after cross-day merging. A flight
  identified by either day now retains its missing-type half and the same canonical key as
  an inclusive query. Separate flights of the same aircraft do not inherit that metadata.
- Prefer a recognized aircraft type over a nonempty unrecognized designator as well as an
  empty one. Preserve the first recognized type when later joined segments disagree.
- Add an offline fetch regression using real day-cache files, missing and unrecognized types
  in both day orders, separate same-aircraft flights, and strict/inclusive geometry and key
  comparisons. Add a merge regression covering conflicting recognized types in both orders.
- **Validation:** all 20 production build/ingest module tests pass in a native component build
  using unchanged source modules, the real `fdop-types`, the pinned nightly and exact locked
  dependencies. This includes recognized-type precedence and the archive-completion tests.
  Independent source review and focused formatting/whitespace checks also pass. The full
  data-crate build and a serial retry were blocked when their `asupersync 0.6.2` compiler ended
  with SIGKILL, before the new offline-fetch regression could run. The shared environment reached
  its 8 GiB cap during validation. The component excludes `fetch.rs` and is not an HTTP,
  workspace or recording-accuracy result.

### Release rejected pair seeds and completed search stages promptly

- Run the unchanged quick trace check immediately after each event-pair join. Only survivors
  carry into the following pair; candidate order, scores, cumulative joined counts and the
  stop-after-complete-pair budget remain unchanged. Rejected seeds still consume the existing
  comparison budget, so streaming cannot silently search further or erase a truncation.
- Release hypothesis clouds after the join phase and fine-grid seed storage after fine scoring,
  before confirmation/refinement. These arrays previously lived until the whole search returned.
- Add a real three-flight regression that rejects every quick seed and requires the first
  completed pair to exhaust a zero quick budget, recording `pairs: 3 -> 1` with no candidates.
- Record native old/new join measurements in README: three alternating 1,048,576-pair runs
  preserved the answer, with median peak RSS 70,360 -> 9,472 KiB (86.5% lower). The updated
  16,777,216-pair case passed at 9,344 KiB. Both use actual production join source, the pinned
  nightly, real Rayon and per-child kernel resource accounting. They do not measure this
  follow-up's end-to-end effect or a recording's runtime.
- **Validation:** the pinned-nightly full-core binary passes the new budget regression, complete
  ordered-join equivalence, and the unchanged two-flyover localization test (correct flights,
  less than 60 m position error, less than 1 s clock error and successful refinement). Independent
  review and focused formatting/whitespace checks pass. The broader core run exposed two existing
  curve-match tests that fail: short-circuit localization is 340 m off and 2.5 s early; the grazing
  fixture recovers its trajectory but supplies 6.334 nats, below the unchanged 6.9-nat hearing line.
  See README Testing for the incomplete workspace checks and exact scope of the native evidence.

### Require complete flight archives before publishing a day's tracks

- Require both tar end blocks and drain the complete underlying stream before ingestion succeeds.
  A missing final archive part, truncation at a member boundary, a late HTTP error or nonzero trailing
  bytes can no longer be mistaken for a complete feed. Normal zero padding remains accepted.
- Select only the release's own full tar or contiguous two-letter split pieces. Ignore checksum
  sidecars; reject gaps, duplicates, mixed full/split sets, empty assets and unfinished uploads.
  Validate cached part lists too, and publish release metadata through private atomic staging.
- Keep every pre-publication listing provisional, including ones containing both prod and MLAT.
  Always re-list split archives before an uncached online download: a valid prefix cannot prove
  the last part exists, even after the publication window. Complete day-cache hits bypass API
  lookup, while offline mode still uses valid cached metadata. A newly empty lookup no longer
  silently restores an older listing.
- Add termination, transport, split-boundary, sidecar and listing-refresh regressions. The archive
  reader tests now pass within the 20-test production build/ingest component; release-discovery
  tests remain unrun after the full data-crate dependency compiler was killed. This change also
  has independent source review.
- Existing day stores do not retain proof of their source tar's completeness. This prevents new
  incomplete ingestion; it cannot reconstruct tracks absent from an older cache.

### Skip unreachable trajectories before repeated exact-curve scoring

- Build conservative trajectory bounds once per regional shortlist and reuse them across real and
  decoy windows, clock alternatives, spatial refinement and along-track alias checks. Reject only
  tracks whose bounds prove they cannot meet the existing observer-distance gate.
- Enclose each cubic Hermite segment using its Bezier control hull, including excursions beyond
  reported positions. Pad outward for floating-point evaluation and use conservative distance
  rounding; malformed or nonfinite geometry falls back to the existing scorer.
- Preserve scoring arithmetic, pass order, hearing/silence evidence, reach limits, detection caps
  and confidence thresholds. Add grazing/invalid-input regressions and an exact-output comparison
  using the original three-flight PCM plus 1,024 distant tracks at three observers/clocks.
- **Validation:** the geometry/fail-open regressions and the complete exact-score equivalence test
  now pass in the actual native core crate with the pinned nightly and locked dependencies.
  Independent source review and 38,514 auxiliary JavaScript geometry cases also passed. Workspace
  checks, CLI selftest and recording replays remain unverified; no measured speedup is claimed here.

### Recover damaged auxiliary caches without colliding with concurrent runs

- Give downloaded files, weather JSON/failure notes, terrain PNGs and geoid heights separate,
  exclusively created sibling staging files. Sync complete contents before atomic publication;
  simultaneous writers cannot truncate or rename one another's in-progress response.
- Add a validated-download path that parses the writer's own completed file before publication
  and returns that parsed value. A transport or validation failure preserves the published cache.
- Read and parse existing Census ZIPs before reuse. Online place/county/ZIP lookups recover from
  unreadable archives, malformed tables and header-only tables; offline errors identify the path
  and request an online refresh. Valid cached tables remain entirely offline.
- Add local HTTP/concurrency regressions for complete independent responses and failed replacement,
  a simultaneous byte-cache writer/reader regression, and generated-ZIP recovery/preservation tests.
- **Validation limit:** source review only. The pinned-nightly RCH workspace build was killed
  during dependency compilation (exit 137); a serial retry was interrupted for focused checks and
  the executor then disconnected before any new test result was observed. Formatting already
  failed on the untouched checkout. Native tests, Clippy, final formatting and selftest remain
  unrun; this change makes no localization-accuracy or measured-performance claim.

### Keep duplicate hypothesis pairs out of the search's working set

- Deduplicate flight/place/clock seeds inside ordered worker chunks as each pair is generated,
  then merge bounded batches in the original input order. A dense hash bucket no longer allocates
  every raw match before selecting its representatives. Earlier batches' duplicates allocate no
  candidate entry. The hash index and distinct surviving seeds remain proportional to the search.
- Preserve every comparison, the original first representative, f32 midpoint arithmetic, spatial
  and clock-cell keys, same-flight separation, budgets and confidence thresholds. No early cut is
  introduced to obtain the memory reduction.
- Add an exhaustive ordered-pair oracle covering duplicate-heavy and unique cells, negative
  coordinates and clock boundaries, same-flight separation, multiple batches, empty inputs and
  one/four Rayon workers. Add an opt-in 16-million-pair resource regression with one true seed.
- **Validation:** the exact original and updated join source both passed the ordered oracle
  natively on the pinned nightly with real Rayon 1.12.0 in a focused component build. The updated
  file passes rustfmt and the diff passes whitespace checks. Full core/workspace checks and
  recording replays are not established by this component result; no field speedup or grade
  change is claimed.

### Let the curve match judge a run with `--start` and the default clock tolerance

- `--clock-tol` now defaults to 120 s (it was 180 s). The curve match ranks its answer against decoy clock
  windows of the searched window's width, each the width plus 60 s apart within about ±2 h, and needs 40 of
  them to judge. A start ±180 s is a 360 s window, which got 2 × ⌊7200 / 420⌋ = 34. So a run with `--start`
  and no `--clock-tol` could never have a standing curve match: it could not raise a low answer, lower a
  medium one, or exempt a medium from the measurement line. ±120 s gets 48, the configuration the curve match
  was built and checked on. The widest window that gets 40 is 300 s (±150 s), and 120 s keeps clear of it.
- The trace search's window narrows with it. With `--start` and no `--clock-tol` it searches ±120 s instead of
  ±180 s, and nothing widens past it; pass `--clock-tol 180` for the old window, where the curve match cannot
  judge. A `creation_time` window (start or end convention) takes 120 s of slack either side instead of 180 s,
  and still falls back to the full dates when neither search finds an answer. The benchmark's R2 and R3
  commands pass `--clock-tol 120` explicitly, so they do not change, and runs with neither a start nor a
  `creation_time` do not use it.
- A curve match that ran with fewer than 40 decoys now says why in a warning, and what would let it judge.
  For a window wider than 300 s (`curvematch::max_judged_window_s`) the warning is told which window it was:
  a `--start` window gets the `--clock-tol` that fits, a `creation_time` window is told it spans the
  recording's length as well as the tolerance either side, and a date window the published archive cut
  short is pointed to `--start`. A narrower window is told its decoys are confined to the UTC days the
  recording needs and the published archive. Limits are rounded down so the advice never names a window
  past the line.
- README: the option table, the curve match's decoy counts, and a Known issue for windows wider than 300 s,
  which include every `creation_time` window of a clip longer than a minute at the default.
- Tests: `the_widest_judged_window_is_where_the_decoys_run_out` (300 s gets 40, 301 s fewer, 240 s gets 48,
  360 s gets 34), `the_default_clock_tolerance_lets_the_curve_match_judge`, and
  `a_curve_match_short_of_decoys_says_why_and_what_would_judge` (each kind of window, and limits that are
  not whole seconds).
- No recording was re-run. This changes which runs the curve match can judge, not how it scores, so it
  claims no gain in accuracy. Where it now judges, its record applies: on sealed set E one wrong medium, from
  one heard pass before the two-pass rule; outside development, its answers "from the curve match" have been
  1 right and 3 wrong, all but the set E marina on a synthetic injection bed (bench/PROTOCOL.md Addenda 14,
  16, 17).

### Let the independent curve matcher score faster subsonic passes

- Extend the default speed grid by two existing 20% steps, to about 250 m/s. Its unchanged 15%
  trajectory allowance now includes the detector's 215–263 m/s fast passes; previously coarse
  and exact scoring excluded everything above about 199 m/s despite those detector seeds.
- Retain the full analysed received-frequency comb. A visible high-tone approach can exceed
  the old `2.2 * f0_hi` crop even when its emitted fundamental and received spectrum are in range.
- Clip each path sample to its valid emitted-tone interval instead of discarding the entire
  sample when another tone would leave the image. Require the existing 25% squared-weight
  coverage independently on both halves for each tone. Reuse scratch and O(1) range-difference
  updates; fully covered paths keep the original scalar arithmetic. Bank/null and exact/decoy
  scoring all share the same kernel.
- Add five regressions: visible-tone edge coverage, an independent per-tone oracle, default-speed
  coarse admission and its limit, real phase-integrated crossing-fast PCM with 52 decoys and
  negative controls, and a high-tone PCM probe beyond the old received-frequency crop. New fast
  fixtures alone suppress supra-band harmonics; all pre-existing PCM fixtures remain unchanged.
- **Cost/calibration:** default bank paths rise from 187 to 221 and the 4 kHz comb from 238 to
  279 received rows. Added templates also change the tone-prior histogram. Hearing, coverage,
  margin, radius and decoy-count thresholds are unchanged; no runtime or field-grade claim is made.
- **Validation:** independent source reviews found no blocker. A separate f32 arithmetic model
  matched all 624 deterministic cases; 2,000 non-dyadic weighted cases kept the same coverage and
  winning tone, with maximum score difference about 3.8e-6. Straight-path arithmetic validates
  the high-tone fixture geometry. These checks are not native Rust/PCM passes: new regressions,
  formatting, Clippy, release selftest and reference/field replays remain unrun during the outage.

### Recover supported track segments hidden by rejected reports

- Preserve the original longest consistent position chain and recover additional supported
  chains entirely outside its time coverage. Sparse corrupt reports can hide a real coverage
  gap in the raw timestamps; previously the single winning chain discarded the other valid
  section, removing a possible flight match from both searches.
- Keep the existing predecessor time limit, physical predicates and tie order. Extra intervals
  must be more than `max_gap_s` apart and reachable under the same speed/vertical bounds. Each
  interval remains a separate interpolation model; overlapping alternatives stay rejected.
- Preserve the original model's key, geometry, velocities and quality fractions. Extra models
  append `#recoveredN` without renumbering later original models. All returned points contribute
  to the existing cleaning counts. A denser ambiguous winning component is deliberately unchanged.
- Reuse the existing predecessor paths: at most one linear root pass, an O(n log n) endpoint
  ranking/ordered-neighbour scan and linear total traceback. Complete clean tracks avoid that
  recovery allocation/sort when no temporally disjoint part could fit.
- Add six regressions for sparse-gap recovery in both directions, ambiguous/overlapping/unreachable
  alternatives, stable keys and minimum support. An independent planar model completed ten checks;
  removing the old time limit instead admitted a mixed path containing all 800 false reports in
  its overlapping-track control. This is arithmetic evidence, not native Rust execution.
- **Validation:** independent source review found no blocker. New tests, formatting, Clippy,
  release selftest and recording/reference replays remain unrun while the executor is unavailable.
  No change in field accuracy or confidence is claimed.

### Release completed archive-day tracks while other downloads continue

- Once a newly downloaded day is atomically cached, release its built point arrays immediately.
  Retain only the open cache file, original metadata, leg count and per-aircraft bounds until
  every day is ready for global aircraft selection. Active/incomplete ingests and final selected
  tracks still use memory; this removes accumulated completed-day histories, not all memory use.
- Add `TrackStore::save_open`, returning the same exclusively created, finished, synced and
  rewound file that was renamed into place. A competing writer replacing the cache pathname
  cannot substitute different points behind a day's bounds. Existing `save` remains compatible.
- Use one compact representation for both pre-existing caches and newly published days. Fresh
  bounds are derived from the built reports without a first decompression; selected fresh files
  are decoded once before the unchanged geoid, merge and final filtering stages.
- Add retained-generation and concurrent-writer checks, full-merge equivalence after replacing
  every fresh cache path, and an updated bounded-channel publication/reload regression. Exact
  keys, sources, points, day metadata, geoid values and both unknown-type policies are checked.
- **Validation:** independent source review completed. New Rust regressions, formatting, Clippy,
  workspace gates and recording replays remain unrun after the executor disconnected before
  the first edit could be installed locally. The prior 24 types and 31 offline component passes
  belong to `a48f935`; they do not validate this publication handoff. No RSS or speedup is claimed.

### Read cached tracks selectively without losing cross-day flights

- Add a streaming `TrackStore::read_legs` reader. Existing `load` uses it, and every read still
  validates record lengths, the gzip checksum, the complete footer and trailing compressed data,
  even when the caller discards all legs. The `FDOPTRK1` format is unchanged.
- Scan each cached day one leg at a time, combine conservative per-aircraft time/position bounds
  across all cached and newly built stores, and then retain every leg of selected aircraft.
  This avoids holding complete unrelated cached histories while preserving midnight crossings,
  leg IDs, duplicate-position preference, unknown-type policy, geoid adjustment and original
  metadata/source order. The second pass rewinds the same open file across atomic cache replacement.
- Keep wraparound and exact dateline endpoints, nonfinite reports and floating-point endpoint
  hits conservative in the prefilter; the original merge and final filter remain authoritative.
  Files with selected aircraft are decompressed twice. Broad requests may save little memory;
  no peak-RSS or overall speedup measurement is claimed.
- Correct two offline location regressions to require the exact canonical cached flight-leg IDs
  `a1dfeb_20260908_0` and `ace391_20260908_0`. The direct selftest's uppercase names are not the
  report's `icao_date_leg` identities. Both aircraft and every physical accuracy assertion remain
  required; the previously reported `missing A1DFEB` failure is not evidence of a lost flight.
- **Validation:** all 24 real `fdop-types` library tests passed for the exact reader. All 31
  offline component tests passed with the complete unchanged fetch/ingest/build/geoid source,
  real types and pinned data dependencies; panic-on-use HTTP/discovery/download adapters were
  never called. Both data files pass focused rustfmt. The full data-lib Cargo build was killed
  in upstream `asupersync 0.6.2` before our crate compiled. Corrected pipeline assertions, full
  HTTP integration, workspace gates, release selftest and recording replays remain unrun.

### Keep valid closest approaches between range samples

- Refine finite local range minima before applying the physical reach gate in coarse curve
  stamping and exact scoring. A valid 7,999.5 m pass at 100 m/s was discarded because its samples
  two seconds either side were about 8,002 m away. The final 8 km limit is unchanged.
- Evaluate a coarse quadratic timing proposal on the actual interpolated track. Keep the
  original sample whenever the proposal is nonfinite or no closer, so a non-quadratic Hermite
  path cannot lose an already eligible sample merely because its timing estimate is worse.
  This is a bounded comparison of existing candidates, with no new search grid or range slack.
- Add straight/turning track regressions across sampling phases and both sides of the reach,
  a real Hermite interpolation regression, and actual PCM exact-scoring coverage with a separate
  crossing flight and an unrecorded outside-reach control.
- **Validation:** the old coarse regression executed through real RCH and failed at the predicted
  straight-track sampling phase. The original 400-second PCM case failed its aligned-seed
  audibility prerequisite at 3.055 nats, before it could check other seeds. Its revised
  900-second PCM and longer grazing tracks provide more off-pass null background while keeping
  geometry, amplitude and all physical gates. Independent source review and geometric
  arithmetic checks passed, but the revised PCM, production fix, Hermite test, formatting,
  workspace gates and recording replays remain unrun after executor loss. No successful new
  PCM recovery is claimed.

### Remove exact-score evidence jumps between calibration templates

- Replace nearest-template null selection for an exact trajectory with bilinear interpolation
  of fitted median and tail scale on the existing log-speed/log-distance grid. These weights
  interpolate the fitted median and upper-tail quantile together, preserve a positive scale,
  reproduce bank nodes exactly and clamp outside the grid, including singleton axes.
- Use the interpolated scale in the existing tone adjustment and evidence conversion. Real
  and decoy windows share the same scorer; coarse voting, bank density, frequency sampling,
  acceptance lines and the synthetic recording remain unchanged. Work per pass uses at most
  four existing nulls, with no new spectrogram or bank sweep.
- **Observed defect:** on `e925900`,
  `alternative_clock_peaks_recover_the_three_flight_recording` recovered the true clock and all
  three flights but failed its 200 m position assertion at about 213.6 m. The displaced point
  put two passes below the 858.53 m nearest-null boundary. For one, the true raw score was
  158.404 with 9.641 nats, while the weaker displaced score was 147.723 with 15.230 nats.
  All three clock-selector Rust regressions passed. These were real RCH runs, not model results.
- Add focused interpolation regressions for exact nodes, clamped corners, fitted quantiles,
  evidence continuity and singleton axes. Keep the original PCM and 200 m assertion.
- **Validation limit:** independent source reviews and JavaScript f32 arithmetic checks passed.
  The new Rust implementation, interpolation tests, unchanged PCM location assertion, formatting,
  full workspace gates and field replays remain unrun after executor loss. This is a smooth
  approximation to the bank's fitted nulls, not a new calibration proof or an observed recovery.

### Detect fast flyovers near recording boundaries

- Extend the supplemental fast-aircraft bank into the existing short edge-template path.
  Full-length fast templates retain priority; fast edges run afterward only near the recording
  ends and only where their corresponding full-length template lacks enough coverage.
- Reuse the original fast speed ladder and edge-window calculation. Both supplemental stages
  share the residual mask, remaining candidate allowance and detection cap, and require the
  existing strong threshold. Keep ordinary-edge silence-line behavior, physical measurement
  tolerances, original-stage order and the extra-measurement confidence gate.
- Add 30-second physical-phase PCM cases for early 215 m/s and late 245 m/s passes across a fixed
  attenuation ladder. Every new accepted event must satisfy the existing tone, speed, distance
  and timing bounds; at least one recovery per end is required. Noise/hum, steady-tone and
  reversed-pass controls must supply no aircraft event. Extend stage-order and speed-bound
  checks to include the new edge stage.
- **Validation:** on the old production code, the early 215 m/s case failed as expected with
  zero fast-edge candidates at all five attenuations. The test-only source compiled through
  real RCH on the pinned nightly after serializing compiler frontend/backend jobs. Independent
  source review found no concrete blocker. New production compilation, formatting, positive
  recovery, negative controls, workspace gates and recording replays remain unrun after
  executor loss; no measured new recovery or runtime improvement is claimed.

### Try a harmonic sibling only when its primary lost its ridge

- A family's harmonic siblings are now measured only when its primary has no fit, fewer than the
  minimum trace points, a trace short of the closest approach on either side, or too little of its
  frequency drop followed. Before this, any refusal or any fit below the aircraft limits sent the family
  to its siblings, and on real recordings that only replaced right measurements with wrong ones.
- On Livermore set D, clip N456CS_R38 (truth 68 m/s at 2,511 m) had been measured from its primary as
  62 m/s at 2,398 m. Its 392.9 Hz primary was then refused for fit RMS alone (2.13 Hz > 1.50), its
  193.8 Hz half-frequency sibling fitted 109 m/s at 4,914 m, and that event became the clip's scored
  answer, so set D at 30 %/60 % fell from 37 to 36 right. After 82f26a9's trace change also lost
  N1387U_R44, main stood at 35, one below dopplertrack's wide setting (36); with this change it is 36.
  On the seven screening recordings the only other changed event was a feederwatch primary measured
  under 28 m/s and replaced by a 1.5x sibling.
- The designed case, a primary whose ridge breaks through the drop while a sibling stays measurable,
  recovers as before. A new test checks which refusals count as a lost ridge, including a NaN reading.

### Cache completed ADS-B days while other downloads continue

- Drain a bounded queue of completed raw releases inside the download-worker scope. Build and
  atomically save each day as soon as all of its requested feeds arrive, instead of holding every
  raw feed until the final download finishes.
- Drop a failed day's buffered raw parts immediately. Other complete days still reach the cache;
  cache-write errors remain fatal even when missing archive days may be skipped.
- Add offline regressions using actual synthetic tar ingestion, store construction and cache
  round-trips. A bounded-channel handshake requires an earlier day to be readable before the last
  download completes; failure cases check that partial days are never published and unrelated
  successes survive.
- **Validation limit:** source review only for the scheduler and its new tests. The executor
  disconnected before these tests could run. No live-download, whole-pipeline timing or measured
  memory reduction is claimed; active ingests and retained track stores still consume memory.

### Use Alaska and Hawaii radiosonde archives

- Select valid four-character US station identifiers using the provider's country metadata,
  admitting Alaska and Hawaii stations excluded by the former K-only filter. Keep legacy
  cached listings without country metadata compatible with continental K identifiers, and
  exclude explicit foreign entries and aggregate identifiers.
- Preserve non-K station prefixes in sounding requests. IEM prefixes three-character requests
  with K, so stripping an Alaska or Hawaii identifier queried another station. Keep the existing
  three-character continental URLs and cache keys; station lifetimes, sounding brackets,
  geographic limits and atmospheric interpolation retain their existing rules.
- Add an actual offline cached-profile regression for Alaska-style, Hawaii-style, continental
  and legacy identifiers. Independently spelled cache URLs, both 00Z/12Z profiles and the public
  weather fetch must yield the expected sound speed and interpolated 30-knot eastward wind.
- **Validation limit:** source review and the primary
  [IEM sounding endpoint](https://github.com/akrherz/iem/blob/main/pylib/iemweb/json/raob.py) and
  [network endpoint](https://github.com/akrherz/iem/blob/main/pylib/iemweb/geojson/network.py)
  confirm the provider contract. The new Rust tests and formatting did not run before the
  executor disconnected. No live-weather accuracy or runtime claim is made.

### Let exact flight evidence choose alternative recording clocks

- At each already selected spatial cell, revisit positive local peaks in the coarse clock votes,
  retaining every bin of tied plateaus. A stronger coarse alias no longer supplies the only clock
  seed; keep the best exact evidence and carry that winning clock into spatial refinement.
- Share unchanged vote accumulation between shortlisting and selected-cell rescans, with reusable
  per-worker scratch and at most one candidate per window bin. Try the original seed first and
  plateau pivots/endpoints before their interior bins. Coalesce a sweep only when all of its
  in-window timestamps were covered by successful sweeps on the same lattice, retaining unseen
  tails and fractional phases. This is timestamp coverage, not proof that physical pass
  reconstruction is seed-invariant.
- Real and decoy windows use the same search. Preserve the spatial shortlist, refinement budgets,
  bank, acoustic gates and confidence rules. Progress logs report coarse candidates and initial
  exact clock starts; refinement and diagnostic probes are outside those counters.
- Add three selector regressions and a 400-second three-flight PCM regression against a competing
  earlier clock with repeated paths and parallel acoustic explanations. Require the correct
  clock within 3 s, observer within 200 m and all three independent true flight identities.
- **Validation limit:** two independent source reviews found no concrete blocker. JavaScript
  component checks passed 16,200 plateau configurations and 2,000 fractional/large-epoch coverage
  cases; the 2,001-bin plateau used 225 starts for 2,009 timestamps. These are not Rust tests or
  production timings. The old-code PCM build was killed before test execution, then the executor
  became unavailable. New Rust compilation, formatting, regressions, workspace gates and field
  replays remain unrun. No measured recovery or performance gain is claimed; additional clock
  peaks can increase runtime, and places excluded by the existing spatial shortlist remain so.

### Recover historical winds from stations that have since closed

- Select radiosonde stations by their archive lifetimes for the requested soundings, rather
  than rejecting every station that is offline today or has an archive end date. Stations
  that opened after the request no longer consume the nearest-station selection slots.
- Include the existing 00Z/12Z brackets when checking lifetimes. IEM exposes archive dates at
  day precision, so the final UTC date remains eligible. Missing or malformed date bounds
  stay open; contradictory valid lifetimes do not exclude a possibly useful station.
  Keep the existing distance ordering, 700 km reach, neighborhood and four-station limits.
- Add date-boundary regressions and an actual offline cached-profile test: four newer stations
  must not displace a closed station's historical temperature and wind profiles, and a station
  opening at the next 00Z must remain available for that bracket. The fixture checks the
  resulting constant-temperature sound speed and 20-knot eastward wind.
- **Validation limit:** the [IEM network endpoint](https://github.com/akrherz/iem/blob/main/pylib/iemweb/geojson/network.py) confirms separate
  `archive_begin`/`archive_end` dates and current `online` status. Two source reviews checked
  date boundaries, selection, real parsers and atmosphere expectations. The new Rust tests,
  formatting, workspace gates and recording replays remain unrun because the executor is
  unavailable; no observed weather-accuracy improvement is claimed.

### Base archive and weather coverage on the selected audio timeline

- Decode the selected audio before publication-window selection. Use its actual sample count
  for the initial archive requirement and weather request, matching the existing track load,
  retry, curve catalogue and search. A short audio recording inside a longer video can now be
  searched without requiring that video's unrelated unpublished UTC day.
- Preserve the larger valid container duration only for the creation-time start/end ambiguity.
  Missing, understated and nonfinite duration tags cannot exclude the decoded timeline or
  expand a hint to infinity. Keep the existing `extra.media.duration_s` meaning and add
  `extra.media.decoded_duration_s` to report the audio duration used.
- Reuse the existing positive offline location fixture for an additional long-video case:
  losslessly copied 1,300-second audio, an eight-hour sparse video, only one cached UTC day,
  and a fixed publication clock. The old container-length request must fail; the real pipeline
  must recover both intended flights, the observer and clock under the existing assertions.
  A private clock callback makes publication checks deterministic in that test; public
  `locate` still reads the current clock separately at every publication check.
- Also retain eligible endpoints in the new region-clipped cross-track fallback after its
  existing interior probes fail. Eight dyadic boundary cases cover both ends and all four
  directions, preserving the earlier interior probe order and all final acceptance gates.
- **Validation limit:** independent source reviews and boundary arithmetic checks passed;
  these are not Rust execution. The new pipeline/FFmpeg tests and formatting remain unrun
  after executor loss. The attempted current-core command was
  `cargo --config 'profile.dev.package."*".opt-level=0' --config 'profile.test.package."*".opt-level=0' test -p fdop-core --lib --locked --offline narrow_region_does_not_skip_close_fast_flyovers -- --nocapture --test-threads=1`
  on the pinned nightly, one compiler job, two Rayon workers, opt-level 1 for workspace crates,
  and no debug/incremental output. It exited 101 after upstream `fsci-linalg` rustc received
  SIGKILL, before core compilation. A diagnostic rerun of the archived `51d9bb0` one-sided
  measurement test printed only its start before disconnection; no assertion or final outcome
  was captured. The concurrent measurement follow-up records its separate diagnosis and
  unrun revised accuracy assertions.

### Check the newly recovered pass against its physical truth

- Diagnose the previously reported one-sided-recovery test failure on `697eb4a`: all three original
  positive cases, their accuracy/direct-seed checks and their reversed, steady-tone and noise controls
  passed. The failure was the final real-pass fixture's assumption that its first trace still covered
  both sides. With restored frequencies it has zero approach support and 61.11 s of recede support;
  the existing retry runs and returns a quality-passing, reseeded event.
- Keep that fixture's PCM and survey seed unchanged, require its one-sided refusal and recovery, and
  check the original 0.5% tone, 3% speed, 5% distance and 0.5 s timing bounds. Preserve independent
  two-sided-refusal coverage with a real 6 m/s source: require an accurate fit, speed-only rejection,
  no retry and identical complete results with retry enabled or disabled. Production code is unchanged.
- **Validation:** all seven trace tests, including the prior boundary correction, passed through real
  RCH. The diagnostic recovery run reached the identified assertion after 303.09 s. Its final retried
  fit parameters were not printed, so its new accuracy checks and the separate slow-source control
  remain unrun after the executor disconnected. Independent source review passed; this is not a
  complete-suite pass and no field-accuracy claim is made.

### Keep late-evening curve searches inside their required archive days

- Stop the curve catalogue inside its final required UTC day rather than at the following midnight.
  Inclusive archive fetching previously demanded an unrelated extra day, preventing an evening curve
  search from using an otherwise complete offline cache. Derive the final day from the real recording's
  padded endpoint, preserving exact-midnight and crossing coverage and the published archive limit.
- Add an actual cached-store/matcher regression for a 23:10–23:14Z clock and 120-second recording.
  It requires all 48 intended decoys, the real recording's padding and every decoy's existing 60-second
  catalogue guard. Additional cached-fetch cases cover just before, exactly at and just after the
  padded midnight boundary, plus a recording crossing midnight.
- **Validation limit:** formatting and independent source review passed, and an independent arithmetic
  check confirmed the day boundaries and 40 earlier / 8 later decoys. These are not Rust test results.
  Application test compilation did not reach these regressions before the executor became unavailable;
  their Rust execution and full workspace checks remain pending. No measured accuracy or runtime
  improvement is claimed.

### Recover a narrow observer region between cross-track samples

- A valid region can lie entirely between the regular 350 m cross-track samples, while the
  existing distance-only fallback chooses an otherwise valid point outside that box. Such a
  pass previously contributed no hypotheses despite its observer satisfying the acoustic gates.
- Preserve the regular grid and original distance fallback. Only when a side still has no
  accepted point, clip its affine latitude/longitude ray to the requested box and probe the
  clipped interval. Bound distance bracketing to 24 bisections per half; accept only actual
  terrain-dependent samples through the unchanged height, range, region and clock predicate.
- Add compact regressions for axis directions, diagonal clipping, rising/falling range
  brackets, missed boxes, invalid values and discontinuous terrain, plus a full two-flight
  search requiring both intended matches, less than 75 m position error, less than 0.2 s clock
  error and no comparison truncation.
- **Validation limit:** independent source reviews and 13 JavaScript arithmetic cases passed.
  These are not Rust tests. The fresh core build was killed by SIGKILL in upstream
  `fsci-linalg` before compiling project code; Rust regressions, formatting, workspace gates
  and reference replays remain unrun after the executor disconnected. This is targeted strip
  recovery, not exhaustive recovery of every terrain pocket or a measured accuracy claim.

### Keep better flight positions when cached track stores overlap

- Prefer a non-MLAT report over a duplicate MLAT report even when the MLAT timestamp is
  slightly earlier. Previously time-first sorting discarded the later, better position,
  which could introduce a false bend or altitude jump where daily stores overlap.
- Preserve the original merge decisions, duplicate intervals and equal-quality report
  identities with indices into the joined reports. Apply better-quality substitutions only
  after grouping is complete and only within each surviving interval. A third overlapping
  store cannot resurrect a discarded same-quality report, widen an interval or reorder
  the resulting trajectory. Same-store legs remain separate.
- Allocate this bookkeeping only for actual joins. Unjoined legs keep their existing
  point vectors; retain existing metadata/source handling, key format and strict 0.05 s cutoff.
- Add eight regressions covering earlier/equal/later MLAT duplicates at midnight, interval
  boundaries, interleaved three-store chains, equal-quality ties, reintroduced timestamps,
  join limits and separate same-store legs.
- **Validation limit:** independent source review and JavaScript model comparisons support
  the selection and grouping logic. The exact Rust regressions, formatting, full workspace
  checks and reference replays could not run because the executor is unavailable. No
  measured accuracy or runtime improvement is claimed.

### Retain close fast aircraft when sampling outside the search region

- Use the existing distance/velocity timestep on both sides of the padded reach boundary.
  The previous ten-second jump outside the box could skip a whole close pass through a small
  region, leaving an otherwise eligible event with no flight hypotheses. Speed, distance,
  terrain, gap, budget and confidence checks retain their existing values.
- Add a regression using real track interpolation, propagation and full search with two
  orthogonal synthetic passes at 250 and 263 m/s, at the published reference coordinates.
  It verifies the true CPAs pass the existing gates, requires hypotheses for both events,
  both intended flight matches, less than 75 m position error, less than 0.2 s clock error
  and no truncated comparison.
- **Validation limit:** two independent source reviews and a numerical geometry check support
  the failure mechanism: all old ten-second probes lie outside the padded boxes, while the
  normal cadence puts seven and eight probes inside. The Rust regression, formatting check,
  workspace gates and reference replays remain unrun after the executor became unavailable.
  More sampling outside the box changes the scan phase and costs additional work; no
  unchanged-output, runtime or real-recording accuracy claim is made.

### Preserve the original sub-bin correction at a spectral basin boundary

- The multi-bin restoration walk could reach the neighbouring basin-boundary bin and discard a
  valid three-bin interpolation that the original code accepted. With normalized samples
  `[8, 12, 10]` and restored samples `[12, 11, 5]`, the concave raw vertex is -0.7 bins,
  safely between the selected bin and its boundary; retaining the normalized vertex instead
  creates a 0.216667 Hz error at 0.25 Hz spacing.
- Try that original concave, finite, at-most-one-bin correction first, with the current guide-band
  guard. When it is unavailable, retain the multi-bin basin walk unchanged. Qualification, SNR/z,
  gates, fixtures and tolerances are unchanged. Add an analytic regression mirrored across both
  boundaries, checking the exact vertex and unchanged normalized evidence.
- **Validation limit.** Independent source review and exact f32 fixture arithmetic support this
  narrow correction. The executor became unavailable before its Rust formatting, compilation or
  regression could run. Earlier passing tests and Clippy cover the preceding source through
  `51d9bb0`, not this follow-up.
- **Known unresolved test failure.** The broader checks reached 128 distinct passing core tests,
  but `a_one_sided_refusal_is_measured_from_a_free_trace_of_its_ridge_and_tones_are_not` reported
  failure before the process was killed by SIGKILL. Its captured assertion was not printed, and
  a focused rerun was also killed after the clean positive recovery. The failing assertion and
  cause remain unknown; this separate boundary correction is not claimed to resolve it. The
  full core suite, application gates and field-recording replays remain incomplete.

### Stream decoded audio without retaining a second full PCM recording

- Convert FFmpeg stdout incrementally into the final sample vector with a 64 KiB byte buffer.
  Preserve partial sample words across reads, retry interrupted reads and reject truncated words.
  This removes the full raw-byte copy previously held alongside the decoded floats (57.6 MB per
  hour at 4 kHz mono); the final vector remains resident and may have spare capacity. No measured
  total-memory reduction or end-to-end speedup is claimed.
- Drain decoder stderr concurrently while retaining at most 64 KiB of its original bytes. Keep
  truncation visible, preserve failure diagnostics, close stdin as before, and reap the decoder
  on sample-read or worker-start failure. Keep the existing selection and timestamp filters.
- Add fragmented/interrupted-read and bounded-diagnostic regressions, plus a real FFmpeg
  integration comparing every decoded sample byte with the old buffered path and checking
  nonzero decoder failures.
- **Validation:** the complete actual audio module, built with the pinned nightly and real
  shared-type/serde dependencies, passed four ordinary tests and all four opt-in FFmpeg/FFprobe
  integrations, including the existing channel/timestamp cases. Formatting passed. The published
  source was checked against the tested snapshot's SHA-256 after executor recovery failed.
  These are component checks; workspace gates, release selftest and real-recording replay were
  not completed for this change.
- Follow-up validation of the earlier weather-deadline repair: all 13 actual helper/test component
  checks passed with the real atmospheric types, including the seven new regressions; weather
  and pipeline formatting passed. This does not validate the full data crate or network pipeline.

### Load the metadata attempt before requiring the full date range

- Resolve the recording's metadata clock before archive availability and data loading. A timestamp
  search now requests only its own tracks and weather, so an offline cache for the relevant day is
  usable even when surrounding dates are uncached. If the hint is unavailable, its ADS-B load fails,
  or neither the trace nor curve search finds an answer, load and search the full date window once.
  Explicit `--start` remains binding, including publication clipping and midnight coverage.
- Each attempt owns its publication bounds, tracks, atmosphere, wind gates and search result.
  Reuse audio measurement and terrain, preserve independent-event caps, and report the final
  attempt's data. Keep the existing rule preventing a narrow curve verdict from grading a wider
  retry. Preserve upstream weather progress and usable-profile fallback handling.
- Share the curve catalogue span between ADS-B and weather requests. Eligible curve decoys receive
  the same weather coverage, including earlier/later radiosonde brackets and one-sided decoy
  placement at archive edges; disabled or ineligible curve searches retain trace-only coverage.
- Add a real 1300-second synthetic media-to-location regression with creation-time metadata and
  one cached archive day, plus unavailable-hint, explicit-clock, empty-search fallback and weather
  coverage regressions. Extract the existing selftest synthesizer without changing its warm/windy
  run; the offline regression supplies the same standard atmosphere its real pipeline will use.
- **Validation limit.** The pipeline and its new regressions have undergone source review and
  formatting, but full application test compilation and workspace Clippy were killed by SIGKILL
  inside upstream `asupersync` before reaching this crate. These pipeline tests remain unrun.
  The actual `src/selftest.rs`, compiled unchanged through an external entry point against the
  fresh core libraries, passed decode/detect/measure/search with both intended flights: 2.365 m
  position error, 0.0249 s clock error and the existing 75 m radius. That validates the real core
  chain and extracted synthesis, not the metadata pipeline or the full CLI release build.

### Detect faint aircraft beyond the original speed bank

- Append faster subsonic templates after the existing primary, edge and contrast stages. Continue
  the original grid's last speed ratio above 175 m/s, up to the existing 0.8 c measurement bound:
  approximately 194, 215, 238 and 263 m/s at the default sound speed. Keep earlier detections and
  their order, residual masking, the shared candidate budget and detection cap. Added seeds must
  clear the existing strong line; no detector or measurement acceptance threshold is lowered.
- Permit supplemental seeds to refine within the existing measurement domain while preserving the
  original refinement path for legacy seeds. Count an event's faster coarse seed as measured past
  the old line, even if refinement lowers its speed, so added evidence retains the existing grading
  safeguard. This does not change aircraft-type filtering or independent-event caps.
- **Validation.** All four focused fast-bank tests passed through real RCH. Three loud PCM scenes
  at 190, 215 and 245 m/s retain the original accuracy assertions. A complete 14-scene attenuation
  ladder additionally establishes one new recovery: a 215 m/s, 5 km pass attenuated by 18 dB yields
  no baseline event, but the supplemental bank fits 118.01067 Hz (truth 118), 214.97648 m/s,
  4999.652 m and 80.36653 s (truth 80.37). Noise/hum and reversed passes yield no aircraft event;
  stage-prefix, detection-cap and speed-bound regressions pass.
- Legacy detections can still mask some faster curves before this final bank runs. The synthetic
  recovery does not establish that the previously reported field miss is fixed. Full application
  compilation was killed in upstream `asupersync` before pipeline tests ran; the provenance test,
  workspace gates and reference-recording replay remain pending.

### Recover unbiased frequencies after a long tonal asymptote

- A pass that dwells near its asymptotic frequency can contaminate the row background enough to
  displace the normalized peak by several FFT bins. Restoring only the selected three bins then
  misses the raw peak and biases the fitted tone, closest distance and arrival time.
- After the unchanged normalized qualification, climb the restored peak only inside that peak's
  strictly descending spectral basin. Stop at a valley or flat noise floor; accept a concave local
  interpolation only inside the original guide band. Keep the original frequency if restoration
  would leave that basin or band. SNR, robust-z and all measurement acceptance gates are unchanged.
- Add analytic sub-bin vertex checks across positive/negative backgrounds and slopes, and controls
  preventing a hidden hum or an out-of-band vertex from relocating the qualified ridge.
- **Validation.** All six trace tests and the real-PCM harmonic-family recovery regression passed
  together through real RCH with the pinned toolchain. The recovered 118 Hz, 120 m/s, 1500 m pass
  fits at 118.00009 Hz, 119.99997 m/s, 1500.071 m and a 110.00011 s arrival (truth: 110 s),
  preserving its original accuracy assertions and independent crossing-aircraft check. Before this
  fix it fitted 119.09793 Hz and 109.54688 s. Full workspace and reference-recording checks remain
  pending; the synthetic correction does not establish field accuracy or confidence calibration.

### Hear every returning pass in the whole-recording curve search

- Coarse curve matching now keeps every sampled local closest approach, rather than only the closest
  visit within each six-minute chunk. A common sampling lattice and ownership interval prevent lost or
  duplicate visits across overlapping chunks and shortened final chunks. Scratch storage stays bounded
  by one chunk's sampled positions. Real and decoy windows use the same path and evidence gates.
- Interpolate interior closest approaches from squared-range curvature, then evaluate the actual
  Hermite trajectory. This removes the straight-chord timing bias exposed by a fractional circular
  pass. Preserve the existing segment projection at real track endpoints and reject catalogue edges.
- **Validation.** All three focused `coarse_stamps_` regressions passed through real RCH with the pinned
  toolchain: six repeated circuit visits across different chunk partitions; straight, fractional and
  boundary approaches; and synthesized PCM with a returning aircraft plus an independent crossing.
  The PCM case uses the production template bank and locates within 150 m and 2 s while crediting at
  least two circuit visits and the crossing. No acceptance tolerances or calibration were relaxed.
  Workspace checks, release selftest and reference-recording replay remain pending; these synthetic
  results do not establish field accuracy.

### Keep usable weather when another source stalls

- **The fault.** After pressure-level weather failed, the surface and radiosonde requests ran concurrently,
  but neither result reached the pipeline until both had joined. A slow radiosonde request could therefore
  exhaust the pipeline deadline and replace already retrieved historical surface temperatures with 15 °C.
  The reverse stall also discarded usable radiosonde profiles.
- **The change.** A progress-capable fetch publishes each usable fallback directly from its source thread.
  The existing synchronous API still returns the same final source selection, including the combined
  radiosonde/hourly-surface atmosphere when both succeed. Profiles are shared immutably.
- The pipeline keeps these provisional results under its existing single 190-second wait budget. At a
  timeout or early channel disconnection, a received profile survives; winds aloft take precedence over a
  later surface-only update. The standard 15 °C atmosphere is used only if no profile arrived. Warnings
  describe missing data and incomplete retrieval; an unfinished fetch does not acquire a completed timing.
- Two weather regressions cover channel-controlled completion in both source orders, the eventual real
  combined atmosphere, and failed sources producing no provisional success. Five pipeline regressions
  cover cold surface weather at the deadline, wind preference in either order, final-result replacement,
  the no-profile default and early disconnection. Existing pressure-level and fallback-selection tests remain.
- **Validation limit.** Source and independent reviews completed, but the executor became unavailable
  before rustfmt, compilation or these tests could run. The complete patch was recovered through GitHub.
  Workspace checks, selftest and reference/negative replays remain unconfirmed; no real-recording accuracy
  or confidence improvement is claimed.

### Scale candidate deduplication without dropping comparisons

- Large candidate groups sharing the same event-flight matches now use a spatial/clock index before
  the original strict 300 m and 5 s duplicate test. Small groups retain the direct scan; unusual
  coordinates or clocks retain the original fallback. Only retained candidates enter the index,
  preserving greedy representatives and output order without introducing a search cap.
- A copy of the previous algorithm is the regression oracle. Tests cover shuffled candidate clouds,
  reordered match sets, strict distance/clock boundaries, discarded-middle-point chains, cell edges,
  poles, the antimeridian and nonfinite or out-of-domain inputs.
- `benchmark_dedupe_large_clouds`, compiled from the actual code with pinned optimized Rust on a
  shared AMD EPYC 9V74 host, preserved the exact retained identities and order in both 16,000-candidate
  cases. At 100 m spacing, 1,370 were retained: 300.219 ms before, 27.336 ms indexed. At 1 km spacing,
  all were retained: 3.885 s before, 62.172 ms indexed. These single trials measure deduplication only;
  no whole-search or real-recording speedup is claimed.
- **Validation.** Both new regressions and the benchmark passed in the standalone actual-code harness;
  the changed source passed rustfmt. The focused Cargo build was killed by SIGKILL inside upstream
  `fsci-linalg`, before compiling `fdop-core`. Workspace tests, core Clippy, selftest and reference/negative
  replays remain unconfirmed. Separately, the integrated cache changes now passed all 22 `fdop-types`
  tests (including seven store tests) and that crate's all-target Clippy check with warnings denied.

### Say which scatter the model-misfit warning divided by

- **The fault.** The model-misfit test divides a fit's RMS by the trace's local scatter, floored at 0.03 Hz, but
  the warning printed the measured scatter beside a ratio taken from the floor: "RMS 0.39 Hz is 13.0x the
  trace's local scatter (0.00 Hz)". `Diagnostics::local_sigma` was documented as a point-to-point scatter. It is
  1.4826 x the median distance of each point from the median of the points within +-1 s, and on a monotone run
  of points sampled every frame that median is the point itself. The scatter is therefore 0 on most traces, and
  the test reads as an RMS over 0.3 Hz.
- **The change.** When the floor was used the warning says so ("(0.000 Hz, taken as the 0.03 Hz floor)"), the
  floor is the constant `LOCAL_SIGMA_FLOOR_HZ`, and the docs say what the scatter measures. The misfit test
  itself, the warning's place among the others, its `model misfit` prefix (which the search reads) and every
  answer are unchanged. A unit test covers the ratio and wording on either side of the floor, the cases with no
  warning, and a scatter of exactly 0 on a densely sampled constant-velocity curve. On six of the recordings the
  development rows and nulls use (all but the eagle-nest clip), `fdop detect` with this change (bench-built on
  `6c48541`, binary `d50592c8`) writes the same traces, images, stdout and stderr as `e93a269d`, byte for byte,
  and the same `events.json` apart from 189 reworded misfit warnings. Nothing reads the reworded part, so no
  answer can change on any row.
- **Evidence.** `fdop detect` with the bench-built binary `e93a269d` (the contrast stage's tree), its traces
  re-read with a replica of the estimator. On those six recordings, the replica agrees with `fdop` on which of
  the 328 fitted events have a scatter of 0 for 324. Of the 123 events that passed the quality gates, 88 have a
  scatter of 0 and 92 carry the warning. On set D, 72 of the 92 events that passed have a scatter of 0 and 62
  carry it. A second-difference estimate of the same traces' noise is 0.002 Hz at the median (0.001-0.008 at the
  10th and 90th percentiles) on the six recordings and 0.008 Hz on set D (0.002-0.031), also mostly under the
  floor: the points are 0.1 s apart from frames of at least 0.5 s, so neighbours share most of their audio and
  scatter far less than the 0.25 Hz bins. Measuring the scatter that way instead would add the warning to 3 and
  9 events and take it from none, so no estimator change is made here. Every event under the warning is also
  weighted half in the search and may take one change of tone. On `9087ec4`'s R1 development rows (`bench`
  `runs/r1_fdop_9087ec4`), four right answers (osprey_1031, osprey_1101, eagle, duke) have a core match that
  uses a tone change, and so does the one wrong medium (osprey_1102b, e12), so removing that freedom is not a
  clean change either.

### Preserve the physical pass when an aircraft returns nearby

- Add a streaming iterator over local closest approaches within a time window. It retains the
  existing two-second coarse scan and millisecond refinement, with constant scan storage. The
  original globally closest approach remains available to callers that need it.
- When a hypothesis already identifies a pass time, recover the interior local approach nearest
  that time. Use it throughout pair evaluation, null calibration, clock checks, refinement and
  explain-at diagnostics, so a closer return does not silently replace the selected pass.
- Confirmation examines each local approach and applies the existing distance, speed, clock,
  coverage and trace-fit gates to it. Duplicate exclusion compares the recovered physical pass:
  a returning aircraft can confirm another event, while the same pass cannot count twice.
- Add straight-pass and window-boundary regressions plus an independently synthesized narrow
  circuit case. The circuit checks pair identity, confirmation and duplicate exclusion in still
  air and wind, and the full search/refinement path with a third event used only as a confirmer.
- **Validation.** These regressions were added but have not completed compilation or execution:
  the executor disconnected during the core build and remained unavailable during recovery.
  Workspace tests, Clippy, selftest and reference-recording replay remain unconfirmed.

### Recover a usable harmonic when its primary measurement fails

- Measure each detection family's primary first. If it fails the existing quality or aircraft
  eligibility gates, try its detected harmonic siblings in descending score order, with detection
  ID breaking ties. Return the first usable sibling, or the original primary refusal when none
  passes. An already usable primary follows the same measurement path as before.
- Use the shared family measurement in locate, detect and the detector example. Keep one result
  and one independent-event opportunity per family, preserve the measured sibling's harmonic link,
  and retain the existing final duplicate grouping and 24-event cap.
- Count a recovered harmonic as measured past the old line even when its original primary was
  strong. A medium answer relying on that added evidence still needs the existing curve-match
  judgment; recovery does not grant the primary's old calibration status.
- Add three PCM regressions for a broken bright harmonic with a recoverable fundamental and
  another independent flyover, an unchanged usable primary, and retention of the primary refusal
  when no sibling is usable. Extend the measurement-line regression for recovery provenance.
- **Validation.** Synthetic regressions were added, but core compilation and test execution did
  not complete before the executor became unavailable. The recovered source has not been rerun;
  real-recording accuracy and confidence calibration remain unvalidated.

### Bound STFT working storage for long recordings

- Replace the complete column-major spectrogram and its per-frame allocations with a reusable block
  of at most 256 frames and 4 MiB of frequency values, followed by a copy into the final row-major
  output. An exceptionally wide band still requires one full column; FFT worker scratch and the
  final output remain separate allocations.
- Drain FFT trace records between blocks. Preserve the FFT, Hann normalization, frequency bins,
  frame centres, return type and public parameters. This is a storage change; no speedup is claimed.
- Add a direct-frame comparison covering a single frame, multiple blocks and a tail, cropped bands,
  zero padding, nonzero time offsets and the byte-budget boundary. It checks every output value
  bit for bit and checks both axes.
- **Validation.** The new regression and workspace checks did not complete before the execution
  environment became unavailable. The patch was recovered through GitHub after the outage; its
  final source has not been compiled or rerun. No reference-recording improvement is claimed.

### Publish complete track caches under concurrent searches

- `TrackStore::save` now creates an exclusive staging file for each writer in the destination's
  directory. Two searches of the same day and region no longer truncate or modify the same staging
  inode while one of them publishes it. A reused process ID or a leftover staging file is skipped;
  compression, flush and sync still finish before rename.
- `TrackStore::load` extends the footer validation in `6c48541`: it also rejects bytes after the
  first compressed member, including a second store, an empty member and raw garbage. The existing
  fallback to another covering cache or a fresh download handles these invalid stores.
- Regressions exercise four concurrent writers with an active reader, staging-name collisions,
  and trailing compressed data; the upstream footer and undeclared-payload regressions remain.
- **Validation.** Before integration with `6c48541`, all five store tests passed on the pinned
  nightly through RCH with one build job and temporary unoptimized development-profile settings
  (`cargo test -p fdop-types --locked --config 'profile.dev.opt-level=0' --config 'profile.dev.debug=0'
  --config 'profile.dev.package."*".opt-level=0' store::tests`). The integrated source passed rustfmt
  and `git diff --check`. The executor then became unavailable during the core build, before the
  seven-test rerun, workspace checks, Clippy or selftest could finish. Those remain unconfirmed.

### Validate cached track stores through the gzip footer

- **The fault.** The store reader returned after the declared legs and points. The gzip decoder can
  supply those bytes before reading the footer, so a missing footer or an invalid CRC32/ISIZE could
  evade validation and make an incomplete or damaged cache look usable.
- **The change.** Loading now reads through the end of the gzip member after the last declared record.
  A decoder error rejects the store; an extra decoded byte rejects undeclared payload. This lets the
  existing covering-cache recovery try another store or download again. Offline runs can try another
  valid covering cache, and otherwise report the missing usable data.
- **Regression coverage added.** Nonempty and empty stores with each of the last eight footer bytes
  truncated or changed must fail loading; restoring the valid bytes must work. A separate test adds
  undeclared data inside a valid gzip member and expects an invalid-data error. The existing roundtrip
  shares the same synthetic fixture.
- **Validation limit.** The complete repair passed the pinned rustfmt check after the executor briefly
  recovered. A single-job fdop-types test build then lost the executor before completion, so tests,
  Clippy and selftest remain unconfirmed. The patch was also reviewed against the reader and decoder
  behavior. No real-recording or runtime validation is claimed.

### Refresh early partial ADS-B listings after publication

- **The fault.** A cached release listing was accepted as final once the archived day was more than
  two days past its end, even if the lookup itself happened before MLAT or prod was published. An early
  prod-only lookup could therefore keep later runs on an incomplete catalogue indefinitely.
- **The change.** An incomplete listing is final only when its successful cache write occurred after
  the publication window. Earlier listings are rechecked online; complete prod-plus-MLAT listings and
  offline reads retain their existing behavior. A missing modification time also requires an online check.
- **Regression coverage added.** Two deterministic on-disk tests cover a provisional listing on an old
  day, a settled partial listing, exact publication boundaries, complete early listings, staging plus
  MLAT still missing prod, unavailable timestamp evidence, and the public offline cache-reading path.
- **Validation.** The complete repair passed the pinned rustfmt check when the executor briefly
  recovered. Workspace test compilation was killed with exit 137; focused fdop-data builds lost the
  executor before completion. Test, Clippy and selftest results remain unconfirmed.
  No real-recording accuracy improvement is claimed.

### Score a compressed survey image once more, contrast first

- **The fault.** A loud nearby pass swells a short clip's level by 20 dB or more around its CPA. The survey
  image takes each row's robust z before the frame contrast removes the swell, so the swell inflates every row's
  MAD and compresses the pass's own ridges. The spread floor (`min_image_scale`) raises the whole image, but it
  cannot restore a ridge's contrast against its row. On set D's N1387U_R44 (33 m/s at 421 m), a 157 Hz tone
  33 dB over its neighbours at the CPA had a row z of 1 to 1.5, and nothing in its image scored over 2.88.
- **The change.** When the floor raises an image, `survey` also builds a second one from the same pooled and
  clamped levels: each frame's contrast over +-1/3 octave first, then each row's robust z, raised to the floor
  (`LogSpec::contrast`). After the passes and the edge stage, the full bank scores it on the residual of every
  accepted curve at the strong line, 4.5 (the lines were set on the survey image, not this one). Its candidates
  go through the same suppression and marking rules. Its detections follow all the others, strongest first, in
  the room left under the detection cap, and carry `from_contrast` (`"from_contrast": true` in `events.json`).
  Every other detection keeps its order, id, harmonic grouping and measurement; a primary may gain harmonic
  ratios, which nothing in measurement reads. No null recording is under the floor, so no null run tests the
  stage. An answer resting on a contrast detection therefore counts as measured past the old line: the
  measurement-line gate lowers a medium answer on one unless a curve match judged it.
- **What did not work.** Replacing the survey image with the contrast image found 39 passes but was right at
  22 / 29 / 29 / 31 / 33, under `3f408f4`. Scoring the second image from the silence line gave the same
  answers as the strong line with 101 added events on set D instead of 54. Taking it from the levels without
  the 20 dB ceiling made weaker curves of its own in a probe on swelled scenes (strongest 4.07 at 35 dB and 4.82
  at 40 dB from the silence line, against 5.26 and 9.58 with the ceiling). At the strong line, though, it put
  N814SR_R07's first event at 91 m/s and 565 m, right only at 30/60 % (right from 15/30 % on `3f408f4`), and was
  right on set D at 22 / 28 / 30 / 33 / 37. The strong line and the ceiling were chosen on this development set.
- **Evidence.**
  - Metamorphic, on 11 scenes: an image at or above the floor has no contrast image and detects exactly as
    before. Under it, every detection without the stage is in the result with it, first and in order, field for
    field. Each added one is flagged, has a half of at least 24 s and a score of at least 4.5, and they are
    sorted. Swells of noise alone add nothing at 20, 25 and 30 dB. At 35 dB they add two curves (5.26 and 4.56)
    and at 40 dB 18 (up to 9.58).
  - A faint 300 Hz pass (90 m/s, 900 m, CPA 22 s) swelling by 20 or 25 dB with its noise is on no detection of
    the survey image, raised or not. The contrast stage finds it at 21.5 s, its refined seed within 1 % of the
    curve. Measured from that detection it passes every gate at 90.02 m/s and 901.0 m, CPA 22.00 s, and
    nothing else passes. The same audio reversed, and swells of 30, 35 and 40 dB alone (1, 4 and 17 measured,
    up to 16 from the contrast stage), give nothing that passes.
- **Set D** (`fdop detect` on the 40 Livermore clips, read with `bench` `tools/score_d.py`; local runs of the
  bench-built binary `e93a269d`). With the stage: 39 found (38), 37 first event right (36), medians 5.5 m/s
  (6.2 %) and 164.1 m (11.5 %), against 5.5 m/s (6.5 %) and 212.2 m (11.0 %). Right at 10/20, 15/30, 20/40,
  25/50 and 30/60 % of speed and distance:

  | Tool | Found | Right at the five tolerances |
  |---|---|---|
  | fdop `3f408f4` | 38 | 22 / 29 / 31 / 33 / 36 |
  | fdop with the contrast stage | 39 | 23 / 30 / 31 / 34 / 37 |
  | dopplerprint | 39 | 13 / 19 / 21 / 24 / 26 |
  | dopplertrack 60-300 Hz | 37 | 17 / 23 / 27 / 29 / 35 |
  | dopplertrack 40-450 Hz | 38 | 19 / 23 / 26 / 30 / 36 |

  The stage adds 54 events on 11 of the 12 clips under the floor; 18 pass the gates, 15 of them within 15 s of
  the true CPA (two of the other three are on clips with no trusted time). Three clips' answers changed.
  N1387U_R44 is found, 41.4 m/s at 476 m, right from 25/50 %. On N814SR_R07 (90 m/s, 1166 m) a new 248 Hz line
  comes first, 98.2 m/s at 1142 m, right from 10/20 %.
  N451EE_R45's first event is now a contrast detection on a 271 Hz line, 61.1 m/s at 531 m for 64 m/s and
  695 m, right from 15/30 % (54.5 m/s at 431 m, right from 20/40 %, before). Every other clip's answer is the
  same. fdop now has more right answers than every rival at every tolerance and finds as many passes as
  `dopplerprint`. Its one miss is N958JX_R50 (215 m/s at 5 km, past the bank's 175 m/s; no tool gets it right).
- **Falsifier 2 and the development rows.** On the six recordings the nulls, development rows and wrong dates
  use (ref_raw, osprey_1031, osprey_1101, osprey_1102b, duke_1101b, feederwatch), none is under the floor, and
  `detect` writes byte-identical `events.json`, traces, stdout and stderr (46-60 events each) with and without
  the stage. `locate` measures exactly as `detect` does, so no null or development answer can differ from
  `3f408f4`'s.
- **Correction (2026-11-07): the development rows use a seventh recording.** The eagle-nest clip (b2_eagle,
  `swfl_20241228.m4a`) is under the floor, so the paragraph above is wrong for it. With the stage, `detect`
  keeps its 41 events field for field and adds 19 contrast detections after them. 4 of those pass the quality
  gates, and one (18 m/s) is under the search's aircraft bounds. `locate` with the bench's R2 arguments, run
  offline from one local cache for both builds (no cached weather, so not comparable with the bench's rows):
  `3f408f4` is right and low at 0.49 km from the site, and with the stage it is right and low at 0.29 km. Both
  fail the same gates (core matches, radius). R1 is not reported here.

### Score a pass heard near a recording's start or end with a short template

- **The fault.** A short transition (shortest 1.25 d/v at or under `min_half_s`, 24 s) is scored by a 24 s
  template, which needs 8 of its 12 path samples on each side: about 16 s of recording before and after the
  CPA. A nearer CPA has no template that can score it, however loud the pass, and a clip under about 32 s gives
  the bank no cell at all. On set D's N451EE_R45 (64 m/s at 695 m, its ridge crossing f0 about 14 s into a
  40.6 s clip) no detection reached the silence line.
- **The change.** `DetectParams::edge_half_s`, 10 s by default (0 turns it off). Each (v, d) whose shortest
  half is the 24 s floor gets an edge template of that half. After both passes, the edge templates score only
  the cells within 24 s of the first or last frame that their 24 s template cannot cover. They score on the
  residual of every accepted curve, from the silence line, and their candidates go through the passes'
  suppression, residual and marking rules. Every detection the passes accept is the same with or without the
  stage, which only adds detections. Only the frames within 24 s of each end are scored, and the stage does not
  run when the passes stop at three times the detection cap. `refine` keeps a detection's half when it is under
  24 s, so an edge detection's refined score stays finite (over the 3 d/v half it was -inf at the edge, written
  as null in `events.json`).
- **What did not work.** Edge templates inside the passes, scoring wherever their template lacked coverage, ran
  over most of a short clip for long transitions (a template half up to 240 s). The events of 31 of the 40 clips
  changed (26 with 10 s templates), and N451EE_R51 lost its right answer: a steep wrong edge template took its
  ridge first. Restricted to short transitions but still inside the passes, they changed three or four clips and
  cost N451EE_R51 the same way. After the passes, 12 s edge
  templates found N451EE_R45 but listed a wrong steep fit (37 m/s, 204 m) first, and 8 s ones lost N500MG_R17
  at 10/20 %. 10 s was the one half that gained without a loss. It was chosen on this development set.
- **Evidence.**
  - A direct per-cell oracle matches the edge bank bit for bit, score and template index, for 8-, 61- and
    251-frame images. The edge templates score 824 cells near the ends of the longer two and none of 8 frames.
    Partial windows score their cells exactly as the full window and leave the rest at -inf.
  - In a 30 s clip the passes return nothing. A short pass heard 9 s in (600 Hz, 64 m/s, 700 m) is found by an
    edge template at 11.5 s and 584.8 Hz, 1.4 % above its curve there. Measured from that detection it
    passes every gate at 599.99 Hz, 64.00 m/s and 700.1 m, CPA 9.00 s. The same audio reversed gives one
    candidate, refused, and the noise gives none.
  - Metamorphic: every detection without the stage is in the result with it, bit for bit, and each added one is
    an edge template's within 24 s of an end. In a 40 s clip a pass heard 9 s in is found by the passes with a
    wrong shape (21.5 s, 40 m/s, 823 m), and the stage adds nothing on its marked ridge. A pass heard 9 s
    before the end gets an added detection at 4.96. A 600 s interior pass with a flat tone, 40 s and 30 s of
    noise, a flat tone, and a tone sounding for the first 12 s add none.
- **Set D** (`fdop detect` on the 40 Livermore clips, read with `bench` `tools/score_d.py`; local runs of the
  bench-built binary `5ec57458`). With the stage: 38 found (37), 36 first event right (35), medians 5.5 m/s and
  212.2 m (198.2 m). Right at 10/20, 15/30, 20/40, 25/50 and 30/60 % of speed and distance:

  | Tool | Found | Right at the five tolerances |
  |---|---|---|
  | fdop `4c30cd1` | 37 | 22 / 29 / 30 / 32 / 35 |
  | fdop with the edge stage | 38 | 22 / 29 / 31 / 33 / 36 |
  | dopplerprint | 39 | 13 / 19 / 21 / 24 / 26 |
  | dopplertrack 60-300 Hz | 37 | 17 / 23 / 27 / 29 / 35 |
  | dopplertrack 40-450 Hz | 38 | 19 / 23 / 26 / 30 / 36 |

  Two clips changed. N451EE_R45 now has four events. The first listed, at 54.5 m/s and 431 m for 64 m/s and
  695 m, is right from 20/40 % up; `dopplertrack` gives 57 m/s and 455 m, and `dopplerprint` 33 m/s and 174 m.
  N563FX_R21 gains a fifth event (29.1 m/s, 147 m) after its first. At every tolerance fdop has at least as
  many right answers as any rival, and more except at 30/60 %, where it ties the wide `dopplertrack`. It still
  finds one pass fewer than `dopplerprint`. Its two misses are N1387U_R44 (33 m/s at 421 m; nothing scores over
  2.88 in fdop's image) and N958JX_R50 (215 m/s at 5 km, past the bank's 175 m/s; no tool gets it right).
- **Falsifier 2 and the development rows.** On the six recordings the nulls, development rows and wrong dates
  use, `detect` gives the same events (46-60 each) and the same candidate count with and without the stage. On
  osprey_1031, osprey_1101, duke_1101b and feederwatch the passes stop at the cap (180+ candidates), so the
  stage never runs. On ref_raw (82) and osprey_1102b (125) it runs and adds none. `locate` measures exactly as
  `detect` does, so no null or development answer can differ from `4c30cd1`'s. The seventh recording, the
  eagle-nest clip (b2_eagle), was checked on 2026-11-07: the same 41 events, traces, stdout and stderr,
  byte for byte, with and without the stage.
- **The replays the entry below promised** (`44ff60a`, whose locate output `4c30cd1` matches; local runs on an
  Apple M5 Ultra, the later rows written to NFS when the local disk filled). Of the 33 nulls, none is high or
  medium: 27 are low, 6 give no location, and the curve match stands on none. The 15 development rows have the
  same grades and distances as `d9dfc88`'s. The reference row and the osprey cam's three days are high, 250 m,
  90 m, 110 m and 220 m off, and Duke Farms is high 0.96 km off. Sapsucker Woods gives no location, and the c
  and wrong-date rows are low or give none. The reference recording's two wrong dates are low: `n_ref_on_0907_gv` has one core match of three
  and a 1.70-nat margin, and `n_ref_on_0909_gv` has both core matches and a 0.54-nat margin, under the medium
  line. The Nassau reference configuration is high (`r_nassau_gv`, margin 14.43).

### Trace a one-sided refusal once more without the model

- **The fault.** A detection whose template has the wrong shape meets the pass's ridge over one stretch and
  leaves it elsewhere by more than the guided band. The trace then holds one side of the pass and the gates
  refuse the fit, though the trace does lie on the ridge where the curve met it. On set D's N394FX_R20 (135 m/s
  at 1758 m) the bank's best template sat at its top speed, about 175 m/s at 3650 m; the guided trace held 86 s
  of approach and no recede, and its fit drifted to 326 m/s at 137 km.
- **The change.** When the gates refuse a fit whose trace covers less than `min_side_s` (8 s) on either side,
  `ridge_trace` (strongest-peak rule, 1.5 s integration) follows the ridge by its own slope from the refused
  trace's first, strongest and last points, inside the event's analysis window. The free trace is fitted on both
  sides. If that fit is plausible (15 m/s to 0.8 c, 60 m to 25 km), the guided rounds start again from it. The
  second attempt replaces the first only if every gate accepts it; `EventQuality::reseeded` (`reseeded` in
  `events.json`) marks it. Accepted fits, refusals whose trace held both sides, and traces too short to fit are
  measured exactly as before. `MeasureParams::free_reseed` (on by default) turns it off. This is the restricted
  form of the second start removed in the entry below: that one also retried two-sided refusals, and two of the
  five events it turned into accepted ones were wrong.
- **Evidence.** A new test measures three passes from detections of the wrong shape: a clean tone and two
  scintillating ones, detected 7 and 14 s off with speed 25-34 % and distance 80-107 % too high. Each first
  attempt is refused one-sided. Each retry is accepted within 0.5 % of f0, 3 % of v, 5 % of d and 0.5 s of the
  CPA: 367.00 Hz, 135.00 m/s, 1758.0 m; 249.97 Hz, 110.05 m/s, 900.3 m; 300.04 Hz, 90.06 m/s, 1200.4 m. From a
  detection of the right shape, the event is the same with and without the retry. The reversed audio, a steady
  tone and noise give nothing accepted or reseeded, and a two-sided refusal is left as it was.
- **Set D** (`fdop detect` on the 40 Livermore clips, read with `bench` `tools/score_d.py`; local runs of the
  bench-built binaries). On `44ff60a`: 36 found, 34 first event right, medians 4.9 m/s and 212.2 m. With the
  retry: 37 found, 35 first event right, 5.5 m/s and 198.2 m. Right at 10/20, 15/30, 20/40, 25/50 and 30/60 % of
  speed and distance:

  | Tool | Found | Right at the five tolerances |
  |---|---|---|
  | fdop `44ff60a` | 36 | 21 / 28 / 29 / 31 / 34 |
  | fdop with the retry | 37 | 22 / 29 / 30 / 32 / 35 |
  | dopplerprint | 39 | 13 / 19 / 21 / 24 / 26 |
  | dopplertrack 60-300 Hz | 37 | 17 / 23 / 27 / 29 / 35 |
  | dopplertrack 40-450 Hz | 38 | 19 / 23 / 26 / 30 / 36 |

  The retry changed three events, each a one-sided refusal now accepted:
  - N394FX_R20 at 128.0 m/s and 1599 m: right.
  - N8085W_R27's second event at 71.8 m/s and 1689 m, for 67.2 m/s and 1490 m: right.
  - N61NG_R37 at 141.4 m/s and 5211 m, for 102.2 m/s and 2786 m: wrong. Its ranges (128-159 m/s,
    4136-6878 m) miss the truth. The pass climbs through a clip that starts mid-drop, so speed and distance
    trade along v²/d: all four tools give 3.8-4.1 against the truth's 3.73. Its refused fit was wrong too
    (56 m/s, 689 m), and `score_d.py` counts refused fits.

  fdop still finds fewer passes than dopplerprint and the wide dopplertrack. Its three misses are 41-47 s clips
  in which no detection reaches the silence line: N1387U_R44 (33 m/s at 421 m), N451EE_R45 (64 m/s at 695 m; its
  ridge crosses f0 about 14 s into the clip, and a 24 s template half needs 16 s on each side) and N958JX_R50
  (215 m/s at 5 km, past the bank's 175 m/s; no tool gets it right).
- **Falsifier 2 and the development rows.** `locate` measures exactly as `detect` does: c = 343 m/s and default
  parameters, before any date or place is read. On ref_raw and osprey_1031 a locate run's events and `detect`'s
  are identical. The 33 nulls, the 15 development rows and the reference's wrong dates use six recordings. On all
  six, `detect` gives the same events with and without the retry (47-60 events each, none reseeded). A full
  `locate` of the reference row (`a_ref`) with the retry matches `44ff60a`'s in every field but wall-clock
  timings: the same high answer and radius (402 m), gates, log lines, `search.json`, `candidates.csv` and curve
  match. So no null or development answer can differ from `44ff60a`'s. Those replays are reported in the next
  entry that reports them. The development rows also use a seventh recording, the eagle-nest clip (b2_eagle);
  checked on 2026-11-07, it gives the same 41 events with and without the retry, apart from the new `reseeded`
  field (false on every event).

### Keep a steep ridge traced after the guided band narrows

- **The fault.** Frames steeper than the 0.5 s main window holds are read from a shorter window. Their noise
  reference was twice the ridge's half-width (half a Hann main lobe, 2 / win, plus half the chirp smear). A real
  tone's skirts are wider than a pure tone's. Once the rounds had narrowed the band to a few fit-RMS, the skirts
  filled the reference and a ridge standing 8-14 dB up read a robust z of 1.9-2.9, under the line of 4. On set D's
  N394FX_R20 (135 m/s at 1758 m), a first round started from a curve within 5 % of the pass traced 3.6-54 s and
  passed every gate (128.4 m/s, 1610 m); the second round, at a 2.5 Hz band, lost every approach frame, and the
  third ended one-sided at 89 m/s. Doubling the reference raised those frames' z to 3.7-11.
- **The change.** The steep frames' reference is four times the ridge's half-width, as the main window's already
  is (`4 x peak_band`). The band, the SNR and z lines and the main window are unchanged.
- **Evidence.** A new test traces passes whose tone scintillates (amplitude noise low-passed at about 4 Hz, 60 %
  deep) and wanders (1 Hz RMS) at bands of 12, 2.5 and 1 Hz. With the old reference 70 % and 54 % of the steep
  frames were traced at 2.5 and 1 Hz; with the new one 97-100 % at every band. Replays of the falsifier-2 nulls,
  the development rows and set D on this code are recorded in the next entry that reports them.
- **Also in this code.** `measure_with_fit` is split into `guided_rounds` (trace, fit, narrow the band) and
  `gate` (the quality gates), with the same arithmetic and order. An experimental second start for refused fits
  (a free ridge trace from the refused trace's points) was committed with it by an automatic commit of a working
  tree and is removed here: on set D it rescued no missed pass, and two of the five events it turned into accepted
  ones were wrong (N4004R_R19 at 55 m/s and 539 m for 82 m/s and 1427 m; N61NG_R37 at 141 m/s and 5211 m for
  102 m/s and 2786 m). A temporary diagnostic test committed the same way is removed too.

### Score a compressed survey image at the spread its detection lines were set at

- **The fault.** A recording that is loud mostly while one pass goes over (a short clip, a livestream near
  traffic) inflates every row's MAD with that swell. Noise and ridge shrink together, so the survey image's robust
  spread (1.4826 x MAD over every cell) falls to 0.18-0.4 on set D's 40 Livermore clips, where the development
  and null recordings sit at 0.567-0.776. The fixed detection lines (3.5 and 4.5) then ask several times more of
  the image.
- **The change.** `survey()` measures the image's spread. Under `DetectParams::min_image_scale` (0.5) the image is
  multiplied up to it, at most `MAX_IMAGE_GAIN` (4) times, and clamped again to [-3, 8]. At or above 0.5 the image
  is left exactly as it is. `locate` warns when it raises an image and the detection log line gives the spread;
  `detect` prints the warning on stderr.
- **Evidence.** A metamorphic test (an image above the floor is unchanged bit for bit; below it, the unscaled image
  times floor / spread, clamped) and a known-answer test (a pass inside a 25 dB swell, spread 0.241, scores under
  the silence line without the floor; with it, gain 2.08, it is detected and measured with the right speed,
  distance and CPA, and its reversal and the swell alone give nothing that passes the gates). Set D, `fdop detect`
  on the 40 clips, read with `bench` `tools/score_d.py`: 34 found on `981fad1` (`runs/setd_981fad1`), 36 on
  `d9dfc88` (`runs/setd_d9dfc88`; N373DF_R06 at 90.5 m/s and 934 m for 91.6 m/s and 941 m, N451EE_R51 at
  100.7 m/s and 690 m for 84.7 m/s and 704 m). First event right 33 and 34; median errors 4.9 m/s on both,
  247.5 m and 235.4 m.
  Every falsifier-2 null and development recording replayed so far on `d9dfc88` logged a spread of 0.567 or more,
  so their survey, detection and everything after it are as before this change.

### Keep sparse flight segments that cross a requested data window

- **The fault.** Archive ingestion and cached-track slicing required a reported ADS-B point inside both the
  region and time window. A valid segment could cross the window between reports and be discarded entirely,
  removing a flight before the acoustic search could consider it.
- **The change.** Both paths retain original bracketing samples when a linear segment intersects space and
  time together, with the existing two-neighbour padding. Ingestion respects restart and ground reports,
  including ground markers on malformed rows; cache slicing respects stored flight-leg boundaries. Dateline
  crossings follow the shorter longitude arc. Track gap, geometry, scoring and confidence rules are unchanged.
- **Cache scope.** Existing wider stores benefit when they still contain the bracketing samples. An older
  narrow store that already omitted the aircraft needs fresh ingestion; no automatic cache invalidation or
  redownload was added.
- **Evidence.** The pre-fix cached-store regression discarded a valid 120 s crossing. Tests cover the same
  crossing after filtering and a gzip-JSON/tar ingestion through track construction, store save/load and time
  slicing, with wrong-time and separated-flight controls. Validation outcomes are recorded in the commit;
  these synthetic tests make no reference-recording or field accuracy claim.

### Let later flyovers fill slots lost to duplicate or non-aircraft fits

- **The fault.** Ranked measurement stopped after 24 fits passed its quality gates. Only afterward did the
  pipeline exclude non-aircraft fits and the search group duplicate seeds and harmonics of one pass. Those
  discarded fits could exhaust the allowance before a later, independent flyover was measured.
- **The change.** Both `locate` and `detect` count independent aircraft events using the search's existing
  eligibility, quality ordering and measured-curve comparison. All measured results remain in detection-rank
  order until reporting, and original ranks still determine the measurement-line gate. No detection, quality,
  harmonic or confidence threshold changes. The search still receives at most 24 independent events; a final
  cap and warning cover changes in greedy grouping caused by a new, better representative.
- **Evidence.** Regressions exercise ranked batches and real measured synthetic flyovers, including duplicate
  first-pass seeds that previously hid a later pass. Validation outcomes are recorded in the commit. No new
  reference-recording or field accuracy claim is made.

### Preserve alternative recording clocks for repeating flight pairs

- **The fault.** The pair join retained 20 s clock cells, but the quick-to-fine merge then keyed survivors
  only by the event/flight pair and 1 km spatial cell. Two aircraft repeating their routes could agree at
  two different recording times, yet only one time reached fine scoring and the later confirming events.
- **The change.** The merge retains the join's clock-cell identity, with deterministic ordering of equal
  quick scores. It still merges duplicate seeds inside the same place/clock cell and scores each flight
  pair's best cell before its next. The existing budgets, truncation warnings, gates and definition of a
  different location explanation are unchanged.
- **Regressions.** A focused merge test covers clock-bin boundaries, duplicate seeds, equal-score ordering,
  invalid clocks and the per-pair ranking order. A full synthetic search test gives two aircraft identical
  repeated circuits and a third flyover that can only confirm the later clock; it requires both alternatives
  to survive and the three-event later explanation to win. These are synthetic checks, not field accuracy
  evidence. Validation results are recorded with this change's commit.

### Keep curve-only recovery inside a usable recording-clock hint

- **The fault.** When the trace search found no candidate in a metadata-derived clock window, `locate` widened
  to the full date before running the independent curve match. That widened clock usually made the curve match
  ineligible, precisely when untraceable ridges needed its alternative reading of the recording.
- **The change.** The curve match now gets its attempt in the original window before fallback. A standing result
  uses the existing curve-only adoption gates. Otherwise the trace search widens; the matcher retries too when
  publication clipping leaves the widened window eligible. A narrow attempt cannot grade a wider trace search.
  `extra.curve_match.clock_window` and `judged_search_window` describe that scope in `result.json`.
- **Work avoided.** Clock and region eligibility are checked before building the whole-file harmonic image;
  date-only and oversized-region searches no longer allocate that unused second spectrogram. `curve_match_s`
  now includes image construction and any eligible fallback attempt.
- **Tests.** Pipeline regressions cover curve-only adoption, absent/failed curve results, explicit-start behavior,
  decoy-count and clock-scope requirements, and both eligibility limits. Scientific scoring and thresholds are
  unchanged. No new real-recording accuracy or timing claim is made here.

### Offline searches reuse the agreed cached geoid height

- `locate --offline` and `fetch --offline` now resolve the validated exact-point cache before using the −30 m
  default. Previously they bypassed that cache, so loading the same `.fdtrk` offline could shift its GNSS-derived
  altitudes from the agreed online height to the default. Missing, unreadable or invalid heights still fall back
  with a warning; offline mode never opens a geoid connection.
- `offline_uses_the_agreed_height_and_defaults_only_without_a_valid_cache` covers valid, absent, malformed,
  nonfinite, implausible and unreadable caches. `offline_cache_roundtrip_and_geoid_adjust` loads a real synthetic
  track-store file with the cached height and requires its original GNSS altitude to remain exactly unchanged.

### Analyse alternate audio tracks and individual microphone channels

- **The gap.** `locate` and `detect` could read only the first audio track's mono mix. Ambient audio in another
  track was inaccessible, and opposite-polarity stereo channels could cancel a useful tone to silence.
- **The change.** Both commands accept zero-based `--audio-stream` and `--audio-channel` selections. The default
  remains the first audio track mixed to mono. Probing, decoding, progress and `result.json`'s `extra.media` identify
  the same selected source. Missing tracks or channels fail before analysis. Recording-clock metadata comes from
  the container or selected audio stream, so an unrelated dubbed track cannot supply the clock hint.
- **Tests.** `selected_audio_supplies_the_probe_and_clock_hint` checks stream/channel bounds and metadata isolation;
  `media_commands_accept_audio_selection_and_keep_defaults` checks both CLIs. The real-FFmpeg regressions
  `multistream_decode_matches_the_probed_audio`, `selected_channel_recovers_antiphase_audio_with_its_timestamps` and
  `decode_preserves_recording_timestamps` cover alternate-track tones, cancelled stereo, delayed starts, packet gaps
  and seeks. These use generated audio; they are not real-recording accuracy evidence.

### A machine with more than 32 GiB runs the same search

- **The fault.** The hypothesis budget grew with RAM, up to 2 G, and the budgets of the stages after it did not: 20 M
  joined points, 2 M fine cells, 2 M confirmations. So a bigger machine built more hypotheses than those
  stages could take, a comparison was cut, and a cut fails the margin gate. The three-day reference search on 256 GiB
  (`r_md_gh`) passed 7,477,352 cells through the quick check to a 2 M fine stage, so the cut was certain and the grade could
  be low at best. It had reached 1.1 M of the 2 M after 4.6 hours when it was paused, and it was stopped unfinished. On 32 GiB
  (`r_md_gc`) the same search scored 102,247 cells and graded high. More memory gave a worse search, and a result could only
  be repeated on a machine of the size that gave it.
- **The change** (`crates/fdop-core/src/search.rs`). `MAX_HYP_TOTAL` caps the budget at what a 32 GiB machine gets, 172 M
  hypotheses and 57 M per event, about 3.4 GB. Unknown memory uses the same cap, not 200 M. Below 32 GiB the budget is
  unchanged. On 32 GiB nothing changes, so no registered build's row and no bench row is affected.
- **Tests.** `budget_scales_with_memory_up_to_32_gib`: 32 GiB, a byte more, 33 GiB, 256 GiB, `u64::MAX` and unknown memory
  all give the same budget, and 16 GiB and zero give what they did.
- **Effect.** The reference recording's five configurations and both feeder runs were replayed with this change on
  `main`'s tip on a 256 GiB machine (`*_cp`). Each equals its 32 GiB row (`*_gc`, AGENTS reference case) in every field
  but the paths in its log. `r_md_cp` is high, radius 388 m, margin 8.44 nats after refinement, with 102,247 fine cells
  and no cut, where `r_md_gh`, without the change, was cut on the same machine. None of these runs gives a start, so the
  curve match is skipped in all of them. No registered build has the change, and set F runs on 32 GiB, where it changes
  nothing (bench/PROTOCOL.md Addendum 16).
- **Docs.** Two sections below said `np_o02_p3` moved 5 cm; it moved 4.9 mm. The comparable-rival section's
  real-recording note now gives the tip's replays.

### The curve match checks its answer along each heard flight's track

- **The fault.** One heard flight fixes when its closest approach was heard, not where along its track the
  recorder stood. Move the place along the track and the clock by the distance over the flight's speed, and that
  flight's curve stays where it was (exactly, for a straight, level, steady flight). Another heard flight with a
  different velocity breaks that tie. One with the same velocity does not: a parallel track flown the same way
  at the same speed, or the same leg flown again. The curve match's radius came only from its other modes, at
  least 1 km apart and 16 at most, and a climb moves the place only a little at a time, so nothing in the search
  had to look along that line. In set E, 1d's reading found `e_olympia`'s one-flight modes moving that way along
  their track. Set E is used up as a test, and this change was not tuned on it.
- **The change** (`crates/fdop-core/src/curvematch.rs`: `along_track_aliases`, `verdict`).
  - After the real window's search, the best place is scored every 1 km out to 6 km either way along each heard
    pass's track, with the clock moved to match. Only places inside the region and clocks inside the window count.
  - Each twin within 3 nats of the best, or better than it, widens the radius as a mode does. Past 5 km, the
    answer does not stand.
  - No evidence, mode, margin or decoy changes, so the decoy calibration is unchanged. The check can only widen
    the real window's radius. In `src/pipeline.rs` that can remove a stand (past 5 km). Where a stand survives,
    it can turn `Adopts` into `Confirms`: a low trace answer inside the wider radius is asserted at its own place
    instead of the curve match's, medium either way. `Agrees` and `Conflicts` use the fixed 3 km, not the radius.
  - The new `aliases` field of `curvematch.json` lists the twins. The verdict line says how many of them tied.
- **Tests.**
  - One heard flight: twins tie past 5.5 km, and the answer is refused.
  - Two flights on parallel tracks, same direction and speed: the same.
  - The same leg flown twice by one aircraft (`a_lap_flown_twice_still_slides`): the same, each pass checked from
    its own closest approach.
  - The parallel pair flown opposite ways: no twin 1 km or more out ties.
  - A twin counts toward the radius exactly as a mode does (`a_tied_alias_widens_the_radius_like_a_mode`).
  - In the three-flight scene, with tracks crossing, nothing ties 4 km or more out. One twin 3 km along a track
    is 2.67 nats down: there that flight's curve, and a silent flight's curve landing on another flight's sound,
    explain the audio. So the radius reaches at least 3 km, and stays inside 5 km.
  - A heard flight is worth only about 3 to 5 nats (11.5 nats of curve give 4.9), so this check is strict by the
    radius rule's own 3-nat standard.
- **The three-flight test.** `3164272` widened its clock tolerance from 2 s to 3 s, justified by a place-clock
  trade along one track that crossing tracks do not have. It keeps the 3 s and now also requires the truth itself,
  scored at its own place over a wide clock range, to be within 3 nats of the best at a clock within 2 s.
- **Effect.** Full `locate` runs with the benchmark's R2 inputs (start ±2 min, 25 km region) on the development
  machine, binary `cd639aca`, against a rule written before any of them ran. Its source differs from this commit's
  only in the test module and one comment. The verdict without the twins is recomputed from each run's own modes,
  so geoid-service noise between runs cannot enter the comparison.
  - **15 development rows** (runs `alias_dev`, with `b3_sapsucker` rerun in `alias_rest/dev` after a disk guard
    stopped it). The check changed no stand, call, grade or radius on any of them.
    - The four right answers the curve match agrees with still stand, radius 1 km: `b1_osprey_1031` (110 twins),
      `b1_osprey_1101` (67), `b1_osprey_1102b` (41) and `b3_duke` (147). No twin tied.
    - `a_ref` is right and high from the traces. The curve match stands neither way there (margin −4.12).
      `b3_sapsucker` has no answer either way (margin −5.45).
    - On the nine rows with no right answer, the curve match stands on none, either way. Twins tied on six of
      them (1 to 7 each, out to 6.0 km), but those radii were already wider from the modes.
  - **33 no-right-answer runs** (`nulls42_full.sh`: wrong dates, and places 90 km or more from the truth; runs
    `alias_nulls` and `alias_rest/nulls`). 28 had finished when this was written; the other five are in the
    benchmark's Addendum 15. The curve match stands on none of the 28, with or without the twins, and none is
    asserted (27 `Silent`, 1 `Unsupported`). Twins widened four radii, all on runs that do not stand:
    `nd_o01_1102` 1000 to 1001 m, `nd_o31_1101` 1000 to 1996 m, `np_o01_p2` 1000 to 2998 m and `np_o31_p3`
    4967 to 5997 m. The largest margin is −0.67 (`np_o31_p2`).
  - **Cost:** 0.0 to 4.4 s per run (`b3_duke`, 147 twins), against 242 to 2202 s for the whole curve
    match in `c_ref_0909`, `b3_duke` and `b1_osprey_1031`.
  - So on these recordings the check changed nothing. Their four stands are all `Agrees`, and no row is
    `Confirms` or `Adopts`. It guards against the one-flight slide, which they do not show. It can raise an
    answer one way only: where the curve match stands more than 3 km from a high trace answer (`Conflicts`,
    lowered to low) and the check removes that stand, the high answer stays high. No row here is `Conflicts`.
- **On `main`.** This lands as a merge of `5f6348c` (the registered build, a child of `c6e5800`) onto `main`,
  after `e13f2d2`.
  - `e13f2d2` keeps every exact clock inside the window, so twin clocks are bounded by `start_hi` here. In
    `5f6348c` the bound is `start_hi`, or `start_lo` plus two frames if the window is narrower than that.
  - Twins are checked for every pass heard at 6.9 nats, credited or not. That is a superset of
    `counts_as_heard`, so no slide is missed.
  - Its two-flyover rule and this check apply together.

### The geoid height comes from two reads that agree, and it is kept

- **The fault.** The NGS geoid API can answer a request with the height and error of another made at the same
  moment, while echoing the coordinates it was asked for, so one reply cannot show it. In a check of four simultaneous
  requests on 2026-11-05, a point in Georgia came back with a Pennsylvania point's values. `fdop` asked once per run
  and kept nothing, against this project's rule to cache every response. Local runs in parallel lanes crossed:
  `np_o02_p1` got −24.07 m where it had −31.36 m (`nh`, `nm`), which moved its curve-match margin from −1.35 to −1.287
  nats, and `np_o02_p3` got −33.48 m for −33.66 m, which moved its place 4.9 mm. No cited row shows one, and none of the
  benchmark's 125 `fdop` rows, on 60 pairs of case and region, differs from another of its pair. A lookup that
  hangs costs a run up to 240 s and then the −30 m default for that run. Two local rows did that around 21:10Z on
  2026-11-05 (`x_32d4e1a`, beside `fh`, `nh`): 0.58 m of height kept `b1_osprey_1031` high but moved it 29 m and took
  its radius from 349 m to 575 m.
- **The change** (`crates/fdop-data/src/geoid.rs`). A height is taken once two reads agree to 1.5 mm, in at most three
  reads, with a pause of 300 ms to 1 s between them that depends on the point. Only different points can cross, and
  a kept height outlives the run, so runs started together must not read in step. It is kept at
  `<cache>/geoid/<lat>_<lon>.txt`, written by rename, and later runs read the file and not the service. Reads that
  fail or never agree fall back to the −30 m default and its warning, as a failed lookup did. An unreadable or
  implausible file counts as nothing kept. `geoid_height_or_default` now takes the cache directory.
- **Tests.** `a_height_needs_two_reads_to_agree`: a crossed read on the first or second read is never taken, and
  three different heights give no height. `a_failed_read_stops_the_reads`. `the_read_gap_depends_on_the_point`.
  `a_kept_height_is_used_and_a_bad_file_is_not`, which reads the kept height back through the lookup without the
  network.
- **Effect.** One real row, `b1_osprey_1031` (`g1`): two reads gave −30.58 m, the height `fh` used, and its
  `result.json` equals `fh`'s in every field but run time and paths. A height that was right before gives the same
  answer. Set F's registered builds ask once, as before, and each run's log keeps the value it used (bench/PROTOCOL.md
  Addendum 14).

### The curve match needs two heard passes

- **The fault.** On the benchmark's sealed set E, `6914ab7` graded the marina `e_harbourgate` **medium, 7.97 km
  off** (limit 1 km; bench/PROTOCOL.md Addendum 14). The trace search detected nothing in its two hours. The curve
  match stood +1.86 nats over the best of 48 decoy clocks on one heard pass, at a clock 110 s from the true one, and
  adopted its own place. That pass was a chance fit. At the true clock the aircraft was 9.76 km from the site, so
  the site did not hear it. Nor is the place a slide along its track: a clock 110 s early needs a place behind the
  truth along the heading, by the 3.2 km the aircraft flew in that time, and this one is 7.9 km ahead of it.
- **Why one pass does not fix a place.** One pass's Doppler curve fixes when and how close it passed. The curve
  match searches a window of clocks, so place and clock trade along the track at the flight's speed: moving the
  observer `v·dt` along it and the clock by `dt` leaves the curve where it was. Set E's Olympia shows this on a real
  recording (`r2_fdop_9087ec4`). Six of its eight best places hear only its first flight, each where that flight
  was at that place's own clock: 147 s early and 10.8 km off, back along the track, or 55 s late and 6.1 km off,
  ahead along it. The best of them scores 13.81 nats against the true place's 17.23, and the second flight adds
  3.97 nats at the true place. The trace search asks for two core events before it grades medium, for the same
  reason. The decoy test does not cover this: it asks whether the recording's clock beats the decoy clocks, not
  whether one curve fixes a place. The tied-place radius can see the slide only where the other places along the
  track fall inside the clock window and among the kept modes.
- **The change, in two forms.** `32d4e1a`, the build registered for set F, does it in `src/pipeline.rs`: a curve match
  that stands on fewer than two heard passes no longer adopts its place or confirms a low trace answer, and a failed
  `curve match` gate says why. An asserted trace answer is settled as before. `e13f2d2` reached `main` with a stricter
  form while `32d4e1a` was being landed: `verdict` itself needs two heard passes, so such a curve match does not stand
  at all. It raises nothing, it neither agrees with nor vetoes an asserted answer, and a medium answer where it ran
  with enough decoy clocks is lowered, as where it stands nowhere. The merge keeps that form and drops the rule in
  `src/pipeline.rs`, which could no longer be reached, with `CurveCall::HeardTooFew`, its gate and its two tests.
  `extra.curve_match.call` gains no value on `main`. These are passes, not aircraft: one aircraft heard twice counts
  as two.
- **Where it was chosen.** This is the one wrong medium in the record that rests on the curve match alone, and it
  is on set E, so set E is not a test of this change; set F is (Addendum 14). In the runs read for this (the
  development, no-answer and benchmark `fdop` runs), the curve match stood on fewer than two heard passes exactly
  once, at `e_harbourgate`. It stood on two at set E's Olympia (two aircraft; right, 66 m, under both registered
  builds) and on 4 to 14 in every development row where it stood (all right). It stood on none of the 42 runs with
  no right answer. On this change's own code the curve match does not stand at harbourgate at all (−0.17 nats, no
  pass heard; a local diagnostic, in-sample, run `eh`). That curve match is `3164272`'s, and its heard-range
  profile already removed the stand. So on the current code this rule changes no recorded row. It closes the path,
  which `ce02222` keeps with `6914ab7`'s curve match.
- **Needed, not sufficient.** Two heard passes flying the same way at one speed leave the same trade, and this
  rule does not catch them: two flights on one approach, or one aircraft flying the same lap twice. See README
  "Known issues". Flights going opposite ways do not leave it, since a later clock moves one's place forward and
  the other's back. Olympia's two flew 158° apart.
- **Tests.** `e13f2d2`'s verdict regression checks zero, one and two heard passes.
  `one_flight_trades_place_and_clock_along_its_track_and_a_crossing_flight_fixes_them` (metamorphic, exact curve
  scores on a synthetic recording): one flight north at 70 m/s scores within 1 nat at the observer and 2.8 km along
  its track with the clock 40 s later. A second heard flight crossing at 90° puts that moved place more than 3 nats
  (the tied line) behind. A second flight on a parallel track, the same way at the same speed, leaves it tied.
- **Effect.** On `32d4e1a`'s form, no grade changes on any run read, on binary `bb8bdce`, that form before its docs and
  wording. On the 33
  no-answer runs outside the development rows (`nh`), 26 are low and 7 give no location, every grade equals
  `edf1a09`'s (`nm`), and no curve match stands. The 15 development rows (`fh`), which hold the other 9 of the 42, are
  identical to `edf1a09`'s (`fm`): 5 right, all high, none wrong. `e_harbourgate` gives no location (`eh`).
  `r_nassau_gh` is identical to `r_nassau_gc`: 270 m off, radius 406 m, high, 14.78 nats. Two `nh` rows differ in
  numbers and not in grade: `np_o02_p3` by 4.9 mm and `np_o02_p1`'s curve margin from −1.35 to −1.287. Both are the NGS
  geoid race (README "Known issues"), not this change. The other four reference and wrong-date configurations and
  the two feeder runs give no start, so the curve match is skipped and this rule cannot reach them; their `gc` rows
  carry over. Replays of those six on a 256 GiB machine were running or queued when this landed. They did not repeat
  `gc`, which ran on 32 GiB, because the hypothesis budget then grew with RAM. With the cap above, they do (`*_cp`).
  `e13f2d2`'s form has not been replayed. Recounting the best place's passes in the saved `fh`, `eh` and `nh` runs under
  its stricter heard line changes no count, and the four stands among them are on 4 to 14 passes, so its heard rule
  alone changes no stand there. That is a recount, not a run, and it does not cover its clock bounds or track filter.

### Preserve sparse passes and bounded clocks in the curve match

- **Sparse tracks** (`crates/fdop-core/src/curvematch.rs`). The exact-stage track filter now samples the
  interpolated trajectory over the catalogue span, at the closest-approach sampling interval. Checking
  only every fourth original ADS-B position could discard a sparse crossing pass that had already voted
  in the coarse search.
- **Recording clocks.** Exact scoring clamps its frame-quantized seed to the supplied start interval and
  keeps every shared-clock trial inside it. Spatial refinement retains those bounds through each move.
  The real window and every shifted decoy use their own intervals, including fixed and fractional-second
  starts. Diagnostic probes retain their wider exploratory clock sweep and never choose the answer.
- **Two credited flyovers.** A curve-match verdict now requires at least two independently credited
  passes as well as its existing margin, decoy-count and radius conditions. A pass counts as heard only
  when it reaches the existing 6.9-nat line, retains positive value after the timing penalty, and is not
  excluded by the shared-pixel assignment. A strong lookup with no positive credited value cannot supply
  a second flyover.
- **Synthetic regressions.** `sparse_interpolated_passes_survive_the_exact_track_filter` checks a crossing
  pass whose original knots lie outside both filters, plus nonoverlapping controls.
  `exact_curve_scores_respect_fractional_start_bounds` checks exact and sub-frame intervals from seeds
  on either side; `a_fixed_fractional_clock_still_locates_the_observer` checks spatial localization with
  a fixed clock. `timing_penalties_do_not_count_uncredited_passes_as_heard` covers positive, zero,
  negative and shared credit. The verdict regression checks zero, one and two heard passes, and
  `one_audio_event_cannot_credit_two_flights` now also checks that a duplicate flight leaves the heard
  count unchanged.
- **Real-recording evidence.** No reference or field replay has been completed for these changes. The
  documented reference video's metadata was reachable, but its audio host returned a 195-byte
  "Site Unavailable" page instead of audio. Existing logged accuracy and grade results remain evidence
  for their named builds, not a validation of this change.

- **Validation of these corrections (2026-11-05).** The pinned `cargo fmt --all -- --check` and
  `git diff --check` passed before these final notes. The combined core/data test build exited 101 when
  `asupersync 0.6.2` compilation received SIGKILL during shared-host memory exhaustion. A smaller core-only
  retry reached `fsci-stats`, but the host subsequently filled its disk and lost the process handles;
  no runtime test result was recovered. Workspace Clippy, the full workspace tests and the release
  self-test have no completed result for this change.
- **Checks on the merged tree (2026-11-05).** `a032bf8`, which also holds `32d4e1a`'s merge and the geoid fix, passed
  `cargo fmt`, clippy with `-D warnings`, the workspace tests (229 passed, 0 failed, 2 ignored, with `--no-fail-fast`)
  and the release self-test (PASS: error 2.4 m, radius 75 m, clock error +0.02 s, 4 events), direct on the benchmark machine. That run was the first compile of this
  section's changes and of the comparable-rival correction below. It needed one correction:
  `margin_compares_the_leader_and_its_rival_at_the_same_polish` failed on every tree with `5ab7f48`, because it kept
  the old list of attempts. When a rival overtakes the leader, the rule below gives an attempt to each rival closer to
  it at the fine cells, so the case now lists those attempts. Its assertions on the leader, the margin, the basis and
  the radius are unchanged, and production code did not change. README's current-check table records the commands.

### Compare every rival at the same stage after refinement

- **The fault.** The margin compared its chosen rival at the same stage as the leader, but chose that rival
  from a list mixing refined and unrefined scores. A refinement gain could hide a stronger comparable rival.
  In the synthetic regression, fine scores 30, 29.8 and 29.7 become 35, 30 and an unrefined 29.7: the old
  selection stopped with a 5-nat gap while the third candidate still gave a 0.3-nat fine-grid gap.
- **The change** (`crates/fdop-core/src/search.rs`). Both rival refinement and the reported margin select
  the smallest comparable gap across different explanations. Another threatening rival gets an attempt
  when earlier gains change that ordering. Each candidate is still attempted at most once; a failed
  refinement keeps its intact fallback, and a promoted winner keeps its own coordinates, clock and radius.
  This may refine additional rivals. The margin remains uncalibrated to the size of the search.
- **Tests.** `refinement_reaches_rivals_hidden_by_earlier_gains` covers a third rival whose refinement fails,
  remains below the second candidate, or overtakes the leader. It checks the margin, comparison basis,
  attempt count, fallback state and winner/radius consistency. Existing equal-stage and promoted-candidate
  regressions retain their numerical and coherence assertions.
- **Real-recording evidence.** Replayed with the later changes on `main`'s tip, `981fad1` (bench/PROTOCOL.md
  Addendum 16). `r_nassau_gt` has the same funnel as `r_nassau_gc`, 93,179 fine cells with no cut, and the same margin.
  On the 15 development rows only `c_ref_0907`'s margin moved, 6.32 to 6.30 nats, from this change's choice of rival, and
  no grade did.

### Recover from an unreadable or incomplete covering ADS-B cache

- **The fault.** Cache lookup chose only the smallest covering filename. If that file could not be read,
  a larger valid cache was ignored: offline runs failed despite having the data, and online runs could
  download the day's archive again.
- **The change** (`crates/fdop-data/src/fetch.rs`). Try covering stores in area order, with path as a stable
  tie-breaker, until one can be loaded. Verify its actual spatial and temporal metadata against the request's
  slice of that UTC day. The same recovery applies to eligible partial feeds, with their priority and
  missing-feed warnings preserved. A damaged or inadequate candidate is named in progress output.
- **Tests.** `offline_cache_recovers_after_unreadable_candidates` covers complete prod, partial prod and
  staging fallback, using real synthetic stores and an unreadable smaller file. It checks the recovered
  flight, cache-hit count and diagnostics. `offline_cache_requires_readable_covering_metadata` refuses
  unreadable stores and filenames whose actual box or either time bound does not cover the request. The
  existing offline roundtrip still checks geoid adjustment and missing-day behavior; its stored fixture now
  truthfully covers the full day that the later request needs.

### Measurement goes down the list until 24 events pass, from the silence line up

- **The fault.** Only primary detections scoring 4.5 or more were measured, and only the strongest 24 of them,
  passing or not. On the osprey cam's 11-02 recording, 19 of the 24 measured places failed the quality gates while
  a 568 m pass sat at detection rank 45 (README, Known issues). On the Livermore single-flyover recordings
  (benchmark set D, pinned `7e66501`, a diagnostic, not blind), 8 of `fdop`'s 14 misses of 40 passes detected
  under 4.5 and were never measured.
- **The change** (`crates/fdop-core/src/measure.rs` `measure_ranked`, `src/pipeline.rs`, `src/cli.rs` for `fdop
  detect`). Every primary from the silence line (`thr_silence_db`, 3.5; pure noise peaks near 3) up is a
  candidate, strongest first. Each batch measures as many as passes are still missing, until `MAX_MEASURED` (24)
  pass or the list ends. The first batch is exactly what the old cap measured, and no more than 24 can pass.
  Detection is unchanged.
- **The safeguard** (`src/pipeline.rs`, `leans_on_added`). More events give chance alignments more to work
  with, and the medium margin does not count them. Build C, this front end on `9087ec4` without a safeguard,
  graded the wrong-date run `nd_o31_1102` medium, 35 km off, on a weak event (detection 2.54 after refinement)
  and one past the old top 24, and was stopped at its own null gate. Here a medium answer whose core matches
  include an event the old line would not have measured is low unless a curve match with enough decoy clocks
  judged it. Where the curve match ran, the medium rule below already requires it to stand (it does not on
  `nd_o31_1102`: −3.21, and −5.17 before the heard range was profiled), so this adds a `measurement line` gate
  only for runs without a start or with too few decoys.
- **Tests.** `measure_ranked_fills_refused_places_and_never_passes_more_than_the_cap`;
  `a_pass_under_the_old_measure_line_is_measured_and_noise_tones_and_reversed_audio_are_refused`, a synthetic
  weak pass between the two lines, a flat tone, noise and the same audio reversed in time;
  `a_medium_on_events_past_the_old_line_needs_a_curve_match_that_judged`.
- **Effect** (full `locate`, the benchmark's R2 inputs, on the development machine). The runs are `fm` and `nm`,
  compared with the parent's `r50_dev` and `r50_nulls` (`r50g_confirm` for the two runs its gate regraded). The
  nulls were read first.
  - On the 15 development rows, 5 right answers are asserted and no wrong one, as in the parent. The osprey cam's
    11-02 goes from medium to high, 240 m to 220 m off. The parent's medium rested on the curve match alone: 0 of 2
    core matches, a trace margin of 0.16. Here 2 of 4 matches are core and the trace margin is 23.16, with the
    curve match agreeing. The trace margin rose on the osprey cam's other two days (11.69 to 13.88, 24.28 to
    29.26) and is unchanged at Duke Farms (13.18). Every other right answer keeps its grade and distance.
  - On the 42 runs with no right answer, none is medium or high and none stands. Seven went from no answer to
    low, six of them on the reference recording. The curve match's margins are the parent's except on two runs,
    which moved by at most 0.86 nats, and the largest is still −0.47.
  - One wrong-date run, `nd_o31_1102`, now has 2 core matches and an 8.07-nat trace margin. It is low only
    because the curve match does not stand there (−3.21). In the parent the same was true of `nd_o31_1101`.
  - The `measurement line` gate failed on none of these runs. Each gave a start, and the curve match judged it
    with 48 decoy clocks.
  - A median of 59 detections were measured per run (27 to 60), against 24.
  - Reference replays (`r_*_gc` and four wrong-date and feeder runs). None gives a start, so the curve match does
    not run. They ran on this change's code before its rebase onto the heard-range profile, the one part they
    never reach. All three configurations stay high, against `9087ec4`'s `*_b1`:
    - state-wide, 264 m off, radius 379 m, margin 12.79 (was 263 m, 397 m, 8.12);
    - county, 270 m, 406 m, 14.78 (was 270 m, 406 m, 16.00);
    - three days, 263 m, 388 m, 8.44 (was 263 m, 388 m, 8.19).

    Each measured all 60 detections from the silence line up, and 12 passed. That gives 10 usable events where
    there were 3 (`funnel.events_used`). The core matches are still N22NY and N501EX. A King Air 350 and a
    Robinson R44 match as weaker events. The wrong dates stay low. On 09-07 the answer is 119 km off, core 1 of 3,
    margin 2.00 (was 138 km, core 2 of 2, 0.60). On 09-09 it is 104 km off, core 0 of 3, margin 0.15 (was 73 km,
    core 1 of 2, 0.35). The Ithaca feeder's two runs stay low, at 0.19 and 0.99 nats (were 0.76 and 2.17).
  - The field recordings quoted in README "Honest status" have not been re-run on this change.
- **Cost.** A full `locate` takes a median 1.18 times the parent's CPU time (quartiles 1.05 to 1.38, at most 2.13,
  48 runs).

### The curve match profiles how far each recording hears

- **The fault.** The curve match credits each pass by its chance of being heard at its slant range. The range
  heard half the time (`r50_m`) was fixed at 2 km for every recording and every type. On the reference recording, a
  diagnostic run confined to 3 km around the published site (it used the truth) found its best place 612 m from
  the site. There, three turboprops 3 to 8 km out carry 8.2, 5.5 and 4.9 nats and were credited 0.8, 0.2 and 0.7. At the osprey cam's
  winning places, passes 4 to 6 km out with 8 to 12 nats were credited the same way.
- **The change** (`crates/fdop-core/src/curvematch.rs`). `r50_m` is a list: 1, 2 and 4 km. Exact scoring takes
  the best total over the clock and the range together, at every place it scores and in every decoy window
  (`search_window` is shared). It reports the range it took as `r50_m` on each mode and decoy in
  `curvematch.json`. The longest is half of `reach_m` (8 km), so a pass the reach leaves out would be heard at
  most about one time in eight. A first version also tried 8 km. With an 8 km reach that range expects to hear
  passes the search never looks at, and in the synthetic three-flight scene it took 8 km on chance lookups at
  the two silent flights.
- **Effect** (full `locate`, the benchmark's R2 inputs, on the development machine). The runs are `r50_dev` and
  `r50_nulls`, compared with `disc_dev` and `disc_nulls` on `6914ab7`.
  - The margin over the best decoy at the osprey cam rose on all three days: 10-31 from +9.23 to +22.24, 11-01
    from +13.29 to +21.36, 11-02 from +1.14 to +5.29.
  - It is unchanged at Duke Farms, +44.81 (it keeps 2 km). It fell on the reference recording, from −3.95 to
    −4.32.
  - Every grade is unchanged. Every distance is unchanged except the osprey cam's 11-02, from 220 m to 240 m.
  - On the 42 runs with no right answer, none stands. The largest margin is −0.47 (it was −0.18): 20 rose, 18
    fell and 4 are unchanged, median change 0.00.
  - The one medium among the 42, `nd_o31_1101`, was medium before and is low under the gate below.
  - The real windows at the osprey cam took 4 km on all three days. Most of their decoy windows took 1 km (31,
    39 and 36 of 48), because at a wrong clock nothing far lines up.
- **How it was chosen.** An offline rescoring of the stored best places suggested it. That rescoring matched the
  stored evidence exactly at 2 km, and in it the osprey cam's wins rose with the range while most null winners
  fell. Those places had been picked under 2 km, so the rescoring was a screen, not the test. The test was the
  full runs above, judged by a rule written before they started:
  - on the 42, no new medium or high and no stand;
  - on the 15, no right answer lost and no wrong one asserted;
  - the margin up on at least 3 of the 5 right rows.

  A four-recording railcam battery was dropped from that rule before any of its results on this build, because
  its operators' terms prohibit recording. All of these recordings are development data; set F is the test.
- **Cost.** A full `locate` takes a median 1.06 times the CPU time it took before (quartiles 1.02 to 1.10, 48
  runs).
- **Tests.**
  - `a_recording_that_hears_far_takes_a_long_heard_range` moves the three synthetic flights 2.8 to 4.6 km out.
    The profile takes 4 km, and its margin over the decoys is 4.42, against 2.08 with 2 km fixed, at the same
    place.
  - `the_heard_ranges_stay_inside_the_reach` is new.
  - `close_silence_costs_and_far_silence_does_not` now also checks three things. The longest range only scales
    distance: it charges a silent pass twice as far out what 2 km charges. Silence 3 km out costs 0.8 nats there.
    And a 10-nat curve 4 km out is worth 1.2 nats more there than at 2 km.
  - `three_flights_locate_the_observer_and_beat_every_decoy`'s clock tolerance goes from 2 s to 3 s. That scene
    has two near-tied fits, 79 m at +0.5 s and 150 m at +2.5 s. A fixed 2 km lands on the first; a fixed 1 km
    or 4 km, and the profile, land on the second, with the decoy margin 6.03 against 6.10. Place and clock trade
    along the first flight's track at 70 m/s, so the test's 200 m place bound allows about 2.9 s.

### A medium answer needs the curve match to stand wherever it ran

- **The fault.** With the knot rule (`069c971`, `9087ec4`, `6914ab7`), the osprey cam's 10-31 recording searched
  on 11-01 with the benchmark's R2 inputs (`nd_o31_1101`, a wrong date) graded **medium** at a wrong place: two
  core matches and a 4.98-nat margin. The curve match ran on the same audio and clock with 48 decoy clocks and did
  not stand (margin −5.65). That medium fired the benchmark's falsifier 2 for those builds (bench/PROTOCOL.md,
  Addendum 8). Earlier recall changes failed the same way: build C (measure primaries from the silence line
  until 24 pass) and a flat-tone guard with a harmonic-line fallback each graded `nd_o31_1102` medium, 35 km off,
  where the curve match does not stand either (−5.17 on `6914ab7`).
- **The change** (`src/pipeline.rs`, `curve_call`). A trace answer graded medium becomes low, with a failed
  `curve match` gate giving the reason, when the curve match ran with at least `MIN_DECOYS` (40) decoy windows
  and stands nowhere, including when it found no place at all (`CurveCall::Unsupported`). High answers, the
  calls where the curve match stands (agrees, conflicts, confirms, adopts) and runs where it did not run or had
  fewer decoys are unchanged.
- **Why, on its own terms.** The medium grade's margin is an evidence-score gap with no calibrated false-alarm
  rate, and a wider or more thorough trace search finds more chance alignments. The curve match's verdict has
  one. With a wrong date or place, the real window is one more clock among at least 41 that the same search
  scores, so it beats every decoy, which it must do to stand, about once in 41 at most, whatever the trace
  search's margin did. The bound assumes the windows are interchangeable, which traffic that changes with the
  hour weakens.
- **Effect.** The rule reads only `result.json`, so applying it to logged runs is exact. On `6914ab7`'s R2 runs
  of the 15 development rows (`disc_dev`) and 33 more no-right-answer rows (`disc_nulls`), it lowers one run,
  `nd_o31_1101`, from medium to low. It keeps all five right asserted answers: the reference recording, the osprey
  cam on 10-31 and 11-01 and Duke Farms (high), and the osprey cam on 11-02 (medium, the curve match's own answer).
  The reference replays have no start, so the curve match does not run there and they keep their grades
  (`r_ny_b1`, `r_md_b1`, `r_nassau_b1`, all high). All of this is in-sample: the rule was written after seeing
  `nd_o31_1101`.
- **Cost.** A right answer that only the trace search grades medium becomes low wherever the curve match runs
  and does not stand. No development row has one, but the reference recording is right and high with a curve
  match that does not stand (−3.95), so such answers exist. On inputs with the right date, which is all the
  benchmark's R2 has, this is the rule's main effect.
- **Tests.** `curve_call_table` covers every grade with the curve match judging or not, standing or not.

### The curve match searches the whole kilometre each of its starts stands for

- **The fault.** The coarse vote keeps one start per kilometre (`nms_m`, 1,000 m: no other start is kept within
  that distance of it). Refinement then made one 5 × 5 move at each of 100, 50 and 25 m, so a start could move at
  most 350 m along each axis, although `refine_point`'s comment said it climbed. On the osprey cam's 11-02 recording with the
  benchmark's R2 inputs (start to the minute ±2 min, a 25 km circle), the real window's best place was a broad
  bump 0.61 km from the cam at 13.84 nats, and the best decoy clock beat it with 14.89 (margin −1.06; 1d's local
  run `f10` on `069c971`). A diagnostic run confined to 3 km around the cam found 15.74 nats 120 m from it, in the
  same disc, across a valley: a sharper peak from a P28A passing at 553 m slant. That run is a diagnostic only;
  it used the cam's position and is not a result.
- **The change** (`crates/fdop-core/src/curvematch.rs`, `refine_point`). Each refined start first scores every
  point of its disc on the coarsest refinement step (radius `nms_m` on 100 m: 316 points besides the start;
  `disc_offsets`, at most 20 steps). Then each scale moves the 5 × 5 grid to its best point until none beats the centre, up to enough
  moves to cross `nms_m` (`refine_moves`: 5, 10 and 20 moves). Every decoy window runs this same code through
  `search_window`, so a more thorough search lifts a decoy exactly as it lifts the real window.
- **Effect, R2 inputs, local runs on an Apple M5 Ultra** (`069c971` plus this change; 15 development rows
  `disc_dev`, 33 more no-right-answer rows `disc_nulls`; baselines `f10` and `nb1`). These are development
  recordings, so none of this is a clean test; the benchmark's run of this commit on set E is. `069c971` lacks
  `9087ec4`'s terrain fix, so osprey 11-02 and `np_o31_p2` were rerun on this commit's own code (`final`). Both
  gave the same numbers to the hundredth.
  - osprey 11-02: low, 14 km (trace search), curve match −1.06 → **medium, about 220 m from the cam**, from the
    curve match (15.95 nats, +1.14 over the best of 48 decoys, which went from 14.89 to 14.81; radius 1,000 m).
  - The other right answers keep their grade and place: the reference recording at 0.25 km, the osprey cam at
    0.09 km on 10-31 and 0.11 km on 11-01, and Duke Farms at 0.96 km, all high. The curve match still stands at
    the last three (+9.23, +13.29 and +44.81, against +9.78, +13.23 and +44.41) and still does not at the
    reference (−3.95 both). Sapsucker Woods still gives no location. Scored with the benchmark's `score.py` on
    R2's 12 development cases: 5 right and 0 wrong, against 4 and 0 for `f10`. The eagle cam (`b2_eagle`) was
    run for neither.
  - No-right-answer runs, 42 in all (the 9 wrong-date and wrong-place development rows and the 33 in
    `disc_nulls`): no grade changed and the curve match stands on none. Their margins rose on 17 and fell on 16
    (median change 0.00, largest rise +3.55). The largest is now −0.18, the osprey cam's 10-31 recording
    searched around a wrong place (`np_o31_p2`; −0.62 before). The real window and every decoy run the same
    search, so when the date is wrong the real window is one more clock and beats all 48 decoys about 1 time in
    49 whatever the search does. The one medium among the 42, osprey 10-31 searched on 11-01 (`nd_o31_1101`), comes
    from the trace search and was medium before; the curve match does not stand there (−5.65).
- **Cost.** About four to five times the curve stage. On the osprey 11-02 row, with eight threads and every
  ADS-B day cached on a shared Apple M5 Ultra, it took 217 s against 47 s for `f10`, both at load about 50 to
  70 (the 1- and 5-minute averages each run logged at its start); in the batch, at load about 190 to 210, it
  took 659 s. The disc adds 316 exact scores for each of the 4 refined starts in each of the 49 windows, against
  73 for the old climb. Runs without a start are not affected.
- **Not affected.** A run with no `--start` (or a clock known only to worse than 1800 s) skips the curve match,
  so the reference replays in AGENTS.md and README (`*_c1`, `*_b1`) cannot change and were not rerun; each logs
  "curve match skipped".
- **Test.** `refinement_climbs_from_a_start_beyond_one_move_per_scale`: on the synthetic three-flight scene, a
  start 600 m from the observer (420 m north and 430 m west, past 350 m on each axis) ends within 200 m of it with
  at least the observer's own evidence. The old refinement, one move per scale and no disc (what `nms_m` 0
  gives), is checked in the same test to stop more than 200 m away with less. The disc's points are checked
  against Gauss's lattice counts (316 for 10 steps, 1,256 for the 20-step cap, 348 for 10.5 steps, besides the
  centre).

### Spike pixels in the terrain tiles no longer put an observer under the sea; the air column extends past its grid

- **The fault.** The terrain elevation is only ever the observer's ground. The cached z13 Terrarium tiles near the
  development sites hold single pixels of −796 to −1,026 m beside pixels of 0 to 3 m (z12 has one of −1,972 m), and
  `fdop`'s bilinear sample reproduced them. Candidate 5 in the pinned benchmark build's osprey run on 10-31
  stood at −599 m, and candidates in two osprey runs on 11-01 at −654 m and −722 m. The 8 × 8 median around each spike is
  0–2 m, and z10 gives about 1 m at the same points. Traced by cc_1 (research session e9).
- **The second fault, which the first exposed.** `Column::cum` clamped its integrals to the column's −500 m to 15 km
  grid while `mean` divided by the full height, so any path reaching outside the grid inflated the sound speed:
  those candidates got 375–432 m/s against about 340. cc_1's fix extends the integral past the grid at the edge
  cell's rate, which is exact because the column is constant beyond its levels; its test fails without the fix
  (376.93 against 340.28 m/s) and passes with it. Landed unchanged.
- **The rule.** A tile pixel below −430 m, lower than the lowest dry land on Earth (the Dead Sea shore), is left out
  of the bilinear blend and the other corners are renormalised. A cell with no corner left is open water and counts
  as sea level, 0 m. Every other pixel is sampled exactly as before, so a field down to −430 m (Death Valley is
  −86 m) is unchanged. Seabed between −430 m and 0 m is still read as ground: a candidate over shallow water stands
  on the seabed, which this does not fix.
- **Tests.** `a_spike_pixel_below_any_ground_is_left_out_of_the_blend` (the −895.4 / 1.0 cell from the cache),
  `a_cell_with_no_ground_is_open_water_at_sea_level`, `real_low_ground_and_values_just_above_the_floor_are_kept`,
  `a_point_on_an_excluded_corner_takes_the_kept_corners_mean`, and cc_1's
  `a_path_reaching_past_the_grid_keeps_the_column_mean`.
- **What it did.**
  - **Development rows.** Full `locate` on the 15 rows with R2 inputs, on a local Apple M5 Ultra with 256 GiB
    (`fb1` against `f10`). Every answer, grade and margin is bit-identical. It removed the three stored
    candidates below −430 m, two on the osprey cam's 10-31 recording and one on 11-01.
  - **33 more runs with no right answer** (`nb1`). One graded medium. The knot rule caused it, not this change
    (README Known issues).
  - **Benchmark machine.** The county reference search is bit-identical to `r_nassau_c1` (`r_nassau_b1`). The
    state-wide, three-day and wrong-date replays were still running when this was committed.

### ADS-B points without GNSS height no longer bend a track between GNSS points

- **The fault.** A merged ADS-B leg mixes points that carry a GNSS height with points that do not. The second kind
  sits a median 89 m from the line through the GNSS points around it (p90 417 m: 21,022 points on the 364 mixed legs
  of 2026-11-01's search box over about four hours). The Hermite track model passes through every point, so each one bends the track,
  and the bend becomes Doppler that no aircraft made. On one pass in the osprey cam's 11-01 recording, the
  predicted tone differed from a smoothed track's by 8.99 Hz rms and 92.55 Hz at worst at a 166 Hz tone, against a
  measurement sigma of 0.36 Hz. Most points without GNSS height are probably multilateration positions. The archive
  keeps a leg's feed (`source`) and drops it per point. On 2026-11-01's search box, the points with a corrected barometric
  height and no flags number 687 on legs that came only from `mlatonly`, 19,598 on legs merged from `prod` and
  `mlatonly`, and 84 on legs from `prod` alone. So
  the origin is near certain for the leg and inferred for the point.
- **The rule.** `drop_bracketed_non_gnss` drops a point without GNSS height when GNSS-height points on both sides lie
  within `BuildParams.gnss_bridge_s` (60 s) of each other. A point outside such a bracket is kept, and so is a leg
  with no such points. The 60 s span comes from a synthetic test with offsets drawn as measured: on a sustained
  standard-rate (3°/s) turn, bridging costs less than the noise it removes up to 60 s and more by 90 s. `the_bridge_stops_where_a_turn_costs_more_than_the_noise`,
  `noisy_points_without_gnss_height_do_not_bend_a_straight_pass`, `a_real_turn_stays_intact_when_its_points_are_dropped`,
  `points_the_bridge_does_not_cover_are_kept` and `a_leg_of_gnss_height_points_is_untouched` are its tests. The
  rule and its tests are cc_1's, landed unchanged.
- **What it did, with the trace search alone.** These runs used the benchmark's R2 inputs: the start to the minute
  ±2 min, and a 25 km region centred 10 km from the truth (`bench/PROTOCOL.md`). They ran on `cb95fbb` on an Apple
  M4 with 32 GiB, with the rule off and then on (cc_1's `cb_off`, `cb_on`). Each cell is grade and distance.

  | Recording | Rule off | Rule on |
  |---|---|---|
  | Reference | high, 0.25 km | high, 0.25 km |
  | Osprey cam 10-31 | low, 13.91 km | **high, 0.09 km** |
  | Osprey cam 11-01 | high, 0.21 km | high, 0.11 km |
  | Osprey cam 11-02 | low, 14.22 km | low, 14.18 km |
  | Duke Farms | low, 1.07 km | **high, 0.96 km** |
  | Sapsucker Woods feeder | no location | no location |

  The nine runs at wrong dates or wrong places stayed low or gave no location with the rule on. The two thinnest
  are both held low by the core gate alone. The Duke Farms recording searched 90 km from its site is low at a margin
  of 3.43 refined against refined, with 1 core match; the flight nearest to core (e8) is 0.59 trace nats short of
  the 2.0 line. The reference recording on 09-07 is low at 2.62 with 1 core match.
- **Reference and feeder replays** (`*_c1`), with bf's flags and environment, on a release build of `703d232` with
  this change. They ran one after another on the benchmark machine, an Apple M4 with 32 GiB, with other jobs (the
  Duke row below among them) sharing it.
  None gives a start, so the curve match is skipped in every one, and these are the knot rule's effect alone. The
  build had the curve match before its review fixes, which change nothing on these runs. Each cell is grade,
  distance from the published point (the feeders' from their town's centre), radius and margin, as in the `cb95fbb`
  entry:

  | Build (runs) | County | State-wide | Three-day | 09-07, wrong | 09-09, wrong | Feeder, wrong |
  |---|---|---|---|---|---|---|
  | `a827820` + `cb95fbb`'s change (`*_r5`) | high, 270 m, r406, 17.85 | high, 264 m, r385, 8.64 | high, 263 m, r388, 7.21 | low, 138 km, 2.95 | low, 73 km, 1.07 | low, 338 km, 1.81 |
  | `703d232` + this change (`*_c1`) | high, 270 m, r406, 16.00 | high, 263 m, r397, 8.10 | high, 263 m, r388, 8.19 | low, 138 km, 0.61 | low, 73 km, 0.35 | low, 338 km, 0.75 |

  The county and 09-07 margins compare fine cells, because refinement was refused for one side; the others are
  refined. Every answer is at the same place as before. The three wrong rows' margins fell, by 2.34 (09-07), 0.72
  (09-09) and 1.06 (feeder). The right ones moved both ways: county down 1.85, state-wide down 0.54, three-day up
  0.98. The 09-07 run still has both core matches and is low on its margin alone. All of this is on the recording
  every build was developed on.

  | Recording | Run | Error | Radius | Grade | Margin | Failed gates | On `cb95fbb` (`*_r5`) |
  |---|---|---|---|---|---|---|---|
  | wrong date, bird-feeder cam | `n_feeder_on_1101_c1` | 254 km | 300 m nominal | low | 2.15 | core, margin, radius | 326 km, low, 2.07 |
  | wrong date, eagle cam (Duke Farms) | `n_duke_on_1102_c2` | 63 km | 300 m nominal | low | 1.36 | core, margin, radius | 64 km, low, 4.79 |

  The feeder's error is from its town's centre and Duke Farms' from the cam's site. The Duke run used a second
  build of the same code (before the review fixes, curve match skipped). On `cb95fbb` it cleared the medium line at
  4.79 and was low only on its 1 core match of 4. With the knot rule it has no core match of 5 and 1.36 nats.

### A curve match reads the spectrogram along every logged flight, ranked against 48 decoy clocks

- **Why.** The trace search can only use a pass whose ridge it measured. A ridge too faint to trace, or one crossed
  by another, adds nothing, and the reference recording's answer rests on 3 events (`funnel.events_used`). `dopplerprint` (MIT,
  © 2026 Henry Alexander) asks the other way round: if the recording was made here at this clock, is there energy
  along each logged flight's exact curve? `crates/fdop-core/src/curvematch.rs` reimplements that question and
  attributes it. Three things differ. A bank of (speed, distance) paths scores every arrival time and tone in one
  sweep per path. Each path's own scores across the recording give the null for that path. And the whole search
  runs again at 48 decoy clock windows, so an answer is ranked against 48 clocks that cannot be right, not two.
- **How it grades.** It stands when it beats every decoy (margin above 0 over at least 40 of them) and every place
  within 3 nats of its answer lies within 5 km. That line was set from runs with no right answer before reading the
  rows with one. On 42 such runs (six recordings at wrong dates and at places 90 km or more from the truth; curve
  match only, `v4k`) none was above 0, and the largest margin was −0.8. On this code's defaults, the largest of the
  same 42 is −0.6 (`v10t`). The runs share six recordings, so they are not 42 independent trials. A verdict
  that stands can raise a low trace answer to medium, at the trace answer if that lies inside its radius and
  otherwise at its own place. It can lower a high or medium answer to low when it stands more than 3 km away. It
  never grades high. It runs only when the start is known to 1800 s or better and the region has at most 200,000
  cells. A state-wide search with no `--start` uses the trace search alone. `--no-curve-match` turns it off, and
  `curvematch.json` records it.
- **The decoys get the real window's search.** In the first versions a decoy window got only the vote, while the
  real window was also rescored and refined. That made margins look better than they were. It was found by
  reading the code, before any grade line was set. Every window now goes through one `search_window`, and
  `a_decoy_window_gets_the_search_the_real_window_gets` is the test.
- **A fresh review before landing found eight faults, all fixed** (another model, reading the code without the
  author's conclusions). Lookup e-values could overflow to infinity on a bright curve over quiet audio, and two
  infinite places then tied without widening the radius; they are now kept in logs, and evidence that is not finite
  never stands. A pass worth under 1 nat claimed no pixels, so faint copies of one sound could each be credited;
  every credited pass now claims. The coarse stage took one sound speed at the prior's time for every window, which
  favoured the real window slightly (about 0.03 s of arrival per 1 °C); each pass now takes the speed at its own
  time. A place exactly 3 nats down was not counted as tied. A confirmed trace answer kept the curve match's radius
  without adding the distance between the two centres. Failing to write `curvematch.json` failed the run. The null
  tests used 10 decoys, too few for any verdict to stand, and checked a margin under 3 rather than the verdict; they
  now run 80 decoys over a repeated sky and assert refusal. The tone prior's mean was 1 under its smoothed
  distribution rather than the counted rows; it is now scaled to the counted rows. Each fix has a test: `huge_lookups_stay_finite`,
  `a_pass_gets_the_same_sound_speed_whichever_window_is_the_prior`, the new stanzas of
  `a_verdict_needs_margin_decoys_and_one_place`, `a_confirmed_answer_keeps_every_tied_curve_place_inside_its_radius`
  and the rewritten null tests. Every row below ran after them.
- **A tone prior, and a first version that failed.** A typed propeller aircraft's tone is weighed by whether its
  blades × rpm can make it (`TONE_TABLE`, typical figures from general knowledge, not checked against type
  certificates; helicopters left out). The first version divided by a flat spread over tones. On real audio it
  added evidence to wrong-date runs (one place's evidence rose from −1.8 to 38.3 nats), because rumble puts chance
  peaks on the propeller bands, and the osprey cam's 11-01 answer moved from 0.20 km to 20.32 km (`v6t`). It was not adopted.
  The version that landed divides by where this recording's best tones fall at chance, scaled so its mean over those rows is 1
  (`the_tone_prior_has_mean_one_under_chance_and_favours_its_bands`,
  `a_tone_prior_on_a_rumbling_recording_lifts_no_place_over_the_decoys`). That centres it; the decoy windows,
  weighted the same way, are what the answer is judged against. On the knot rule's base with the prior off and
  on, the largest of the 42 null margins went from −0.8 to −0.7. The osprey cam's 11-02 recording went from −0.1 to
  +1.6, so it stood, until the review fixes; 11-01 went from +16.9 to +13.2, 10-31 stayed at +9.8 and Duke Farms went from +44.1 to
  +44.8 (`v8n`, `v8t`). **The default was turned on after those rows with a right answer were read**, and two
  table entries were corrected after them (the Meridian's blades, the King Air class's top rpm). The grade line
  did not move. Its synthetic test shows only the direction: 0.79 nats over three flights.
- **What it did on the development rows: it changed no grade.** These are full `locate` runs with R2 inputs on this
  code, run locally on an Apple M5 Ultra with 256 GiB (`f10`). The fifteen rows gave four right answers, all high,
  and no wrong one: the reference recording at 0.25 km, the osprey cam at 0.09 km on 10-31 and 0.11 km on 11-01, and
  Duke Farms at 0.96 km. The curve match stood and agreed within 3 km on three of them; on the reference
  recording it did not stand. On the osprey cam's 11-02 recording it stood at 0.61 km before the review fixes
  (`f9`, margin +1.62), and the run graded medium from it. After them the same place scores 13.84 nats against
  13.69, but the best decoy rose from 12.07 to 14.89, so the margin is −1.06 and the trace search's low answer
  (14.18 km) stands. Across the 48 decoys of that run the median rose 1.03 nats, most for windows over an hour from
  the prior. That is what giving each window its own sound speed predicts, but the fixes were not run one at a time.
  On every other row the margin moved by under 0.7 nats. The Sapsucker Woods feeder gave no location: its three
  passes in reach all sit at track ends on the band floor. The five wrong-date and four wrong-place runs were low or
  gave no location, and the curve match stood on none of them. The pinned benchmark build `7e66501` gave one right
  answer on these inputs. All of these recordings were used in development, so the benchmark's labelled rerun and
  its sealed set E are the tests. The curve match took 4–80 s per run on that machine, up to about half the run.
- **Interface.** `result.json` gains `extra.curve_match`. It is null when the curve match did not run. Otherwise it
  holds the call (`Silent`, `Agrees`, `Conflicts`, `Confirms` or `Adopts`), the verdict, and a line naming the passes
  that gave its best place at least 1 nat. `result.json` also gains `timings.curve_match_s`. When the curve match
  stands, `answer.gates` gains a sixth gate, `curve match`, which fails only on a conflict. The run directory gains
  `curvematch.json`, and `locate` gains `--no-curve-match`.

### The margin compares a leader and its rival at the same polish; refinement survives a bound it sits just past

- **Refinement no longer refuses a match just past a search bound.** On `630bba5` all three reference searches lost
  their refined radius and graded medium at a nominal 300 m (`r_nassau_t630`, `r_ny_t630`, `r_md_t630`). `refine_on`
  profiles the fit with gates off, so its minimum can land past a bound a nearby grid cell passed, and
  `refresh_refined_candidate` then required every match inside its bounds with no slack. In the county search the
  C208's refined distance was 8443 m against a bound of 8379 m, 0.76 % over, inside a 399 m contour. Locating matches
  now get `LOCATING_GATE_SLACK` (5 %), the value hypotheses already used; optional confirmers keep none, as in
  `best_confirmer`. `refinement_survives_a_gate_bound_just_inside_its_minimum` and the last stanza of
  `refined_reporting_recomputes_physics_evidence_and_veto_at_one_fixed_clock` fail without it.
- **The margin credited the leader alone with refinement's gain.** Since `630bba5` the leader is rescored at its
  refined point while its rival stayed at its fine cell. Measured on `630bba5` with the slack above, the gain was 0.23,
  0.80 and 0.90 nats on the county, state-wide and three-day searches and 0.15 and 0.25 on the wrong dates; on the
  Ithaca feeder it was 0.95, on `b8c0819`. On this code it is 1.21 on the county search and 0.94 on the feeder, the two
  runs whose margin keeps the fine score (`top_score` minus `margin_top_score` in `r_nassau_r5`, `f_feederwatch_r5`).
  **The slack fix alone would have added a wrong medium:** it grades the
  feeder's wrong answer medium at 3.10 (`f_feederwatch_b8`). Without the slack that refinement is refused, by one speed
  of 37.0 m/s against a bound of 36.8, and the fine cells' margin is 35.12 − 32.97 = 2.15, which is low. That is
  arithmetic from an instrumented run (`f_feederwatch_diag`); exact `b8c0819` was not replayed on the feeder.
- **Now both sides are refined, or neither counts.** The best different explanation is refined as well. A rival that
  overtakes becomes the answer with its own radius, one that falls yields to the next, and no candidate is attempted
  twice. If either side cannot be refined, the margin compares both at their fine-grid cells. Refining the rival was
  not enough by itself: on the feeder its refinement was refused, and the margin stayed 3.10 until that case compared
  fine cells (`f_feederwatch_ri`, then `f_feederwatch_r3`). No threshold or constant changes.
  `margin_compares_the_leader_and_its_rival_at_the_same_polish` is the metamorphic test. Two mutants of the search code
  ran against the same tests on `42677a4`: crediting the leader alone fails it and
  `refinement_reranks_promoted_leaders_without_mixing_candidate_state` (88 passed, 2 failed), and taking the margin from
  the reported scores fails it at the refined leader's refused rival, `margin_nats` 2.5 for a gain of 0.5 (89 passed, 1
  failed).
- **`result.json`:** `margin` gains `margin_top_score`, `margin_rival_score` and `margin_basis` (`refined` or
  `fine_grid`). `margin_nats` and `ratio_distinct` follow that pair; `top_score` and `next_distinct_score` stay the
  reported scores. The margin gate's text prints the compared pair at two decimals, because 2.95 printed as "3.0 nats"
  on a gate that failed, and names the fine-grid case. The refused-refinement warning now reads "that candidate keeps
  its fine-grid position and evidence", because it can be about a rival.
- **The clock at the reported point has a test.** `630bba5` regenerates the winner at its refined point, which removes
  an outlier: on the code before the wind change, the county search reported a start of +4.66 s from its fine cell while
  the fit at the refined point gave −0.75 s. Reverting `refine_on`'s `t_start_utc` to the grid centre's clock still
  passed all 81 runnable core tests on `630bba5`, so `refinement_fits_the_clock_at_the_point_it_reports` now starts
  seconds off the true clock and fails that mutant at +2.38 s. The reference starts are now −0.40, −0.26 and −0.25 s
  against 20:17:29.1 UTC, itself known to ±1 s. This removes an outlier and does not make starts exact.
- **What `e13c7fc`'s gap-aware `n_eff` also moves.** It feeds the profile threshold, so it widens or narrows the distance
  and speed gates, and it feeds `quality.score`, so events reorder and IDs renumber: match them by `t_rec`. In the
  three-day search the C208's hypotheses now overflow what the budget has left (64,412,380 against 57,266,230,
  `r_md_r5`), so that event only confirms, with a warning.
- **Reference case and wrong dates.** Every row is a replay, on one recording, and the runs ran with other jobs on the
  machine. Each cell is grade, distance from the published point (the feeder's from its town's centre), radius and margin.

  | Build (runs) | County | State-wide | Three-day | 09-07, wrong | 09-09, wrong | Feeder, wrong |
  |---|---|---|---|---|---|---|
  | `7e5cf3e` (`*_s7*`) | high, 306 m, r316, 12.77 | medium, 271 m, r304, 5.01 | low, 271 m, r304, 2.92 | low, 138 km, 2.37 | low, 73 km, 5.49 | low, 338 km, 2.89 |
  | `630bba5` (`*_t630`) | medium, 130 m, r300 nominal, 15.68 | medium, 383 m, r300 nominal, 8.25 | medium, 62 m, r300 nominal, 7.63 | low, 138 km, 2.51 | low, 73 km, 6.67 | not run |
  | `b8c0819` + slack (`*_b8`) | high, 270 m, r407, 19.26 | high, 264 m, r385, 10.03 | high, 264 m, r387, 8.21 | low, 138 km, 2.23 | low, 126 km, 0.90 | **medium**, 338 km, 3.10 |
  | `42677a4` + this change (`*_r3`) | high, 270 m, r407, 19.29 | high, 264 m, r385, 9.43 | high, 264 m, r387, 7.99 | low, 138 km, 2.18 | low, 126 km, 0.87 | low, 338 km, 2.15 |
  | `0693e30` + this change (`*_r4`) | high, 270 m, r406, 17.87 | high, 264 m, r385, 8.63 | high, 263 m, r388, 7.23 | low, 138 km, 2.95 | low, 73 km, 1.04 | low, 338 km, 1.81 |
  | `a827820` + this change (`*_r5`) | high, 270 m, r406, 17.85 | high, 264 m, r385, 8.64 | high, 263 m, r388, 7.21 | low, 138 km, 2.95 | low, 73 km, 1.07 | low, 338 km, 1.81 |
  | `a827820` as it stood, before this change (`*_a82`) | medium, 130 m, r300 nominal, 17.85 | medium, 383 m, r300 nominal, 8.64 | medium, 73 m, r300 nominal, 7.20 | **medium**, 138 km, 3.01 | low, 73 km, 1.11 | low, 338 km, r300 nominal, 1.81 |

  Refused refinements, by run on `*_r5`: the county's rival and the feeder's rival, neither within a nat of a grade
  line; the other four compare refined scores. No run recorded a cut. Selftest on the same binary: 2.4 m, radius 75 m,
  clock +0.02 s. Refinement's distance from the published point is not a score: the fine cells kept when it was refused
  (`*_t630`, `*_a82`) were nearer on the county and three-day searches, inside that nominal 300 m, and further on the
  state-wide one, outside it at 383 m. The refined answers lie inside their own radii, and one reference point cannot say
  which estimate is better. Radii grew from 304–316 m to 385–406 m.
- **`0693e30`'s first replay: it narrowed the gap between right and wrong on this recording.** Compare the `*_r3` and
  `*_r4` rows, where only upstream differs. The commit holds the robust tone candidate and cubic interpolation, not
  separated here. The candidate replaces a fit only when its capped residual is lower, so it can only lower a match's chi,
  chance matches included. On 09-07 the leader rose from 28.30 to 32.19 and its rival from 26.11 to 29.24, so that wrong
  date's margin rose from 2.18 to 2.95, **0.05 under the medium line**, with both core matches. Right-date margins fell:
  state-wide 9.43 to 8.63 and three-day 7.99 to 7.23, both refined against refined. The county fell from 19.29 to 17.87,
  but its rival changed (34.6 km to 20.1 km) and that rival's refinement is refused, so that pair is not like with like.
  The feeder's fell from 2.15 to 1.81, at fine cells both times. The 09-09 answer moved from 126 km back to 73 km, with
  one core match of two instead of none. This is one recording and a few wrong answers, and a near miss is not a
  regression, but the trend is the wrong way for a margin that has to separate them. **On `a827820` as it stood, 09-07
  is a wrong-date medium:** 138.14 km off, core 2 of 2, margin 3.008 (`n_ref_on_0907_a82`), the refined leader's 32.19
  against the rival's fine 29.18. The same rival refined scores 29.24, so this change grades it low at 2.95. That
  removes the regression by 0.05 nats and does not reopen the gap. Its three reference searches and the feeder lose
  refinement there, so they report a nominal 300 m; the reference margins match this change's within 0.01 nats, and
  the radius gate alone separates its medium from the high here.
- **`a827820`'s dedicated profile bounds barely moved these rows.** Compare `*_r4` and `*_r5`: margins moved by 0.03 nats
  or less. Hypothesis counts per event match on all six rows, compared to 0.1 M, and no new budget
  warning appears, so the wider `t_tol` it can pass to `SearchEvent` did not visibly grow the search here. One refusal went
  away: the 09-09 rival refines on `*_r5`, so that margin is refined against refined (1.04 at fine cells, 1.07 now).
  `measure_s` rose on five of six rows and fell on the three-day, under different load, so that timing says nothing.
- **Field recordings.**

  Town level or coarser, and only the rows finished when this was written; the rest follow in a later commit. A wrong answer at low
  is a grading pass and a locating fail. The moves come from every change since `7e5cf3e`, not this one alone.

  | Recording | Run | Error | Radius | Grade | Margin | Failed gates | On `7e5cf3e` |
  |---|---|---|---|---|---|---|---|
  | wrong date, bird-feeder cam | `n_feeder_on_1101_r5` | 326 km | 1,002 m | low | 2.07 | core, margin, radius | 56 km, low, 1.31 |
  | railcam, Rochelle, IL | `f_rochelle_r5` | 371 km | 885 m | low | 1.59 | core, margin | 36 km, low, 0.49 |
  | railcam, Tucson, AZ | `f_tucson_r5` | no location | | | | | no location |
  | railcam, Plant City, FL | `f_plantcity_r5` | no location | | | | | no location |
  | railcam, Fullerton, CA | `f_fullerton_r5` | no location | | | | | no location |
  | eagle-nest cam, Lee County, FL, state search | `s_swfl_fl_r5` | about 17 km from the county's centre | 487 m | low | 0.07 | core, margin | 135 km from the site, low, 0.20 |
  | same recording, county search | `s_swfl_lee_r5` | about 17 km from the county's centre | 467 m | low | 1.78, fine cells | core, margin | about 4 km from the site, low, no rival |
  | eagle cam, Hillsborough, NJ, county search | `f_duke_county_r5` | about 6.5 km from the township's centre | 300 m, nominal | low | 26.83, fine cells | core, unheard, radius | not replayed |
  | wrong date, same eagle cam | `n_duke_on_1102_r5` | about 70 km from the township's centre | 158 m | low | 4.79 | core, margin | 106 km, low, 2.07 |
  | osprey cam, Oyster Bay, NY, 11-02 | `f_osprey_1102b_r5` | 365 km | 244 m | low | 0.16 | margin | 365 km, low, 0.47 |

  Only the core gate keeps `n_duke_on_1102_r5` from a wrong-date medium: its margin, 4.79 refined against refined,
  clears 3, and 1 of its 4 matches is core. The nearest other match, e11, passes on chi (1.86) and on its own clock
  (−4.0 s, exactly the 4 s limit) but its trace is only 0.7 nats better than chance, against 2.

- **Checks.** `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings`
  exit 0; `cargo test --workspace --locked -j 2` 179 passed, 0 failed, 2 ignored; `cargo run --release --locked -j 2 -- selftest`
  PASS: 2.4 m error, 75 m radius, +0.02 s clock error, 4 events, both intended aircraft. They ran on a probe commit with this code and `a827820`'s docs, from clean snapshots on remote workers. `ubs` on
  `search.rs` and `pipeline.rs`: 0 critical, 726 warnings and 355 info items across the two files, not triaged here. Not run on
  this code: the full decoy bank, and any field row not in the table above. The replays ran on a build that differs only in the
  margin gate's printed decimals.

### Profile tone and received CPA directly instead of borrowing speed/range slices

- The fit previously reported tone and received-CPA extrema encountered while profiling speed and distance.
  Those nuisance paths cover only two slices of the accepted parameter region and can exclude valid timing
  solutions before they reach the shared-clock search. Both coordinates now have dedicated lower/upper
  profile searches under the existing F cutoff and model bounds. Previously accepted extrema are retained.
- The received-time profile fixes `t0r`, refits speed and distance, and solves emitted tone analytically.
  Centered times make it independent of the recording's timestamp origin. The original emission-time box
  becomes a distance constraint through `t0e = t0r - d/c`; boundary cases keep that constraint too. Four
  extra profile sides run only in final `PreparedFit::complete`, after iterative ridge fitting has finished.
  Point fitting, generous floors, dependencies and output fields are unchanged. A profile that reaches
  the model box uses the existing `unbounded-within-search` warning form for `f0` or `t0r`.
- `reported_tone_and_heard_time_ranges_profile_their_own_coordinates` independently refits nuisance
  parameters inside and beyond each interval. On its 80 Hz / 60 m/s / 8 km fixture, the timing interval
  changes from 1022.542478–1024.586037 s to 1018.326404–1028.824827 s. Two accepted clock witnesses at
  the best time ±4 s (costs 3.590266 / 3.529333, threshold 3.785475) no longer fall outside the generous
  timing gate. The tone interval changes from 79.886360–80.068367 Hz to 79.499727–80.460123 Hz.
  Independently optimized points 2% beyond every corrected endpoint exceed the unchanged threshold.
- `heard_time_profile_preserves_emission_bounds_and_timestamp_translation` checks the original parameter
  box, including extreme allowed received times, and shifts of ±10,000.25 s. The one-sided profile test
  retains independently accepted tone/time witnesses on both sides of CPA. These are conditional synthetic
  calibration checks; real-recording/wrong-date replays and empirical confidence coverage remain pending.
- Final integrated validation passes all 97 core and 13 shared-type tests (110 total, none failed or ignored),
  including FFmpeg/FFprobe integration and the added checks that accepted tone/time nuisance solutions remain
  inside the reported speed/distance ranges. Core/shared-type all-target Clippy passes with warnings denied;
  workspace formatting and diff checks pass. The unchanged 1,300 s synthetic selftest, linked to fresh core
  libraries through an external entry point, again gives 2.365 m error, 75 m radius, +0.0249 s clock error,
  four events and both intended aircraft. Its position and clock results match the preceding `b8c0819` run.
  Full-workspace/release CLI validation remains incomplete after the previously observed `asupersync 0.6.2`
  build memory failures; the entry-point selftest does not exercise ledger/report integration.

### Robust emitted-tone fitting and accurate fractional Doppler predictions

- Search trace scoring adds a median-frequency-ratio tone candidate, followed by the existing trimming
  and bounded tone refit. The original single-tone and tone-change proposals remain available; the new
  candidate replaces the result only when its existing capped residual score is strictly lower. It reuses
  the trace scratch buffer. No residual cap, tone bound, coverage penalty or evidence threshold changes.
  `robust_tone_recovers_doppler_curve_despite_masked_contaminants` reproduces the old fit's chi 7.596113
  and 0.372711 Hz tone error with 15% stray tone points; the new fit recovers the known tone and chi 3.75,
  passing the unchanged core-fit limit of 4. Raw RMS still reports contamination. The paired negative
  controls cover wrong curves, out-of-bounds tones and incomplete coverage.
- `RatioGrid` interpolates finite monotone stretches with a four-point cubic, constrained to the two
  bracketing values, with the existing linear fallback at boundaries, reversals and gaps. Invalid or huge
  query times return an unavailable prediction rather than overflowing an index. The grid density and
  number of physical emission-time solves do not increase.
- `ratio_grid_interpolation_resolves_close_passes_between_samples` checks an independent analytic
  retarded-time/radial-velocity oracle across six physical passes, three grid phases, two timestamp
  origins and both grid spacings. Full-grid maximum error falls from 3.352338 to 0.228055 Hz; quick-grid
  maximum error falls from 20.252859 to 6.355502 Hz. `ratio_grid_interpolation_preserves_support_and_local_bounds`
  checks cubic exactness, gaps, boundaries, plateaus and reversal bounds.
- `close_pass_refinement_is_stable_across_prediction_grid_phases` exercises actual spatial and shared-clock
  refinement with 200 m and 228 m slant-distance passes. All three phases recover the synthetic observer
  within 0.212 m, retaining the 75 m radius floor and strict 0.5 m position / 0.01 s clock test limits.
  The old linear interpolation also gives 0.211475 m maximum error on this fixture: it checks integration
  stability, not a measured position improvement. The fractional-query test establishes the frequency gain.
  These are conditional numerical tests, not empirical coverage or field accuracy; real-recording and
  wrong-date replays remain pending.
- All 43 search tests pass with the pinned nightly, including unchanged exact trace-buffer and tone-change
  regressions, candidate/refinement coherence, wrong-curve controls, and still-air/wind localization.
  Workspace formatting and `git diff --check` pass.

### `search.json` no longer grows with the search; missing winds aloft fall back

- **`search.json` keeps the best 10,000 candidates, with a new field `candidates_total`.** A complete comparison had
  made it 521 MB on the busy osprey run and 290 MB on the osprey cam's 10-31 recording, built in memory and written
  before `result.json`, so a run short of memory or disk could fail at the very end. The answer sits at the front.
  `margin` keeps the rival's score, distance and flights wherever it ranks, but a rival past the cap loses its place
  and clock from this file. `result.json` does not change.
  `search_json_keeps_the_best_candidates_and_counts_the_rest` fails without this.
- **A forecast with no winds aloft is refused instead of copied from the surface.** A pressure-level response with
  no upper winds at some hour copied the 10 m wind to every height, and a radiosonde with no winds read as calm air.
  Both still reported `winds_aloft: true`, which keeps the speed gates narrow. Such a response now fails with
  `open-meteo: no winds aloft`, which falls back to radiosondes with the usual warning, and a sounding with no wind
  above its lowest level is skipped. This covers only the all-missing case: wind at one upper level is still spread to
  the others, and a missing 10 m wind still reads calm. One such hour at one grid point refuses the whole grid, and the
  refused response stays cached, so it replays until its file under `<cache>/weather/` is removed. In the cached
  weather of the recordings here, 0 of 40 pressure-level responses (235 locations) are refused and at most 1 of 106
  soundings is skipped. `pressure_levels_without_winds_aloft_are_refused` and `a_sounding_without_wind_is_not_calm_air`
  fail without this.
- **The radiosonde source now reads `N of M soundings usable`.** A sounding that could not be fetched, or was too short
  or too low, was dropped silently, and the source counted only those kept. This changes the atmosphere's description
  in `result.json` on any radiosonde run. `radiosonde_source_counts_the_soundings_it_could_not_use` fails without this.
- **The ridge check's refusal now says what it measures:** `trace follows only N % of the central frequency drop`,
  in `quality` and in `reasons`. The check is unchanged. It compares against the central 80 % of the fitted drop,
  so the 15 % line is 12 % of the whole drop, and a tone that keeps falling can read above 1.
- **Correction to the message of `4b88d7c`.** It says the 2 Hz step limit comes from the guided trace because "its
  band floors at 1 Hz, so a clean fit cannot step further than that". A floor is not a ceiling: the band reaches
  4.5 Hz at a passing RMS, a steep frame allows the larger of that and 0.25 s of chirp, and a wider band can step
  further, which makes a real ridge read low. Removing the limit
  changed none of the synthetic passes, and the comment now says this.
- **Replays on this code.** On `630bba5` with these changes, the county and state-wide reference searches match exact
  `630bba5` on answer, radius, margin, rival, funnel, gates, warnings and every candidate's score, and `fdop selftest`
  gives the same 7.1 m. The three-day run and the wrong dates were not replayed. The weather change does feed the
  search, and what stands in for those replays is the scan above: no cached pressure-level response is refused, so
  these recordings' forecast winds do not change. At most 1 of 106 cached soundings is skipped, which matters only
  on a radiosonde fallback.
  `b8c0819`, which this lands on, has no real-recording replay of its own yet.

- **Corrections to `42677a4`, all in text or comments except the radiosonde source above.**
  - `max_roc` is the rate at the CPA as heard, `t0r`. Its comment said the steepest fall comes just after that, and the
    README called it the steepest slope. The steepest fall comes a full `d / c` earlier, at `t0e`, faster by
    `(1 - M^2)^(-3/2)`. `max_roc_is_the_rate_at_the_heard_cpa_and_the_steepest_comes_earlier` checks this numerically.
  - The README said a rough `--start` would rule out the wrong field answers, "so give one". That advice could exclude
    the true clock: `--start` means the exact start, the default `--clock-tol` is 180 s, and nothing widens past it.
  - "Twice, one of them graded low" overstated the recordings not developed on. One right answer is at the reference
    site, and both recordings informed the ridge check's line.
  - The Lee County "about 4 km" was stated as chance without a test. Now it has one, and reads "probably". With the
    events of `s_swfl_lee_s7a` and archived passes within 8 km of the site, the stream's clock lines up one usable event
    with a flight, in gates (dt 0.2 s). The two answers' clocks (+53 to +56 min) and eight control offsets from −90 to
    +120 min line up none. That is one event, and why the state and county answers' clocks agree within about 2 minutes
    is unexplained.

### Frequency calibration through the full Doppler curve

- Ordinary-window ridge integration now follows the complete model curve, as the steep branch already
  did. A local tangent had biased an averaged peak even with the correct physical guide. The analytic
  ridge test failed at 0.172154 Hz in its first case on the old path; the final maximum across six
  cases is 0.004141 Hz, below the unchanged 0.01 Hz bound. The guide is deliberately offset by 0.37 Hz
  so copying the guide cannot pass the test.
- The ordinary noise exclusion accounts for the Hann main-lobe half-width (`2 / win`) and half of
  the within-window chirp smear. Its reference extends beyond that signal support. This keeps a real
  ridge from entering its own noise estimate when the iterative frequency tolerance shrinks. The
  frequency tolerance and SNR/z thresholds remain unchanged; the reference bins change deliberately.
- Row normalization can tilt the local log-spectrum and bias sub-bin frequency interpolation.
  Measurement now retains transient per-row offsets, uses the normalized profile for every peak
  qualification/clearance check, then restores the three local interpolation bins along the curved
  path. A finite, concave vertex must remain within that same three-bin neighborhood. Otherwise the
  normalized estimate is retained. The final original frequency-band check still applies. Public
  `normalize_rows`/`guided_trace` signatures and the stored `Spec` format are unchanged, with no new
  dependency or full production spectrogram clone.
- `curved_integration_limits_distance_bias_across_audio_phases` synthesizes six 180 s recordings from
  retarded emission phase, physical `1/r` amplitude and seeded noise, with perturbed detection seeds.
  All pass the original 0.5% distance, 0.5 m/s speed and 0.1 s CPA-time limits plus the quality gates.
  Maximum final distance error is 2.339 m (0.2924%), speed error 0.0270 m/s, and CPA-time error
  0.00721 s. The traced drop covers 98.3–99.1% of the expected drop. Distance errors before and after
  restoring the local background shape are:

  | True slant distance | Before, phases 0.13 / 1.27 | Final, phases 0.13 / 1.27 |
  |---|---|---|
  | 800 m | 7.310 / 6.951 m | 1.580 / 2.339 m |
  | 2,000 m | 4.155 / 6.125 m | 0.663 / 0.508 m |
  | 1,500 m | 1.638 / 1.703 m | 0.465 / 0.297 m |

- Five trace tests pass, including 27 combinations of frequency phase, background slope and offset;
  frequency invariance below 20 microhertz for a known parabolic log-spectrum; identical normalized
  SNR/z values during restoration; a fractional-frequency curved-path oracle; and rejection of
  unqualified shoulders, invalid curvature and remote vertices. Both new measurement tests pass.
  These are synthetic frequency/range calibration results, not real-recording location accuracy or
  empirical confidence-coverage claims. Reference and field replays remain pending.
- Integrated validation passes all 88 core tests, including both FFmpeg/FFprobe cases, and all 13
  shared-type tests (101 total, none ignored). The unchanged `src/selftest.rs` also passes when linked
  through an external entry point to the fresh core libraries: 2.365 m position error, 75 m radius,
  +0.0249 s clock error, four events and both intended flights. The archived prior selftest reported
  0.417 m and +0.192 s: position error is larger on this one fixture while clock error is smaller.
  This verifies the full synthetic core path, not the CLI release/ledger/report integration.
  Core/shared-type all-target Clippy passes with warnings denied, and workspace formatting passes.

### Continuous spatial refinement with coherent reported evidence

- The observer is now polished below the contour grid spacing with bounded physical-model evaluations.
  The same step improves the pilot before estimating excess-noise scales. Every accepted move reprofiles
  the shared clock under the fixed calibrated objective; interpolated contour values never replace
  trace fits. The search remains local to the selected assignment and refinement window.
- Four continuously profiled edge minima detect thin rotated contours crossing between grid nodes.
  Edge minima can replace the position minimum before deltas are formed, preventing negative deltas.
  An open contour widens the window. A contour with zero accepted nodes instead recenters and halves
  the grid spacing; if still unsampled, it fails the radius gate. `radius_lower_bound` keeps its existing
  window-clipping meaning, while `region_cells == 0` identifies unresolved sampling. Padding is now
  half a cell diagonal rather than half a cell side. The 75 m systematic floor and 5.99 cutoff stay.
- `search::RefineResult` adds the fitted `t_start_utc`. The winning candidate's position, elevation,
  clock, event fits, free-clock checks, evidence, core status and unheard-close-pass veto are rebuilt at
  the refined solution. The CLI reports and plots consume this coherent candidate. Every locating
  assignment must survive; failed reevaluation retains the coarse candidate with a warning and no
  refined radius. Newly promoted candidates are checked and refined before the final margin is built,
  with each candidate attempted at most once. Refined clocks obey the existing search interval slack.
- All 37 search tests pass. `subgrid_polish_recovers_analytic_observers_across_grid_phases` uses an
  independent closed-form Doppler generator across 25/50/100 m grids and three phase offsets: worst
  grid-only error is 68.809 m, versus 0.265 m after polishing. The noisy-third-pass regression now
  improves from a 52.0 m raw-grid pilot to 6.0 m. The still-air and wind two-flyover searches each
  recover the observer to about 0.2 m while retaining a 75 m reported radius.
- Further regressions cover parallel equal-velocity tracks rotated by 0/7/17 degrees, including an
  open contour missed by all boundary nodes; a narrow contour requiring resampling; bounded search
  around unscorable points; and direct-emission verification of regenerated trace fits, terrain
  elevation and a close-pass veto that changes at the refined point. Candidate promotion tests
  prevent an unchecked single-event survivor or a stale radius from becoming the reported winner.
- These results calibrate numerical behavior under the synthetic model. Reference/field recordings
  have not been replayed; empirical spatial coverage, propagation/track error and search-wide
  confidence calibration remain unvalidated by these tests.

### Consistent spatial calibration and shared-clock refinement

- Fit diagnostics count the observed union of six-second correlation supports, capped by the samples
  actually observed and retained for search. A long silent gap no longer earns independent information;
  repeated or more densely sampled points do not increase the count. Two 24 s stretches separated by
  156 s previously earned 34 effective points and now earn 10. This retains the existing six-second
  correlation assumption, not a covariance-derived or empirically validated effective sample size. An
  uninterrupted span now earns `1 + span / 6`, rather than `span / 6` with an artificial floor of eight.
  The F-distribution's separate minimum residual-degree guard remains a numerical safeguard.
- Refinement previously chose its location and recording clock with the raw trace objective, then reported
  a confidence region using per-event excess-noise scales and misfit weights. The selected point could fail
  to minimize that reported surface, producing negative delta-chi-square cells. Refinement now estimates
  noise scales from a pilot fit, freezes them across window expansions, and uses the same calibrated objective
  for the joint clock, spatial minimum and confidence deltas. The broad search and its ranking retain their
  existing objective.
- Focused synthetic regressions compare the shared clock against direct emission-time solves, check exact
  equivalence with unit weights and invariance to assignment order, and exercise a noisy third flyover with
  an off-grid observer. The third-pass fixture explicitly reproduces the old negative-delta inconsistency.
  The existing four-edge expansion test also checks that widening retains the original calibration.
  Additional tests check sampling-density and duplicate invariance, sparse traces, gap-length invariance,
  and the actual fitted uncertainty ranges of a gapped physical Doppler curve.
- Reference and field replays are not available in this environment: there is no recording/archive cache
  or `yt-dlp`. No new real-recording accuracy or confidence-coverage claim is made. The 75 m radius floor,
  5.99 contour threshold, detection caps and confidence-grade thresholds are unchanged.
- Validation at `e13c7fc`: both `search::tests::calibrated_` regressions, all five `fit::tests`, and all 13
  `fdop-types` tests pass. In `calibrated_refinement_recovers_clean_pair_and_has_nonnegative_deltas`,
  the noisy third pass has an 80 m transverse bias: pilot position error is 52.0 m and calibrated
  grid position error is 17.5 m. The previous mixed objective gives a minimum delta of -27.80 in
  this fixture; the calibrated surface has no negative deltas. These are synthetic results.

### Consistent moving-medium physics and a faster frequency kernel

- The wind-aware propagation model now solves the advected acoustic wavefront
  `|r + w T| = c T`, including crosswind displacement, and differentiates that same travel time
  for both the Doppler ratio and Newton emission-time steps. Previously travel time, predicted
  frequency and Newton updates used different approximations. The stable positive root avoids
  cancellation for downwind propagation; invalid media cannot yield an emission-time match.
  Sound speed and wind are still path averages, treated as a homogeneous medium: this is not
  refractive ray tracing. First-order coarse-gate rescaling and all confidence thresholds stay as before.
- Constant-velocity frequency evaluation now differentiates the retarded-time root directly.
  It needs one square root instead of two, retaining the same subsonic model. Two regressions
  cover 53,970 comparisons against the previous reconstruction and 675 independent pulse pairs.
  Five wind regressions cover a bisection wavefront oracle, analytic one-dimensional propagation,
  reception-time derivatives, rotation invariance, near-sonic cancellation, 72 cold/warm solves,
  and invalid media. They passed on the pinned nightly.
- Optimized component probes on a shared AMD EPYC 9V74 host measured a 1.78× median speed ratio
  for 16,777,216 frequency calls. The earlier detector changes measured 3.57× and 8.67× median
  ratios on 3,600- and 10,800-frame synthetic spectrograms, respectively; all six full outputs
  agree exactly with the baseline. Clean two-event point scoring was 1.17× faster, with no
  material improvement for the changing-tone or quick probes. These are isolated measurements
  with scheduling noise, not real-recording or end-to-end speedups; details are in README.
- **Real-recording limit.** Reference, wrong-date and field replays with the corrected wind model
  remain unrun: the reference media download returned a “Site Unavailable” HTML page. README
  now explicitly labels the existing real-recording errors and grades as `7e5cf3e` baseline
  evidence. No real-world accuracy improvement is claimed from the synthetic checks alone.
- **Checks.** All 70 default core tests and 13 shared-type tests pass, with two opt-in media tests ignored. Core/shared-type
  Clippy passes for all targets with warnings denied, and workspace formatting passes. The actual
  unchanged selftest, invoked through an external launcher using a fresh optimized current-core
  build, passes with 0.417 m error, 75 m radius, +0.192 s clock error and both intended flights.
  Two full workspace test attempts, including a one-job/lower-memory retry, were killed while
  compiling the unchanged `asupersync 0.6.2` dependency, before tests ran. The standalone selftest
  covers the real core pipeline but not the full CLI, ledger or reporting build.

### Reuse trace-fit buffers throughout the search

- Clock scans reuse model-ratio, robust-deviation and tone-change prefix buffers instead of
  allocating them for every trial. The per-clock match-results vector is reused too. This applies
  to fine/quick point scoring, null calibration, confirming flights and independent clock checks.
  The ratio callback can be inlined. Search coverage, clock/spatial grids, arithmetic order,
  tone-change candidates, thresholds and budgets are unchanged.
- `reused_trace_scratch_preserves_fit_results` checks all fit fields bit-for-bit against outputs
  recorded from the allocating Rust implementation at `695f952`, repeatedly alternating long and
  short traces, clean/outlier-contaminated tones, tone changes, missing coverage and early returns.
  An external comparison of the actual old/new functions also passed 2,178 trace/clock cases.
  The complete point evaluation agrees bit-for-bit at 75 synthetic observer points covering clean,
  changing-tone and quick pairs, with the same unchanged track implementation on both sides.
  These are synthetic equivalence checks, not real-recording performance measurements.

### Detector scoring with bounded per-worker scratch

- The matched-filter bank now scores independent frequency rows with reusable row buffers. It
  removes the padded coverage mask, four full work matrices per template, and per-worker copies
  of the winning-score map. Scratch per worker scales with the number of time frames instead of
  frames times frequency rows. Valid-frame intervals provide the same sample counts without
  adding a mask value for every template sample.
- The templates, evidence addition order, float32 coverage boundary, template-index tie break,
  residual denominators, thresholds and detection caps are unchanged.
  `template_bank_matches_scalar_with_edges_and_residuals` checks every score bit and winning
  template against a direct scalar oracle, including short recordings, both grid edges,
  duplicate templates, masked evidence and one versus three worker threads. The existing
  noisy-audio detection, cap, overlap and measurement regressions also passed on the working tree.
- A standalone comparison against `695f952` produced identical serialized full detection results
  in three runs each of synthetic 240-row, 3,600- and 10,800-frame log spectrograms. This is synthetic equivalence
  evidence; the historical reference-recording results and timings below have not been remeasured.

### Low answers say they are unconfirmed; `fdop detect` warns on high-passed audio

- **The text of `verdict` in `result.json` changes** for answers below high, and so does the same line in the CLI
  summary, `report.md` and `report.html`. A low answer now reads `unconfirmed, failed: <gates>. This place can be
  close or far off, and the radius holds only if it is right. <match>`. A medium answer reads
  `failed: <gates>. <match>`. A high answer keeps the bare match, and "no location" is unchanged. No field name,
  gate, grade, number or exit code changes. Before this, a low answer printed the same "N flyovers matched" sentence
  as a high one, under a location and a radius: `f_feederwatch_s7a` did so at 338 km from the true town with an
  818 m radius. `verdict_says_when_an_answer_is_unconfirmed` fails without this.
- `fdop detect` now prints the high-pass warning that `fdop locate` already reported, through a shared
  `low_band_warning`. Pink noise put through three cascaded 200 Hz high-passes (`c1_detect/steep.wav`, a local
  synthetic file) warns "39 dB below" on `7e66501`; `7e5cf3e` printed nothing, and the unfiltered file warns on neither.
- **Checks.** `cargo test --workspace --locked` passed 140 with 0 failed and 2 ignored, clippy with warnings denied
  passed, and `fdop selftest` passed: 0.4 m error, 75 m radius (known answer, not evidence about real recordings).
  Reruns of three real searches, one at each grade (`v_nassau` high, `v_ny` medium, `v_rochelle` low), match their
  `7e5cf3e` runs in every field of `result.json` except the verdict text, the timings and the run's own metadata.
  The change moves no number or grade, so the reference and field results recorded on `7e5cf3e` stand. Four
  deliberate breaks of the new code each failed the test meant to catch them.

### Complete comparisons, and refusing fits with no ridge through their drop

- **Why.** `9620d2e` fails the margin gate on any recorded comparison cut, and `fine: N -> 4000` fired on every real
  search with candidates, so every real recording graded low. The cut also hid real answers: on the county search
  with `3f00c61`'s events, the best explanation sat at position 40,145 of 48,347 in the fine queue.
-
  [`2605b0d`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/2605b0dbe97c9ff81145fc79da48226f7e7b6d81)
  **Budgets.** `max_quick` 3 M → 20 M, `max_fine` 4,000 → 2 M, `max_confirm` 2,000 → 2 M, above the largest searches
  estimated from real runs (about 11 M joined points, 1 M fine cells). A cut is still recorded and warned, and it
  still fails the gate. There is no flag for them. `dedupe` compares a candidate only with kept candidates that have
  the same matches, with the same result, and the fine grid logs its progress on stderr.
-
  [`f390c60`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/f390c6039bbe02fde6b5c554cd809c9d1c94005c)
  **Speed.** Each solve in a ratio grid's time sweep starts from the last solve's interval, and Newton stops at a
  1e-4 s step there, which makes these grids 2.54 times faster on 806 cells of a real run. Chi moved by at most
  0.024 and no best point changed. Extrapolating the start reached 3.57 times but changed 2 best points, and a
  coarser ratio table lost best points in 21–37 % of cells, so neither is used.
-
  [`4b88d7c`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/4b88d7c1708cd85b33826ace03562ead48828029)
  **False events.** A complete comparison reached fits that join two steady tones across an empty stretch. With the
  budgets alone the state-wide reference search went 34.8 km off, low, resting on one. `events.json` now has
  `quality.drop_traversed`, the share of the central 10–90 % of the fitted drop that the trace travels, and an event
  under 0.15 is refused. The 0.15 line was picked with these recordings in view; synthetic tests justify its two
  sides, and the replays can show harm, not validate it. This is a new field in `events.json`. `result.json` keeps
  its fields, and a refused event's `quality` text there gives the reason.
-
  [`4b88d7c`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/4b88d7c1708cd85b33826ace03562ead48828029)
  **Steep passes.** A first version of that check exempted chirps above 4 Hz/s, because real passes read 0 traversal
  there too. An exempt false event (805 Hz, about 30 nats at chi 0.18) then put the state-wide search 15.6 km off,
  so the exemption is gone and the tracer follows those passes. The limit came from `window_for_roc(roc).clamp(0.5,
  2.0)`: a 0.5 s window stops holding a chirp at 1/win² = 4 Hz/s. Frames steeper than the main window holds read a
  second spectrogram with a window matched to the latest fit. The first hypothesis, that the tangent integration was
  the cause, was wrong: integrating along the curve alone left traversal at 0 from about 13 Hz/s.
- **Synthetic evidence**, through `measure()` with the gate off, 81 passes at three tone strengths: silent crossings
  over 0.15 went from 5 rows (3 accepted) to 1 (at 4.00 Hz/s, rejected by another gate). Real passes at or below 4
  Hz/s are unchanged; median traversal above 10 Hz/s went from 0.00 to about 0.8; 6 of 81 stay under the line, all
  at the weakest strength and at least 20 Hz/s. A detection half as steep as the pass still reads at least 0.42; a
  quarter leaves 8 of 12 under the line, so the event is refused.
- **Replays**, a local build of `7e5cf3e` whose code is that of `4b88d7c`, on an Apple M4 with 32 GiB, two runs at a
  time (so the times are not clean). Each is read from its own `result.json`. No run recorded a pair, fine or
  confirmation cut. The detection and measurement caps cut on most of them, with warnings: the reference runs kept
  60 of 82 curves and measured 24 of 42, and the busiest recordings measured 24 of up to 59. Hypothesis-budget
  demotions, also with warnings: four on the osprey run, five on its wrong-date run, one on `f_osprey_1031` and one
  on each bird-feeder run.

| Configuration | Run | Off by | Radius | Grade | Margin (nats) | Fine cells | Time |
|---|---|---:|---:|---|---:|---:|---:|
| county | `r_nassau_s7b` | 306 m | 316 m | high | 12.77 | 755 | 58 s |
| state-wide | `r_ny_s7b` | 271 m | 304 m | medium | 5.01 | 12,119 | 226 s |
| three-day | `r_md_s7b` | 271 m | 304 m | low | 2.92 | 50,751 | 509 s |
| wrong date 09-07 | `n_ref_on_0907_s7a` | 138 km | 539 m | low | 2.37 | 30,171 | 359 s |
| wrong date 09-09 | `n_ref_on_0909_s7a` | 73 km | 412 m | low | 5.49 | 8,456 | 124 s |
| wrong date, bird-feeder cam | `n_feeder_on_1101_s7b` | 56 km | 1,619 m | low | 1.31 | 53,514 | 534 s |
| wrong date, osprey cam | `n_osprey1101_on_1031_s7a` | 40 km | 665 m | low | 0.07 | 627,310 | 6,685 s |
| wrong date, eagle cam | `n_duke_on_1102_s7a` | 106 km | 269 m | low | 2.07 | 233,794 | 1,489 s |
| osprey cam, right date | `f_osprey_1101_s7b` | 223 m | 214 m | high | 24.93 | 893,697 | 9,602 s |
| osprey cam, 10-31 | `f_osprey_1031_s7a` | 152 km | 403 m | low | 0.51 | 553,311 | 7,382 s |
| osprey cam, 11-02 | `f_osprey_1102b_s7b` | 365 km | 193 m | low | 0.47 | 125,677 | 1,142 s |
| eagle cam, Hillsborough, NJ | `f_duke_s7a` | about 2.3 km from the town | 214 m | low | 0.11 | 282,170 | 1,950 s |
| bird-feeder cam, Ithaca, NY | `f_feederwatch_s7a` | 338 km | 818 m | low | 2.89 | 41,012 | 434 s |
| railcam, Rochelle, IL | `f_rochelle_s7b` | 36 km | 474 m | low | 0.49 | 1,170 | 81 s |
| railcam, Tucson, AZ | `f_tucson_s7a` | no location | | | | 0 | 80 s |
| railcam, Plant City, FL | `f_plantcity_s7a` | no location | | | | 0 | 52 s |
| railcam, Fullerton, CA | `f_fullerton_s7b` | no location | | | | 0 | 54 s |
| eagle-nest cam, Lee County, FL, state search | `s_swfl_fl_s7b` | 135 km | 75 m | low | 0.20 | 408 | 219 s |
| same recording, county search | `s_swfl_lee_s7a` | about 4 km | 92 m | low | no rival | 20 | 237 s |

- **The clock each field answer implies, against the stream's.** Osprey cam on 11-01 +0.1 s; eagle cam at Duke Farms
  −6.4 s; osprey cam on 10-31 −3.8 h and on 11-02 +1.1 h; bird-feeder cam +7.0 h; Rochelle +9.3 h; eagle-nest cam in Lee
  County, FL +53 min searched by state and +55 min by county. So the county search's "about 4 km" is probably chance, and the
  Duke Farms answer is a real match despite its low grade: it searched the whole day and its 0.11-nat rival is 140 m
  away. Without a known start the search cannot make this check. `--start` means the exact start, the default `--clock-tol`
  is 180 s and nothing widens past it, so a start 10 minutes off excludes the true clock too. A start known to within
  25 minutes with `--clock-tol 1500` would keep the true clock and exclude all of these wrong ones; what the search
  answers instead has not been measured.
- **What got worse.** Against `d55ef79` the reference answers are 47–85 m further from the published location, the
  state-wide search falls from high to medium and the three-day window from medium to low. The reference recording
  has 4 usable events of 24 where `d55ef79` had 19, 3 of them searched. One refused event, at 13 %, was a confirmer
  in a correct county answer, and whether it was a real pass is not settled. The osprey run's time went from 584 s
  to 9,602 s for a margin of 24.93 nats against 23.29, and four of its events dropped to confirmer-only on the 32
  GiB hypothesis budget (one on the bird-feeder run). The osprey cam on 11-02 went from 39 km to 365 km off, low on
  both builds: a site check found no refused event that was a real pass, and a 25 km box round the site does not
  locate it on either build. Usable events also fell on the osprey cam's other days (17 to 9, 13 to 5), the
  bird-feeder cam (19 to 9) and Rochelle (15 to 5). Fullerton's one usable event is refused at 3 %, with no location
  on both builds. Tucson's wrong answer (238 km) became no location because every event went, and nobody has checked
  whether any of them was real.
- **What got better.** The county search returns to high with no comparison cut. The state-wide answer is no longer
  low. A wrong bird-feeder answer `d55ef79` graded medium is low, by 0.11 nats, which is a narrow miss and not a
  fix.
- **Margin does not separate right from wrong here**: true 2.92 (low) against false 2.89 (low), and true 5.01
  (medium) below a wrong-date 5.49 (low on one core match). Core passes on three wrong answers, leaving margin alone
  to keep them low: the bird-feeder cam (2.89), the reference recording on 09-07 (2.37) and the osprey cam on 11-02
  (0.47). A small margin can also mean other flights at one place: the eagle cam's 0.11 is against a rival 140 m
  away, and the bird-feeder's against one 1.1 km away. With no rival the gate passes: the Lee County search found
  none on 20 fine cells, and is low on its other gates. These gates do not scale with the size of the search (README
  "Known issues").
- **A lead, not landed.** On one recording, measuring all detections instead of the strongest 24 moved a wrong low
  answer (365 km) to the right place (0.61 km, still low, radius 338 m): `f_osprey_1102b_s7b` against a throwaway build
  with `MAX_MEASURED` at 60, `f_osprey_1102b_cap60`. Its new event is a 568 m pass at detection rank 45, 99 % traversed.
  On the reference recording that build gave, with times overlapped:

| Run | At 24 | At 60 | Time |
|---|---|---|---:|
| `r_nassau_cap60` | high, 306 m, margin 12.77 | high, 306 m, margin 11.06 | 58 → 92 s |
| `r_ny_cap60` | medium, 271 m, margin 5.01 | medium, 271 m, margin 4.72 | 226 → 340 s |
| `r_md_cap60` | low, 271 m, margin 2.92, rival 138 km away | low, 271 m, margin 1.91, rival 52 km away | 509 → 940 s |
| `n_ref_on_0907_cap60` | low, 138 km, core 2 of 2, margin 2.37 | low, 138 km, core 2 of 2, margin 0.25 | 359 → 513 s |
| `n_ref_on_0909_cap60` | low, 73 km, core 1 of 2, margin 5.49 | low, 73 km, core 1 of 2, margin 1.89 | 124 → 210 s |

  Peak memory went from 3.06 to 4.66 GiB on the county run and from 8.83 to 9.67 GiB on the three-day run; the state-wide
  run stayed near 5 GiB. Only one new event passes quality on this recording (at 958 s, 534.5 Hz). No archived flight
  fits it out to 40 km, and whether it is a real sound is unresolved: its ridge is faint and intermittent, and the fit
  could be joining separate sounds. It joins the best rival on all five runs, so margins fall on right and wrong dates
  alike. The other three wrong-date runs and the other field recordings have not finished on it, so the effect on false
  grades is only partly measured, and AGENTS wants the medium grade's scaling fixed first.
- **Checks.** On `7e5cf3e`, whose code is that of `4b88d7c`: `cargo test --workspace --locked -j 2` passed 139 with
  0 failed and 2 ignored, and clippy with warnings denied passed. A release build passed `fdop selftest`: 0.4 m
  error, 75 m radius, +0.20 s clock error, 4 events (known answer, not evidence about real recordings).

### Track, weather and feed fixes

-
  [`a4df296`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/a4df296c3209d46371b8c77d6beab8ce8e3628cb)
  The turn check added in `f717f6f` no longer excuses half of a wrong heading. It built its expected chord from the
  heading under test, so a wrong heading moved its own prediction: at 100 m/s on straight flight it accepted heading
  errors up to 28.8°, against 14.3° for the finite-difference check it backs up. The tangent at a knot is now
  predicted from the chord and the neighbour's reported heading, and the whole reported vector is compared, so wrong
  speeds and vertical rates are still rejected. Exact 3°/s turns sampled every 24–29 s keep their reported tangents.
  One chord that agrees is enough: a turn that starts or ends between samples breaks the constant-rate assumption on
  that side only. In a planar simulation of 4,000 sparsely sampled flights, requiring both chords kept 76 % of the
  correct reports that fail the finite difference, with a 349 m worst interpolation error; one chord keeps 99 %, at
  205 m, against 100 % and 155 m for the old check. At the knot where a turn starts or ends, a wrong heading
  matching the other side's arc can still pass.
-
  [`a306fb1`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/a306fb1d5bb66e60f6bed1fadff2b74b3f4b4a27)
  The binary crate no longer lists frankenpandas; only `fdop-geo` uses it.
-
  [`42d659f`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/42d659f3c46d3b9ee6839e8466ccbd12129e3a31)
  Radiosondes without hourly surface temperatures no longer count as "no wind data". The pipeline read an empty
  weather-warning list as "winds modelled", so that fallback's one warning widened the speed gates by 10 m/s and put
  "No wind data" in the report, although soundings carry wind profiles. `fetch_atmosphere` now says whether winds
  aloft are modelled. Falling back from Open-Meteo to radiosondes also warns now; with hourly surface data it used
  to leave no trace.
-
  [`4df0133`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/4df013301a3b0a15b42c9843d63d0e5020638130)
  A UTC day built without its mlatonly release, or from staging in place of prod, is now a report warning; it used
  to reach only the log. Without mlatonly, aircraft located only by multilateration (transponders without ADS-B Out)
  are missing: they can neither match an event nor count as an unheard close pass.

### A guided peak must clear any stronger bin; reference and field reruns

- `9aeeb9a`'s nearest-peak rule (next section) also accepted ripple on a ridge's top. The nearest ripple hugs
  the model, so a trace could confirm its own model: the fit RMS fell while speed and distance went wrong. On
  the reference county search one matched event moved from v 76.2, d 7374, RMS 0.190 to v 60.5, d 4909,
  RMS 0.124, against 81 m/s and 8,430 m from ADS-B, and stopped matching its flight.
- A candidate must now stand `min_snr` above the valley separating it from any stronger bin (`clearance` in
  `trace.rs`). No parameter was added, and a separate aircraft's ridge still qualifies because a real valley
  divides them: the same-tone overlap tests still pass. `guided_peak_ignores_ripple_on_a_stronger_ridge` fails
  without this. An audio-level wrong-seed test passed with and without the change, so it was not kept; the
  replays below are the evidence that this fixes the reference recording.
- Replays at default budgets, each read from its own `result.json` and `search.json`. `18d8476` is a local build
  with the same production code as `8b4bea3`, which has one test fewer. "Three-day" is the New York window that
  `md2` used; the `r_md` run names follow that name and do not mean Maryland.

| Build | Configuration | Run | Off by | Radius | Grade | Margin (nats) | Recorded cuts |
|---|---|---|---:|---:|---|---:|---|
| `d55ef79` | state-wide | `r_ny` | 224 m | 292 m | high | 7.27 | pairs 66→64, fine 434,223→4,000 |
| `d55ef79` | three-day | `r_md` | 224 m | 292 m | medium | 5.37 | fine 292,213→4,000 |
| `d55ef79` | county | `r_nassau` | 221 m | 292 m | high | 8.05 | fine 48,344→4,000 |
| `3f00c61` | county | `r_nassau_3f00c61` | 221 m | 292 m | low | 7.73 | fine 48,347→4,000 |
| `9aeeb9a` | state-wide | `r_ny_9aeeb9a` | 86,692 m | 360 m | low | 4.51 | pairs 66→58, fine 348,439→4,000 |
| `9aeeb9a` | county | `r_nassau_9aeeb9a` | 14,357 m | 757 m | low | 6.03 | fine 58,725→4,000 |
| `18d8476` | state-wide | `r_ny_18d8476` | 267 m | 316 m | low | 7.72 | pairs 66→63, fine 395,037→4,000 |
| `18d8476` | three-day | `r_md_18d8476` | 267 m | 316 m | low | 6.56 | fine 225,239→4,000 |
| `18d8476` | county | `r_nassau_18d8476` | 306 m | 316 m | low | 11.47 | fine 43,497→4,000 |

- **Why everything is low.** `9620d2e` (after `d55ef79`) fails the margin gate on any recorded pair, fine or
  confirmation cut, and the fine stage cut on every run above. On `18d8476` the other four gates pass on all three
  configurations. So these grades say the comparison was incomplete, not that the point is wrong. No flag raises
  the fine budget.
- **What got worse against `d55ef79`.** The answers are 43–85 m further from the published location. A Robinson
  R44 is now the confirmer where the King Air 350 was; the county search also matches the King Air, at chi 8.79.
  Against `3f00c61`, the corrected event's trace is identical on all 911 points they share but loses the first
  14, giving v 69.6, d 6,437, RMS 0.172. That fit is weakly determined: dropping the first 14, 60 or 100 of
  `3f00c61`'s points gives v 69.6, 73.4 or 80.4. Those 14 points were not restored, because they sit 0.58 Hz above
  the curve with ADS-B's v and d. Other traces also differ, some for the worse: at 2560 s d goes from 526 to 978
  and RMS from 0.368 to 0.865, and 1641 s now fails the RMS gate at 1.54.
- **Field recordings.** Two were rerun on `18d8476`. The osprey cam lands 222 m from the reference recording's
  published location, radius 214 m, low, failing only margin on its recorded cuts at 13.20 nats (`f_osprey_1101_18d8476`).
  `d55ef79` had it 150 m from that point at high, 23.29 nats. The bird-feeder cam stays wrong, on the same Long
  Island town: low, radius 1,176 m, 2.85 nats (`f_feederwatch_18d8476`). `d55ef79` graded that wrong answer
  **medium**, radius 2,019 m, 5.06 nats, which retracts the README's earlier claim that no committed build graded
  a wrong answer above low. The other seven field recordings were not rerun.
- **Wrong dates.** All five wrong-date runs on `d55ef79` were low, failing the core and margin gates on their
  own numbers, one also failing the unheard-pass gate. They have not been repeated on the current code.
- **Checks.** See README "Testing". Release builds of `18d8476` and of the pushed tip `b4c3107` each passed
  `fdop selftest`: 0.4 m error, 75 m radius, +0.20 s clock error, 4 events (known answer, not evidence about real
  recordings).

### Warn when the audio looks high-passed

- `fdop locate` adds a report warning when 45–95 Hz is more than 15 dB below 150–600 Hz, measured as mean power
  over 1 s Hann frames. Nine outdoor recordings read +10.0 to +19.5 dB; the Fullerton, CA railcam, high-passed at
  about 125 Hz, read −26.1 dB. Turboprops' fundamentals sit at about 95–135 Hz, so the warning says the filter
  removes slower propellers' fundamentals and weakens those near the cut, not that most lie below 100 Hz.
  Speech-heavy audio can also read low. The warning changes no answer or grade. Test:
  `low_band_ratio_flags_high_passed_audio`.

### Offline county estimate scales by county size

- When the Census API is unavailable, the county came from the nearest county centroid in the state. Over all
  36,291 county-subdivision internal points in the 2024 Gazetteer, that named the wrong county for 11.8%. The
  estimate now divides distance by each county's equal-area radius and names a second county when its scaled
  distance is within 1.5× of the best. On the same points this names two counties for 33%, and 2.7% end up
  wrong. These percentages come from a one-off evaluation that is not in this repository. The Census API path
  is unchanged, and the search and grades do not use the county. Test:
  `offline_county_names_both_when_too_close_to_call`.

### Recover overlapping tones through detection and measurement

- Detection no longer suppresses a nearby same-tone seed unless its Doppler shape also agrees. Strong
  curves are accepted before weak fallback detections; one residual template-bank scan can recover a
  hidden curve that lost every initial time/pitch cell to another aircraft. The residual scan keeps
  masked samples in the original coverage counts and denominators. Both scans share the existing
  400,000-candidate allowance, detection cap and measurement cap. Weak-only recordings retain their
  ordinary scan at the unchanged silence threshold.
- Guided measurement associates each frame with the nearest local spectral peak meeting the existing
  SNR and robust-z gates inside its model band. This addresses a separate failure where the brighter
  aircraft took over the distant aircraft's trace, even from an exact initial model. Generic unguided
  ridge tracing keeps its strongest-peak behavior; fitting thresholds and physical models are unchanged.
- `simultaneous_same_tone_passes_survive_detection_and_measurement` first reproduced a 1 km pass being
  lost behind a simultaneous 7 km pass with the same 118 Hz tone. Both original and stronger-near-aircraft
  cases now retain two independent measured events and pass the unchanged 0.3 s CPA, 0.3 Hz tone, 4 m/s
  speed and 8% distance tolerances. The single-aircraft controls each contribute one independent event.
  The guided-selector regression and weak-only/cap control also pass. Reference and field recordings
  had not been rerun at this commit; the section above reports the regression those reruns found.
- The extra bank scan has bounded storage: each retained score/index map uses 6 bytes per cell instead
  of the previous 14; temporary scoring arrays still require additional memory. Raw aliases remain a
  measurement-budget cost: the distant-only control now measures two seeds instead of one, although
  measured-curve filtering correctly counts one physical pass. No detection or measurement cap was raised.
- Final targeted and integrated verification passes all 49 default core tests, all 13 shared-type tests
  and both opt-in media integration tests. Core/shared-type all-target Clippy passes with warnings denied,
  and workspace formatting passes. A fresh run of the unchanged `selftest::run` against the final libraries
  exercises the real decode, detect, measure, search and refinement path on 1,300 s of generated audio:
  0.4 m synthetic position error, 75 m radius, +0.20 s clock error and both intended aircraft matched.
  This uses a small external entry point; full workspace checks and the release CLI remain unverified
  after the earlier dependency-compilation SIGKILLs documented in the README.

### Match individual passes by returning aircraft

- An aircraft already assigned to one event can now explain a later confirming event. Confirmation
  excludes the used physical pass, with the same 30 s emission-CPA separation used by pair joining,
  rather than excluding its whole track. The original CPA is recovered geometrically from its stored
  heard time; failed or boundary recovery keeps the conservative exclusion. Existing fitting, clock,
  quality and event-limit rules are unchanged.
- The unheard-pass check also no longer exempts an entire explained track. Its half-distance threshold
  already excludes the explained CPAs themselves; later, closer CPAs must satisfy the same 25 s detection
  window as any other aircraft. Hearing one distant pass cannot hide a silent close return.
- `returning_aircraft_pass_can_confirm_a_candidate` first reproduced a genuine third event being lost
  when two driver slots were occupied. It now retains that event as a core confirmer, recovering the
  synthetic observer within 60 m and the clock within 1 s. The same-pass duplicate control passes in
  still air and wind. `unheard_closer_return_of_an_explained_aircraft_counts` reproduced the silent return
  being missed and covers the original explained passes, a detected return and a detection outside the
  existing hearing window. These are synthetic checks; reference and field recordings have not been rerun.

### Preserve recording time through missing audio packets

- Decoding now preserves timestamp gaps as silence instead of shifting later flyovers earlier in the
  recording. It retains long transport-stream discontinuities, normalizes arbitrary container origins,
  preserves audio delayed behind video, and shifts seeked output to the requested relative origin.
  FFmpeg's fill/trim mode does not enable signal stretching. A duration limit is applied after filling,
  so a requested interval wholly inside a gap returns the appropriate silence. First-stream selection
  remains consistent with the media probe.
- `decode_preserves_recording_timestamps` reproduced 4 s of decoded samples from a 6 s timeline before
  the fix. Both actual FFmpeg/FFprobe integration tests now pass, covering the exact 2 s gap, four seek
  windows, a nonzero 123 s origin, delayed audio and a 16 s MPEG-TS gap. Surviving unresampled PCM samples
  are identical to the continuous reference, and the first-track selection test stays green. These are
  generated fixtures; the reference configurations and field recordings have not been rerun.

### Provisional numerical margins after comparison cuts

- Both numerical and absent-rival margins now fail the confidence gate after a recorded pair, fine or
  confirmation comparison cut. An observed gap against partially evaluated rivals is an upper bound on
  the completed gap, so it cannot establish either minimum-margin threshold. Candidate ranking and the
  existing 3/6-nat thresholds are unchanged. Reports preserve the numerical gap, label it provisional and
  identify the cut; they no longer describe an exponentiated evidence difference as a likelihood ratio.
- Both margin tests pass. `margin_from_incomplete_confirmer_scores_cannot_certify_a_winner` uses the actual
  evidence calculation to reproduce an 8.79-nat apparent lead whose rival wins once two unchecked confirmers
  are included, with just a 0.0245-nat final separation. The new test and updated boundary test failed before
  the correction. Pipeline formatting passed; full CLI compilation and real-recording replays remain
  unverified. Historical truncated reference runs may receive lower confidence under this gate.
- This completes the explicitly truncated-comparison correction below. It does not establish search-wide
  calibration: driver and hypothesis limits can also leave unseeded explanations. Detection and measurement
  caps remain unchanged.

### Track recovery across dense corrupt reports

- Track cleaning now considers every possible predecessor within the configured time gap, replacing its
  fixed 48-report lookback. Dense bursts of corrupt ADS-B positions can no longer hide both sides of a
  genuine flight from the longest-chain calculation. An exact prefix bound stops the scan when earlier
  chains cannot improve the answer, retaining fast processing of clean tracks without changing physical
  speed, vertical-rate or gap limits.
- `dense_bad_position_bursts_do_not_replace_the_real_track` first reproduced half of a valid trajectory
  being discarded after 49 bad reports. All five track tests pass with the correction. The regression
  retains all 160 genuine samples with 2, 49 or 97 corrupt reports, keeps the actual 21 s gap visible and
  interpolates through it within 1 m of synthetic truth. The reference configurations and field recordings
  have not been rerun; integrated core and selftest checks are recorded separately when completed.

### Interior hypotheses inside the requested region

- A cross-track side now skips its interior sample only when an ordinary grid point passed all
  hypothesis acceptance checks. A terrain/distance-valid point outside the requested region or clock
  window cannot replace a usable interior point. The shared central point follows the same rule.
  This resolves an interaction between the close-pass fallback and the expanded hillside scan;
  the original distance, terrain-clearance, region and clock predicates are retained.
- The actual hypothesis-generation regression `outside_region_grid_points_do_not_suppress_interior_hypotheses`
  reproduced a valid observer losing every hypothesis when the scan continued to an out-of-region
  hillside point. It covers both flight sides and an otherwise valid shared center outside the region.
  The regression failed before the flag correction; all 21 search tests passed on the combined code,
  including the concurrent confidence-margin correction. The other
  core and shared-type tests passed in the preceding integrated run (39 default core tests and 13
  shared-type tests), and the explicit multi-track media test passed too. The reference configurations
  and field recordings have not been rerun.
- The unchanged `src/selftest.rs`, invoked through an external entry point linked to the final core
  libraries, passed the real FFmpeg decode/detect/measure/search/refine chain on its full 1,300 s
  synthetic recording: 0.4 m error, 75 m radius, +0.20 s clock error and both intended aircraft matched.
  This was not a full CLI or default release-profile build; workspace compilation was blocked by the
  host killing the `asupersync` compiler process.
- Core and shared-type Clippy passed for all targets with warnings denied, and workspace formatting
  passed, using the pinned nightly and the same one-job, unoptimised-dependency settings as the tests.
  The final workspace-wide Clippy attempt also ended in SIGKILL, while checking `asupersync 0.5.0`
  metadata before reaching project crates. Neither workspace-wide check is reported as a pass.

### Confidence after incomplete rival comparisons

- An absent distinct rival no longer automatically passes either confidence margin threshold when pair,
  fine or confirmation comparisons were cut. Confirmation-budget cuts are now included in the funnel and
  warnings. Explicit numerical margins retain the existing 3/6-nat comparisons; missing or malformed margin
  records cannot supply confidence. Reports explain when the comparison is incomplete.
- The new policy regression covers both threshold boundaries, absent/malformed margins, empty candidates,
  and all three recorded comparison cuts. It passes in an isolated Rust build of the production helper and
  data fields using the pinned `serde_json` dependency. Both changed Rust files pass pinned rustfmt, and
  targeted UBS reports no critical issues. Core integration subsequently passed in the 21-test search
  suite above. Full CLI integration, workspace checks, release selftest and real-recording replays
  remain unverified under the host's resource limits.
- This does not establish search-wide calibration or resolve the documented expanded-cap false-medium case;
  detection and measurement caps stay unchanged. The concurrent Doppler-shape harmonic fix is preserved.

### Overlapping flyovers and harmonic families

- Distinct Doppler curves no longer disappear solely because their closest-approach times overlap and
  their tones have a harmonic ratio. Detection compares modelled shapes using the existing coarse
  ridge-mask width; search compares measured traces at the same recording times using their combined
  noise scales and the existing clean-fit threshold. A fitted frequency ratio allows for different
  sampling and imperfect tone estimates. Insufficient two-sided overlap keeps a possible harmonic
  grouped rather than treating it as another aircraft.
- Harmonic ratios are symmetric, so a stronger high harmonic and a weaker fundamental stay in one
  family. Search now uses the detector's small-integer ratio limits (numerators through eight,
  denominators through four); its former numerator limit of six could let a seventh or eighth
  harmonic of the fundamental count as an independent flyover.
- A synthetic simultaneous two-flight regression recovers both correct matches, the observer within
  60 m and the recording clock within 1 s. Regressions also cover fifth, seventh and eighth harmonics
  in both strength orders, neighbouring coarse templates, unequal sampling, independent noise,
  fitted-tone/CPA offsets and insufficient overlap. The reference configurations and field recordings
  have not been rerun; these are synthetic checks only.
- After integrating the concurrent terrain, hypothesis-budget and clock-pruning changes, all 39 default
  core tests and all 13 shared-type tests passed. The explicit multi-track media test passed too. Workspace
  formatting passed; the full workspace test build was killed while compiling `asupersync` under the
  shared host's memory limit, before any workspace tests ran.

### Resumable archive completion

- A successful partial response no longer implies that the whole archive is complete. Its range-body length
  stays separate from the object's total length, including `Content-Range: bytes 10-49/*`. Downloads continue
  through successive capped ranges and require an explicit matching total, a complete full response, or an
  exact `416 bytes */N` completion proof. Successful progress does not consume the no-progress retry budget.
- Each response's end offset is checked before forwarding bytes, including chunked responses without
  `Content-Length`. Invalid or changed totals, reversed or overflowing ranges, and premature EOF are rejected.
  Servers that ignore `Range` still use the existing prefix-skip recovery without duplicating bytes.
- Three tests of the exact production byte-extent helper passed, with the previous unknown-total inference
  reproducing a failure. Standalone Clippy with warnings denied, pinned formatting and the Rust bug scan passed.
  Three additional loopback regressions are present but unrun because the execution environment disconnected.
  The recovered source's Git blob matches the tested/formatted source exactly. Object identity/ETag handling
  is unchanged; these checks establish byte accounting and completion, not cryptographic identity.

### Close-flyover search coverage

- Close flyovers can now seed the search when their allowed distances fall entirely between the
  regular 350 m cross-track samples. On a side with no sample satisfying the distance and
  terrain-clearance gates, the search tries an interior offset using the actual terrain at
  every probe. The ordinary grid and all acceptance gates are retained.
- The synthetic regression `close_flyovers_between_cross_track_grid_lines_locate_the_observer`
  reproduced two valid event/flight combinations with zero hypotheses. With the correction,
  both correct flights seed the answer, recovering the observer within 60 m and the recording
  clock within 1 s. Additional checks cover rising and falling terrain, impossible altitude,
  missing or discontinuous terrain, and minimum-clearance rejection. Both new tests passed.
  The reference configurations and field recordings have not been rerun.

### Multi-track media decoding

- Audio decoding explicitly selects the first audio stream, matching the stream inspected by the probe.
  Previously FFmpeg could automatically prefer a different default or higher-channel-count track, so the
  detector could analyse an alternate mix instead of the probed recording.
- An opt-in media integration test synthesises a two-track file with distinct tones and a preferred stereo
  alternate, then verifies that the actual probe/decode path reads the first mono track. It passed with
  FFmpeg and FFprobe installed; it is ignored by the default offline unit suite because it needs those tools.

### Confidence-radius refinement

- Refinement checks whether the accepted 95% contour reaches any edge of its actual grid before
  deciding whether to widen and recenter. Previously it compared the contour's extent around the best
  point with the grid's half-width, which could mistake a clipped contour for a complete one when the
  best point was near an edge. The search now explores the wider window in that case; if the contour
  remains clipped after the existing expansion limit, the radius is reported as a lower bound.
- A synthetic two-flight regression covers minima at all four grid edges and checks that widening
  recovers a complete contour around the known observer. The reference configurations and field
  recordings have not been rerun; their historical distances and grades do not validate this change.

### Search recovery and memory use

- Hypothesis generation now follows the full cross-track reach across varying terrain. A valley point outside
  the slant-distance gate no longer suppresses valid observers on a later hillside. The synthetic regression
  covers both sides of a flight and a flat-ground negative control; the old loop produces no eligible cloud.
- Clock pruning now removes candidates that no longer explain two distinct events, and checks newly promoted
  leaders until the leading set is finalized. A strong one-event coincidence can no longer block a valid weaker
  pair, and the former eight-round cutoff cannot leave a promoted winner unchecked. Synthetic regressions use
  180 rejected candidates and an actual trace/track case whose incompatible pair collapses to one event.
- Hypothesis budgets now respect the intended tenth of detected RAM on smaller machines instead of imposing
  a 4 GB minimum. Generation accounts for retained hypotheses in batches, before completing an entire track;
  over-budget and confirmer-only events still keep exact counts and the unchanged deterministic chance sample.
  Regressions compare retained clouds and samples with the former algorithm and bound retained elements across
  interleaved workers. This is a payload allowance, not an exact process-memory ceiling.
- All 30 core tests pass on these search changes before integration of the concurrent close-flyover, confidence-radius
  and multi-track audio fixes. The terrain regression fails with the former early exit. The combined tree has not
  been rerun since the execution environment disconnected.
  Cargo used the pinned nightly, one build job and unoptimised dependencies under the shared host's memory limit.
  Core Clippy was attempted but its `simba` dependency received SIGKILL before project checking completed;
  workspace formatting and the targeted Rust bug scan passed.
- Detection and measurement caps, scoring formulas and confidence thresholds are unchanged. Reference recordings
  and field cases have not been rerun, so these changes establish synthetic recovery and memory behavior only.

### Search driver selection ([`b2a6906`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/b2a69069876e5d25f319305ffc7f46ca9ffd6ba0))

- Events with no eligible ADS-B hypotheses no longer consume `--max-events` driver slots or form empty
  event pairs. Previously, a stronger unmatched flyover could occupy a slot and exclude a weaker, genuinely
  matchable flyover, leaving no location even when the necessary pair was present. Zero hypothesis counts
  remain visible in the funnel. Detection caps, measurement caps, memory budgets and confidence rules stay
  the same.
- The synthetic regression `unmatched_events_do_not_consume_pair_driver_slots` reproduced that failure with
  two driver slots: one empty cloud, one retained real cloud, and the second real cloud demoted to confirmer.
  With the correction, both archived flyovers seed the answer and recover the observer within 60 m and the
  recording clock within 1 s. The reference configurations and field recordings have not been rerun.

### Curved-track reconstruction ([`f717f6f`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/f717f6faec84803d360207884e91c8b830e3578c))

- Correct reported ADS-B velocities are retained on sparsely sampled turns. The old comparison treated a
  position chord as an instantaneous tangent, so a 100 m/s aircraft turning at 3 degrees/s could lose its
  correct velocity when reports were 25–29 s apart. The builder now also compares each nearby position chord
  with the circular-arc average implied by the reported headings, using the existing 25 m/s residual limit.
  The fallback requires neighbouring headings and still rejects inconsistent speed, heading and vertical rate.
- Synthetic regressions cover both turn directions, uneven sampling, endpoints and heading wraparound, with
  interpolated positions checked against an analytic circle. These are synthetic checks only: the three
  reference configurations and field recordings have not been rerun on this change.

### Changes

- Streamed download resumption now validates every `206 Partial Content` response before accepting bytes: `Content-Range` must start at the requested offset, its span must agree with `Content-Length`, and the total object length must stay constant across reconnects. A contradictory partial response fails instead of silently splicing the wrong bytes into an ADS-B archive. Servers that ignore `Range` with `200 OK` still use the existing safe skip fallback. Three loopback regressions cover wrong offsets, changed totals and mismatched partial lengths.
- The pipeline's weather deadline now includes the preferred pressure-level request's 40 s timeout in
  addition to the previous 150 s fallback allowance. Without that adjustment, deferring fallback requests
  could discard weather that the previous concurrent schedule would have received and instead use a standard
  atmosphere. The deterministic scheduling regression passes in an isolated Rust build and fails with the
  former wait; it performs no network calls or sleeps. Workspace Clippy was retried with one job and
  unoptimised dependencies but its `asupersync` dependency was killed by the host's memory limit, so full
  workspace tests, weather-selection tests and release selftest remain unverified.
- The deferred-measurement regression now also exercises first-round and later-round fitting failures on
  synthetic audio. It checks that a failed later round retains the last successful trace and its own complete
  fit, reports the failure, and rejects the event. A failed first round retains its trace for inspection and
  returns no fit. The concurrent `PreparedFit` implementation and its eager/deferred equivalence tests are
  preserved. All 25 core tests pass on the integrated code, including curved-track and search-driver recovery.
  Core all-target Clippy with warnings denied, workspace formatting and the Rust bug scan also pass.
- Successful Open-Meteo pressure-level weather no longer waits for or requests unused radiosonde and surface
  fallbacks. If the preferred source fails, the two fallbacks still load concurrently with the same selection
  order and warning behavior. Five offline tests cover the preferred source and every fallback combination.
  Pinned formatting and source review passed; the new tests could not run after dependency OOM failures and
  execution-environment loss. No live-service latency claim is made.
- Measurement now computes profile-likelihood ranges and manoeuvre diagnostics only once, for its final
  accepted ridge trace. All refinement rounds keep the same robust point fits and model/band updates; the
  public fitting API still returns complete diagnostics. A failed later round retains the last successful
  trace with its own fit. Two synthetic regressions compare exact point estimates, trace arrays and all
  serialized fit fields against eager refinement. The diagnostic arithmetic is unchanged. These equivalence
  tests now pass as part of the 25-test core suite after recovery from the earlier dependency OOM and
  environment loss. The run used the pinned nightly with one build job and unoptimised dependencies;
  real-recording timings remain unverified.
- An explicit `locate --start` now limits archive-availability checks, weather retrieval and ADS-B downloads
  as well as the search. A broader `--dates` range no longer requires unrelated UTC-day archives, so a known
  recording clock can run with only its required days cached. Track padding and recordings crossing UTC
  midnight still request both days. The clock window must overlap the selected local dates, its tolerance
  must be finite and nonnegative, and unpublished coverage clips the actual requested window. An unusable
  metadata hint no longer creates an inverted search window; a metadata fallback reports the window actually
  searched. Four offline regressions cover day selection, midnight coverage, invalid clocks and metadata
  conventions. No new real-recording accuracy or timing claim is made.

### Representative commits

- [`3e62867`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/3e62867b3d5d278a8588bccf9528a43e5ebd99bb) Initial
  public import of the workspace: the `fdop` binary and the five `fdop-*` crates, 67 offline unit tests and the
  synthetic `selftest`.
- [`d79d90d`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/d79d90dc334099e9a4594bc28c0b1dea216c1a5c) Evidence
  calibration for tiny regions: when an event has fewer than 20 chance matches to calibrate against, a fit with
  chi ≤ 2 can stand in for the nats-over-chance test of a core match (section 11). The reference-case runs
  `ny9`, `md2` and `nassau3` used this code.
- [`593e41f`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/593e41fd673a9e026178cc31a63462ab36697c3c) README,
  AGENTS.md, this changelog, and a GitHub Actions workflow (later removed: the project uses dsr, see below).
- [`e872b1c`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/e872b1c001592b2d732d3bc49779a15c6b15238e) Recent
  recordings no longer fail on the newest UTC day. adsb.lol publishes each day a few hours after it ends, and a US
  local date runs into the next UTC day, so `--dates <yesterday>` used to stop with "no ADS-B data for UTC day(s)".
  `locate` now searches up to the published part and warns which days are pending.
  The same commit fixes two cache bugs:
  - a day streamed before its mlatonly (or prod) release existed was cached as complete;
  - an incomplete `releases.json` was never re-checked.

  It also adds the `truth` example, which lists the archived passes near a known place and clock.
- [`1f321cb`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/1f321cbcc9cc861a49eb2d12dce95716bd304fb5) Junk
  positions inside an archived leg no longer fake an unheard close pass. MLAT mis-solutions, or a second aircraft
  under the same ICAO address, can put isolated points kilometres off the real path. Track models are now built
  from each gap-free segment's longest physically consistent chain of points:
  - horizontal step ≤ v_lim·dt + 250 m, with v_lim = max(1.5 × median ground speed + 30, 80) m/s;
  - vertical step ≤ 30 m/s·dt + 120 m.

  Evidence: in the 2026-11-01 osprey-cam field test (below), a P28R leg with such points made a phantom pass
  300 m from the answer, and the unheard-pass gate failed. With the filter, the same point went from medium
  (margin 5.9 nats) to high (margin 8.3 nats). The reference case (`ny10`) is unchanged. The `truth` example gains
  `dump=<icao>`, which prints an aircraft's archived points and its track models' closest approaches. 2 new tests,
  70 in total.
- [`6c7f535`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/6c7f535a1eb8a19a51aba77387625add87266a32) Three silent fallbacks now put a warning in the report:
  - a default geoid height when the NGS lookup fails or the run is `--offline`;
  - terrain tiles that could not be fetched and were read as 0 m, with their count;
  - an event dropped to confirmer-only because its hypotheses did not fit the budget.

  `fdop fetch` now fetches only the published part of the archive, like `locate`. Before, it failed on a date
  that ran into an unpublished UTC day. The `--near` and `--cache` help texts now match the parser and the cache
  precedence. 1 new test, 71 in total.
- [`25f12ff`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/25f12ffa96af6411dbb7a3a177a9b514b11641b9) A
  streamed download whose server goes silent no longer hangs. `fdop fetch --dates 2026-11-01 --near CA` sat at
  0% CPU on open connections to the GitHub release CDN: the client's request timeout does not cover a streamed
  body. Each body frame now has a 60 s idle timeout, after which the download resumes with an HTTP `Range`
  request. 1 new test (a loopback server that stalls mid-body), 72 in total.
- [`852067a`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/852067a4442c5bf96da8ed1c0dd50216dc5b265d) The GitHub Actions workflow is removed. The project never uses GitHub Actions: its checks (rustfmt, clippy
  with `-D warnings`, the tests and `fdop selftest`) run through `dsr quality franken_doppler_finder`.
- [`4cb7aff`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/4cb7affc057fa9eabcc78d9cde8f8e0d310c2369) Unused dependencies
  dropped: `fsci-signal` leaves the build, and `fsci-runtime` and `fsci-opt` stop being direct dependencies where
  nothing used them.
- [`5770663`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/5770663ff5ec510421d38cf01504ccb84ffcdb8a) CLI
  fixes:
  - a date must start with its four-digit year: `09/08/26` used to parse as 26 August of year 9, and now gets
    "put the four-digit year first";
  - `fdop detect` no longer takes `--cache` and `--offline`, which it ignored;
  - the `--dates` help lists every accepted form;
  - a selftest that finds no candidate prints FAIL and exits 1 instead of 2.
- [`b2256c7`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/b2256c73df4fc170ecaa1ab280dd35eb8a5d1855) The detection cap
  (60 candidate curves) and the measurement cap (24 events) now put a warning in the report when they cut. Every
  reference run cuts both (82 curves, 32 strong flyovers). A state search whose answer reverse-geocodes into
  another state is flagged. A `locate` that stops with an error is recorded in the run ledger. 2 new tests, 74 in
  total.

  The caps themselves stay. A working-tree build that scaled them with duration gave the reference recording
  the ny10 answer. On the 3 h osprey-cam recording of 2026-10-31 it measured 72 events instead of 24, and a
  wrong answer graded low became a wrong answer graded **medium**, 103 km off. Its two core events were ones the
  old cap never measured, and its margin was 3.01 nats against the medium bar of 3. The medium bar does not grow
  with the number of events searched; raising the caps waits on that.
- [`532fb15`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/532fb15ab0d4daf2c6b7303d4ad89e78465d6384) `fdop doctor` prints an `[info]` line naming the Census gazetteer tables that are not cached yet. `--offline`
  needs them for place, county and ZIP regions and for the report's place names.
- [`77529e3`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/77529e368ec5c6b54508676b1352e918315d2892) When the answer's state comes
  from the gazetteer (offline, or when the Census Geocoder fails), the outside-state warning says it is an
  estimate, since only the Census lookup tests containment. A `locate` that fails without `-o` now records the
  default run directory holding its partial outputs.
- [`b92a799`](https://github.com/Dicklesworthstone/franken_doppler_finder/commit/b92a799ceca8f6b92b9ede73b718cd0d8807e8e9) A `locate` that fails
  without `-o` names its run directory in the error message, if it wrote anything there. README Troubleshooting
  covers the year-first date refusal, the cap warnings and the outside-state warning.

---

## Version Timeline

| Version | Kind | Date | Summary |
|---|---|---|---|
| `0.1.0` | Workspace version only (no tag, no GitHub Release) | 2026-11-03 | Initial public import: the full `fdop locate` pipeline, `detect`, `fetch`, `runs`, `doctor`, `selftest` |

### Reference-case runs

All runs use the raw recording from the source video (one hour) against the published answer
(40.876466, −73.524951). The distance is great-circle, from that answer.

| Run | Time | Arguments | Result | Off by | Radius | Confidence | Total |
|---|---|---|---|---|---|---|---|
| `ny1` | 09:12 | `--dates 2026-09-08 --near NY` | did not finish (hypothesis volume) | — | — | — | — |
| `ny2` | 09:25 | same | Old Lyme, CT | 116 km | 490 m | low | 167 s |
| `ny3` | 14:51 | same | Oyster Bay (town), NY | 843 m | 1,441 m | low | 607 s |
| `ny4` | 15:07 | same | Oyster Bay (town), NY | 831 m | 1,446 m | low | 300 s |
| `ny5` | 15:19 | same | Oyster Bay, NY 11771 | 129 m | 304 m | medium | 262 s |
| `ny6` | 15:27 | same | Oyster Bay, NY 11771 | 129 m | 304 m | medium | 264 s |
| `ny7` | 15:34 | same | Oyster Bay, NY 11771 | 129 m | 304 m | **high** | 212 s |
| `nassau1` | 15:50 | `--dates 2026-09-08 --near "Nassau County, NY"` | Oyster Bay, NY 11771 | 202 m | 304 m | low | 133 s |
| `md1` | 15:51 | `--dates 2026-09-07..2026-09-09 --near NY` | Southold, NY | 102 km | 377 m | low | 833 s |
| `nassau2` | 16:05 | `--dates 2026-09-08 --near "Nassau County, NY"` | Oyster Bay, NY 11771 | 344 m | 976 m (lower bound) | high | 119 s |
| `ny8` | 16:07 | `--dates 2026-09-08 --near NY` | Oyster Bay, NY 11771 | 383 m | 997 m (lower bound) | high | 273 s |
| `nassau3` | 16:38 | `--dates 2026-09-08 --near "Nassau County, NY"` | Oyster Bay, NY 11771 | 221 m | 292 m | **high** | 101 s |
| `ny9` | 16:41 | `--dates 2026-09-08 --near NY` | Oyster Bay, NY 11771 | 224 m | 292 m | **high** | 295 s |
| `md2` | 16:50 | `--dates 2026-09-07..2026-09-09 --near NY` | Oyster Bay, NY 11771 | 224 m | 292 m | **high** | 440 s |

Times are when each run finished. ADS-B was cached for every run after `ny1`, on an Apple M5 Ultra. Every wrong
answer was graded low. `nassau2` and `ny8` kept the R44 as a fourth match, and their radius touched the
refinement window's edge; the import's radius gate fails a radius that is only a lower bound. `nassau3`, `ny9`
and `md2` are the results on `d79d90d` (section 11). `ny10` repeated `ny9` on `1f321cb`:

| Run | Time | Arguments | Result | Off by | Radius | Confidence | Total |
|---|---|---|---|---|---|---|---|
| `ny10` | 17:56 | `--dates 2026-09-08 --near NY` | Oyster Bay, NY 11771 | 224 m | 292 m | **high** | 226 s |

### Field tests

Nine recordings from public YouTube live cams, 2–3 h each, all at sites with published locations. Each true
clock came from the stream's fragment timestamps. Each was run blind with
`fdop locate FILE --dates <local date> --near <state>`. The first six ran on working-tree builds made just before
`e872b1c` was committed, and all six again on `1f321cb`; the table gives both where they differ. The last three
ran on a build of the tree at `25f12ff`.

| Recording | Date | Result | Confidence |
|---|---|---|---|
| Osprey cam at the reference site (Oyster Bay, NY) | 2026-11-01 | Oyster Bay: about 150 m from the cam's geocoded street address, just outside the 147 m radius; start off by 0.33 s | medium before `1f321cb`, **high** after |
| Same osprey cam | 2026-10-31 | wrong: Commack, NY before `1f321cb`, Wethersfield, CT after | low |
| Railcam, Rochelle, IL | 2026-11-02 | wrong: Charrette, MO (about 400 km off) at first; Roscoe, IL (about 50 km off) on `1f321cb` | low |
| Railcam, Tucson, AZ | 2026-11-02 | wrong: Williamson, AZ (about 300 km off) at first; Wet Camp Village, AZ (about 140 km off) on `1f321cb` | low |
| Bird-feeder cam, Ithaca, NY | 2026-11-02 | wrong: Lincoln, VT (about 330 km off) at first; Hillsgrove, PA (about 120 km off) on `1f321cb` | low |
| Railcam, Plant City, FL | 2026-11-02 | no location (0 usable flyover events) | none |
| Railcam, Fullerton, CA | 2026-11-01 | no location (1 usable flyover event of 4) | none |
| Eagle cam, Duke Farms (Hillsborough, NJ) | 2026-11-01 | Hillsborough, NJ 08844, the right township: about 2.2 km from the property's entrance address. The nest position is not published and the grounds span about 3 km, so a smaller error cannot be scored. Start 5.3 s before the stream's first-frame stamp | low |
| Same osprey cam, a thin day | 2026-11-02 | wrong: Islip, NY (about 40 km off) | low |

Of the first six, one was located, at the site the reference recording also came from. Every wrong answer was
graded low. On `1f321cb` each wrong answer moved but stayed low. Each had only 1 of 2 core matches, and each margin
over the best different explanation was 0.4–2.1 nats. In Rochelle the first build's answer became the runner-up.
A low answer that moves between builds is what a chance match looks like.
The four other sites had only 0–3 non-jet passes within reach during their windows, and their detections were
mostly trains, idling locomotives, wind and traffic.

The last three:
- **Fullerton** had 13 non-jet passes within 3 km in its 3 h window, training traffic from an airport about 5 km
  away, but none was audible. The audio is high-pass filtered, −3 dB at about 125 Hz and falling 24 dB per
  octave. Relative to 200–300 Hz, 45–95 Hz is 20–40 dB down and 100–120 Hz is 8–13 dB down, so the
  fundamentals of slower propellers are gone and turboprop fundamentals (95–132 Hz in the reference recording)
  are weakened. Broadband wind and road noise fills 120–300 Hz, where the harmonics sit. Either could have
  hidden the passes; the audio cannot say which. Its 4 detections were trains and vehicles (one with a 42 m
  closest approach). An earlier version of this entry blamed wind and road noise across 50–300 Hz alone, but
  such noise raises the low band, and here 45–95 Hz is 26 dB below 150–600 Hz.
- **Duke Farms** had 16 passes within 3 km, but a wind-saturated microphone. All four matched aircraft (two core,
  two confirming) were real passes over the property, so the search found the right flights and clock and
  placed them loosely. Only 1 of 4 traces fit cleanly (chi 1.2; the others 10–12), and a C172 passed 604 m from
  the answer unheard. The margin passed at 8.7 nats; the core-match and unheard-pass gates failed, correctly.
- **The osprey cam on 2026-11-02** (15:45–18:30Z) had only 4 passes within 3 km, one of them close: margin
  4.8 nats, 1 of 2 core.

Neither dense-traffic site reached high: one was high-pass filtered under road noise, the other had a
wind-saturated microphone. Every wrong answer from a committed build is still low: nine across the field tests
and builds.

---

## 0) Method capture, before this repository

Starting at 04:39, the user had colsto's video ("I discovered a sound that leaks your exact location (and it's
everywhere)", https://www.youtube.com/watch?v=UytYrhYLOKI) downloaded, transcribed and analysed frame by frame,
and the datasets it uses researched. That pass produced the private `doppler-sounds-for-geolocation` agent
skill, outside this repository. It holds a Python reference oracle; method, physics, data-source and
countermeasure notes; and validation gates V1–V4 on the author's raw recording and the free adsb.lol data.

Those notes fixed the physics (the emission-time model and its closed forms at c = 343 m/s), the reference
answer, and the reference flights: a PC-12 and a King Air 350, 924 s apart.

At 07:03 the user asked for every step to be automated in high-performance Rust on the franken libraries and
asupersync, in this repository. The target: given a recording, a date range and a rough US region, produce a
latitude and longitude, a radius, a map, and the nearest town and ZIP code.

## 1) Workspace and foundations (07:32–07:48)

These are the Cargo workspace, the HTTP client and the shared types everything else builds on.

### Delivered capability

- **Workspace:** Rust 2024, resolver 3, pinned to `nightly-2026-08-31`; the `fdop` binary skeleton.
- **`fdop-http`:** an asupersync HTTP/1.1 client over rustls. It retries transient failures with backoff,
  resumes streamed downloads with `Range`, and reads several URLs as one stream. The `probe` example measures
  archive throughput.
- **`fdop-types`:**
  - WGS84, ECEF and ENU geodesy;
  - civil time and lenient ISO 8601 parsing;
  - the ICAO engine-class table (2,614 type designators);
  - the gzip `.fdtrk` track-store format (magic `FDOPTRK1`, 42-byte points).
- **`src/probe_deps.rs`:** written first to confirm the franken APIs. It was never declared in `src/lib.rs`, so it
  is unreferenced at import.

### Key files at import

- [`Cargo.toml`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/Cargo.toml),
  [`crates/fdop-http/src/lib.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-http/src/lib.rs),
  [`crates/fdop-types/src/store.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-types/src/store.rs)

## 2) Data layer: adsb.lol ingest and the track store (07:56–08:24, subagent)

This wave turns a ~4 GB global daily archive into a small regional store that the search can scan in
milliseconds.

### Delivered capability

- **Release discovery** per UTC day: `prod`, with `staging` as a fallback and `mlatonly` added. Split tar parts
  are concatenated into one stream.
- **Streamed ingest** of readsb `trace_full_*` members, parsed in parallel on rayon without extracting the
  archive. Jets are dropped and region and time filters are applied as each member is parsed.
- **`build_store`:**
  - merges feeds per ICAO address and deduplicates points;
  - splits legs on gaps and readsb new-leg markers;
  - corrects barometric to geometric altitude, from the leg's own offsets first, then regional medians.
- **Caching:** day stores are reused whenever their box covers the request. Multi-day windows merge legs across
  midnight.
- **Terrain and geoid:** AWS Terrarium elevation tiles with an offline fallback; the NOAA NGS geoid API with a
  −30 m default.
- **Examples:**
  - `ingest_ny`: the reference-case ingest;
  - `check_oracle`: interpolated positions of the two reference flights against the oracle;
  - `store_stats`.

### Key files at import

- [`crates/fdop-data/src/ingest.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-data/src/ingest.rs),
  [`build.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-data/src/build.rs),
  [`fetch.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-data/src/fetch.rs),
  [`adsblol.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-data/src/adsblol.rs)

## 3) Geo layer: regions, places, maps and reports (07:54–08:10, subagent)

This wave covers everything between a user's "somewhere near Oyster Bay" and the finished report.

### Delivered capability

- **The `--near` grammar:**
  - states and state lists, and CONUS;
  - Census places and counties;
  - ZIP codes (ZCTAs) and DC;
  - points with an optional radius, and S,W,N,E boxes;

  each with its UTC-offset range, DST included.
- **Place data:** the Census 2024 gazetteer, downloaded lazily; reverse geocoding through the Census Geocoder,
  falling back to the gazetteer's internal points offline. The headline reads "Town, ST ZIP (County)".
- **Static maps:** a pure-Rust static map on USGS Topo tiles, labelled with an embedded bitmap font, plus an SVG
  twin.
- **Reports:** HTML (Leaflet), Markdown and CSV, the tables built with frankenpandas.

### Key files at import

- [`crates/fdop-geo/src/region.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-geo/src/region.rs),
  [`geocode.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-geo/src/geocode.rs),
  [`staticmap.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-geo/src/staticmap.rs),
  [`report.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-geo/src/report.rs)

## 4) Signal core: from audio to fitted flyovers (08:31–08:52)

This wave finds inaudible curves automatically and measures them with honest ranges. It replaces the oracle's
hand-placed seeds.

### Delivered capability

- **`physics.rs`:** the closed-form emission-time model for a straight pass, plus the readouts used to seed
  fits.
- **`audio.rs`:** decoding through ffprobe and ffmpeg, plus parsing of `creation_time` and ISO 6709 location
  tags.
- **`spectro.rs`:** STFT on fsci-fft, row-median normalisation, and PNG rendering.
- **`detect.rs`:** a log-frequency matched-filter bank of emission-time templates over speed × distance, scored
  by the weaker of the approach and recede halves. It is followed by non-maximum suppression, harmonic grouping
  and coordinate-descent refinement.
- **`measure.rs` and `trace.rs`:** an adaptive STFT window (≈ 1/√ steepest slope), a model-guided ridge trace,
  and quality gates.
- **`fit.rs`:**
  - fsci-opt bounded trust-region least squares, with multistarts and soft-L1 then linear loss;
  - profile-likelihood ranges via an fsci-stats F test;
  - manoeuvre, side-disagreement and structured-residual diagnostics that widen the ranges.
- **`track.rs`:** velocity-Hermite ECEF track models, emission-time solves and CPA search.
- **The `detect_file` example.**

### Key files at import

- [`crates/fdop-core/src/detect.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/detect.rs),
  [`measure.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/measure.rs),
  [`fit.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/fit.rs),
  [`physics.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/physics.rs)

## 5) Search and the `fdop` CLI (09:01–09:31)

This wave joins measured events to flights and wraps the whole pipeline in one command.

### Delivered capability

- **`search.rs`:**
  - flight-centric hypothesis clouds;
  - a spatio-temporal pair join;
  - a fine grid with the exact emission-time model and a clock scan;
  - confirmers, the unheard-close-pass veto, and 25 m refinement with a Δχ² radius.
- **`pipeline.rs`:** `fdop locate` end to end. It derives the search window from local dates and the
  region's time zones, then writes the figures and reports.
- **The other subcommands:**
  - `ledger.rs`: the FrankenSQLite run ledger behind `fdop runs`;
  - `doctor.rs`: `fdop doctor`;
  - `selftest.rs`: `fdop selftest`, a synthesised two-turboprop recording with three silent decoys.
- **The `explain` example,** which scores named (event, flight) pairs at a point.

### Key files at import

- [`crates/fdop-core/src/search.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/search.rs),
  [`src/pipeline.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/src/pipeline.rs),
  [`src/cli.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/src/cli.rs),
  [`src/selftest.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/src/selftest.rs)

## 6) Scaling the search to a whole state (09:13–09:27)

The first state-wide run, `ny1`, did not finish. Jets were already dropped at ingest, so the 7,167 legs were all
tonal or unknown-type, and the problem was hypothesis volume.

### Delivered capability

- **The hypothesis and join layer was rewritten:**
  - hypotheses are compact (20 bytes of f32 each) with per-event and total budgets;
  - event selection and pair ordering account for cloud size;
  - a quick check runs on a precomputed ratio grid, with no cold emission-time solves.
- **Results:** the selftest still recovered the observer to 0.4 m. `ny2` then completed in 167 s but named the
  wrong place (Old Lyme, CT, 116 km off). The gates graded it low.

## 7) Weather-aware search (14:25–15:01)

The search moved from still air to the real atmosphere.

### Delivered capability

- **Atmosphere types** (`atmo.rs`): standard, uniform, gridded, scattered and surface-adjusted. Sound speed uses
  moist air; each path's speed of sound is a harmonic mean, and its wind a height average.
- **Propagation through a moving medium** in the track models. The fitted speed and distance are rescaled
  consistently, by λ, before they are compared with ADS-B. The medium is threaded through every search stage,
  and the selftest gained wind; it still recovered 0.4 m.
- **The weather chain** (`weather.rs`). A 64-location × 64-variable Open-Meteo request hung, so the chain became:
  1. a lean Open-Meteo pressure-level grid;
  2. IEM radiosondes, adjusted to surface observations in the boundary layer;
  3. surface values with a lapse rate;
  4. a 15 °C standard atmosphere.

  It runs in the background under a 150 s deadline.
- **Results:** `ny3` named the right town, 843 m off, with a 1.4 km radius at low confidence. `ny4` was 831 m
  off.

### Key files at import

- [`crates/fdop-types/src/atmo.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-types/src/atmo.rs),
  [`crates/fdop-data/src/weather.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-data/src/weather.rs),
  [`crates/fdop-core/src/track.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/track.rs)

## 8) Calibrated evidence and graded confidence (15:10–15:34)

The ranking moved from fit quality alone to evidence against chance.

### Delivered capability

- **A per-event chance-match null.** Evidence is measured in nats, with a margin over the best *distinct*
  explanation.
- **Refinement restricted** to well-locked, well-fitting events.
- **Confidence gates** that use the margin in nats.
- **Faster weather:** the three sources are queried concurrently, and failures are cached for 30 minutes.
- **Results:** `ny5` landed 129 m from the published answer, inside a 304 m radius, at medium confidence. `ny7`
  gave the same point at high confidence in 212 s.

## 9) Robustness runs and review findings (15:37 to import)

### Delivered capability and findings

- **Two robustness runs:**
  - `nassau1`, with a narrower region, found the right spot (202 m off) but graded it low: it kept a
    coincidental R44 helicopter pass as a fourth match;
  - `md1`, with a three-day window, named Southold, NY, 102 km off, and graded it low.

  Both are open problems, recorded as regression rows in `AGENTS.md`.
- **Labelled spectrogram figures:** the bitmap font moved into `fdop-types` so the figures could get time and
  frequency axes, and the report tables were reworked.
- **A read-only review** by the session that published the repository found seven defects. The builder
  confirmed all seven and fixed them before import, together with an eighth (see section 10).

## 10) Fixes and search revisions before import (15:52–16:33)

The builder reached a green checkpoint at 16:29: `cargo fmt --check`, `cargo clippy -D warnings`, the workspace
tests and `fdop selftest` (0.4 m) all passed. The gate rewrite landed just before the import snapshot at 16:34.
That snapshot passed fmt, clippy with `-D warnings`, and the tests again.

### Fixed

- **`--include-jets` removed.** It never worked: jets are excluded at ingest.
- **`--cache` on `runs` and `doctor`.** Both now read the same ledger `locate` writes.
- **`fetch --offline`** no longer calls the NGS geoid API.
- **Gazetteer path:** the doubled `gazetteer/gazetteer/` directory is gone.
- **Failed downloads:** `fetch_window` saves finished day stores when a stream fails, then reports the failure.
- **`doctor`:** the FrankenSQLite check is now a real write and read-back round trip.
- **`fdop detect`** writes `survey.png`, `e<N>.png` and `e<N>_trace.csv`, as its help text says.
- **Cache root:** the `fdop-data` examples and `fdop-geo` resolve it with `fdop_types::default_cache_dir`.
- **Lint:** the code is rustfmt-clean, and clippy passes with `-D warnings`.

### Changed (re-validated in section 11)

- **Lock pruning** now also drops unlocked *paired* events, then refits the clock. This targets nassau1's R44
  coincidence.
- **The margin's "different explanation"** now means a shared event matched to another flight, no shared
  match, or more than 5 km away.
- **Hypothesis budgets** are sized from physical memory (a tenth of RAM, between 200 M and 2 G). In md1 the true
  pair had been lost to the fixed 30 M per-event cap.
- **Fine-stage truncation** keeps the best cell of every flight pair first.
- **Misfit events** get a one-change-point tone model, for an RPM or power change during the pass.
- **Five gates instead of six.** "Core matches" replaces "events explained" and "curve fit". A core match has
  chi ≤ 4, its own clock within 4 s, and ≥ 2 nats over chance. Lock tolerance is capped at 4–6 s. The radius
  gate also fails when the radius is only a lower bound.
- **Reports:** reworked tables (winning matches, all candidate flights, calibration, atmosphere), and labelled
  axes on the figures.

### Key files at import

- [`src/pipeline.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/src/pipeline.rs)
  (the gates),
  [`crates/fdop-core/src/search.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/3e62867b3d5d278a8588bccf9528a43e5ebd99bb/crates/fdop-core/src/search.rs)
  (`hyp_budget`, `lock_tol`, the tone change)

## 11) Evidence calibration for tiny regions, and re-validation (16:35–16:51)

A county-sized region leaves some events with too few chance matches to calibrate their null, so a good fit
could not reach "≥ 2 nats over chance" and could not count as core. The builder's change to `evidence()`: when an
event has fewer than 20 null samples, a fit with chi ≤ 2 (close to trace noise) passes in place of the
nats-over-chance test. The core match still needs chi ≤ 4 and its own clock within 4 s. With 20 or more
samples, nothing changes. In all three runs below, both core matches also cleared 2 nats over chance.

### Re-validation

All three reference configurations were re-run on the import plus this change. All three pass the five gates at
high confidence, match N22NY (PC-12) and N501EX (C208) as the core pair, and use N93CQ (B350) as a confirmer:

| Run | Configuration | Off by | Radius | Margin |
|---|---|---|---|---|
| `ny9` | `--dates 2026-09-08 --near NY` | 224 m | 292 m | 8.5 nats, over PC-12 + PA-32 + P212 23.8 km away |
| `md2` | `--dates 2026-09-07..2026-09-09 --near NY` | 224 m | 292 m | 7.9 nats, over a PC-12 + PA-32 + SR22 130 km away |
| `nassau3` | `--dates 2026-09-08 --near "Nassau County, NY"` | 221 m | 292 m | 6.8 nats, over a PC-12 + S76 10.3 km away |

The R44 coincidence from `nassau1`/`nassau2` is gone, and the true pair now survives the three-day search that
`md1` lost. The county margin (6.8 nats) is the closest to its gate (6 nats).

### Key files

- [`crates/fdop-core/src/search.rs`](https://github.com/Dicklesworthstone/franken_doppler_finder/blob/d79d90dc334099e9a4594bc28c0b1dea216c1a5c/crates/fdop-core/src/search.rs)
  (`evidence`)

---

## Notes for agents

- **Chronology:** read the reference-case table. For the reasons behind each change, read sections 6–8. They
  are the history of the search.
- **Current behaviour:** the README is the contract, and its "Honest status" and "Known issues" describe the
  state at import (section 11 for the reference-case results).
- **Before changing detection, fitting or search:** reproduce the three reference configurations in `AGENTS.md`,
  and report all three, including regressions.
- **New entries:** user-visible changes go under "Unreleased", with live commit links.

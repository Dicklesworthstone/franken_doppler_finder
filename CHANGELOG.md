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

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
| `n_ref_on_0907_cap60` | low, 138 km, core 2 of 2, margin 2.37 | low, 138 km, core 2 of 2, margin 0.25 | 359 → 513 s |
| `n_ref_on_0909_cap60` | low, 73 km, core 1 of 2, margin 5.49 | low, 73 km, core 1 of 2, margin 1.89 | 124 → 210 s |

  Only one new event passes quality on this recording (at 958 s, 534.5 Hz). No archived flight fits it out to 40 km, and
  whether it is a real sound is unresolved: its ridge is faint and intermittent, and the fit could be joining separate
  sounds. It joins the best rival on all four runs, so margins fall on right and wrong dates alike. The three-day run and the other three wrong-date runs have not been run on it, so the effect on false grades
  is only partly measured, and AGENTS wants the medium grade's scaling fixed first.
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

# franken_doppler_finder (`fdop`)

<div align="center">

[![Status](https://img.shields.io/badge/status-alpha%20%C2%B7%201%20real%20case%20validated-orange)](#honest-status)
[![Core](https://img.shields.io/badge/core-Rust%202024%20%C2%B7%20nightly--2026--08--31-b7410e)](#installation)
[![Runtime](https://img.shields.io/badge/runtime-asupersync%200.6-6f42c1)](#architecture)
[![Data](https://img.shields.io/badge/ADS--B-adsb.lol%20(ODbL)-2ea44f)](#data-sources)
[![License](https://img.shields.io/badge/license-MIT%20%2B%20AI%20rider-yellow)](LICENSE)

</div>

**Give `fdop` a recording, the day it was made and a rough US region. It finds the propeller aircraft you
can't hear in it, fits their Doppler curves, matches them against archived ADS-B flight tracks, and returns a
latitude and longitude, a confidence radius, a map, and the nearest town and ZIP code.**

```bash
git clone https://github.com/Dicklesworthstone/franken_doppler_finder && cd franken_doppler_finder
cargo build --release                  # rustup fetches the pinned nightly from rust-toolchain.toml
./target/release/fdop selftest         # offline known-answer run on a synthesised recording (needs ffmpeg)
./target/release/fdop locate clip.m4a --dates 2026-09-08 --near "NY"
```

A recording with no GPS tag and nothing recognisable in frame usually still carries the drone of light
aircraft passing kilometres away. Each pass leaves a falling tone whose shape fixes the aircraft's speed and its
closest distance. Two passes, with their real time gap, fix a place and a clock. Public ADS-B archives record
where every transponder-equipped aircraft was, second by second. `fdop` automates the whole chain, from
finding the faint curves in a spectrogram to naming the town.

> **Use it responsibly.** Only analyse recordings you made or have permission to analyse. The most useful
> application is defensive: checking whether your own footage leaks where it was recorded before you publish
> it (see [Protecting your own recordings](#protecting-your-own-recordings)).
>
> **Credit and affiliation.** The method comes from colsto's video
> ["I discovered a sound that leaks your exact location (and it's everywhere)"](https://www.youtube.com/watch?v=UytYrhYLOKI)
> (2026-11-02). colsto released no code; `fdop` is an independent re-implementation from the video. It is not
> affiliated with colsto, adsb.lol or any data provider named here.

---

## Honest status

**Real-recording validation baseline:** the numbers below belong to the archived builds and runs named with
them. `main`'s code with the budget cap was replayed on the reference recording's five configurations and both feeder runs
(`*_cp`, 2026-11-06), and every row equals the measurement build's (`*_gc`) in every field but paths. None of them gives a start,
so the curve match is skipped there. `main`'s tip before the cap kept every right answer on the 15 development rows (5, all
high), gave no wrong one, and put none of the 42 no-answer runs at medium or high ([bench/PROTOCOL.md](bench/PROTOCOL.md)
Addendum 16). No accuracy or grade change is claimed from these.
The reference searches, the reference recording's two wrong dates and the Ithaca feeder were replayed on the
measurement build (`*_gc`), `9087ec4` (runs named `*_b1`), on the commit before it that added the knot rule and the curve match (`*_c1`), and on the build before the knot rule (`*_r5`). Between `*_c1` and `*_b1` no grade changed and no margin moved by more than 0.02 nats. Field rows replayed on those builds are in the [CHANGELOG](CHANGELOG.md). The row on the knot rule and the curve match names its own runs. Every other number in this
section comes from the archived `7e5cf3e` replays, which predate the moving-medium propagation correction, spatial
refinement, frequency calibration and robust tone fit. Those are not measured results for this code.

| Question | Answer |
|---|---|
| Does it run end to end? | **Yes.** `fdop locate` goes from a media file to a report with a map, a radius and a ZIP code. Every stage is automatic: detection, curve fitting, ADS-B download, weather, terrain, search, geocoding and reporting |
| Has it located a real recording? | **One recording, three ways, one area: high on all three.** The recording is the raw upload colsto published with the video: one hour of audio. Three configurations were tried: the right date across the whole state (`--dates 2026-09-08 --near NY`), a three-day window (`--dates 2026-09-07..2026-09-09 --near NY`), and a county (`--near "Nassau County, NY"`). All three match the same aircraft, a Pilatus PC-12 and a Cessna 208 as the core matches and a King Air 350 and a Robinson R44 as weaker ones, and land near the location published in the video. The county search is 270 m off, radius 406 m, margin 14.78 nats (`r_nassau_gc`). The state-wide search is 264 m off, radius 379 m, margin 12.79 nats (`r_ny_gc`). The three-day window is 263 m off, radius 388 m, margin 8.44 nats over a rival 119 km away (`r_md_gc`). Before measurement went past the old line they were 263–270 m off with margins 16.00, 8.12 and 8.19 nats (`*_b1`), and before the knot rule 263–270 m with 17.85, 8.64 and 7.21 nats (`*_r5`). The grades have moved between builds on this same recording: `7e5cf3e` gave high, medium and low at 271–306 m with a 304–316 m radius, and `d79d90d` gave high on all three at 221–224 m with a 292 m radius, the results shown in [See it work](#see-it-work). Each answer lies inside its own radius, and one reference point cannot say which of these estimates is best. The margin is not calibrated, so these grades are not a measured error rate ([Known issues](#known-issues)) |
| Has it been wrong? | **Yes, at low and at medium.** On the reference recording, earlier builds named Old Lyme, CT (116 km off) and later Southold, NY for the three-day window (102 km off); `9aeeb9a` put the county search 14.4 km off and the state-wide search 87 km off, both low, until the guided-peak fix. A complete comparison without the ridge check described below put the state-wide search 34.8 km off, low. In the field tests below, five answers are wrong on the validated `7e5cf3e` build, all low, and a sixth lands near its site, probably by chance. On `d55ef79` one of them was **medium**: a bird-feeder cam in Ithaca, NY placed on Long Island. The validated `7e5cf3e` build grades it low, 0.11 nats under the medium line, which is a narrow miss and not a fix. An intermediate build of the current refinement code graded it medium again, margin 3.10 (`f_feederwatch_b8`), because refinement raised the leader's score by 0.95 nats and left its rival's alone. `cb95fbb` compares the two at the same polish and grades it low, margin 1.81 (`f_feederwatch_r5`), and with the knot rule 0.75 (`f_feederwatch_c1`). The answer is still wrong. A working-tree experiment that measured more of a 3 h recording's flyovers also graded a wrong answer **medium** (103 km off; see [Known issues](#known-issues)), so medium alone does not prove an answer. A low grade does not mean wrong either: on `7e5cf3e` the three-day reference search landed 271 m off at low. All five runs with a deliberately wrong date grade low on the validated `7e5cf3e` build, but two came close to medium: one had both core matches and was 0.63 nats short, and one had a 5.49-nat margin but only one core match of two. On the current code the reference recording's two wrong dates stay low: 119 km off with one core match of three and a 2.00-nat margin (`n_ref_on_0907_gc`), and 104 km off with no core match of three and a 0.15-nat margin (`n_ref_on_0909_gc`). Before measurement went past the old line they were 138 km off with both core matches and a 0.61-nat margin (`n_ref_on_0907_c1`), and 73 km off with one core match of two and 0.35 (`n_ref_on_0909_c1`). Before the knot rule the first was 2.95, **0.05 under the medium line** (`n_ref_on_0907_r5`), and `a827820` graded it **medium** at 3.01 (`n_ref_on_0907_a82`), a wrong-date regression that `cb95fbb` removed only narrowly (CHANGELOG). The field rows replayed on `cb95fbb` so far stay low or give no location, though two wrong answers moved much further off: the feeder on a wrong date from 56 km to 326 km (`n_feeder_on_1101_r5`; 254 km, low, with no core match with the knot rule, `n_feeder_on_1101_c1`), and Rochelle from 36 km to 371 km (`f_rochelle_r5`). The [CHANGELOG](CHANGELOG.md) has every run. On the benchmark's sealed set E, `6914ab7` graded a marina **medium**, 8 km off, from the curve match alone on one heard pass ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 14); on the same set `dopplerprint` with a ground model was wrong at medium at another site, 14 km off. Treat **low** as "unconfirmed", whichever way it points |
| Is it tested? | **Offline unit tests,** plus four opt-in FFmpeg/FFprobe integration tests: synthetic known-answer checks of detection, fitting, emission-time physics, track models and a two-flyover search, plus audio-track/channel selection, parsers, formats and a stalled download resuming over a loopback socket. See [Testing](#testing) for the latest checks and earlier validation. `fdop selftest` runs the real decode, detect, measure and search path on a synthesised recording |
| Has it located a recording it was not developed on? | **Two right answers, neither a clean test.** One is at the reference site, one is graded low, and both recordings informed the ridge check's line. Nine recordings from public YouTube live cams (2–3 h each, with published locations, and true clocks from the stream timestamps) were searched with only the local date and the state, and all nine were replayed on the validated `7e5cf3e` build. An osprey cam at the reference site, recorded on a different day with different flights, came out **high**: 223 m from the reference recording's published location, just outside its 214 m radius, with a 24.9-nat margin. The same cam on two other days gave wrong answers graded **low**, 152 km and 365 km off. An eagle cam at Duke Farms (Hillsborough, NJ) came out in the right township, about 2.3 km from the town centre, graded **low** (the nest position is not published, so a smaller error cannot be scored). The clock its answer implies is within 6.4 s of the stream's, from a search of the whole day, so that match is real whatever its grade: the low comes from a rival 140 m away. At five other sites, a bird-feeder cam in Ithaca, NY was placed on Long Island (338 km off) and a railcam in Rochelle, IL at Sycamore, IL (36 km off), both **low**, and railcams in Tucson, AZ, Plant City, FL and Fullerton, CA gave no location, because no event passed the quality gates. All but Fullerton held only 0–3 non-jet passes within reach. Fullerton (13 within 3 km) and Duke Farms (16) had more traffic but poor audio. Fullerton's is high-pass filtered at about 125 Hz, which removes or weakens the propeller fundamentals, under broadband wind and road noise; Duke Farms has a wind-saturated microphone. A tenth recording, a public eagle-nest live cam in Lee County, FL, recorded 2024-12-28, is the only one that did not inform the ridge check's line: searched across Florida it is wrong and **low** (135 km off), and searched in the county it lands about 4 km away, **low**, on two events. Its clock there is 55 minutes off the stream's, so the 4 km is probably chance and not a location: at the stream's clock one usable event matches an archived flight within 0.2 s, and at the two answers' clocks and eight other offsets none does. That rests on one event, and it leaves a coincidence unexplained: the state and county answers' clocks agree within about 2 minutes though they are about 135 km apart. Each answer's clock is a check the search cannot make on its own: the two real matches are within 6.4 s of the stream's clock, and the wrong answers are 53 minutes to 9.3 hours off. Using this takes care. `--start` means the exact start, `--clock-tol` defaults to 120 s and the search does not widen past it, so a start 10 minutes off excludes the true clock too. A start known to within 25 minutes with `--clock-tol 1500` would keep the true clock and exclude all of those wrong ones. What the search answers instead has not been measured |
| Do the knot rule and the curve match change this? | **Both do, on the development recordings with a known start. On the first sealed test the better build tied `dopplerprint`, and another was wrong at medium.** With the start given to the minute (±2 min) and a 25 km region whose centre is 10 km from the truth (the head-to-head benchmark's R2 inputs in [bench/PROTOCOL.md](bench/PROTOCOL.md)), fifteen runs on this code gave five right answers and no wrong one. They were the reference recording at 250 m, the osprey cam at 90 m, 110 m and 220 m on its three days, and Duke Farms within its 5 km limit, all graded high. None of the nine runs at wrong dates or wrong places was graded above low. Of 33 more runs with no right answer, none is above low. One would be medium on the trace search alone: the osprey cam's 10-31 recording searched on 11-02. The curve match does not stand there, and a medium needs it to, so it is low (CHANGELOG). That rule was written after seeing the same at its 11-01 search, before measurement went past the old line. On the benchmark machine the trace search gave two right answers on these inputs without the knot rule and four with it, and the pinned benchmark build `7e66501` gave one. Of the other four, the curve match stood at three and agreed with each, did not stand on the reference recording, and changed none of their grades. Since it profiles how far each recording hears (CHANGELOG), its margin over the best decoy on the osprey cam's three days rose from +9.2, +13.3 and +1.1 nats to +22.2, +21.4 and +5.3; it is unchanged at Duke Farms and went from −3.95 to −4.32 on the reference recording, and no grade changed. On the osprey cam's third day the trace search measured only the strongest 24 detections, 19 of which failed the quality gates, and its answer was 14 km off and low. The curve match stood near the cam and graded that day medium from its own place: 220 m off once it searched the whole kilometre around each of its starts, 240 m with the heard range profiled (CHANGELOG). Now that measurement goes down the list until 24 pass, the trace search itself lands 220 m from the cam with two core matches and a 23.16-nat margin, the curve match agrees, and the day is high. All fifteen recordings were used to develop these changes, and these runs (`f10` before that search fix, `disc_dev` with it, `r50_dev` with the heard range profiled, `fm` with measurement past the old line, `fh` with the two-pass rule below, the same in every grade and distance as `fm`) were on an Apple M5 Ultra with 256 GiB of RAM, not the benchmark machine. The benchmark's sealed set E is the first test, read on 11-05 ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 14). On its eight cases with these inputs, `9087ec4` gave one right answer, Olympia at 66 m (medium), and no wrong one: the same count as `dopplerprint`, so it did not beat it. `6914ab7` gave the same right answer and one wrong **medium**, a marina 8 km off, placed by the curve match alone on one heard pass. The curve match no longer stands on one heard pass (CHANGELOG). The pinned build gave no right answer. Set F, captured on 11-05, was read on 11-06 with these inputs and with no area ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 19): no tool was right on its ten cases. No `fdop` build asserted an answer, nor did `dopplerprint`, and `dopplertrack` was wrong once, so set F cannot tell the tools apart. On set E with no area (Addendum 21), `6914ab7` asserted nothing: it put Olympia 67 m off but graded it low, and `dopplerprint` was wrong once. `9087ec4` and the pinned build asserted nothing there either, and `dopplerprint` with its ground model declined all eight (Addendum 24). With only the date and the state (R1, Addendum 22), neither `6914ab7` nor `9087ec4` asserted anything on set E: both put Olympia 65 m off and graded it low, and `dopplerprint` gave no answer on any case. Sets G and H, live cams captured on 11-05 and chosen for aircraft passing within 3 km, were read on 11-06 with the R2 inputs ([bench/SET_G.md](bench/SET_G.md), [bench/SET_H.md](bench/SET_H.md)). Every registered build declined all three cases: the one where no aircraft passed, and the two that passed the hearing screen. That screen's tonal read was later found biased: against chance lines of passes as close, `fdop`'s own filter, aimed with the true passes, hears neither case's aircraft, and only band power hears one (Deerfield Beach). No true pass reached the detector's floor, so no build found a flyover to fit. Without a start the curve match does not run (CHANGELOG) |
| What is not validated? | Any site other than the reference one; low-traffic or rural sites (all four tried failed); following real passes steeper than 4 Hz/s (real ridges that steep exist in the recordings, but no event follows one, so that side rests on synthetic tests); CONUS-wide searches on real data; recordings outside the US; phone-call or heavily processed audio |
| Which code produced these results? | The `*_r5` runs used a release build of the commit that added this sentence, on `a827820`; the only difference is that the margin gate's text now prints two decimals, because 2.95 printed as 3.0 on a gate that failed. They are the three reference runs (`r_ny_r5`, `r_md_r5`, `r_nassau_r5`), two wrong-date runs (`n_ref_on_0907_r5`, `n_ref_on_0909_r5`) and `f_feederwatch_r5`, and the field runs named in the CHANGELOG. They ran one at a time on an Apple M4 with 32 GiB of RAM, with other jobs sharing the machine. The other replays used a local build of `7e5cf3e`, whose code is the same as `4b88d7c`: the earlier reference runs (`r_ny_s7b`, `r_md_s7b`, `r_nassau_s7b`), five wrong-date runs, all nine field recordings and the Lee County recording. The later verdict wording and high-pass warning changes were replayed without changing numbers or grades (CHANGELOG). Those ran on the same machine, two at a time; the Performance timings are separate runs with nothing else running. The older reference runs `ny9`, `md2` and `nassau3` used the second commit, `d79d90d`: the import plus a 5-line change to evidence calibration, which only matters when a region leaves an event fewer than 20 chance samples to calibrate against (CHANGELOG section 11). `ny10` repeated the state-wide run on `1f321cb` with the same answer. The field tests ran first on working-tree builds made just before `e872b1c` was committed, then all six again on `1f321cb`. The last three ran on a build of the tree at `25f12ff`. Before `1f321cb` the osprey-cam answer was the same point graded **medium**, and the four wrong answers named other towns, also graded **low**. See [CHANGELOG.md](CHANGELOG.md) |
| Is it fast? | **It depends on the search.** On an Apple M4 with the ADS-B days cached, the reference recording took 36 s for a county, 119 s state-wide and 356 s for a three-day window. A busy 3 h recording searched across a state took about 2 hours 40 minutes while another replay ran. Narrowing the region is the way to shorten a search. See [Performance](#performance) |

The real-recording evidence is limited to these replays: one recording that every build was developed on, and the
field tests. It does not establish the wind model's accuracy in the field. The search now compares every rival at its
default budgets, and none of these runs recorded a pair, fine or confirmation cut. Two findings came with that. A complete comparison reached false events, fits that join
two steady tones across an empty stretch, and they could decide the answer, so an event with no ridge through its
drop is now refused. That took the reference recording from 19 usable events on `d55ef79` to 4 on `7e5cf3e`, 3 on `9087ec4`, and 10 on the current code, which measures past the old line (`funnel.events_used`). A complete
comparison also finds more chance alignments, so margin does not separate right from wrong on these runs ([Known
issues](#known-issues)). The detection and measurement caps still cut on most of these recordings, with a warning.
Historical high grades from truncated runs are not evidence of the current grade.

---

## TL;DR

**The problem.** Light aircraft are everywhere, and their propellers and engines hum at steady pitches,
typically 80–500 Hz. As one passes, the Doppler effect bends that hum into a falling S-shaped curve. The curve
is usually inaudible under wind, voices and traffic, but it shows up clearly in a properly normalised
spectrogram. The height of the drop gives the aircraft's speed; the steepness gives its closest distance.
Locating a recording this way by hand takes a spectrogram viewer, a curve-fitting tool, a paid flight-data API,
custom search scripts and hours of judgement.

**The solution.** One command. `fdop` detects every flyover in the file, fits each with a physically correct
emission-time model, downloads the free adsb.lol archive for the dates, and searches every flight in the region
for the single place and recording clock at which two or more flyovers agree. It then checks that answer
against everything else in the recording and grades it.

### Why use `fdop`?

| Feature | What it means |
|---|---|
| **Fully automatic** | Media file + dates + region in; lat/lon, radius, map, town and ZIP out. No spectrogram reading, no seeds, no API keys |
| **Physically correct** | Fits the emission-time ("retarded") Doppler model, not the naive one. At 6 km the sound of closest approach arrives about 17 s after the aircraft was there, and the curve is asymmetric |
| **Weather-aware** | Speed of sound and wind come from Open-Meteo pressure levels or nearby radiosondes, averaged along each sound path |
| **Refuses rather than guesses** | Five gates grade every answer **high / medium / low**. One flyover, or no consistent flight pair, gives "no location" and exit code 1 |
| **Explains itself** | `report.html` shows a map, a heat map, ranked alternatives, per-event fits against the ADS-B prediction, the search funnel and the margin over the next-best explanation |
| **Free public data** | adsb.lol (ODbL), NOAA, USGS, Census and Open-Meteo. Everything is cached, so reruns work `--offline` |
| **One native binary** | Rust on frankenscipy (FFT, bounded least squares, F distribution), asupersync (HTTP over rustls), frankenpandas and FrankenSQLite. `ffmpeg` and `ffprobe` are the only external tools |

---

## See it work

The reference case: colsto's raw upload of the recording ([youtu.be/4CxVxuMVkMc](https://youtu.be/4CxVxuMVkMc),
the recorder's 1:00:00–2:00:00 window), searched across all of New York State with only the date given.

This section shows run `ny9` from an early build (`d79d90d`), kept for its complete walk through the stages. **The
current code does not reproduce these numbers:** the same search matches the same three aircraft, also grades high, and
lands 264 m off with a 379 m radius and a 12.79-nat margin, and also matches an R44 (`r_ny_gc`; 263 m, 397 m and
8.12 nats before measurement went past the old line, `r_ny_b1`; 264 m, 385 m and 8.64 nats before the knot rule,
`r_ny_r5`). `7e5cf3e` graded it medium, 271 m off, at 5.01 nats
([Honest status](#honest-status)).

```console
$ fdop locate ref_raw.m4a --dates 2026-09-08 --near NY -o runs/ny9
location   40.876388, -73.527613
radius     292 m  (high)
place      Oyster Bay, NY 11771 (Nassau County)
zip        11771
verdict    3 flyovers matched to N22NY PC12, N501EX C208, N93CQ B350 at one location and one recording clock (start 2026-09-08T20:17:29.254Z)
report     runs/ny9/report.html
map        runs/ny9/map.png
```

That is run `ny9` (2026-11-03), 224 m from the location published in the video. Its [gates](#the-gates), from
the run's `result.json`:

| Gate | Passed | Detail |
|---|---|---|
| core matches | yes | 2 of 3 matches locate the observer: e15 chi 1.21, trace 11.1 nats over chance; e20 chi 0.65, 3.9 nats. e16 (chi 5.59, 0.9 nats) is not core |
| clock lock | yes | each match's own best clock offset: e15 −0.2 s, e20 −0.2 s, e16 +2.8 s |
| margin | yes (historical gate) | evidence 39.2 nats vs 30.7 for the best different explanation (PC-12 + a PA-32 + a P212, 23.8 km away): 8.5 nats of evidence-score separation |
| no unheard close pass | yes | every much-closer tonal pass left a detection |
| radius | yes | 292 m |

The three matched passes. The fitted values come from the audio alone, in still air at 343 m/s; the search
rescales them for the real atmosphere before comparing them with ADS-B:

| Event | Heard CPA (s into file) | f0 (Hz) | Fitted v, d | Matched aircraft | ADS-B v, d at CPA | Role |
|---|---|---|---|---|---|---|
| e15 | 2155.8 | 112.79 | 140.5 m/s, 6,407 m | N22NY, Pilatus PC-12 | 136.8 m/s, 6,153 m | paired |
| e20 | 778.2 | 94.66 | 76.2 m/s, 7,374 m | N501EX, Cessna 208 | 81.0 m/s, 8,414 m | paired |
| e16 | 3074.2 | 131.99 | 95.4 m/s, 2,328 m | N93CQ, King Air 350 | 102.8 m/s, 3,483 m | confirmer |

In the search, the King Air's match uses the one-change-point tone model: its tone steps up by about 4 Hz
partway through the pass.

The search funnel for that run:
- 10,550 track models (from 7,167 ADS-B legs) in the padded state box;
- 24 flyovers measured, 14 used: 12 drove the pair join and 2 only confirmed;
- about 3.4 M joined place-and-clock hypotheses, after which the pair join stopped at its 3 M budget with 64 of
  its 66 event pairs done (reported in `funnel.truncated` and the warnings);
- 1.1 M passed the quick check and merged into 426 k cells, of which the best 4,000 were scored exactly and 535
  passed.

On that build, the same recording with a three-day window (`--dates 2026-09-07..2026-09-09`, run `md2`) gave the same point,
radius and flights at high confidence, with a 7.9-nat margin over a rival 130 km away. Searching only Nassau
County (run `nassau3`) gave a point 221 m from the published one, also high, with the same 292 m radius.

A known-answer check that needs no network:

```console
$ fdop selftest
selftest: PASS | error 2.4 m, radius 75 m, clock error +0.02 s, 4 events, flights ["ACE391", "A1DFEB"]
```

---

## How it works

### The physics

A propeller aircraft emits a steady tone `f0`. Sound leaves the aircraft at emission time `t_e` and reaches the
recorder `x` at

```text
heard time        t_r = t_e + |p(t_e) − x| / c
heard pitch       f(t_r) = f0 · c / (c + v_r(t_e))            v_r = radial speed, positive receding
```

For a straight pass at speed `v` with closest approach (CPA) distance `d`, the emission offset has a closed form
(`crates/fdop-core/src/physics.rs`):

```text
τ   = (c²u − √(c²v²u² + d²(c² − v²))) / (c² − v²),      u = t_r − t0e
t0r = t0e + d / c                                          the CPA as heard, later than the CPA itself
f_pre = f0·c/(c − v)    f_post = f0·c/(c + v)    slope at t0r = f0·v²/(c·d)
steepest slope, at t0e = f0·v²/(c·d) · (1 − v²/c²)^(−3/2)
```

The frequency calculation differentiates the closed-form emission time directly, evaluating one square root
per prediction instead of reconstructing the range with a second one. It agrees with the radial-velocity
form over the subsonic fit domain to floating-point roundoff.

The pitch drop sets `v`, the steepness sets `d`, and the curve's centre sets the heard CPA time `t0r`. Matched to
one flight, one flyover therefore puts the recorder on a line beside that flight's path, each point of it tied to
a different recording clock. Two flyovers, with the real time gap between them, intersect at a point and fix the
recording clock. Detection and fitting use still air at 343 m/s. The search uses path-averaged sound speed
and wind for each pass as a homogeneous moving medium. For `r = source − observer`, wind vector `w`,
source ground velocity `v_s` and acoustic travel time `T`, its predictions satisfy
`|r + w T| = c T` and `f/f0 = 1 / (1 + dT/dt_e)`. The emission-time solver uses that same derivative,
including crosswind displacement. This approximation does not trace refracted rays through atmospheric
layers. Coarse gates retain the existing first-order wind rescaling of the fitted `v` and `d`.

### The pipeline

```text
 media file ──ffprobe/ffmpeg──► mono f32 @ 4 kHz
      │
      ▼  detect   1 s STFT, 35–1100 Hz ─► 48 bins/octave robust z-map (raised to a robust spread of
      │           0.5, at most ×4, when one loud pass compressed it) ─► matched-filter bank of
      │           emission-time templates over speed (40–175 m/s) × CPA distance (250 m–12 km);
      │           both halves of a pass must light up, so steady hum and clicks are rejected;
      │           a short pass heard too near the start or end for the 24 s templates gets a
      │           10 s one, on what the full bank left unexplained; a compressed image is scored
      │           once more, contrast first, for strong curves the rest left unexplained
      ▼  measure  per event: STFT window ≈ 1/√(slope at the CPA), at least 0.5 s, with a matched shorter
      │           window for frames steeper than that holds; model-guided ridge trace, bounded
      │           trust-region fit with multistarts and robust loss, profile-likelihood ranges,
      │           manoeuvre / side-disagreement / structured-residual warnings that widen the ranges;
      │           a fit with no ridge through its frequency drop is refused; a fit refused because its
      │           trace held one side of the pass is traced once more without the model
      ▼
 events (f0, v, d, t0r, each with a "generous" range)
      │
      │   adsb.lol day archives ─► regional track store     Open-Meteo / IEM radiosondes ─► air column
      │   AWS Terrarium DEM (z10, z13)    NOAA NGS geoid    Census region and time-zone tables
      ▼
 search   1. hypotheses: for every (event, flight), the ground points where that flight's CPA matches the
             fitted speed and distance, with the recording start each implies
          2. pair join: a spatio-temporal hash join keeps (place, clock) cells that two events share
          3. quick check, then a 100 m grid with the exact emission-time model and a 0.01 s clock scan
          4. confirmers (other events at the locked clock) and the unheard-pass veto (a tonal aircraft that
             passed much closer than anything heard costs evidence)
          5. evidence in nats against a calibrated chance-match null; margin over the best distinct rival
          6. refine the leader and its best rival below grid spacing; sample the 95 % Δχ² region on a 25 m grid (75 m floor)
      │
      ▼
 curve match  each logged propeller flight predicts one exact Doppler curve for a (place, clock): score the
              spectrogram along it against the same curve at other times, then run the same search at 48
              decoy clocks; the answer stands only if it beats every one
      │
      ▼
 gates ─► confidence ─► reverse geocode ─► map.png / report.html / report.md / result.json ─► run ledger
```

### The gates

| Gate | Passes when |
|---|---|
| core matches | at least 2 matches locate the observer: each fits its ADS-B prediction with chi ≤ 4, has its own best clock within 4 s, and beats chance by ≥ 2 nats. When an event has fewer than 20 chance samples to calibrate against (a tiny region), chi ≤ 2 stands in for the nats test |
| clock lock | every match's own best clock is within its timing tolerance (clamped to 4–6 s) of the shared clock |
| margin | no pair, fine or confirmation comparison cut was recorded, and the answer beats the best *different* explanation by ≥ 6 nats or no rival survived. A rival is different if it matches a shared event to another flight, shares no match, or lies more than 5 km away. Both are compared at the same polish (below) |
| no unheard close pass | no tonal aircraft passed much closer than the heard ones without leaving a detection |
| radius | the 95 % radius is ≤ 1,000 m, its contour is sampled by the grid, and it does not reach the refinement window boundary |
| curve match | listed when the [curve match](#the-curve-match) stands: the answer is within 3 km of where it stands. Also listed, failed, on a medium answer when the curve match ran with enough decoy clocks to judge (40) and stands nowhere |
| measurement line | listed, failed, only on a medium answer that rests on a core match measured past the old line (a detection score under 4.5, past the strongest 24, found only on the contrast image, from a faster supplemental seed, or recovered from a harmonic after its primary failed) when no curve match ran with enough decoy clocks to judge it |

Refinement uses the grid to find a basin, then improves the observer position with a bounded continuous
search that evaluates the same physical trace model at every proposed point. The grid spacing controls
contour sampling, not the number of decimal places permitted in the location. The pilot used to estimate
event noise is also polished, so grid quantization is not mistaken for excess measurement noise.

The contour is checked both at grid nodes and at continuously profiled minima on all four window edges.
This catches thin, rotated uncertainty regions crossing between edge nodes. Refinement widens and recenters
the window when the contour reaches a boundary; if it still reaches an edge after the available expansions,
the radius remains a lower bound. If no grid node samples the contour, refinement recenters and halves the
grid spacing instead. A still-unsampled contour fails the radius gate. Radius padding uses half a cell's
diagonal, and the systematic floor remains 75 m. Sub-metre numerical convergence in synthetic cases is not
a claim of sub-metre accuracy for recordings.

Refinement estimates each event's excess noise once from a pilot fit and holds those scales fixed, including
when the window expands. The shared recording clock, best location and confidence contour all minimize the
same weighted trace objective. This prevents a noisy pass from pulling the reported position under one
weighting while the radius describes another. The radius remains conditional on the flight assignments and
propagation model; this numerical consistency does not calibrate the search-wide confidence grade.

Trace scoring retains the original least-squares tone fit and also tries a robust emitted-tone estimate
from the median of observed frequency divided by predicted Doppler ratio. The alternative is used only
when it improves the same capped residual objective. This prevents a minority of stray ridge points from
shifting the fitted tone enough to escape the existing outlier trim. Tone bounds, missing-coverage penalties,
the raw RMS diagnostic and the core-match thresholds remain in force.

Between stored Doppler predictions, the search uses a four-point cubic on a finite, monotone stretch,
bounded by the two bracketing predictions. This reduces curvature bias for close, fast passes without
additional emission-time solves or a denser table. Boundaries, turns, plateaus and gaps retain the linear
fallback; missing bracketing samples remain missing.

The reported candidate is regenerated at the refined position and its fitted recording clock: terrain
elevation, trace fits, independent clock checks, core matches, unheard close passes, evidence and the final
margin all refer to that same solution. A locating match may sit up to 5 % past its distance or speed bound, the
slack hypotheses get: those bounds limit the search, and the trace fit is what scores the match. If a locating match
still fails reevaluation, the coherent coarse candidate is retained with a warning and no refined radius. A changed
ranking causes the newly promoted leader to be checked and refined before reporting it.

The rival is the different explanation with the smallest comparable score gap: refined scores when both
refinements succeeded, or both fine-grid scores otherwise. The search attempts refinement for that rival too,
then checks whether another explanation now gives a smaller comparable gap. Each candidate is attempted at
most once. A rival that overtakes becomes the answer with its own radius. When either side cannot be refined,
`margin_basis` in `result.json` reads `fine_grid`; the answer still reports its coherent position and evidence.
The reported rival can therefore differ from the runner-up in current-score order.

Effective trace counts use the observed union of six-second correlation supports, rather than counting the
elapsed time across silent gaps. Duplicate samples and denser resampling do not add independent information,
and the count cannot exceed the points retained for scoring. Six seconds remains an assumed correlation
length; these counts are not an empirical covariance or coverage calibration.

Fit intervals profile each reported coordinate directly: speed, distance, emitted tone and the heard
closest-approach time. The heard-time profile fixes received CPA time, refits speed and distance, and
solves the emitted tone analytically. It keeps the original emission-time bounds through the corresponding
distance constraints. This prevents a valid recording clock from being discarded because timing uncertainty
was inferred only from the nuisance fits along the speed and distance profiles. The F cutoff and generous
floors are unchanged; the extra profiles run once on the final retained trace.

**High** confidence needs every gate. **Medium** needs two core matches, clock lock, and a margin of at least
3 nats (or no rival), also with no recorded comparison cut. Everything else is **low**. The full gate details
are in `result.json` and `report.html`. An incomplete comparison still reports its observed numerical gap,
labelled provisional: an unchecked rival can increase its score or overtake the leader. These gaps are
evidence-score differences, not calibrated likelihood ratios or a correction for searching many events.

The curve match then reads the recording a second way, independent of the traces. Where it stands it can raise a
low answer to **medium**: at the trace answer if that lies inside its radius, otherwise at its own place, and the
verdict then says the answer comes from the curve match. It does not stand on one heard pass: one pass's Doppler
curve fixes when and how close it passed, which leaves place and clock free to trade along its track, and its decoy
test checks the clock, not the place. That rule was added after a curve match on one heard pass graded a marina
medium 8 km off on the benchmark's sealed set E (CHANGELOG).
Where it stands more than 3 km from a high or medium
answer, that answer becomes **low**. It never makes an answer high. Where it ran with at least 40 decoy clocks
and stands nowhere, a **medium** trace answer becomes **low**. The medium grade's margin has no calibrated
false-alarm rate. The curve match's decoy test has one: with a wrong date or place, the real window is one of at
least 41 interchangeable clocks, so it stands by chance about once in 41 at most. This rule was added after a
wrong-date run graded medium at a wrong place (CHANGELOG), so its effect on the development recordings is not a
test. A high answer keeps its grade. Where the curve match does not run or has too few decoy clocks, the grade
is the trace search's, except that a medium answer resting on a core match measured past the old line (below)
becomes **low**: nothing calibrated then checks that the extra events did not just give chance more to work with.

### The curve match

The trace search can use only a pass whose ridge it measured; a ridge too faint to trace, or one that crosses
another, adds nothing. The curve match asks the question the other way round. If the recording was made at this
place with this clock, every logged propeller flight predicts one exact Doppler curve: is there energy along it,
more than along the same curve at other times? Scoring logged flights along their curves with one shared clock,
and checking the result against flights from other times, follows
[`dopplerprint`](https://github.com/Hexay/dopplerprint) (MIT, © 2026 Henry Alexander). `fdop` reimplements it
in `crates/fdop-core/src/curvematch.rs`:

- **Image.** A 1 s STFT at 0.5 s hops on 48 rows per octave, each row standardised against its own running low
  quantile over 60 s and clipped, then a weighted comb over the first six harmonics.
  The comb retains the full analysed received-frequency band. An approaching high-tone fast pass is
  no longer cropped merely because its received frequency exceeds 2.2 times the maximum emitted tone.
- **Bank of curves.** On a log-frequency axis a constant-speed pass is one fixed path for each (speed, distance),
  and the emitted tone is only a row shift, so one sweep per path scores every arrival time and tone. That
  path's scores at all times of the recording are its null: a lookup becomes a p-value, then an e-value. Both
  halves of a pass must light up.
  The default speed bank keeps its original eleven rows and adds two 20% steps, about 208 and
  250 m/s. The existing 15% trajectory allowance then includes the detector's 215–263 m/s fast
  passes in both coarse voting and exact scoring. Each emitted-tone hypothesis independently
  needs at least 25% of the full squared weight on each half within the time/frequency image;
  an out-of-band neighbouring tone cannot discard its usable pixels. Reused range-difference
  buffers accumulate clipped coverage, while fully covered paths keep scalar normalization.
  Real windows, null calibration and decoy windows all use this same scorer.
- **Tone by type.** For common propeller types a tone the propeller can make (blades × rpm / 60, from typical
  published figures) earns more and other tones less. The weights are divided by where this recording's best
  tones fall with no weighting, and scaled to average one over those tones, so they do not favour every flight.
  Weighing against a flat spread instead credited every flight at every clock on rumbling audio, because the
  rumble sits where the propeller tones are. The decoy clocks are weighted the same way.
- **Search.** Flights vote for (place, clock) cells 200 m apart. The best 16 places are rescored with the exact
  curve (the real trajectory, emission-time solve, modelled wind and sound speed). At each selected place,
  positive local peaks in the clock votes, including tied plateaus, supply alternative recording times.
  Exact evidence chooses the clock that reaches spatial refinement; a stronger coarse alias is no longer
  the only timing seed. The original seed is tried first. A later sweep is skipped only when every
  in-window timestamp it would score was already covered on the same clock lattice by successful sweeps.
  Each of the best 4 places stands for every place within 1 km of it, so every point of that disc is scored
  on a 100 m grid before it is climbed to 25 m. A close pass with no energy costs evidence, as in the trace
  search, and one audio event can credit only one flight. This recovers clock alternatives at selected
  places; the existing spatial shortlist still limits which places reach exact scoring.
  Flights are screened for exact scoring using their interpolated paths over the catalogue span, at the
  closest-approach sampling interval. This retains sparse passes that enter the search area between logged
  ADS-B positions.
  The coarse votes retain every sampled local approach, including several visits by a circling aircraft
  within one six-minute chunk. A shared sampling lattice and chunk ownership give each visit one vote per
  place. Interior approach times use the curvature of squared range and the interpolated trajectory;
  real track endpoints remain eligible, while an artificial catalogue boundary cannot create a pass.
  Coarse and exact scoring refine a sampled local minimum before applying the physical reach limit,
  so a pass just inside the limit can survive when its nearby samples are outside. Coarse refinement
  keeps the original sampled point if its proposed time is no closer on the actual trajectory.
  An actual closest approach outside the reach remains excluded.
  Exact trajectories between bank shapes use interpolated fitted null quantiles on the existing
  log-speed/log-distance grid. This removes evidence jumps caused solely by selecting a neighbouring
  calibration template. Bank nodes remain exact, exterior values clamp to the endpoints, and real
  and decoy scoring share the same interpolation and tone adjustment. It uses at most four existing
  nulls per pass, with no additional image scan or denser bank.
- **How far it hears.** A pass is credited by how likely it was to be heard at its slant range: a logistic in dB
  around the range heard half the time. That range depends on the microphone, wind and noise, and is not known
  before the match: the osprey cam's 10-31 recording has 9- to 12-nat curves from passes 4 to 6 km out. So every
  place is scored with it at 1, 2 and 4 km (6 dB apart)
  and keeps its best. A longer range credits far curves more and charges more for far passes that left
  nothing. The longest is half the 8 km reach, so a pass beyond the reach would be heard at most about one time
  in eight. Each decoy window profiles the same way. On the development recordings the real window took 4 km
  on all three of the osprey cam's days and 2 km at Duke Farms; most of the osprey cam's decoy windows took 1 km
  (31, 39 and 36 of 48).
- **Decoy clocks.** The same search, step for step, runs at other clock windows of the same width, each the
  width plus 60 s apart, out to about ±2 hours, on whichever side the fetched days cover. A clock that far off
  cannot be right, so the best decoy is what this recording and this sky give by chance. A start known to
  ±120 s (the default `--clock-tol`) gets 48 decoys and ±150 s gets 40, the fewest that can judge. A wider
  window gets fewer (±180 s gets 34), and so does the `creation_time` window of any clip longer than a
  minute, which also spans the recording's length. Up to 30 min such a window is still searched, but its
  verdict can neither stand nor change a grade, and a warning says why.
  Exact scoring and every spatial refinement keep the recording start inside the supplied interval,
  including fixed and fractional-second starts. Each decoy enforces the same interval shifted to its own
  clock window.
- **When it stands.** At least two independently credited passes must be heard: each reaches the 6.9-nat
  heard line and retains positive value after the timing penalty and the shared-pixel check. The answer
  must also beat every decoy (margin over the best of them above 0), with at least 40 decoys, and every
  place within 3 nats of it must lie within 5 km. Those places are the other modes and the answer's twins
  along the tracks of the flights it heard (next item). Its radius is that spread, and at least
  1 km, because its places are 1 km apart. Under exchangeable windows a place beats all 48 decoys by chance
  about 1 time in 49. On 42 runs with no right answer (six recordings at wrong dates and at places 90 km or more
  from the truth) none beat every decoy. The largest margin in those logged runs was −0.47, the Ithaca feeder's
  11-02 recording searched on 11-01 (full `locate` runs `r50_dev` and `r50_nulls`, and the same with measurement past
  the old line, `fm` and `nm`). It was −0.18 before the
  heard range was profiled, −0.6 before each start's whole disc was searched, and −0.8 on the version the line
  was set on. Those runs share six
  recordings, so they are not 42 independent trials.
- **Twins along a track.** One heard flight fixes when its closest approach was heard, not where along its
  track the recorder stood. Move the place `s` metres along the track and the clock `s / v` later, and that
  flight's curve does not move (exactly, for a straight, level, steady flight). A second heard flight with a
  different velocity breaks the tie: a crossing track, or a parallel one flown the other way. One with the same
  velocity does not: a parallel track flown the same way at the same speed, or the same leg flown again. The
  search's modes are at least 1 km apart and few, so nothing made it look along that line. So the answer is
  also scored every 1 km out to 6 km either way along each heard flight's track, with the clock moved to match,
  wherever the place is in the region and the clock in the window. A twin within 3 nats of the answer, or
  better, counts toward the radius as a mode does. It changes no evidence, margin or decoy, so it can only widen
  the radius, and a radius past 5 km removes the stand. Where the answer still stands with a wider radius, a low
  trace answer inside it keeps its own place (confirmed, medium) instead of taking the curve match's (adopted,
  medium). A stand it removes no longer conflicts with the trace answer either. The verdict line says how many
  twins tied, and `curvematch.json` lists them under `aliases`.
- **When it runs.** Only when the recording clock is known to 30 min or better and the region holds at most
  200,000 cells (about 8,000 km²); otherwise it is skipped with a log line. A state-wide search with no start
  therefore uses the trace search alone. It fetches its own wider ADS-B span, and the trace search's tracks are
  unchanged. Eligibility is checked before constructing its harmonic image, so a skipped match does not build
  an unused whole-file spectrogram. `--no-curve-match` turns it off.

What it did on the development recordings is in the [CHANGELOG](CHANGELOG.md). The sparse-track,
clock-bound and counted-pass changes have synthetic regressions. On `main`'s tip, which has all three, the 15
development rows kept their 5 right answers, all high, and gave no wrong one ([bench/PROTOCOL.md](bench/PROTOCOL.md)
Addendum 16). The logged outcomes describe the builds named with those runs.

---

## Design principles

1. **Refuse rather than guess.** A single flyover constrains the recorder to a line, not a point, so `fdop`
   refuses it. "No location" is a normal outcome with its own exit code. A low-confidence answer is labelled
   low, its verdict starts `unconfirmed` and names the failed gates, and the report shows the alternatives it
   beat.
2. **Physics before heuristics.** Templates, fits and the search all use the emission-time model. The search
   uses the real atmosphere and Hermite-interpolated ADS-B tracks, not straight lines.
3. **Every number carries its uncertainty.** Each fit reports profile-likelihood ranges, widened when
   diagnostics fire. Candidates are scored in nats against a calibrated null. The radius comes from a Δχ²
   region, not a guess.
4. **Free, public, cached data.** No accounts or API keys. Each source is cached on first use, so a rerun, or a
   run on another recording from the same days, is fast and can be fully offline.
5. **One native binary on the franken stack.** No Python, no tokio, no C solvers. frankenscipy does the
   numerics, asupersync does the networking, and `ffmpeg` decodes the media.

---

## How it compares

| | **`fdop`** | Same method by hand | Thunder timing | Mains-hum (ENF) analysis | Visual geolocation | Embedded GPS tag |
|---|---|---|---|---|---|---|
| Needs in the recording | ≥ 2 propeller flyovers | ≥ 2 flyovers | thunder | mains hum | landmarks, skyline | a location tag |
| Typical result | point, radius and recording clock | point | city-scale (about 5 km per colsto) | power grid and time, not position | varies | exact point |
| Effort | one command, minutes | hours of spectrogram reading, fitting and scripting | manual | specialist tools and grid records | manual | none |
| Flight data | free adsb.lol archive | the video used a paid Flightradar24 API | n/a | n/a | n/a | n/a |
| Survives metadata stripping | yes | yes | yes | yes | yes | no |
| Grades its own answer | five gates, high / medium / low | analyst judgement | analyst judgement | analyst judgement | analyst judgement | n/a |

---

## Installation

### Requirements

- **Rust** via [rustup](https://rustup.rs). The repository pins `nightly-2026-08-31` (Rust 2024 edition) in
  `rust-toolchain.toml`, and rustup installs it on the first build.
- **A C compiler** (Xcode Command Line Tools, or `build-essential`): a few dependencies compile C or assembly,
  among them `ring` for TLS and the SQLite that frankenpandas bundles.
- **ffmpeg and ffprobe** on `PATH`, for decoding (`brew install ffmpeg`, `apt install ffmpeg`, ...).
- **Network** on first use for each date and region, plus disk for the cache (see
  [Configuration](#configuration)). Expect about 4 GB of download per uncached UTC day; only the regional
  store, tens of MB, is kept.

### From source (recommended)

```bash
git clone https://github.com/Dicklesworthstone/franken_doppler_finder
cd franken_doppler_finder
cargo build --release
./target/release/fdop --version
```

### With `cargo install`

```bash
cargo +nightly-2026-08-31 install --locked \
  --git https://github.com/Dicklesworthstone/franken_doppler_finder franken_doppler_finder
```

`cargo install` does not read the repository's `rust-toolchain.toml`, hence the explicit `+nightly-…`. The
installed binary is `fdop`. There are no prebuilt binaries or crates.io releases yet.

### Check the setup

```bash
fdop doctor             # ffmpeg/ffprobe, cache directory, numerics smoke tests
fdop doctor --network   # also probes adsb.lol releases, NGS geoid, Census geocoder and terrain tiles
fdop selftest           # offline end-to-end known-answer run
```

---

## Quick start

1. **Check the tools:** `fdop doctor --network`.
2. **Run the known-answer test:** `fdop selftest`. It should print `PASS`.
3. **Locate a recording.** Give the local date and a coarse region:
   ```bash
   fdop locate clip.mp4 --dates 2026-09-08 --near "Nassau County, NY"
   ```
   The first run on a new date downloads the ADS-B archive, which takes minutes depending on bandwidth.
4. **Read the report:** open `fdop_runs/<name>_<time>/report.html` in a browser. The map, the per-event
   `*_match.png` figures and the gates show why the answer was chosen and how strongly.
5. **If it says "no location",** check the dates and region, widen them, or look at `survey.png`: the recording
   may hold fewer than two clean propeller passes.
6. **List past runs:** `fdop runs`.

---

## Command reference

`fdop` has six subcommands. Run `fdop <cmd> --help` for the built-in text. The shared options `--cache`,
`--offline` and `-q/--quiet` belong to `locate` and `fetch`, and go after the subcommand. `detect` needs no data
and takes `-o`, `-q` and the audio selection options below. `runs` and `doctor` take `--cache` too.

For media with multiple audio tracks, `locate` and `detect` analyse the first audio track by default, matching
the stream described by the media probe. Select another with `--audio-stream N` (audio tracks counted from 0).
The selected track is mixed to mono unless `--audio-channel N` selects one channel, also counted from 0. This
lets an ambient track be used instead of commentary, or one microphone channel be used when a stereo mix
cancels its tones. An unavailable track or channel is an error. The selected audio retains its timestamps,
including initial delays and missing-packet gaps; only the container and that track supply `creation_time`.
Decoding preserves elapsed recording time: missing packet intervals become silence, and audio delayed
relative to video retains that delay. The container's arbitrary starting timestamp is normalized without
collapsing internal gaps. Tones are not stretched to repair timing, so a gap does not shift later flyovers
or change their Doppler frequencies.

| Shared option | Meaning |
|---|---|
| `--cache <PATH>` | Cache root for ADS-B day stores, terrain, gazetteer, map tiles and geoid heights. Default: see [Configuration](#configuration) |
| `--offline` | Never use the network; cached data only |
| `-q, --quiet` | Less progress output on stderr |

### `fdop locate <MEDIA>`: the full pipeline

| Option | Default | Meaning |
|---|---|---|
| `<MEDIA>` | required | Audio or video file, anything ffmpeg reads |
| `--audio-stream <N>` | `0` | Audio track to analyse, counted from 0 among audio streams |
| `--audio-channel <N>` | mono mix | One channel of that audio track, counted from 0; avoids mixing other microphone channels |
| `--dates <D>` | required | Local date(s) of the recording: `YYYY-MM-DD` or `YYYY-MM-DD..YYYY-MM-DD` (also `D0 to D1`, `D0,D1`, `YYYY/MM/DD`, `YYYYMMDD`). The year comes first: `09/08/2026` is refused as ambiguous. At most 15 days |
| `--near <REGION>` | required | Coarse US region; see [Region syntax](#region-syntax) |
| `-o, --out <DIR>` | `fdop_runs/<name>_<unix time>` | Output directory |
| `--start <ISO>` | — | Exact recording start if known, e.g. `2026-09-08T19:17:30-04:00`. Without a zone it is read as **UTC**. Unix seconds also work |
| `--clock-tol <S>` | `120` | Finite, nonnegative clock tolerance (s) around `--start` or the file's `creation_time`. The [curve match](#the-curve-match) can judge only a clock window of 300 s or less: with `--start`, 150 s or less. Before 2026-11-07 the default was 180 s, at which it never could |
| `--ignore-metadata-time` | off | Ignore the container's `creation_time` |
| `--max-events <N>` | `12` | Maximum events that drive the pair join; the others can still confirm |
| `--no-curve-match` | off | Skip the [curve match](#the-curve-match); the answer then comes from the trace search alone |
| `--json` | off | Print the full result JSON on stdout instead of the summary |

```bash
fdop locate clip.m4a --dates 2026-09-08 --near NY
fdop locate clip.mov --dates 2026-09-07..2026-09-09 --near "Oyster Bay, NY" -o runs/oyster
fdop locate clip.wav --dates 2026-09-08 --near "40.87,-73.52,25" --start 2026-09-08T15:17:30-04:00 --clock-tol 60
fdop locate clip.m4a --dates 2026-09-08 --near CONUS --json > result.json
fdop locate clip.m4a --dates 2026-09-08 --near NY --offline        # cached data only
fdop locate clip.mov --dates 2026-09-08 --near NY --audio-stream 1 --audio-channel 0
```

**The time window.** Without `--start`, recording starts are searched from local midnight on the first date to
local midnight after the last, using every UTC offset the region spans, DST included. If the file's
`creation_time` falls inside that window, the search first narrows to it, allowing for both start and end
conventions. Both the trace search and, when eligible, the independent curve match try that window. A standing
curve match can locate the recording even when no trace candidate survives. Only when there is neither a trace
candidate nor a standing curve match does the search widen to the full window, with a warning. The curve match
retries too if the wider window is still eligible. A curve test from the earlier narrow window does not grade
that wider search. Its original `clock_window` and
`judged_search_window` flag are recorded in `result.json` under `extra.curve_match` when it has a verdict.

Archive availability, track loading and weather follow the window actually being tried. A metadata
attempt can therefore run offline with just its required cached UTC day, even when `--dates` includes
uncached surrounding days. If its archive cannot be loaded, or neither search finds an answer, the
full-date attempt loads its own tracks and weather and rebuilds the wind-dependent speed gates. An
eligible curve match still needs its wider decoy catalogue; weather covers those same decoy times,
including the radiosonde brackets, before adding the normal padding. An evening catalogue that fits
inside one UTC day can use just that day's archive; its rounded day limit does not require an
unrelated following day. If the recording's padded window reaches or crosses UTC midnight, the
next day is still required.

Archive and weather coverage use the actual decoded length of the selected audio stream. A longer
video track therefore does not force the audio search to wait for an unrelated unpublished day.
Decoding happens before publication clipping; weather still overlaps detection and measurement.
For a creation-time hint, a finite container duration is also retained when it is longer, because
the tag may identify the end of the container. Missing or understated duration tags cannot shorten
the decoded timeline. In `result.json`, `extra.media.duration_s` retains the probed container value;
the additive `extra.media.decoded_duration_s` records the audio duration used by the search.

With `--start`, its clock window is intersected with the local dates before checking archive availability or
fetching weather and tracks. Only the UTC days needed by that window, the recording duration and track padding
are fetched; unrelated days in a broader `--dates` range are not required, including in `--offline` mode. A
recording that crosses UTC midnight still needs both days. A supplied clock window outside `--dates` is an
error. If the archive is not published through the requested window, the search clips to the covered starts
with a warning, or errors when none remain; it does not widen past an explicit `--start`.

### `fdop detect <MEDIA>`: flyovers only, no ADS-B

Detects flyovers and measures them in detector order, strongest first within each stage, until 24 independent aircraft
events survive the same eligibility and harmonic checks used by `locate`. If a family's primary lost its
ridge around the closest approach, its detected harmonic siblings are tried strongest first until one passes
the existing quality and aircraft gates. The family supplies at most one result. Refused fits, non-aircraft fits and duplicate traces of one
pass do not consume that allowance. The selected result for each measured family remains in `events.json`,
including duplicates and unrecovered refusals. A warning on stderr says when either cap omits candidates, when the audio
looks high-passed, or when the survey image was raised because its spread was under 0.5. A detection found
only on the contrast image of such a recording has `"from_contrast": true` in `events.json`. The command
writes `events.json`, `survey.png`, and `e<N>.png` plus `e<N>_trace.csv` for every fitted event. The default
output directory is `fdop_runs/detect`.

`--audio-stream N` and `--audio-channel N` select the same source as in `locate`. For example,
`fdop detect clip.mov --audio-stream 1 --audio-channel 0` measures the first channel of the second audio track.

One line per event goes to stdout:

```text
e<N>  t_rec <seconds into file> s  f0 <Hz> Hz  v <m/s> m/s  d <m> m  rms <Hz>  ok | <failed quality gates>
e<N>  no fit: <reasons>
```

```bash
fdop detect clip.m4a -o runs/detect
```

Use `detect` to audit your own recordings (see
[Protecting your own recordings](#protecting-your-own-recordings)).

### `fdop fetch`: pre-download ADS-B

```bash
fdop fetch --dates 2026-09-08 --near NY             # default --pad-km 30
fdop fetch --dates 2026-09-07..2026-09-09 --near "Nassau County, NY" --pad-km 50
```

This caches the ADS-B day stores that `locate` will need, which is useful before going offline or before
several runs. It prints `<legs> legs, <points> points cached under <cache>`. Like `locate`, it fetches only the
published part of the archive. When the dates run into a UTC day adsb.lol has not published yet, it warns
which days are pending and stops before them. If no published day is left, it exits with an error: rerun after
the release.

### `fdop runs`: the run ledger

```bash
fdop runs                 # last 20 locate runs
fdop runs --limit 100 --json
fdop runs --cache DIR     # the ledger of runs made with locate --cache DIR
```

Each `locate` run is recorded in FrankenSQLite at `<cache>/runs.sqlite`: time, input, dates, region, lat/lon,
radius, confidence, headline, run directory and total time. A run that stops with an error is recorded with
confidence `error`, the error message as its headline, and the run directory if the run wrote anything there
before failing. Without `-o`, the error message also names that directory. If the ledger cannot be written,
`locate` warns and carries on.

### `fdop doctor [--network] [--cache DIR]`

Checks `ffmpeg` and `ffprobe`, that the cache directory is writable, smoke-tests the FFT and k-d tree, and does
a FrankenSQLite write/read round trip (plus a read of `runs.sqlite` if present). An `[info]` line says which
Census gazetteer tables are cached; `--offline` needs them for place, county and ZIP regions and for the report's
place names, and they download on first use. With `--network`, it also GETs the adsb.lol release index, the NGS
geoid API, the Census geocoder and one terrain tile. It prints `[ ok ]` or `[FAIL]` per check and exits 1 if any
check failed.

### `fdop selftest [-o DIR]`

This needs `ffmpeg` but no network. It synthesises a 1,300 s recording at a known place and clock: two audible
turboprops with harmonics, three silent decoy tracks, a 61 Hz ground hum, noise, and a 26 °C atmosphere with
wind. It writes `selftest.wav` and runs the real decode → detect → measure → search path on a flat DEM.
**PASS** requires a position error within max(radius, 100 m), both audible flights among the winners, and a
clock error under 2 s. It writes to `fdop_runs/selftest` by default.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | `locate` found no location; `selftest` FAIL (a wrong answer or no candidate); `doctor` had a failed check |
| 2 | Error (bad input, region not found, fetch failure, ...), printed as `fdop: error: ...`; also clap usage errors |

### Region syntax

`--near` is case-insensitive, with one exception: a two-letter state after a place name with no comma must be
upper case (`Oyster Bay NY`; `oyster bay, ny` also works). Leading words such as "near", "in", "around" and
"somewhere near" are ignored.

| Form | Examples | Search area |
|---|---|---|
| State (abbreviation, name or a single FIPS code), or a list of abbreviations or names | `NY`, `New York`, `36`, `NY, NJ, CT`, `NY and NJ` | union of state bounding boxes |
| Whole country | `CONUS`, `US`, `USA`, `lower 48`, `contiguous US` | 24.4–49.5° N, 125–66.9° W |
| Place | `Oyster Bay, NY`, `Oyster Bay NY`, `Oyster Bay New York` | Census place, radius max(30 km, 3 × its equal-area radius) |
| County | `Nassau County, NY`, `Orleans Parish, LA` | Census county, radius max(25 km, 1.5 × its equal-area radius) |
| ZIP | `11771`, `11771-1234` | Census ZCTA internal point, 25 km |
| DC | `Washington DC`, `District of Columbia` | DC |
| Point | `40.87,-73.52`, `40.87 -73.52`, `40.87N 73.52W` | 30 km radius |
| Point and radius | `40.87,-73.52,20`, `40.87,-73.52,20km` | the given radius, up to 3,000 km |
| Box | `40.6,-73.9,41.1,-73.2` (S,W,N,E) | the box |

A bare place name must be unique in the US; otherwise `fdop` lists the matches and asks for a state.

### Outputs

Each `locate` run writes a directory:

| File | Contents |
|---|---|
| `report.html` | Single-page report with a Leaflet map (USGS Topo, USGS Imagery and OSM layers), heat map, top candidates and answer circle. The map loads Leaflet and tiles from the internet |
| `report.md` | The same report in Markdown: answer, gates, ranked candidates, events, funnel, margin, timings, warnings, data attribution |
| `map.png`, `map.svg` | Static 1024² map on USGS Topo tiles with the radius ring and runner-up markers |
| `result.json` | Everything: answer and gates, place, top 25 candidates, events, chance-match calibration, atmosphere, funnel, margin, warnings, heat map, figures, timings |
| `candidates.csv` | The top 25 candidates, one row per (candidate, matched event) |
| `survey.png` | Whole-file log-frequency spectrogram with detections and traces marked |
| `e<N>.png`, `e<N>_trace.csv` | Per-event spectrogram with the fitted curve, and the traced points |
| `e<N>_match.png` | Each winning event's trace against the ADS-B prediction |
| `curvematch.json` | The curve match, when it ran: its places with every pass's evidence, the best place's twins along the heard flights' tracks (`aliases`), each decoy window's best place and score, the margin over the best decoy and its rank p |
| `detections.json`, `events.json`, `search.json` | Intermediate results. `search.json` keeps the best 10,000 candidates, and `candidates_total` counts them all |

---

## Configuration

`fdop` has no config file. It is controlled by flags and these environment variables:

| Variable | Effect |
|---|---|
| `FDOP_CACHE` | Cache root. Precedence: `--cache`, then `FDOP_CACHE`, then `FDOP_CACHE_DIR`, then `$XDG_CACHE_HOME/fdop`, then `~/.cache/fdop` |
| `FDOP_CACHE_DIR` | Alternative name for the cache root |
| `GITHUB_TOKEN` | Optional. The adsb.lol archive is listed through the GitHub API, which allows 60 unauthenticated calls per hour. A day costs about 2 calls |
| `FDOP_SURVEY_PNG`, `FDOP_EV_PNG_DIR` | `detect_file` example only: write the survey and per-event images |
| `EXPLAIN_DUMP` | `explain` example only: dump raw ADS-B points near each CPA |

Cache layout:

```text
~/.cache/fdop/
├── adsb/<YYYY-MM-DD>/<feeds>_<S>_<W>_<N>_<E>.fdtrk   regional track store per UTC day (gzip, magic FDOPTRK1)
├── adsb/<YYYY-MM-DD>/releases.json                   archive release metadata
├── weather/                                          Open-Meteo and radiosonde responses
├── geoid/<lat>_<lon>.txt                             NGS geoid height at a region's centre
├── terrarium/<z>/<x>/<y>.png                         elevation tiles
├── usgs_topo/<z>/<y>/<x>.jpg                         basemap tiles for map.png
├── gazetteer/                                        Census 2024 gazetteer zips
└── runs.sqlite                                       the run ledger (FrankenSQLite)
```

Cached day stores are tried from the smallest covering box to the largest. A store is reused only when it
can be read and its stored box and time interval cover the requested slice of that UTC day. Loading also
verifies the gzip checksum and complete footer and rejects undeclared decompressed data. An unreadable
or incomplete cache is identified in the progress output, and other eligible caches are tried before
downloading again or failing in `--offline` mode. Partial feeds retain their missing-feed warnings. A larger
region on the same day downloads that day again if no usable cache covers it.

Cached day stores are scanned one leg at a time to summarize each aircraft across all requested days,
then rewound on the same open files to retain every leg of aircraft that may intersect the requested
region and time. Combining bounds before selection preserves a flight crossing the region between two
UTC days; keeping the whole aircraft also preserves merge keys, feed preference and interpolation
anchors. Original day metadata and altitude references remain available even when no aircraft from a
day is selected. This reduces retained track data for selective requests. A cached file containing
selected aircraft is decompressed twice, so broad requests may save little memory and pay for the extra
scan. The store format and final spatial/time filter are unchanged.

Concurrent searches publish track stores through separate, exclusively created staging files, so one
writer cannot truncate another writer's cache. A load also rejects bytes after its compressed member,
including additional gzip members; a damaged cache goes through the recovery path above.

Weather responses, terrain tiles, geoid heights and file downloads also use private, exclusively
created staging files. Concurrent runs cannot share or truncate one another's in-progress response.
Census gazetteer ZIPs are parsed before reuse; online searches replace an unreadable, malformed or
empty table only after downloading and parsing a valid replacement. A failed download or validation
leaves the published cache intact. Offline searches report the unusable cache path and how to repair
it; a valid cache is reused without any request.

For uncached multi-day searches, each UTC day is built and atomically cached as soon as all of its
requested feeds finish, while the remaining days continue downloading. Completed raw feeds pass through
a bounded queue instead of accumulating until the last download. A failed feed prevents publication
of its own day; other completed days remain reusable. Cache-write failures remain errors, after the
other days receive their save attempts. After publication, a completed day keeps its actual open cache
file, original metadata and aircraft bounds; its point arrays are released while other downloads run.
Fresh bounds come directly from the built reports, and selected fresh files are decoded once when all
days are ready for the final merge. The saved handle stays attached to that day's data if another writer
replaces its public cache pathname. Active ingestion, incomplete days, the day currently being built,
and the final selected tracks still consume memory; this is not a total process-memory bound.

Every release listing captured before the publication window closes is rechecked online. Split-archive
listings are also rechecked before each uncached online download, regardless of age: a contiguous
`.aa`, `.ab`, ... prefix cannot prove that its final piece is present. Existing complete day-store hits
bypass that lookup, and settled whole-tar metadata remains reusable. Archive parts must be one matching
tar or a contiguous split sequence; checksum sidecars are ignored, and missing, duplicate, empty or
unfinished parts are rejected. Offline runs use valid cached listings and retain missing-feed warnings.

An ingested archive is complete only after both tar end blocks and the entire transport have been
read. Missing end blocks, a late download failure or nonzero trailing data prevent that feed's day
store from being published; ordinary zero padding is consumed. This also catches a missing final split
part even if every listed part downloaded successfully. Existing day stores cannot reveal whether an
older ingester accepted an incomplete source archive; recovering such missing data requires fresh
ingestion.

Day caches retain aircraft with unknown types. For data API callers using
`IngestSpec.keep_unknown = false`, that policy is applied after cross-day legs are merged:
a recognized type on either adjoining day identifies the complete joined flight, preserving
both halves of its geometry. A missing or unrecognized designator can be replaced by a known
one; the first recognized type still takes precedence over a later conflicting known type.
Only flight segments that actually join share this metadata. Separate flights of the same
aircraft and flights whose type remains unknown are still excluded by the strict policy.

When neighboring track stores overlap, reports within the existing 0.05 s duplicate interval
prefer non-MLAT positions even if an MLAT report arrived slightly earlier. The merge preserves
the previous flight-leg joins and same-quality choices, including interleaved overlaps from
three or more files. Unjoined legs retain their original point allocation.

Archive ingestion and cached-window slicing retain the original samples bracketing a sparse segment that
crosses the requested region and time window, even when neither sample is inside. The segment must enter
both at the same time. Ingestion does not infer a crossing across a ground report or restart marker;
cached-window slicing does not cross stored flight-leg boundaries. Two neighbouring samples on each side
still anchor interpolation, and the track model's existing gap and geometry checks still apply. No positions
are invented. Older narrow caches that already discarded an aircraft need fresh ingestion to recover it;
the fix can use bracketing
samples in an existing wider cache, but cannot restore samples absent from the stored data.

`locate` and `fetch` reuse the agreed NGS geoid height cached for the region's centre in both online and
`--offline` mode. Offline mode uses the −30 m default only when that height is absent, unreadable or invalid,
and warns when it does. Going offline therefore preserves the altitude reference of a valid cached run.

### Data sources

| Data | Source | Notes |
|---|---|---|
| ADS-B and MLAT tracks | [adsb.lol](https://adsb.lol) `globe_history_<year>` GitHub releases | ODbL. One global archive per UTC day (`prod` about 3.6–3.9 GB, `mlatonly` about 0.3 GB), streamed and filtered to the region; `staging` is used when `prod` is missing |
| Aircraft engine class | `crates/fdop-types/data/icao_engine_class.tsv` | 2,614 ICAO type designators → piston, turboprop, jet, electric or other. Jets are dropped; unknown types are kept |
| Weather | [Open-Meteo](https://open-meteo.com) historical forecast (pressure levels) and archive; [Iowa Environmental Mesonet](https://mesonet.agron.iastate.edu) radiosondes | Pressure levels first; only on failure, load radiosondes and surface weather concurrently. A slow service does not discard a usable profile from the other source. Radiosonde stations are selected by archive dates covering the requested sounding brackets, so a station closed today can still serve historical recordings. US country metadata includes Alaska and Hawaii stations; their full identifiers are preserved when requesting soundings, while continental stations retain their existing cache keys. The 15 °C standard atmosphere is used only if no usable profile arrives, with a warning |
| Terrain | [AWS Terrain Tiles](https://registry.opendata.aws/terrain-tiles/) (Terrarium encoding) | z10 for the coarse search, z13 for the fine search; a tile that cannot be fetched counts as 0 m, with a warning giving the number of such tiles. A pixel below −430 m, lower than any dry land, is seabed or a spike in the tiles: it is left out of the blend, and a cell with nothing left counts as sea level |
| Geoid | [NOAA NGS](https://geodesy.noaa.gov) GEOID API | Converts GNSS heights to MSL. Taken once two reads agree, then kept in the cache; −30 m, with a warning, if the reads fail or never agree, or under `--offline` |
| Regions, places, ZIPs | U.S. Census 2024 Gazetteer files and the Census Geocoder | Offline fallback uses the gazetteer's internal points |
| Basemap | [USGS The National Map](https://www.usgs.gov/programs/national-geospatial-program/national-map) Topo tiles; OSM in the HTML report | |

---

## Architecture

```text
franken_doppler_finder/
├── src/                     fdop binary: cli.rs, pipeline.rs (locate), ledger.rs, doctor.rs, selftest.rs
├── examples/explain.rs      score named (event, flight) pairs at a point; residual diagnostics
├── examples/truth.rs        known place + clock: archived passes near it vs the detected events
└── crates/
    ├── fdop-types/          shared types: geodesy (WGS84, ECEF, ENU), time, atmosphere, .fdtrk store, engine table
    ├── fdop-http/           HTTP/1.1 client on asupersync + rustls: retries, resumable streaming, idle timeout
    ├── fdop-data/           adsb.lol ingest and track-store build, weather, terrain, geoid
    ├── fdop-core/           audio, spectrogram, detection, tracing, fitting, physics, track models, search
    └── fdop-geo/            region parsing, gazetteer, reverse geocoding, static map, reports
```

| Crate | Depends on | Main external crates in use |
|---|---|---|
| `fdop-types` | — | serde, serde_json, flate2 |
| `fdop-http` | — | asupersync (`tls-webpki-roots`), serde_json |
| `fdop-data` | types, http | tar, flate2, png, rayon, serde_json |
| `fdop-core` | types | fsci-fft, fsci-opt, fsci-stats, rayon, png, serde_json |
| `fdop-geo` | types, http | frankenpandas, zip, png, jpeg-decoder |
| `franken_doppler_finder` (`fdop`) | all five | clap, fsqlite, asupersync, rayon, serde_json; fsci-fft and fsci-spatial in `doctor`'s smoke tests |

Design notes:
- **Detection is a template bank, not peak picking.** On a log-frequency axis, changing `f0` only shifts a
  curve vertically, so one template per (speed, distance) scans every (time, pitch). Scoring takes the
  *minimum* of the approach and recede halves, which rejects flat tones and one-sided transients.
- **A compressed survey image is scored at the spread its lines were set at.** A recording loud mostly while
  one pass goes over inflates every row's MAD, so noise and ridge shrink together and the image's robust spread
  falls (0.18–0.4 on short Livermore clips, against 0.57–0.78 on the development and null recordings). Under
  0.5 the image is multiplied up to 0.5, at most ×4, and clamped again; at or above it nothing changes.
- **The search is flight-centric.** It does not grid the whole region. For each event and each flight, every
  moment of the flight is a candidate CPA: the recorder then sits on the perpendicular to the path through that
  point, at a distance that fits the measured CPA distance, and the moment fixes the recording start. A hash join
  on (east, north, recording start) finds the places two events share in O(N) time.
- **Rejected reports cannot hide a separate supported flight segment.** Track cleaning keeps the
  original longest physically consistent chain, then can recover another supported chain wholly
  before or after it. Every retained interval must be separated by more than the existing 180 s
  gap limit and physically reachable from its neighbours under the same speed/vertical limits.
  Each interval becomes a separate model; interpolation never crosses the hole. The original
  model and existing split keys stay intact; additional models use deterministic `#recoveredN`
  suffixes. Overlapping alternatives and a denser ambiguous winning component remain unresolved.
- **Close passes can seed the search between grid lines.** A measured distance interval can fit wholly
  between the default 350 m cross-track samples. When a side has no accepted grid point, the search tries
  an interior point using the terrain height at every probe. A rejected point outside the requested
  region or recording-clock window cannot suppress that attempt. The distance, terrain-clearance,
  region and recording-clock gates apply to every accepted point.
- **Overlapping flyovers are compared by shape.** Close CPA times and a small-integer tone ratio alone do
  not identify one aircraft. Detection compares modelled curve shapes; search compares measured traces at
  the same recording times after fitting their frequency ratio. Distinct shapes can seed separate flight
  matches. Compatible harmonics remain one event whichever is strongest, and insufficient overlapping
  trace support keeps them grouped.
- **Hidden curves get a residual scan before weak detections can mask them.** The first template-bank
  pass accepts strong events. One additional pass searches their remaining evidence, retaining the
  original coverage counts and score normalization. Weak-only recordings keep their ordinary scan.
  Both passes share the existing candidate allowance and detection cap. This lets different curves
  with the same tone and CPA reach measurement; their initial cell winner no longer has the only chance.
- **A pass heard near the start or end gets a short template.** A short transition (1.25 d/v at most
  24 s) is scored by a 24 s template, which needs 8 of its 12 path samples on each side, about 16 s of
  recording before and after the CPA. A nearer CPA cannot be scored however loud the pass, and a clip
  under about 32 s gives the bank no cell at all. After both passes, a 10 s edge template
  (`DetectParams::edge_half_s`) scores exactly the cells its 24 s template cannot cover within 24 s of
  either end. It works on the evidence the accepted curves left, from the silence line, under the passes'
  suppression, residual and marking rules, so every detection the passes accept is the same with or
  without it. `refine` keeps the 10 s half. The stage runs only on the frames near each end, and not at
  all when the passes stop at three times the detection cap.
- **A compressed image is scored once more, contrast first.** The swell that compresses an image also
  shrinks the ridges of the pass that made it. It raises every row together, which inflates each row's MAD
  before the frame contrast can take the swell out. On Livermore's N1387U_R44, a 157 Hz tone 33 dB over its
  neighbours at the CPA had a row z of 1 to 1.5, and nothing in its image scored over 2.88. An image under the 0.5
  floor therefore gets a second one from the same levels, with the frame contrast taken first and each row's
  robust z after it (`LogSpec::contrast`). After the primary and edge stages, the full bank scores it on what the accepted
  curves left, at the strong line, 4.5, since the lines were not set on this image. Its detections follow all
  the others, which keep their order, harmonic grouping and measurement, and carry `from_contrast`. No null
  recording is under the floor, so no null run tests them, and an answer resting on one counts as measured
  past the old line. On swells of noise alone the stage adds nothing up to 30 dB. From 35 dB it adds strong
  curves of its own, and the measurement gates refuse them.
- **A final bank covers faster subsonic passes.** After the primary, edge and contrast stages, templates
  continue the original speed spacing above 175 m/s, up to the existing measurement bound of 0.8 times
  sound speed (about 194, 215, 238 and 263 m/s at 343 m/s). Full-length fast templates run first, followed
  by their short edge templates near the recording's ends. The fast edge scan uses the ordinary edge
  bank's coverage guard, so it supplies only cells the corresponding full-length template cannot cover.
  Both fast stages use residual evidence, share the remaining candidate budget and detection slots,
  and must clear the existing strong line. Ordinary edge templates still use the silence line.
  Earlier detections keep their order. Refinement permits these faster seeds inside the same measurement
  bound and preserves an edge seed's short half; legacy seeds keep their original refinement path.
  A faster coarse seed counts as measured past the old line even if refinement lowers its speed.
  A fixed 14-scene synthetic signal-strength sweep establishes one new faint 215 m/s full-length
  recovery with no baseline event; it does not establish recovery of a real field miss. The new
  fast-edge PCM recovery assertions remain unrun (Testing).
- **STFT working storage stays bounded as recordings grow.** Frames transform into a reusable block of
  at most 256 columns and 4 MiB of frequency values, then copy into the final row-major spectrogram.
  A band wider than that budget still needs one complete column; parallel FFT scratch is separate.
  The final spectrogram still scales with recording length, but a second complete spectrogram and a
  separate allocation for every frame are no longer retained. Frequency bins and frame centres are unchanged.
- **Guided measurement follows the predicted ridge.** It selects the nearest qualified local spectral
  peak within the existing model band, using the existing SNR and robust-z requirements. A brighter
  neighbouring aircraft no longer automatically takes over the trace when both tones enter that band.
  The peak must also stand the SNR requirement above the valley separating it from any stronger bin, so
  ripple on a stronger ridge's top does not qualify, while a separate aircraft's ridge still does.
  Standalone unguided ridge tracing retains its strongest-peak rule.
- **Frequency estimates preserve the spectral peak's shape.** Ordinary and short windows integrate
  along the full Doppler curve. Noise exclusion accounts for the Hann main lobe and chirp smear, so
  narrowing the frequency tolerance does not count the signal as its own noise. Normalized spectra
  still select and qualify peaks. Only after qualification, local frequency interpolation restores
  the removed row background; its slope must not shift the measured tone. Preserve a valid concave
  three-bin interpolation first, including a vertex between the selected bin and a neighbouring
  basin boundary. If that correction is unavailable and normalization moved the peak by several FFT
  bins, restoration climbs only within its original spectral lobe, stopping at a valley or flat noise
  floor. Every restored vertex stays inside the original frequency tolerance. Restoration cannot
  cross to another ridge or a removed hum, and the original
  SNR/z thresholds stay in force. No extra full spectrogram is retained.
- **Steep passes get a matched window.** A 0.5 s main window stops holding a chirp at 1/win² = 4 Hz/s. Frames
  steeper than that read a second spectrogram, with the window that matches the latest fit's chirp. There the
  peak integrates along the model's curve, the band is at least 0.25 s of chirp to allow for detection's error,
  and the noise reference spans four times the ridge's half-width (half a main lobe plus half the chirp's smear),
  as the main window's does. A real tone's skirts are wider than a pure tone's; at twice, they filled the
  reference once the band narrowed, and later rounds dropped steep frames whose ridge stood 8–14 dB up.
- **A fit needs a ridge through its drop.** Two steady tones at different pitches fit a Doppler curve with nothing
  between them, at an excellent RMS. `drop_traversed` in `events.json` is the share of the central 10–90 % of the
  fitted drop that the trace travels: each continuous stretch adds its net fall, and a stretch ends at a time gap
  or where the trace steps more than 2 Hz off the model's own step. Under 15 % the event is refused. It still
  counts as heard, but it does not use an independent-event slot.
- **A one-sided refusal is traced once more, without the model.** A template of the wrong shape (at the edge
  of the bank, or with speed and distance wrong together) meets the ridge over one stretch and leaves the band
  elsewhere, so the guided trace holds one side of the pass and the gates refuse the fit. When a fit is refused
  and its trace covers less than 8 s on either side, the ridge is followed by its own slope from that trace's
  first, strongest and last points, inside the event's analysis window. The free trace is fitted, and the guided
  rounds start again from that fit if it is plausible. The second attempt replaces the first only if every gate
  accepts it; `reseeded` in `events.json` marks it. Accepted fits, and refusals whose trace held both sides, are
  measured exactly as before. A wrong fit can pass this way too: a climbing pass in a clip that starts mid-drop
  trades speed against distance, and the retry accepts the wrong pair.
- **A primary that lost its ridge can yield to a usable harmonic.** A bright harmonic may lose its ridge
  through the drop while another tone from the same pass remains measurable. Both `locate` and `detect`
  try the primary first, then its detected siblings in score order only if the primary has no fit, fewer
  than 80 trace points, less than 8 s of trace on either side of the closest approach, or less than 15 %
  of its frequency drop followed. A primary that was traced but refused for another reason (such as its
  fit RMS) or measured below the aircraft limits keeps its own result: on the recordings
  checked, every sibling chosen over such a primary measured the pass worse. The first usable sibling
  supplies the family's single event. If none passes, the
  original primary refusal remains. The recovered detection retains its `harmonic_of` link and always
  counts as measured past the old line, including when the primary had a strong survey score.
- **Measurement slots belong to independent aircraft events.** The same measured-curve comparison used by
  the search groups duplicate seeds and harmonics before deciding whether the 24-event allowance is full.
  Fits below the existing aircraft limits (28 m/s or 200 m closest distance) do not fill it either. Later
  detections can therefore supply missing flyovers. Distinct overlapping Doppler shapes still count separately.
  `locate` keeps at most 24 independent events after final grouping, with a warning if a better representative
  changes the grouping enough to exceed that limit. Original detection ranks still determine the measurement-line
  gate; grouping does not give a newly reached weak event the status of an earlier one.
- **Hypotheses are compact.** Each is 20 bytes (a `u32` track index and four `f32`). The budgets are sized from
  physical memory: a tenth of RAM, capped at what a 32 GiB machine gets, 172 M hypotheses (3.4 GB). More would
  overflow the comparison budgets below and lower the grade, so every machine from 32 GiB up runs the same search.
  Smaller machines no longer receive a mandatory 4 GB allowance; an 8 GiB machine budgets about 819 MiB. If memory
  cannot be detected, the cap applies. Generation checks the budget while building a track's cloud, and a confirmer-only
  event keeps its exact hypothesis count and chance sample without materialising the full cloud. Vector
  capacity, temporary collections, audio, tracks and pair joins require additional memory. One event may
  take a third of the total, so a 16 GiB machine gets 86 M hypotheses and 29 M per event, and a 32 GiB machine
  172 M and 57 M. With less than a run needs, the events built last drop to confirmer-only, with a warning, so
  below 32 GiB the answer can depend on RAM. The three reference runs fit in 32 GiB; there the busy osprey-cam search demoted
  four events and two other field searches one each.
  The comparison budgets default to 20 M joined points, 2 M fine cells and 2 M confirmations, above the largest
  searches estimated from real runs (about 11 M and 1 M). A pair-join, fine-stage or confirmation-budget cut is
  reported in `funnel.truncated` and the warnings. No flag changes these budgets, and they do not scale with RAM:
  the largest runs here peak at about 10 GiB on a 32 GiB machine, and smaller machines have not been tried. An
  event that exceeds its hypothesis budget, or falls beyond `--max-events`, drops to confirmer-only, which
  `funnel.hypotheses` shows;
  a budget demotion is also a warning. An event with no eligible flight hypotheses is reported with a zero
  count and does not consume a driver slot: later matchable flyovers can still seed the search.
- **Fast tracks retain the normal sampling cadence outside the reach box.** The previous fixed
  ten-second skip could jump across a small region and miss an entire close flyover. Both inside
  and outside the box now use the existing distance/velocity timestep, before the same speed,
  distance, terrain and gap checks. This adds samples on outside segments; its runtime and
  real-recording effects have not been measured.
- **A narrow region can lie between cross-track grid samples.** When a side has no accepted
  hypothesis after the regular grid and its distance fallback, clip the cross-track ray to the
  requested box and probe that interval. Every added point still passes the terrain, distance,
  region and clock checks. This recovers an in-box strip without changing the regular grid;
  inclusive interval endpoints remain eligible when the interior probes fail. This is not an
  exhaustive search of disconnected terrain pockets.
- **Terrain is checked across the full cross-track reach.** A hillside can bring a later observer point back
  inside the slant-distance gate after a valley falls outside it. One failed point no longer cuts off that side.
- **A location still needs two events after clock checks.** A candidate pruned to one flyover is removed.
  Newly promoted leaders receive the same clock checks before the winner is chosen, even when many higher
  scoring coincidences fail first. Separate passes by the same aircraft remain supported.
- **Returning aircraft keep distinct physical passes.** A hypothesis's emission time selects the
  nearest interior local closest approach in time, so a closer return elsewhere in the window does not
  replace its pass during pair scoring, clock checks or refinement. Confirmation considers each local
  approach inside its existing window and applies the same timing, geometry and trace-fit gates.
  It excludes an already assigned physical pass rather than its entire track, using the pair join's
  existing 30 s emission-CPA separation. Another pass by that aircraft can therefore provide a match.
  An aircraft already heard once is also checked for later, much closer passes with no detection;
  the half-distance and 25 s hearing rules apply to those passes too.
- **Repeated flight pairs keep their alternative recording clocks.** Quick-check survivors retain the
  pair join's 20 s clock cells when they merge into 1 km spatial cells. Two aircraft repeating their routes
  can therefore reach fine scoring at both plausible recording times, where another event can distinguish
  them. Each flight pair still gets its best cell scored before its next; the comparison budgets, truncation
  warnings and confidence gates are unchanged.
- **Parallel, and repeatable up to exact ties.** rayon runs the STFT frames, the template bank, tracing and
  every search stage. Each FFT is pinned to one worker so it does not nest inside rayon. Null sampling uses a
  fixed hash. Candidates with exactly equal scores can come out in a different order from run to run.
- **Sparse turns retain valid reported velocities.** If a reported tangent disagrees with the finite
  difference of nearby positions, the track builder also checks the position chords against the circular arc
  implied by neighbouring headings. Both checks use the same 25 m/s residual limit; a turn does not disable
  checks for incorrect ground speed, heading or vertical rate.
- **Dense bad-position bursts do not hide valid predecessors.** Track cleaning searches every possible
  predecessor within its allowed time gap. The former 48-report lookback could discard usable sections or
  choose the corrupt positions merely because bad reports arrived more frequently. Exact chain-length
  bounds keep clean trajectories fast without imposing a sample-count cutoff; the physical speed,
  vertical-rate and time-gap limits are unchanged.

---

## Performance

Historical runs of the reference case on one machine, an Apple M4 with 4 performance and 6 efficiency cores and 32 GiB of
RAM, one after another with nothing else running. Treat them as an order of magnitude, not a benchmark.

| Stage | `r_nassau_clean`: county, 1 day (s) | `r_ny_clean`: NY, 1 day (s) | `r_md_clean`: NY, 3 days (s) |
|---|---|---|---|
| decode (1 h of audio → 4 kHz mono) | 1.6 | 1.6 | 1.5 |
| detect | 8.7 | 8.8 | 8.7 |
| measure (24 events) | 16.7 | 17.2 | 17.2 |
| ADS-B (UTC days cached) | 0.9 | 0.7 | 1.3 |
| tracks and terrain | 0.0 | 0.1 | 0.1 |
| search | 6.5 | 89.1 | 326.0 |
| answer, geocode, map, report | 0.7 | 0.7 | 0.4 |
| **total** | **35.8** | **118.8** | **356.1** |
| fine cells scored | 755 | 12,119 | 50,751 |
| peak memory (GiB) | 3.1 | 3.9 | 8.2 |

The state-wide run searched 10,542 track models. All three had the ADS-B days already cached. A first run adds the
archive download (about 4 GB per UTC day) and the store build. One local date usually spans two UTC days. Stored
hypothesis payload is budgeted to a tenth of RAM, capped at 3.4 GB; temporary collections and the rest of the
pipeline add to process memory.

These runs search only 3 of the recording's events, because the others do not pass the quality gates, so they are
not typical of a busy recording. Search time follows the fine cells, at 50–70 ms of CPU each. On that machine, with
a second replay running and so not a clean timing, a 3 h osprey-cam recording searched across its state scored
893,697 cells in 9,602 s (45,474 CPU-seconds, 8.8 GiB). It took 584 s when the fine stage stopped at 4,000 cells.

Audio decoding now converts FFmpeg's output incrementally with a 64 KiB conversion buffer and a
bounded diagnostic buffer. It no longer retains a second full recording as raw PCM bytes while
constructing the final sample vector: that byte copy alone was 57.6 MB per hour at 4 kHz mono.
Detection still retains the decoded sample vector, whose allocation can have spare capacity.
The decoded samples are checked byte-for-byte against the previous path; total peak memory and
real-recording runtime have not been remeasured.

Detector scoring now reuses one row of scratch per worker instead of full temporary spectrograms for every
template. Scalar-oracle and before/after synthetic comparisons preserve the scores and detections. The real
recording timings above predate this optimization; they have not been remeasured.
Search clock scans also reuse trace-fit and result buffers, preserving the original clock grid and all
candidate comparisons. Before/after synthetic comparisons check complete point scores and chosen clocks
bit-for-bit; the size of the end-to-end speedup has not yet been measured on a real recording.

Pair joining now discards duplicate flight/place/clock seeds as they are generated, using bounded
batches of 4,096 input hypotheses and ordered worker chunks. Previously it held every compatible
raw pair before deduplicating: a dense bucket could exhaust memory before the quick-check budget
was considered. The hash index and distinct surviving seeds still consume memory; this is not a
fixed total-memory bound. The same first representative, candidate order, spatial and clock cells,
and complete comparison are retained. No detection cap, search budget or confidence line changes.

Each event pair now reaches the quick trace check before the next pair is joined, so rejected
seeds do not accumulate across the entire search. The joined-seed budget still counts rejected
seeds and stops at the same complete-pair boundary. Hypothesis clouds are released before fine
scoring, and fine-grid input seeds before confirmation and refinement. These later stages retain
their surviving candidates; the changes do not impose an additional search cut.

**Pair-join component measurement, 2026-11-07.** The
`dense_pair_join_with_one_million_raw_matches` workload has 1,024 hypotheses on each side,
1,048,576 compatible raw pairs and one retained seed. Exact original and updated join source
ran with the pinned nightly, Rayon 1.12.0, four workers and an optimized test build (level 2,
no debug information) on a shared AMD EPYC 9V74 Linux host. All three alternating before/after
pairs passed the same assertion; per-child kernel peak RSS and monotonic elapsed time gave:

| Median over three runs | Original join | Bounded join |
|---|---:|---:|
| Peak process RSS | 70,360 KiB | 9,472 KiB |
| Elapsed time | 0.680 s | 0.148 s |

That is 86.5% lower measured peak RSS on this duplicate-heavy component fixture. Scheduling
was shared, and these are not whole-recording timings or total `fdop` memory measurements.
The updated `dense_pair_join_keeps_one_seed_without_raw_pair_storage` regression also passed
with 16,777,216 compatible pairs and 9,344 KiB peak RSS; its original-version counterpart was
not run under the shared 8 GiB memory limit. No end-to-end memory reduction is claimed from
the additional per-pair quick-scoring and early-release changes.

The exact-curve scorer now builds one conservative ECEF box per shortlisted trajectory and reuses it
across candidate locations, clock alternatives, refinement and decoy windows. The box encloses the full
Hermite curves, including turns outside the reported points. A trajectory proven farther away than the
existing reach limit skips the repeated closest-approach scan; uncertain geometry keeps the original
path. This does not change the reach limit, score formulas, candidate order or confidence thresholds.
The new shortcut has source review and regression coverage, but native equivalence and runtime
measurements remain pending; no end-to-end speedup is claimed.

**Component measurements, 2026-11-04.** Optimized Rust builds on a shared AMD EPYC 9V74 host, with
alternating old/new execution order. The detector uses four Rayon workers; other probes are serial.
These medians compare the actual old and new functions, with unchanged supporting physics for the
detector and search probes. Competing builds caused substantial scheduling noise, especially in the
three-hour detector case, so these are indicative component measurements, not `fdop locate` timings.

| Component and synthetic workload | Old median (s) | New median (s) | Ratio, old/new |
|---|---:|---:|---:|
| Full detector, 240 rows × 3,600 frames, 3 trials | 22.566 | 6.317 | 3.57× |
| Full detector, 240 rows × 10,800 frames, 3 trials | 93.303 | 10.766 | 8.67× |
| Complete point scoring, clean two-event traces, 375 evaluations × 7 trials | 0.227 | 0.194 | 1.17× |
| Complete point scoring, changing-tone traces, 125 evaluations × 7 trials | 0.262 | 0.257 | 1.02× |
| Quick point scoring, 750 evaluations × 7 trials | 0.0748 | 0.0752 | 0.99× |
| Constant-velocity frequency kernel, 16,777,216 predictions × 7 trials | 0.438 | 0.246 | 1.78× |

All six detector comparisons produced identical serialized detections and cap results. Paired detector
ratios ranged from 3.3–3.8× for 3,600 frames and 5.3–11.2× for 10,800 frames. Search equivalence checks
cover 2,178 trace/clock fits and 75 complete point evaluations, bit-for-bit. Changing-tone and quick
timings show no material improvement in these probes; the clean-pair result does not imply a uniform
search speedup. The direct frequency kernel is algebraically equivalent, with worst observed relative
error below 6e-15 against the previous formula in the numerical sweep.

Candidate deduplication now indexes large groups by spherical position and clock while retaining the
same 300 m / 5 s test, match sets, greedy representatives and ordering. The saved
`benchmark_dedupe_large_clouds` compared 16,000 synthetic candidates against the previous algorithm on
a shared AMD EPYC 9V74 host, using the pinned nightly and optimized standalone Rust: 300.2 ms to 27.3 ms
at 100 m spacing (1,370 retained), and 3.885 s to 62.2 ms at 1 km spacing (16,000 retained).
Both outputs matched exactly. These are single-trial deduplication measurements, not total search
timings; real-recording runtime has not been remeasured.

The independent curve match now has 221 default bank paths (13 speeds across 17 distances) and
279 received-frequency rows at 4 kHz input. The previous counts were 187 paths and 238 rows.
Supporting faster and higher received tones therefore adds bank work and retained image storage.
Its per-tone boundary handling reuses scratch buffers, adds each clipped coverage interval in constant
time, and keeps the original scalar normalization when every tone fits. Runtime and peak memory have
not been remeasured. Additional bank rows also change the tone-prior histogram, so field grades need
replay even though real and decoy searches share the scorer and the acceptance thresholds are unchanged.

Weather retrieval overlaps audio analysis. After audio analysis, the pipeline waits up to 190 s for weather:
the preferred source's 40 s request allowance plus the previous 150 s allowance for fallbacks. Usable hourly
surface weather or radiosonde profiles are retained as they arrive; updates do not restart that deadline.
If both fallback sources finish, their final combined profile is used. If retrieval times out or stops early,
the best received profile survives, with winds aloft preferred over surface-only data and warnings describing
what is missing. The 15 °C standard atmosphere is used only when no usable profile has arrived.
An unfinished fetch keeps `weather_fetch_s` unavailable even when a provisional profile is usable.

---

## Testing

### Complete archive ingestion (2026-11-07): native reader component verified

New regressions require both tar end blocks, surface errors reported after the marker, accept zero
padding and short reads across split boundaries, and reject nonzero trailing data. These archive-reader
regressions pass as part of 20 production build/ingest tests in a native component using the unchanged
modules, real `fdop-types`, pinned nightly and exact locked dependencies. The component also verifies
recognized-type precedence across joined days. It does not include the fetch or release-discovery
modules: their offline-cache, sidecar, contiguous-part, unfinished-upload and listing-refresh tests
still await the full data-crate build. This is not an end-to-end archive replay or workspace pass.

### Conservative trajectory bounds (2026-11-07): native validation pending

Added geometry regressions for between-knot Hermite excursions, signed ECEF coordinates, grazing
faces/edges/corners and malformed input. The exact-score regression adds 1,024 distant tracks to the
existing synthetic three-flight PCM fixture and compares all returned float bits, counts and serialized
pass metadata against the original unpruned path at three observer/clock combinations. It includes
unchanged silence and chance-calibration handling; it does not assert a speedup.

Independent JavaScript double-precision arithmetic checks covered 38,514 translated/grazing cases and
confirmed the fixture's distant tracks lie outside every tested observer's reach. These are auxiliary
geometry checks, not execution of the Rust tests. Native scoring equivalence, formatting, Clippy,
selftest and real-recording replays remain unrun because the executor became unavailable.

### Shared cache recovery (2026-11-07): native validation pending

New regression tests cover simultaneous downloads to one cache path, validating each writer's own
response, preserving the published file after transport or validation failures, concurrent byte-cache
writers, and repairing damaged Census ZIP/table contents before offline reuse. Network cases use
loopback HTTP servers and generated payloads; no live Census request is used.

The pinned-toolchain workspace build through RCH ended with exit 137 during dependency compilation.
A single-job retry was interrupted to prioritize focused checks, then the execution environment
became unavailable before any new test result could be observed. Untouched-head formatting already
reported 66 differences. These changes have source review, but no new native test, Clippy, formatting,
selftest or real-recording pass is claimed.

### Default clock tolerance (2026-11-07)

The `--clock-tol` default change (CHANGELOG) was checked through real RCH with the pinned nightly, on one
worker with two build jobs. These are focused checks, not a suite pass.

- **On `c5545ca` with the change:** the application's `cli::tests::` and `pipeline::tests::` passed, 31 of
  31, including `the_default_clock_tolerance_lets_the_curve_match_judge` and
  `a_curve_match_short_of_decoys_says_why_and_what_would_judge`. In `fdop-core`,
  `curvematch::tests::the_widest_judged_window_is_where_the_decoys_run_out` and
  `decoys_fill_from_the_side_the_catalogue_covers` passed. Workspace all-target Clippy passed with
  warnings denied. rustfmt reports nothing in the lines this change adds; it reports 10 differences elsewhere
  in `src/pipeline.rs` and `crates/fdop-core/src/curvematch.rs`, the same 10 as without the change.
- **On `e925900` with the change, all 39 `curvematch::tests::`:** 38 passed, including the new one, and
  `alternative_clock_peaks_recover_the_three_flight_recording` (added in `e925900`) **failed**. It found the
  true clock (error +0.000 s) and all three heard flights, but placed the observer 213 m from the truth, over
  its 200 m bound. This change does not touch that search, and the test was not re-run on the curve-match
  changes that followed `e925900`.
- **Not run:** the full core and workspace suites, `fdop selftest`, and any recording. No recording was
  re-run with the new default, so no change in accuracy is claimed.

### Recovery changes (2026-11-07): validation incomplete

**Fast passes in the independent curve matcher.** Five new regressions cover per-tone frequency
clipping, a simple independent coverage/score oracle, production-speed coarse votes, two crossing
215/263 m/s synthetic passes with 52 decoy windows and reversed/steady-tone controls, and a 300 Hz
emitted tone received near 1.2 kHz beyond the former image crop. Only these new PCM fixtures suppress
harmonics above the usable sample band; earlier fixtures remain unchanged.

Independent f32 arithmetic checks matched all 624 deterministic oracle cases. Another 2,000 weighted
cases matched coverage and winning tone, with a maximum score difference of about 3.8e-6. These are
arithmetic models, not Rust or PCM execution. Source review completed; native regressions, formatting,
workspace checks, release selftest and reference/field replays remain unrun while the executor is
unavailable. No new recording recovery or confidence result is claimed.

**Separated track-segment recovery.** Six new regressions preserve the original model bit for bit
while recovering either side of a coverage gap hidden by sparse false reports. Controls retain an
ambiguous denser winner, reject overlapping and unreachable alternatives, keep later split IDs stable,
and enforce the existing two-report minimum. An independent planar arithmetic model checked ten
cases and exposed why unlimited predecessor lookback would admit a mixed false path; the implemented
recovery keeps the original bounded predecessor search. Independent source review completed, but
native tests, formatting, release selftest and recording replays remain unrun during the outage.

**Fresh-day publication handoff.** Completed days now release their built point arrays after atomic
publication and retain the same opened cache inode for later selective loading. New regressions cover
three retained generations across cache replacement, concurrent writers reading their own published
files, and full-merge equivalence after every fresh cache pathname is replaced. The existing bounded
channel test now uses this handoff and reloads its published days. Independent source review found no
blocker; the new native tests, formatting and Clippy remain unrun after another executor disconnection.
No peak-RSS or runtime improvement has been measured.

**Cached-track memory and incremental ADS-B day publication at `a48f935` (before the fresh-day handoff).** All 31 offline component tests passed
using the complete, unchanged production `fetch.rs`, `ingest.rs`, `build.rs` and `geoid.rs` files, the
real `fdop-types` crate and the pinned parsing/compression/parallelism dependencies. The harness replaces
only HTTP, release discovery and download adapters with panic-on-use adapters; none were called. It
exercises the actual offline `fetch_window`, including streamed day publication, feed and cache-write
failures, full-versus-selected cross-day results, exact flight keys and points, metadata and geoid
preservation, partial feeds, malformed-cache recovery, and dateline/rounded-endpoint cases. These checks
cover all eight merge regressions as well.

The exact streaming reader passed all 24 `fdop-types` library tests. Focused formatting checks passed
for both changed data files. The full `cargo test -p fdop-data --lib --locked --offline` attempt stopped
when rustc compiling upstream `asupersync 0.6.2` was killed by SIGKILL, before `fdop-http` or `fdop-data`
compiled. The component results therefore do not establish full HTTP integration. The corrected
pipeline flight-ID assertions, workspace gates, release selftest and recording replays remain unrun.
No live-download runtime, peak-RSS reduction or overall speedup measurement is claimed.

**Checks before the final boundary correction (through `51d9bb0`).** Real RCH with the pinned nightly
ran six guided-ridge/background-restoration tests and the harmonic-family recovery PCM case together.
All three harmonic-family cases, five local-approach/search regressions, three coarse curve-stamping
regressions, four fast-bank tests and the STFT bitwise-reference test also passed. Core/shared-type
all-target Clippy passed with warnings denied, and workspace formatting passed. These used one build
job, two Rayon workers, optimization level 1 for workspace crates, unoptimized dependencies and no
debug information.

**Follow-up checks and the reported recovery failure.** On `697eb4a`, all seven guided-ridge tests
passed through real RCH, including the previously unrun basin-boundary regression. The focused
`a_one_sided_refusal_is_measured_from_a_free_trace_of_its_ridge_and_tones_are_not` test completed its
three original recovery cases, their physical accuracy checks, direct-seed equality checks and all
reversed-audio, steady-tone and noise controls. It then failed at the final fixture's obsolete
assertion that the guided-only trace still covered both sides.

That unchanged 250 Hz, 110 m/s, 900 m PCM fixture now has zero approach support and 61.11 s of recede
support on its first attempt. The existing retry correctly runs, and its result passes the quality
gates with `reseeded=true`. The retried fit's complete parameter values were not printed, so this run
does not establish its newly added accuracy assertions. The test now keeps that same PCM and seed,
requires a one-sided initial refusal and successful recovery, and applies the same 0.5% tone,
3% speed, 5% distance and 0.5 s timing bounds used by the original positive cases. A separate real
6 m/s PCM source checks accurate two-sided measurement, refusal specifically by the unchanged speed
gate, no reseeding, and identical complete results with retry enabled and disabled.

Those revised assertions and the new slow-source control received independent source review but
remain **unrun**. A subsequent core test build was killed by SIGKILL before the new clock-alternative
regression could execute, then the executor disconnected. Across the earlier focused and partial
broader runs, 128 distinct core tests passed; no complete current-core or workspace suite pass is
claimed. Production acceptance gates, the original PCM fixtures and accuracy tolerances were not
relaxed.

Before the basin-boundary correction, the actual `src/selftest.rs`, compiled unchanged through an external entry point
against fresh core libraries, passed the real decode/detect/measure/search chain: both intended flights,
2.365 m position error, 0.0249 s clock error and the existing 75 m radius. The complete application build
and workspace Clippy were killed by SIGKILL in upstream `asupersync`, including a retry with one compiler
frontend and backend job. The metadata/offline pipeline tests, full workspace gates, CLI release
selftest and reference-recording replay therefore remain unconfirmed; the external selftest does
not replace them.

**Clock-alternative execution on `e925900`.** All three selector regressions passed through real
RCH on the pinned nightly. The 400-second PCM case recovered the correct recording clock and all
three true flight identities, but **failed** its unchanged 200 m position assertion at about
213.6 m. Its known observer had higher raw acoustic scores for two flights but lower evidence:
the displaced point crossed the nearest-null bank boundary at about 858.53 m slant range.
For one pass the true point scored 158.404 / 9.641 nats, versus 147.723 / 15.230 nats at the
displaced point. The diagnostic rerun confirmed this; it is not a passing location regression.

The null-interpolation repair replaces that discontinuous lookup with interpolation of the fitted
quantiles. Model checks cover exact bank nodes, clamping, singleton axes, quantile arithmetic and
continuity at the former cell boundaries. Independent source review and JavaScript f32 arithmetic
checks passed. The original PCM, bank, frequency resolution and 200 m assertion remain unchanged.
The new Rust implementation, its model tests and the PCM recovery assertion remain **unrun**
after the executor disconnected. No corrected position or field-accuracy improvement is claimed.

The clock coverage guard tracks timestamps, not equivalence of physical pass reconstruction from
different seeds. More clock peaks can add work; progress logs expose candidate counts and initial
exact starts separately from refinement and diagnostic probes. Full workspace checks, formatting,
the CLI release selftest, field replays and runtime effects of the new changes remain unverified.

The grazing-range repair adds straight/turning sampled-track cases, an actual Hermite-path
fallback case, and a PCM test spanning several exact-clock seeds. The old coarse regression
executed and lost a valid 7,999.5 m pass at sampling phase 2, as predicted. The first 400-second
PCM fixture failed its aligned-seed audibility prerequisite at 3.055 nats, before checking the
other seeds. Its revised 900-second recording supplies more off-pass background for the empirical
null, with the same geometry, amplitude and physical assertions. That revision, the production
fix and the Hermite regression remain unrun; source review and independent geometry checks
passed. The final reach and heard thresholds are unchanged.

The historical-radiosonde repair adds lifetime-boundary checks and an offline cached-profile
regression, including a closed station with usable historical winds and a station opening at
the following sounding bracket. The provider date contract and production/parser paths were
reviewed independently; the new Rust regressions and formatting remain unrun.

The fast-edge extension adds 30-second PCM cases for early and late fast flyovers, through
survey, detection, refinement, measurement and independent-event selection, plus reversed,
steady-tone and noise/hum controls. The old implementation failed the early 215 m/s case:
all five fixed attenuations supplied no fast-edge candidate. Production source review checked
the reused speed ladder, coverage guard, residual mask, stage order and shared budgets. The
new implementation and recovery/control assertions remain unrun after the executor disconnected.

The US radiosonde-prefix repair adds an offline cached-profile regression for Alaska-style,
Hawaii-style, continental and legacy station identifiers. It requires both sounding brackets
and the expected interpolated wind and sound speed through the public weather fetch. The IEM
provider code confirms the identifier and country-metadata contract. The new Rust tests and
formatting did not run before the executor disconnected again.

The selected-audio coverage repair adds an offline known-location case with 1,300 seconds of
audio inside a longer video. A fixed publication clock makes the container request reach an
unpublished day while the audio fits its cached day. It also checks missing/nonfinite duration
metadata and the container-end tag convention. The two location cases were reported failing at
`missing A1DFEB`; those assertions compared direct-selftest names with canonical cached flight-leg IDs.
They now require `a1dfeb_20260908_0` and `ace391_20260908_0`, retaining both aircraft and all physical
accuracy checks. The corrected Rust/FFmpeg cases have not been rerun.
The region-clipping follow-up adds eight boundary cases covering both interval endpoints in four
directions; independent arithmetic checks support them, with Rust execution still unconfirmed.

The cross-track region-clipping repair adds two geometry regressions and a two-flight full-search
case where the observer's box lies between the 350 m samples. Independent source review and
JavaScript arithmetic checks cover the clipped ray and range bracketing, including terrain jumps.
A current-core Cargo build was killed in upstream `fsci-linalg` before compiling the core crate;
the Rust regressions, formatting and recording replays remain unrun after another executor outage.

The outside-region sampling repair adds a full-search regression with two close synthetic passes
at 250 and 263 m/s. Independent geometry checks confirm that every old ten-second probe misses
the padded reach boxes and that the normal cadence enters both. The actual Rust regression,
formatting and real-recording replays remain unrun because the executor is unavailable; source
review and arithmetic do not substitute for those checks.

The overlapping-store merge repair adds regressions for MLAT timestamp offsets, duplicate
boundaries, interleaved three-store reports, reintroduced timestamps and unchanged leg joins.
Source review and independent JavaScript models check representative selection and legacy
grouping; the Rust regressions, formatting, workspace gates and recording replays remain unrun
while the executor is unavailable.

**Earlier recovery checks.**

The integrated `fdop-types` crate passed all 22 library tests on the pinned nightly, including all seven
store tests for footer validation, trailing members, staging collisions and concurrent writers.
`cargo clippy -p fdop-types --all-targets --locked --offline -- -D warnings` also passed. These ran directly
with one build job and unoptimized profiles because RCH is unavailable in this environment.

The indexed-deduplication regressions and timing benchmark passed in an optimized standalone Rust
harness extracted from the actual production functions, candidate fields, geodesy and saved tests.
A focused `fdop-core` Cargo build then stopped in upstream `fsci-linalg` when rustc was killed by SIGKILL,
before compiling the core crate. The standalone checks do not replace the complete crate or pipeline tests.

The weather-deadline helpers and their repository tests now pass all 13 component checks, including
the seven new regressions for partial results and source completion, compiled with the pinned nightly
and the real shared types. Formatting of the weather and pipeline files passes. This component build
does not compile the whole data crate or exercise the network and CLI pipeline.

The complete audio module passes all eight tests in a standalone build against the pinned nightly
and real shared-type/serde dependencies: four ordinary tests and all four opt-in FFmpeg/FFprobe
integrations. The integrations cover channel selection, timestamp preservation, byte-for-byte
streamed-versus-buffered PCM and decoder failure reporting; formatting also passes. These checks
do not replace the workspace gates, release selftest or real-recording replays.

A fresh unoptimized Cargo build of the core crate also completed at `51d9bb0`. Its suite was stopped
after ten passes and three ignored tests to prioritize focused checks as shared resources filled;
the unfinished run is not a full-suite pass.

The harmonic-family, nearby-return and bounded-STFT patches were recovered through GitHub after
the outage; their focused regressions have now run as described above. These changes make no claim
of improved real-recording accuracy or calibrated confidence. See [CHANGELOG.md](CHANGELOG.md)
for the exact scope and validation record; results below belong to the earlier builds identified there.

### Previous checks (2026-11-06)

These cover `c1cfa61`, the budget cap on `main`'s tip. They ran on its code: a `git archive` of an unpushed commit with the
same code and older docs, built with the pinned `nightly-2026-08-31` toolchain and direct cargo on the benchmark machine, an
Apple M4 with 32 GiB.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo test --workspace --locked --no-fail-fast` | 234 passed, 0 failed, 2 ignored |
| `cargo build --release --locked -p franken_doppler_finder --bin fdop`, then `fdop selftest` | PASS: error 2.4 m, radius 75 m, clock error +0.02 s, 4 events |

The reference recording's five configurations and both feeder runs were replayed on this code (AGENTS, reference case).

### Correction checks (2026-11-05)

These results cover the cache-recovery, comparable-rival and curve-match corrections, merged with `32d4e1a` and the
geoid fix, on one exact tree (`a032bf8`, extracted from `git archive`), using the pinned `nightly-2026-08-31` toolchain.
They ran with direct cargo on the benchmark machine, an Apple M4 with 32 GiB, because `rch`'s clean-overlay sync failed
on two workers. They are the first complete compile and test run of those corrections: the host they were written on
ran out of memory and then disk.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo test --workspace --locked --no-fail-fast` | 229 passed, 0 failed, 2 ignored |
| `cargo build --release --locked -p franken_doppler_finder --bin fdop`, then `fdop selftest` | PASS: error 2.4 m, radius 75 m, clock error +0.02 s, 4 events |

The reference and field recordings have not been replayed on this tree. The historical results below do not validate
these corrections.

### Earlier recorded checks

These checks predate the comparable-rival, cache-recovery and curve-match corrections. They ran on the earlier probe
commit with `a827820`'s docs, each from a clean snapshot (`rch exec --base <sha> --clean-overlay --no-overlay`)
on a remote worker; they are historical evidence, not test results for the current tree.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings` | exit 0 |
| `cargo test --workspace --locked -j 2` | 179 passed, 0 failed, 2 ignored (the opt-in FFmpeg tests); the tree holds 181 `#[test]` items |
| `cargo run --release --locked -j 2 -- selftest` | PASS: 2.4 m error, 75 m radius, +0.02 s clock error, 4 events, both intended aircraft |

A selftest does not show that real recordings work: the archived replays are in [Honest status](#honest-status).

### Refinement and the margin

`refinement_reaches_rivals_hidden_by_earlier_gains` covers a third explanation hidden by the first two
candidates' refinement gains. Fine-grid scores of 30, 29.8 and 29.7 become 35, 30 and an unrefined 29.7.
The third candidate must receive an attempt: if it cannot be refined, the reported margin is 0.3 rather than
5 nats; if it rises to 36, it becomes the winner with its own position, clock and radius. This is a synthetic
comparison regression; it does not establish real-recording accuracy.

`margin_compares_the_leader_and_its_rival_at_the_same_polish` is metamorphic: a gain only one side could have cannot
move the margin, whatever the gain is. It also covers a refused rival, a refused leader, a rival that overtakes, a leader
only by refusal, and a rival that falls and yields to the next. Two mutants of the search code ran against the same tests on `42677a4`: crediting the leader alone fails it and `refinement_reranks_promoted_leaders_without_mixing_candidate_state` (88 passed, 2 failed), and taking the margin from the reported scores fails it at the refined leader's refused rival, `margin_nats` 2.5 for a gain of 0.5 (89 passed, 1 failed).
`refinement_survives_a_gate_bound_just_inside_its_minimum` tightens a bound to just inside the refined minimum and
requires the same observer within 10 m. `refinement_fits_the_clock_at_the_point_it_reports` starts seconds off the true
clock and requires under 60 m and under 0.5 s; the searches here could not tell the clock fitted at the refined point
from the grid centre's before it.

### Calibration validation at `a827820`

The final pinned-nightly run passes **97 core tests and 13 shared-type tests: 110 total**, with zero failures
or ignored tests, including both FFmpeg/FFprobe integration tests. It includes eight added regressions for
robust tone fitting, fractional prediction accuracy, close-pass refinement and independently checked profile
intervals. The command is
`cargo test -p fdop-core -p fdop-types --locked -- --include-ignored --test-threads=2`, using one build job,
two Rayon workers, workspace optimization level 1, the existing level 2 dependency profile and no debug info.
`cargo clippy -p fdop-core -p fdop-types --all-targets --locked -- -D warnings` and workspace formatting pass. That is the run this subsection reports; [Current checks](#current-checks) has the workspace-wide ones.

The unchanged 1,300 s `src/selftest.rs`, linked through the external entry point to fresh core libraries,
passes the full synthesis → FFmpeg decode → detection → measurement → search → refinement path with
**2.365 m** position error, **75 m** radius and **+0.0249 s** clock error, four events and both intended
aircraft. These agree with the preceding `b8c0819` core run; this fixture shows no position or clock gain
from the new changes. It does not verify the release CLI, ledger or report integration. Earlier workspace-wide
Clippy attempts stopped while checking the unchanged `asupersync 0.6.2` dependency with host out-of-memory
kills; full-workspace tests and the normal release CLI selftest remain unverified here.

### Direct tone and received-time profiles

The three new fit regressions use independent emitted-wave traces and independently optimized nuisance
fits to test interval membership. They cover two-sided and one-sided recordings, original physical bounds,
and timestamp shifts of ±10,000.25 s. The five existing fit tests also pass.

For the 80 Hz, 60 m/s, 8 km fixture in
`reported_tone_and_heard_time_ranges_profile_their_own_coordinates`, the old timing interval was
1022.542478–1024.586037 s; the corrected interval is 1018.326404–1028.824827 s. Independently refitted
clocks four seconds before and after the best time have costs 3.590266 and 3.529333, both below the unchanged
3.785475 threshold. The original generous timing interval excluded both; the corrected profile retains them.
The emitted-tone interval changes from 79.886360–80.068367 Hz to 79.499727–80.460123 Hz. Points 2% beyond
every corrected endpoint exceed the same threshold, so the change follows the fitted model's accepted region
rather than a larger uncertainty floor. This is conditional profile calibration, not empirical 95% coverage.

### Trace scoring and prediction interpolation

All **43 search tests pass**, including the existing exact trace-buffer oracle, tone-change regression,
candidate/refinement coherence, negative controls and the still-air/wind localization cases.

`robust_tone_recovers_doppler_curve_despite_masked_contaminants` puts stray emitted tones in 15% of a
synthetic Doppler trace. The original tone fit gives chi 7.596113 and a 0.372711 Hz tone error; the additional
robust candidate recovers the known tone and gives chi 3.75 under the unchanged residual cap. The core-fit
limit remains chi 4. Negative controls retain wrong-curve rejection, tone bounds and missing-data penalties;
raw RMS continues to expose the contaminated samples.

`ratio_grid_interpolation_resolves_close_passes_between_samples` checks fractional queries against an
independent retarded-time/radial-velocity oracle, across six speed/range/tone combinations, three sample
phases, two timestamp origins and both prediction-grid spacings. Maximum frequency error falls from
3.352338 to 0.228055 Hz on the 0.2 s full grid, and from 20.252859 to 6.355502 Hz on the 0.5 s quick grid.
Support and local-extremum tests check the fallback beside gaps, at boundaries and around reversals.
`close_pass_refinement_is_stable_across_prediction_grid_phases` follows two close, fast passes through the
actual position/clock refinement; its three phases recover the synthetic observer within 0.212 m while
retaining the 75 m radius floor. Its linear-interpolation baseline has the same maximum position error:
that test checks integration stability, while the fractional-query test establishes the frequency improvement.
These are conditional numerical calibration cases, not field accuracy or
empirical confidence coverage. The first reference and wrong-date replays of these changes are in the [CHANGELOG](CHANGELOG.md), paired with the build before them: on 09-07 they raised a wrong date's margin from 2.18 to 2.95, and right-date margins fell.

### Prior spatial and frequency calibration (`b8c0819`)

The combined pinned-nightly run passes **88 core tests and 13 shared-type tests: 101 total**, with
zero failures or ignored tests. This includes the two FFmpeg/FFprobe integration tests. The new coverage
checks missing-data information counts, weighted recording clocks, grid-phase-independent position
refinement, rotated positional ambiguity, consistent reported evidence, spectral curvature, background
normalization and actual audio range recovery. The commands use one build job, two Rayon workers,
optimization level 1 for workspace crates, the repository's level 2 dependency profile, and no debug info.
Core/shared-type Clippy passes for all targets with warnings denied, and workspace formatting passes.
The combined test command was
`cargo test -p fdop-core -p fdop-types --locked -- --include-ignored --test-threads=2`.

`subgrid_polish_recovers_analytic_observers_across_grid_phases` covers nine independent observer cases
with a maximum grid-only error of 68.809 m and a maximum polished error of 0.265 m.
`curved_integration_limits_distance_bias_across_audio_phases` covers six physical emission-phase audio
cases meeting the unchanged 0.5% closest-distance,
0.5 m/s speed and 0.1 s CPA-time bounds; their maximum errors are 2.339 m (0.2924%), 0.0270 m/s and
0.00721 s. These isolate numerical and measurement errors under known synthetic assumptions.

The unchanged `src/selftest.rs`, linked through an external entry point to fresh core libraries, also
passes the complete 1,300 s synthesis → FFmpeg decode → detection → measurement → search → refinement
path: **2.365 m** position error, **75 m** radius, **+0.0249 s** clock error, four events, and both intended
aircraft. The archived prior run below reported 0.417 m position error and +0.192 s clock error; the current
run improves its clock error but has a larger position error on that one fixture. The grid-phase and audio
calibrations are not a claim that every recording improves. This entry-point run exercises the actual core
pipeline, without substituting for a full CLI release build or validating ledger/report integration.

### Prior detector, search and propagation validation (`b26c059`)

The detector oracle compares every score bit and template index across edges, partial residual masks,
duplicate templates and one versus three workers. Trace-buffer reuse is checked against golden outputs
from the allocating Rust implementation, including alternating trace sizes, outliers, tone changes,
missing coverage and early returns.

The constant-velocity frequency regressions compare 53,970 cases against retarded-time/radial-velocity
reconstruction and 675 independent emitted-pulse pairs against their received spacing. Moving-medium
tests check the advected wavefront with an independent bisection solver, one-dimensional pulse speeds,
the reception-time derivative, rotation invariance, near-sonic cancellation, and 72 cold/warm emission
solves. Invalid media cannot return an emission-time match. These checks establish the model's numerical
consistency. The reference recording and its wrong dates have since been replayed with the corrected wind model
(`*_r5`, [Honest status](#honest-status)), and the field rows finished so far are in the [CHANGELOG](CHANGELOG.md).

The pinned-nightly core suite passes **70 tests**, with zero failures and two opt-in media tests
ignored; all **13 shared-type tests** also pass. Core/shared-type Clippy passes for all targets with warnings denied, and workspace formatting
passes. The full workspace test build was attempted with two jobs,
then one job and `profile.dev.package.asupersync.opt-level=0` / `debug=0`; both were killed by SIGKILL
while compiling the unchanged `asupersync 0.6.2` dependency on the shared host. Those workspace attempts
did not reach test execution.

The unchanged `src/selftest.rs`, linked through an external launcher to a fresh optimized build of
the current core, passes its full 1,300 s synthesis → FFmpeg decode → detection → measurement → search
→ refinement path: **0.417 m** position error, **75 m** radius, **+0.192 s** clock error, four events,
and both intended aircraft (`ACE391`, `A1DFEB`). This verifies the synthetic core pipeline; it does
not replace a full CLI release build or exercise ledger/reporting integration.

### Earlier validation

The ridge tests send synthetic passes through `measure()`. Eight passes with chirps up to 76 Hz/s keep more than 40
% of their drop and are accepted; the same curves with a silent crossing, at two tone strengths, fall under the 15 %
line and are refused. A pass that detection rates half as steep as it is still gets followed, and a full
detect-then-measure run follows a 100 Hz, 140 m/s, 300 m pass. Each of nine mutations of the new code fails at least
one of these tests. These are synthetic fixtures: real ridges steeper than 4 Hz/s exist in the recordings, but no
event in them follows one.

The actual-audio overlap regressions recover two same-tone, simultaneous passes at 1 km and 7 km,
including when the nearer aircraft is louder. Both measured curves satisfy the existing known-answer
tolerances: CPA within 0.3 s, tone within 0.3 Hz, speed within 4 m/s and distance within 8%. Each aircraft
alone still counts as one independent event after measured-curve filtering. The guided-peak tests check
qualified local maxima, below-threshold neighbouring noise, a strong shoulder outside the model band and
ripple on a stronger ridge; the detection-cap test also verifies weak-only recordings retain their detections. These are synthetic
fixtures, not evidence of reference-recording accuracy.

The dense-burst track regression failed on the previous cleaner: 49 corrupt reports removed half of a
160-sample genuine flight despite a permitted 21 s coverage gap. The corrected cleaner passes all five
track tests, retaining the entire genuine trajectory with 2, 49 or 97 corrupt reports and interpolating
through that gap within 1 m of the synthetic truth. This is synthetic evidence, not a real-recording replay.

Both margin-completeness tests also pass. The adversarial regression uses the production evidence function:
a partially evaluated rival trails by 8.79 nats, but its two unchecked confirming events reverse the winner
and leave only a 0.0245-nat separation. The old gate incorrectly certified that incomplete numerical margin.
The existing 3/6-nat boundaries still apply when no comparison cut is recorded.

Both actual media integration tests pass. The timestamp regression failed on the previous decoder,
which returned 4 s of PCM from a 6 s timeline with a 2 s packet gap. The corrected decoder preserves the
gap, four seek intervals (including one wholly inside the gap), a 123 s container origin, audio delayed
behind video and a 16 s MPEG-TS gap. Surviving unresampled PCM samples match a continuous reference exactly.
The first-audio-stream regression also remains green. These fixtures are synthesized with FFmpeg.

The per-pass aircraft regressions cover a continuous circuit track, duplicate tones and a silent close
return. With only two search drivers, a third event from the returning aircraft is retained as a core
confirmer; the synthetic observer is recovered within 60 m and the clock within 1 s. Duplicate matches
from one pass remain excluded in still air and wind. A closer return with no detection counts against
the candidate, while the original explained passes and a detected return do not.

Together, the targeted and integrated runs pass all **49 default `fdop-core` tests**, all **13
`fdop-types` tests** and **both opt-in media integration tests**: 64 test cases, with no failures.
The long same-tone audio and detection-cap regressions ran separately; the remaining tests then ran
together, including the actual FFmpeg/FFprobe checks. Coverage includes close-pass geometry, independent
overlapping flyovers, harmonic grouping, returning aircraft, incomplete numerical margins, confidence-grid
expansion, hillside coverage, bounded hypothesis accumulation and clock-pruning recovery. Core and
shared-type Clippy passed for all targets with warnings denied, and workspace formatting passed. These
checks use the pinned nightly, one build job, two Rayon workers and unoptimised dependencies with debug
information disabled to fit the shared host's memory limit.

The unchanged `src/selftest.rs` was rebuilt and rerun against these final core libraries: **0.4 m**
synthetic position error, **75 m** radius, **+0.20 s** clock error and both intended aircraft matched. A small external entry
point invoked `selftest::run` directly, exercising the real FFmpeg decode, detection, measurement, search
and refinement on the full 1,300 s generated recording. This run covers the core path.

Eight tests of the exact clock helpers and time module also passed in an earlier isolated Rust build.
The three new HTTP byte-extent tests passed in an isolated build of the exact production helpers, and
standalone Clippy passed with warnings denied. Three loopback regressions for successive capped ranges,
ignored-Range recovery and range overruns remain unrun in the current workspace. The recovered HTTP source
matches its pre-disconnection Git blob byte-for-byte.
The margin-policy regression also passed earlier in an isolated Rust build of the production helper and
data fields using the pinned `serde_json` dependency. Formatting of both newly changed Rust files passed,
and targeted UBS found no critical issues; the combined core integration is covered by the current core tests above.
The weather-deadline scheduling regression passed in an isolated Rust build of the exact test and constants;
it fails with the former 150 s wait. The full workspace test build was attempted with the same memory
settings, but rustc received SIGKILL while compiling `asupersync 0.6.2`, before the tests ran. A subsequent
workspace-wide all-target Clippy attempt also received SIGKILL while checking `asupersync 0.5.0` metadata,
before reaching the project crates.

Those builds have since completed on remote workers. On `7e5cf3e`, whose code is that of `4b88d7c`, `cargo test
--workspace --locked -j 2` passed 139 tests with 0 failed and 2 ignored, and `cargo clippy --workspace --all-targets
--locked -j 2 -- -D warnings` passed. A release build of the same code passed `fdop selftest`: 0.4 m error, 75 m
radius, +0.20 s clock error and 4 events. A selftest does not show that real recordings work; the reference and
field reruns are in [Honest status](#honest-status).

```bash
cargo test --workspace --locked                               # unit tests, no network
cargo test -p fdop-core audio::tests -- --ignored              # media integration, needs ffmpeg/ffprobe
cargo run --release -- selftest                               # end-to-end known answer, needs ffmpeg
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings  # warnings are errors
```

The project has no GitHub Actions CI. The maintainer runs these four checks through
[dsr](https://github.com/Dicklesworthstone/doodlestein_self_releaser) (`dsr quality franken_doppler_finder`)
before a push or a release.

The unit tests check:
- emission-time physics against the light-time equation;
- detection: it finds a pass and ignores a flat tone, and noise stays below threshold;
- detect → measure recovery of `f0`, `v`, `d` and `t0r`;
- same-tone overlapping aircraft survive actual detection, refinement and measurement in both tested
  strength cases; one-aircraft controls still supply one independent event, and guided peak selection
  rejects unqualified noise, non-peak shoulders and ripple on a stronger ridge;
- deferred fit diagnostics match eager refinement, and failed refinement keeps its trace and fit consistent;
- fit recovery and range coverage;
- physical emission-phase audio at six geometry/phase combinations, recovering closest distance within
  0.5%, speed within 0.5 m/s and heard CPA time within 0.1 s; analytic curved ridges and normalization
  slope/offset invariance, with the original peak-rejection controls still enforced;
- gap-aware effective information, including duplicate/density invariance and unchanged support across
  longer silent gaps; a weighted shared-clock oracle and a noisy third-pass regression with nonnegative
  confidence deltas;
- independent analytic observers at nine grid-spacing/phase combinations, narrow contours requiring
  resampling, rotated parallel-flight ambiguity, and fixed-clock regeneration of reported fits and vetoes;
- Hermite track models against closed forms;
- sparse turning tracks, including unequal sample spacing and heading wraparound, against an analytic circle;
- a turn cannot excuse a wrong reported heading, speed or vertical rate, and a turn starting or ending between
  samples keeps its correct reports;
- a two-flyover search, with and without wind, that locates the observer within 60 m and the clock within 1 s;
- two close flyovers whose distance gates fall between cross-track grid lines, recovering the synthetic
  observer within 60 m and the recording clock within 1 s;
- simultaneous independent flyovers with harmonic tone ratios, and true harmonic families in either
  strength order, including unequal trace sampling, noise, fit offsets and limited overlap;
- a stronger unmatched flyover does not prevent two archived flyovers from seeding a search with two driver slots;
- a returning aircraft supplies a third confirming event with only two driver slots, while duplicate
  matches of one pass are rejected and an unheard, closer return is still counted;
- hillside observers are retained after a valley leaves the slant-distance gate;
- rejected grid points outside the requested region cannot hide valid interior hypotheses, including a
  shared central point and observers on either side of the flight;
- single-event survivors and a long queue of clock-incompatible candidates do not hide a valid pair;
- absent or numerical margins cannot certify confidence after recorded comparison cuts, including a
  rival whose untested confirming events reverse the apparent winner;
- hypothesis storage respects small-host budgets while preserving exact counts and chance samples;
- junk positions inside an ADS-B leg are dropped before track models are built, including dense bursts
  between genuine sections whose elapsed gap remains within the configured limit;
- a point without GNSS height that GNSS-height points bracket within 60 s is dropped, and on a sustained
  3°/s turn that costs less than the noise it removes up to 60 s and more by 90 s;
- the curve match finds three synthetic flights' observer and clock, beats every decoy there and not in noise
  alone, gives a decoy window the same search as the real one, lets one audio event credit one flight only,
  and moves only its clock when every flight moves in time;
- one heard flight's curve scores the same at the observer and along its track with the clock moved to match, a
  crossing flight breaks that tie and a parallel one, the same way at the same speed, does not; a curve match on
  fewer than two heard passes does not stand;
- the curve match's refinement, started 600 m from the synthetic observer, ends within 200 m of it with at
  least the observer's own evidence, which one 5 × 5 move per scale does not;
- the curve match takes a long heard range when the synthetic flights pass 2.8 to 4.6 km out, and beats the
  decoys there by more than one fixed 2 km range does; no default range expects to hear much past the reach;
- a steep pass keeps its ridge through the CPA, a fit with no ridge there is refused, and the ratio-grid time
  sweep matches independent solves, including over a hole in the samples and past both ends of a track;
- each weather fallback reports its warnings and whether winds aloft are modelled;
- every lesser ADS-B feedset a day can fall back to produces a warning;
- trailing UTC days that adsb.lol has not published yet are recognised;
- the store format, ADS-B parsing and leg building, weather payloads, terrain decoding, region parsing,
  gazetteer and geocoder payloads, reports and maps.

Developer examples (`cargo run --release -p <package> --example <name> -- ...`; the root package is
`franken_doppler_finder`):

| Example | Crate | What it does |
|---|---|---|
| `detect_file <media> [sr]` | fdop-core | Detection and measurement with timings; optional survey and event PNGs |
| `explain <run_dir> <stores> <lat> <lon> <t_start> <event:icao>...` | franken_doppler_finder | Scores named (event, flight) pairs at a point; residuals over a grid of sound speeds. `t_start` is unix seconds; `stores` is a comma-separated list of `.fdtrk` files |
| `truth <run_dir> <stores> <lat> <lon> <t_start> [elev_m] [max_km] [dump=<icao>]` | franken_doppler_finder | For a recording whose place and clock are known: lists the archived passes near that place while it recorded and pairs them with the detected events. `t_start` is ISO 8601 or unix seconds. Without `events.json` in `run_dir` it lists the 24 h after `t_start`, which helps pick test windows. `dump=<icao>` also prints that aircraft's archived points near the place and each track model's closest approach, which is what the unheard-pass gate sees |
| `probe [MiB]` | fdop-http | Streams part of an adsb.lol archive and reports throughput |
| `ingest_ny [icao_prefix...]`, `check_oracle`, `store_stats <file.fdtrk>` | fdop-data | Reference-case ingest (New York, 2026-09-08 to 2026-09-10, about 12 GB of download), oracle position checks, store summaries |
| `live_check <out_dir>` | fdop-geo | Live region parsing, geocoding, map and report rendering |

---

## Protecting your own recordings

The video's lesson is that this leak is easy to miss: the curves are usually inaudible. Before you publish
audio or video recorded somewhere sensitive:

1. **Audit it:** `fdop detect your_file.mp4`. Every line marked `ok` is a flyover an analyst can measure. Two or
   more, with the time between them intact, is high risk. One, plus a known recording time, is also high
   risk.
2. **What the source video found does not work:**
   - high-pass filtering (the 80–500 Hz band overlaps speech, and the curve came back after a boost). `fdop detect`
     warns on such audio and may list fewer flyovers, but a lower count there does not make the file safe;
   - talking over it;
   - phone-call compression and noise suppression;
   - uniformly pitch-shifting or time-stretching the whole file. This last one is derived, not tested:
     `f0` is a free parameter, so `v` and `d` survive a uniform shift.
3. **What works:**
   - cutting out the minutes around each flyover, harmonics included;
   - cutting or reordering the stretches between flyovers so their time gaps mean nothing;
   - spectral noise reduction tuned on the curve.

   Then run `fdop detect` again on the **final exported file**.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `ffprobe not runnable (...); install ffmpeg` (or `ffmpeg not runnable`) | ffmpeg is not on `PATH` | Install ffmpeg; check with `fdop doctor` |
| Audio is audible in the player but the survey is silent, or the selected track contains commentary | The first track or its mono mix lacks the useful ambient sound; opposite-polarity stereo can cancel | Select the ambient track with `--audio-stream N`, or a microphone channel with `--audio-channel N`; both indices start at 0 |
| `GitHub API refused ... limited to 60/hour, set GITHUB_TOKEN` | Unauthenticated GitHub API quota | `export GITHUB_TOKEN=$(gh auth token)` or any read-only token |
| `no location: no location found: N usable flyover events; need at least two clear propeller flyovers` | Fewer than two usable passes | Look at `survey.png` for faint falling curves the detector missed. One flyover cannot be located; a longer recording may hold more |
| `no location: ...; no ADS-B flight pair explains two events at one place and clock (...)` | Wrong date or region, or the aircraft had no ADS-B | Check `--dates` (local dates) and widen `--near`; try `--ignore-metadata-time` if the file's `creation_time` is wrong |
| `ambiguous location: 'Springfield' matches places in K states` | Bare place name | Add the state: `"Springfield, IL"` |
| `date range ... spans more than 15 days; narrow it` | Range too long | Split it into several runs |
| `unrecognised date '09/08/2026': put the four-digit year first` | The year is not first. `09/08/2026` is 8 September in the US and 9 August in most other countries | Write it year first: `2026-09-08` |
| Warning `detection kept the 60 strongest of N candidate curves` or `measured the K strongest of N flyover detections, reaching the limit of 24 independent aircraft events` | A long or busy recording hit a fixed cap (see [Known issues](#known-issues)) | Nothing to change: the remaining flyovers are not used |
| Warning `the audio has little energy below 100 Hz (45-95 Hz is N dB below 150-600 Hz)` | A high-pass filter does this, removing the fundamentals of slower propellers and weakening those near the cut. Speech-heavy audio can also read low | Use the unfiltered recording if you have one. Otherwise expect fewer usable flyovers, and do not read a failed search as evidence about the place |
| Warning `the answer lies in CT, outside the searched state NY` (or `appears to lie in`, when the state is a gazetteer estimate) | A state search covers the state's bounding box, which takes in parts of its neighbours | If you know the recording was made in the searched state, distrust the answer whatever its grade, unless its radius reaches back across the border. A narrower `--near`, such as a county, takes in less of the neighbours |
| `needs network or a cached file: Census gazetteer ...` | No usable cached table: it is absent, unreadable, malformed or empty | Run once without `--offline` to fetch and validate a replacement, or use a state, point or box region |
| `ADS-B fetch: ...` | Network failure or a missing archive day | Rerun; finished days are cached. A day missing from adsb.lol cannot be searched |
| A download pauses for a minute, then carries on | The server stopped sending with the connection still open. After 60 s with no data, `fdop` reconnects and resumes with an HTTP `Range` request | Nothing. Resumed `206` responses are checked for their byte offset, range boundary and total length before new bytes are accepted. Capped ranges continue until the full object is proved complete; a range's body length is never treated as the archive length. After repeated failures with no new bytes it stops |
| Warning `the ADS-B archive for UTC day(s) ... is not published yet ...: only recording starts up to ... were searched` | The local date runs into a UTC day that adsb.lol has not published yet. Each day appears some hours after it ends | Rerun after the release to search the rest of the date. A recording that ended before the cut was searched in full. With no published part at all, `locate` stops with `no published day covers ...: rerun later` |
| Answer graded **low** | One or more gates failed | Read the gates in `report.html`. A margin marked `provisional, comparison truncated` failed because a comparison was cut: an unchecked rival could still win, so the answer is unconfirmed, not refuted. Otherwise, a small margin means other flights explain the curves almost as well, possibly at nearly the same place: `margin.next_distinct_km` in `result.json` says how far away |
| Margin gate reads `both at their fine-grid cells, because one could not be refined` | Refining the answer or its best rival moved a locating match out of its gates, so the margin compares the two scores from before refinement. A warning gives the refused position | Nothing to change. `margin_top_score` and `margin_rival_score` in `result.json` hold the pair compared; `top_score` is still the answer's refined score |
| An event reads `trace follows only N % of the central frequency drop: no ridge through the CPA` | The fit joins two steady tones with nothing between them, or the pass is too faint to trace through its steepest part | Nothing to change: the event can neither drive nor confirm an answer, though it still counts as heard |
| A search runs for an hour or more | A busy recording and a wide region: every rival is compared, at 50–70 ms of CPU per fine cell, and `fine grid N/M` on stderr shows how far it has got | Narrow `--near` to a county, a point or a box |
| Warning `joined N of M event pairs before the quick-check budget (...)`, `N quick-check survivors > 2000000; scoring the best 2000000` or `confirmation budget: checking 2000000 of N fine candidates; rival comparison is incomplete` | The search outgrew a comparison budget | The margin gate fails and the answer grades **low**; a numerical margin is labelled provisional. Narrow the region or the dates and rerun |
| Warning `ADS-B <day>: no mlatonly release, so aircraft located only by multilateration ... are missing` (or `prod release missing, using staging`) | adsb.lol did not publish that release for the day | Rerun later if the day is recent. Aircraft missing from the data can neither match nor count as unheard passes |
| Disk filling up | Many cached regions and days | Delete old `adsb/<date>/` folders under the cache root |

---

## Limitations

- **The US in practice.** Region parsing, place names, ZIP codes, time zones, radiosondes and the geoid are
  US-specific. ADS-B coverage is global, so a `lat,lon` or box region elsewhere runs, but the place report and
  time-zone handling assume US data.
- **At least two propeller flyovers**, each heard on both sides of its closest approach, from aircraft that
  broadcast ADS-B or were seen by MLAT. Aircraft without transponders, or outside adsb.lol's feeder coverage,
  cannot be matched.
- **A stationary recorder, and simple passes.**
  - Detection and fitting assume a straight, constant-speed, constant-pitch pass. A likely manoeuvre raises
    a warning and widens the ranges. A model misfit, or approach and recede halves that disagree, also
    halves the event's weight in the search. In practice a fit is a model misfit when its RMS is over 0.3 Hz:
    the test divides by the trace's local scatter, which is 0 on most traces, so its 0.03 Hz floor decides.
    That flagged 92 of the 123 events that passed the quality gates on six of the recordings the development
    rows and nulls use, and 62 of 92 on set D.
  - The search follows each aircraft's real ADS-B path. For misfit events it also allows one change of tone,
    such as an RPM or power change.
- **Detection envelope:** 35–1100 Hz and 250 m–12 km, with the decode at 4 kHz. The primary speed bank
  spans 40–175 m/s; a final strong-only bank adds faster subsonic templates, about 194–263 m/s at the
  default sound speed. Earlier detections can still mask a faster curve before that final bank runs.
- **Atmosphere:** a straight-ray, height-averaged speed of sound with horizontal wind only. No refraction.
- **Data volume:** each new UTC day costs about 4 GB of download, and adsb.lol archives cannot be fetched
  partially. adsb.lol has gaps on some days.
- **The HTML report's map needs internet** to load Leaflet and tiles. `map.png` is self-contained.
- **High grades at one site only.** The reference recording, and a public osprey cam at the same
  site on a different day with different flights, have both produced high grades. The reference recording's
  grade depends on the search configuration; see the archived results in [Honest status](#honest-status).
  Eight further field recordings were graded low or gave no location on
  those earlier builds: two from the same cam on other days, four from sites with sparse light-aircraft
  traffic, and two from busier sites with poor audio (a high-pass filter under road noise, and a
  wind-saturated microphone). One of those, at Duke Farms, NJ, was low but a real match, in the right township with its clock
  within 6.4 s of the stream's. One later build graded the bird-feeder answer medium, and it was wrong. See [Honest status](#honest-status).

### Known issues

- Jets are excluded when the ADS-B archive is ingested, so they can neither explain an event nor count as an
  unheard close pass. There is no option to include them.
- The curve match stands only on two heard passes, because one pass's curve lets place and clock
  trade along its track: moving the observer `v·dt` along it and the clock by `dt` fits as well. Two heard passes
  flying the same way at one speed leave the same trade: two arrivals on one approach, or one aircraft flying the
  same lap twice, since it counts passes, not aircraft. The rule does not catch that case. The tied-place radius
  catches it only where the other places along the track fall inside the clock window and among the kept modes.
- The curve match can judge only a clock window of 300 s or less: its 40 decoy windows of the same width,
  each the width plus 60 s apart, must fit within about ±2 h (`curvematch::max_judged_window_s`). With
  `--start` that is a `--clock-tol` of 150 s or less. A `creation_time` window spans the recording's length
  plus the tolerance either side, so at the default it is too wide for any clip longer than a minute. Up to
  30 min the curve match still runs there, as a diagnostic that can neither stand nor change a grade, and a
  warning says so.
  Drawing decoys from further away would bring in hours with other traffic, and the decoy test's rate of
  about 1 in 41 has not been checked against that.
- Detection and measurement are capped: at most 60 candidate curves; flyovers measured strongest first, from the
  silence line (detection score 3.5) up, until 24 independent aircraft events survive; and 12 of those driving the pair join
  (`--max-events`). Before this change only primaries scoring 4.5 or more were measured, and only the strongest 24
  of them, so a refused detection used up a place. The reference recording hits the detection cap (82 curves;
  before this change also the measurement cap, 32 strong flyovers), and a 3 h recording loses most of its
  flyovers. Each cut is a report warning.
  Raising the caps does not simply help. A working-tree build that scaled them with duration (180 curves, 72
  measured) was tried on a 3 h osprey-cam recording. Its answer went from wrong and **low** to wrong and
  **medium**, 103 km off, and rested on two weak events that the old cap never measured. The medium grade does
  not allow for how many events were searched. Measurement now goes further down the list only because a medium
  answer resting on an event past the old line needs the curve match to have judged it (Grades above). The
  detection cap and the 24 are unchanged. On one recording, measuring all
  detections instead of the strongest 24 moved a wrong low answer (365 km) to the right place (0.61 km, still low):
  `f_osprey_1102b_s7b` against a throwaway build, `f_osprey_1102b_cap60`. Its missing event was a 568 m pass at
  detection rank 45. On the reference recording the same build keeps all three answers and grades (margins 12.77 to
  11.06, 5.01 to 4.72 and 2.92 to 1.91, the last against a nearer rival, 52 km instead of 138 km), and both wrong
  dates stay low (2.37 to 0.25 and 5.49 to 1.89), for 43–85 % more time and up to 52 % more memory. Its one new usable
  event there joins the best rival on all five runs; no archived flight fits it out to 40 km, and whether it is a
  real sound is unresolved. The other three wrong-date runs and the other field recordings have not finished on it.
  With the benchmark's R2 inputs, the change that landed grades that osprey-cam recording high, 220 m off (`fm`).
  Explicitly truncated
  comparisons now fail the margin gate whether a rival was found or not; that correction does not calibrate
  the larger event search. Events omitted from pair seeding by driver or hypothesis limits can still leave
  unexplored explanations, even when no pair/fine/confirmation cut was recorded.
  Earlier builds also spent slots on events the ridge check refused: 19 of the 24 on that recording.
  Refusals, duplicate measured curves and fits outside the aircraft limits now leave room for later detections;
  the independent-event limit and the measurement-line gate remain in force.
- **A complete comparison can take hours.** The fine stage costs 50–70 ms of CPU per cell, and real searches run
  from tens of cells to about 900,000. A busy 3 h recording searched across a state took 9,602 s and 45,474
  CPU-seconds on an Apple M4 with 32 GiB while another replay ran, against 584 s when the fine stage stopped at
  4,000 cells. Its margin barely moved (23.29 to 24.93 nats). Narrow the region; the county reference search needs
  under a minute.
- **Margin is not calibrated to the size of the search, and does not separate right from wrong on these runs.** On the
  current code the reference recording's right answers have 8.44–14.78 nats and its wrong dates 0.15 and 2.00, with the
  feeder's wrong answers at 0.19 and 0.99 (`*_gc`). Before measurement went past the old line they had 8.12–16.00,
  0.35 and 0.60, and 0.76 and 2.17 (`*_b1`). The build before the knot rule gave 7.21–17.85 against 1.07 and
  2.95, the last 0.05 under the medium line, and 1.81 for the feeder (`*_r5`). That gap is on the recording every build
  was developed on, and it moved by several nats between builds, so it is not a separation. On `7e5cf3e` the
  true three-day reference answer has 2.92 nats and a false field answer 2.89, both low. The true state-wide
  answer's 5.01 (medium) is below a wrong-date run's 5.49 (low, on one core match of two), and another wrong-date
  run with both core matches was 0.63 nats short of medium. A wider search finds more chance alignments, so widening
  it can lower a right answer's margin. A small margin can also mean other flights at the same place, because a
  shared event matched to another flight counts as a different explanation at any distance: one field run has 0.11
  nats against a rival 140 m away. A wrong answer can pass the core gate, leaving margin alone to keep it low: three
  runs on `7e5cf3e` (2.89, 2.37 and 0.47 nats), three on `cb95fbb`: the 09-07 wrong date (2.95), the feeder (1.81)
  and the osprey cam on 11-02 (0.16; `n_ref_on_0907_r5`, `f_feederwatch_r5`, `f_osprey_1102b_r5`), and with the knot
  rule the first two again (0.61 and 0.75; `n_ref_on_0907_c1`, `f_feederwatch_c1`). The other way round, on `cb95fbb` a
  wrong date on the eagle cam cleared the medium line at 4.79 nats and was low only on its 1 core match of 4
  (`n_duke_on_1102_r5`); with the knot rule it is low at 1.36 nats with no core match of 5 (`n_duke_on_1102_c2`).
  And no rival passes the gate, however small the search: one county search scored 20 fine
  cells, found no rival, and is low only on its other gates. A large margin means nothing when the core gate fails:
  the eagle cam's county search has 26.83 nats with 1 core match of 7, and is low (`f_duke_county_r5`).
- **Trace evidence past what the chance null measures is extrapolated.** Below the null's 5 % quantile `trace_nats`
  follows a log-normal tail fitted to two quantiles, capped at 25 nats, where an empirical null of n samples supports
  at most ln(n+1), about 6. On the `*_r5` runs one reference event earns 2.3–8.3 nats past that bound on right and
  wrong dates alike, and rivals carry it too: 10.6 for the feeder's. Capping it at ln(n+1) in the stored candidates
  (arithmetic on `search.json`, not a rerun) moves margins both ways, by up to 3.7 nats. The stored scores mix refined
  points and fine cells, and refinement moved scores by 0.15–1.21 nats on these runs, so each capped margin is
  uncertain by about that much: county about 14 and state-wide 7.2–8.4, both high; three-day 5.8–7.0, either side of
  the high line; 09-07 0.8–3.2 and 09-09 at most 0.2, both with new leaders; the feeder's wrong answer 2.0–3.2, either
  side of the medium line. So the arithmetic cannot say what a cap does to grades; that needs replays.
- **The ridge check rests on limited evidence.** The 15 % line was picked with these recordings in view; synthetic
  tests justify its two sides, and the replays can only show harm. Real ridges steeper than 4 Hz/s exist in the
  recordings, but no event follows one, so that side is synthetic only. A faint steep pass can fall under the line
  (amplitude 0.012 at 20 Hz/s and up, in 6 of 81 synthetic rows), as can a pass whose detection is 4 times too
  gentle. At 8 times most are lost, and 3 of 108 synthetic rows with a detection 4–8 times too gentle were accepted
  with a wrong fit (a 120 m/s pass at 1,500 m read 69 m/s at 576 m). Tones spaced under 2 Hz can still pass for a
  ridge, and a tone that keeps falling and jumping back earns each fall, though no real event has read much above a
  full drop (1.02 of 1). On the reference recording it left 4 usable events of 24 on `7e5cf3e`, and 3 on the current
  code (`r_ny_r5`). One it refuses, at 13 %, was a confirmer in a correct county answer; whether that event was a real
  pass is not settled.
- Measurement took 205 s of a 509 s run on a 3 h recording. Full uncertainty profiles and manoeuvre
  diagnostics now run once for the final accepted trace, instead of in every ridge-refinement round. The
  point fits and final diagnostics are retained; the wall-clock improvement has not yet been measured.
- `--near <state>` searches the state's bounding box, which takes in parts of neighbouring states, so a wrong
  (low) answer can land outside the state (Connecticut for a New York search). The report warns when the
  answer's reverse geocode is in another state.
- Helicopter passes (rotor tones, speed changes) can fit with confidently wrong speed and distance. They do
  harm only through false pairings, which the margin gate is meant to catch.
- **With the knot rule, a wrong date can grade medium near the true site.** The development recordings were run
  with a start given 42 times with no right answer: 20 at wrong dates and 22 at places 90 km or more off.
  - One graded **medium**: the osprey cam's 10-31 recording searched on 11-01 (`nd_o31_1101`, local run `nb1`). It
    landed 2.6 km from the cam on 2 core matches, one of them a Sikorsky S-76 helicopter, at 4.98 nats.
  - On `cb95fbb` the same run is low at 2.49 nats, and with only the knot rule added it is medium at 4.98, so the
    rule caused it. The pinned benchmark build `7e66501` grades it low.
  - The curve match ran there and did not stand (−6.87), and the grade ignores a curve match that does not stand.
  - None of the 22 wrong-place runs graded above low.

---

## FAQ

**Does it use the GPS tag in my file?** No. If the container has a location tag, `fdop` says it ignored it. It
does use `creation_time` to narrow the clock search, unless you pass `--ignore-metadata-time`.

**Why does it need two flyovers?** One curve gives a speed, a closest distance and a moment, which places the
recorder on a line beside that flight. A second curve, at its real time offset, intersects that line at one
point and fixes the recording clock. More curves confirm the answer, and an aircraft that passed close but was
not heard counts against it.

**Why ignore jets?** Jet noise is broadband roar without a stable tone, so there is no clean curve to fit.
Unknown aircraft types are kept.

**How precise is it?** On the reference recording, the current code lands 263–270 m from the published location with a
379–406 m radius (`r_ny_gc`, `r_md_gc`, `r_nassau_gc`). The validated `7e5cf3e` build landed 271–306 m away with a
304–316 m radius, and earlier builds' high-confidence answers 221–224 m away, inside a 292 m radius. On
the osprey-cam field recording, the validated `7e5cf3e` build is 223 m from the reference recording's published location, just
outside its 214 m radius; an early build was about 150 m from the cam's geocoded street address, just outside its
147 m radius. An address geocode only approximates where a camera is. Precision depends on how many clean passes
there are, how close they were, and the ADS-B cadence. The radius is the run's own measure of its uncertainty, and
is meaningful only for a **high** or **medium** answer. A **low** answer can be close or far off; one was 365 km
off.

**How long does it take?** With the data cached on an Apple M4, the reference recording took 36 s for a county
and 119 s for a state. A busy recording takes far longer: about 2 hours 40 minutes for a state-wide search of a 3 h
osprey-cam recording. The first run on new dates is dominated by the archive download.

**Can I search the whole country?** `--near CONUS` is accepted. It downloads the same daily archives but keeps
far more tracks, and the budgets may truncate the search: a pair-join, fine-stage or confirmation-budget cut appears in the funnel
and the warnings, and an event over its hypothesis budget drops to confirmer-only with a warning. It has not
been validated on real data at that scale.

**Will it work on a phone call or a compressed video?** The source video reports curves surviving a phone call's
audio path. `fdop` decodes anything ffmpeg reads, but it has only been validated on the reference recording.

**Why Rust and the franken libraries?** One native binary that runs the whole pipeline, from FFT to geocoding,
with no Python environment. The franken stack (frankenscipy, asupersync, frankenpandas, FrankenSQLite) provides
the numerics, networking, tables and ledger without tokio or C solvers.

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## Acknowledgments

- **colsto**, for the method and the reference recording.
- **Henry Alexander's [`dopplerprint`](https://github.com/Hexay/dopplerprint)** (MIT), whose scoring of logged
  flights along their curves on the whole spectrogram the curve match reimplements, and whose check against
  flights from other times it extends to many decoy clocks.
- **adsb.lol** and its volunteer feeders, for the open ADS-B archive (ODbL).
- **Data providers:** the U.S. Census Bureau, USGS The National Map, NOAA NGS, Open-Meteo, the Iowa Environmental
  Mesonet, and AWS Terrain Tiles.
- **Report map:** Leaflet and OpenStreetMap.

## License

MIT with the OpenAI/Anthropic rider. See [LICENSE](LICENSE).

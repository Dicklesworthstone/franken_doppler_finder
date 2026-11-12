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
| Has it been wrong? | **Yes, at low and at medium, and on `c2c7578` at high on a wrong date. `11313ba` was wrong at medium on a sealed set, from the curve match alone; the current code reports such an answer at low.** On the reference recording, earlier builds named Old Lyme, CT (116 km off) and later Southold, NY for the three-day window (102 km off); `9aeeb9a` put the county search 14.4 km off and the state-wide search 87 km off, both low, until the guided-peak fix. A complete comparison without the ridge check described below put the state-wide search 34.8 km off, low. In the field tests below, five answers are wrong on the validated `7e5cf3e` build, all low, and a sixth lands near its site, probably by chance. On `d55ef79` one of them was **medium**: a bird-feeder cam in Ithaca, NY placed on Long Island. The validated `7e5cf3e` build grades it low, 0.11 nats under the medium line, which is a narrow miss and not a fix. An intermediate build of the current refinement code graded it medium again, margin 3.10 (`f_feederwatch_b8`), because refinement raised the leader's score by 0.95 nats and left its rival's alone. `cb95fbb` compares the two at the same polish and grades it low, margin 1.81 (`f_feederwatch_r5`), and with the knot rule 0.75 (`f_feederwatch_c1`). The answer is still wrong. A working-tree experiment that measured more of a 3 h recording's flyovers also graded a wrong answer **medium** (103 km off; see [Known issues](#known-issues)), so medium alone does not prove an answer. A low grade does not mean wrong either: on `7e5cf3e` the three-day reference search landed 271 m off at low. All five runs with a deliberately wrong date grade low on the validated `7e5cf3e` build, but two came close to medium: one had both core matches and was 0.63 nats short, and one had a 5.49-nat margin but only one core match of two. On the current code the reference recording's two wrong dates stay low: 119 km off with one core match of three and a 2.00-nat margin (`n_ref_on_0907_gc`), and 104 km off with no core match of three and a 0.15-nat margin (`n_ref_on_0909_gc`). Before measurement went past the old line they were 138 km off with both core matches and a 0.61-nat margin (`n_ref_on_0907_c1`), and 73 km off with one core match of two and 0.35 (`n_ref_on_0909_c1`). Before the knot rule the first was 2.95, **0.05 under the medium line** (`n_ref_on_0907_r5`), and `a827820` graded it **medium** at 3.01 (`n_ref_on_0907_a82`), a wrong-date regression that `cb95fbb` removed only narrowly (CHANGELOG). The field rows replayed on `cb95fbb` so far stay low or give no location, though two wrong answers moved much further off: the feeder on a wrong date from 56 km to 326 km (`n_feeder_on_1101_r5`; 254 km, low, with no core match with the knot rule, `n_feeder_on_1101_c1`), and Rochelle from 36 km to 371 km (`f_rochelle_r5`). The [CHANGELOG](CHANGELOG.md) has every run. On the benchmark's sealed set E, `6914ab7` graded a marina **medium**, 8 km off, from the curve match alone on one heard pass ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 14); on the same set `dopplerprint` with a ground model was wrong at medium at another site, 14 km off. On the sealed set M, `b8abf86` and `11313ba` graded Washington Square, Manhattan **medium**, 5.9 km off, from the curve match alone with six passes heard ([bench/SET_M.md](bench/SET_M.md); [Known issues](#known-issues)). On `c2c7578`, the osprey cam's 11-02 recording searched on 10-31, one of the 33 runs with no right answer described below, grades **high**, 34 km from the cam, with all five gates passed and a 6.34-nat margin (`n33_c2c7578`). `c0204f6`'s code grades the same run low, and so does `c2c7578` with `8be452d`'s peak-chirp window factor taken out (`bis_o02_1031`; [Known issues](#known-issues)). With the [decoy-clock gate](#the-gates) it is low: its evidence, 18.41 nats, leads the same search at four decoy clocks by only 1.07 (`n1_dec1`). Treat **low** as "unconfirmed", whichever way it points |
| Is it tested? | **Offline unit tests,** plus four opt-in FFmpeg/FFprobe integration tests: synthetic known-answer checks of detection, fitting, emission-time physics, track models and a two-flyover search, plus audio-track/channel selection, parsers, formats and a stalled download resuming over a loopback socket. See [Testing](#testing) for the latest checks and earlier validation. `fdop selftest` runs the real decode, detect, measure and search path on a synthesised recording |
| Has it located a recording it was not developed on? | **Two right answers, neither a clean test.** One is at the reference site, one is graded low, and both recordings informed the ridge check's line. Nine recordings from public YouTube live cams (2–3 h each, with published locations, and true clocks from the stream timestamps) were searched with only the local date and the state, and all nine were replayed on the validated `7e5cf3e` build. An osprey cam at the reference site, recorded on a different day with different flights, came out **high**: 223 m from the reference recording's published location, just outside its 214 m radius, with a 24.9-nat margin. The same cam on two other days gave wrong answers graded **low**, 152 km and 365 km off. An eagle cam at Duke Farms (Hillsborough, NJ) came out in the right township, about 2.3 km from the town centre, graded **low** (the nest position is not published, so a smaller error cannot be scored). The clock its answer implies is within 6.4 s of the stream's, from a search of the whole day, so that match is real whatever its grade: the low comes from a rival 140 m away. At five other sites, a bird-feeder cam in Ithaca, NY was placed on Long Island (338 km off) and a railcam in Rochelle, IL at Sycamore, IL (36 km off), both **low**, and railcams in Tucson, AZ, Plant City, FL and Fullerton, CA gave no location, because no event passed the quality gates. All but Fullerton held only 0–3 non-jet passes within reach. Fullerton (13 within 3 km) and Duke Farms (16) had more traffic but poor audio. Fullerton's is high-pass filtered at about 125 Hz, which removes or weakens the propeller fundamentals, under broadband wind and road noise; Duke Farms has a wind-saturated microphone. A tenth recording, a public eagle-nest live cam in Lee County, FL, recorded 2024-12-28, is the only one that did not inform the ridge check's line: searched across Florida it is wrong and **low** (135 km off), and searched in the county it lands about 4 km away, **low**, on two events. Its clock there is 55 minutes off the stream's, so the 4 km is probably chance and not a location: at the stream's clock one usable event matches an archived flight within 0.2 s, and at the two answers' clocks and eight other offsets none does. That rests on one event, and it leaves a coincidence unexplained: the state and county answers' clocks agree within about 2 minutes though they are about 135 km apart. Each answer's clock is a check the search cannot make on its own: the two real matches are within 6.4 s of the stream's clock, and the wrong answers are 53 minutes to 9.3 hours off. Using this takes care. `--start` means the exact start, `--clock-tol` defaults to 120 s and the search does not widen past it, so a start 10 minutes off excludes the true clock too. A start known to within 25 minutes with `--clock-tol 1500` would keep the true clock and exclude all of those wrong ones. What the search answers instead has not been measured |
| Do the knot rule and the curve match change this? | **Both do, on the development recordings with a known start. On the first sealed test the better build tied `dopplerprint`, and another was wrong at medium.** With the start given to the minute (±2 min) and a 25 km region whose centre is 10 km from the truth (the head-to-head benchmark's R2 inputs in [bench/PROTOCOL.md](bench/PROTOCOL.md)), fifteen runs on this code gave five right answers and no wrong one. They were the reference recording at 250 m, the osprey cam at 90 m, 110 m and 220 m on its three days, and Duke Farms within its 5 km limit, all graded high. None of the nine runs at wrong dates or wrong places was graded above low. Of 33 more runs with no right answer, none was above low on that code, but on `c2c7578` one is **high**, and the decoy-clock gate grades it low ([Known issues](#known-issues)). One would be medium on the trace search alone: the osprey cam's 10-31 recording searched on 11-02. The curve match does not stand there, and a medium needs it to, so it is low (CHANGELOG). That rule was written after seeing the same at its 11-01 search, before measurement went past the old line. On the benchmark machine the trace search gave two right answers on these inputs without the knot rule and four with it, and the pinned benchmark build `7e66501` gave one. Of the other four, the curve match stood at three and agreed with each, did not stand on the reference recording, and changed none of their grades. Since it profiles how far each recording hears (CHANGELOG), its margin over the best decoy on the osprey cam's three days rose from +9.2, +13.3 and +1.1 nats to +22.2, +21.4 and +5.3; it is unchanged at Duke Farms and went from −3.95 to −4.32 on the reference recording, and no grade changed. On the osprey cam's third day the trace search measured only the strongest 24 detections, 19 of which failed the quality gates, and its answer was 14 km off and low. The curve match stood near the cam and graded that day medium from its own place: 220 m off once it searched the whole kilometre around each of its starts, 240 m with the heard range profiled (CHANGELOG). Now that measurement goes down the list until 24 pass, the trace search itself lands 220 m from the cam with two core matches and a 23.16-nat margin, the curve match agrees, and the day is high. All fifteen recordings were used to develop these changes, and these runs (`f10` before that search fix, `disc_dev` with it, `r50_dev` with the heard range profiled, `fm` with measurement past the old line, `fh` with the two-pass rule below, the same in every grade and distance as `fm`) were on an Apple M5 Ultra with 256 GiB of RAM, not the benchmark machine. The benchmark's sealed set E is the first test, read on 11-05 ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 14). On its eight cases with these inputs, `9087ec4` gave one right answer, Olympia at 66 m (medium), and no wrong one: the same count as `dopplerprint`, so it did not beat it. `6914ab7` gave the same right answer and one wrong **medium**, a marina 8 km off, placed by the curve match alone on one heard pass. The curve match no longer stands on one heard pass (CHANGELOG). The pinned build gave no right answer. Set F, captured on 11-05, was read on 11-06 with these inputs and with no area ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 19): no tool was right on its ten cases. No `fdop` build asserted an answer, nor did `dopplerprint`, and `dopplertrack` was wrong once, so set F cannot tell the tools apart. On set E with no area (Addendum 21), `6914ab7` asserted nothing: it put Olympia 67 m off but graded it low, and `dopplerprint` was wrong once. `9087ec4` and the pinned build asserted nothing there either, and `dopplerprint` with its ground model declined all eight (Addendum 24). With only the date and the state (R1, Addendum 22), neither `6914ab7` nor `9087ec4` asserted anything on set E: both put Olympia 65 m off and graded it low, and `dopplerprint` gave no answer on any case. The pinned build and `dopplerprint` with its ground model asserted nothing there either, and no tool was right on set F with only the date and the state (Addendum 25). Sets G and H, live cams captured on 11-05 and chosen for aircraft passing within 3 km, were read on 11-06 with the R2 inputs ([bench/SET_G.md](bench/SET_G.md), [bench/SET_H.md](bench/SET_H.md)). Every registered build declined all three cases: the one where no aircraft passed, and the two that passed the hearing screen. That screen's tonal read was later found biased: against chance lines of passes as close, `fdop`'s own filter, aimed with the true passes, hears neither case's aircraft, and only band power hears one (Deerfield Beach). No true pass reached the detector's floor, so no build found a flyover to fit. Set I, three wildlife and nature cams with close traffic, was read on 11-08 with the R2 inputs ([bench/SET_I.md](bench/SET_I.md)). At Winter Park `dopplerprint` was right, 3.2 km off within the 5 km limit, and so was `9087ec4`, 1.3 km off at medium, so `9087ec4` tied it. The pinned build declined all three cases, and `a0795de` placed Winter Park 32 km off at low. No tool asserted anything on the other two cases or on the three wrong-date runs. Set J, live cams in large US metros away from busy airports, kept one case, a Chicago cam, and was read on 11-09 with the R2 inputs ([bench/SET_J.md](bench/SET_J.md)): no tool was right, every `fdop` build declined the case and its wrong-date run, `dopplerprint` asserted nothing, and `dopplertrack` was wrong on both. Set K, stationary field recordings in New York and Los Angeles whose descriptions give the site and the start, was read on 11-09 with the R2 inputs ([bench/SET_K.md](bench/SET_K.md)). `9087ec4` was right on two of its eight cases, at 137 m and 20 m (medium), and `3c51eae` on one, at 33 m (high), and neither asserted anything wrong on a case or a wrong-date run. `dopplerprint` was right on one case and wrong at high on one wrong-date run, and could not download the ADS-B archive for five cases (it reads only releases split into parts). With those days ingested by hand, as its documentation describes (a run registered before it ran), it was right on one more case and wrong at high on one more wrong-date run. That makes 2 right and 2 wrong, so `3c51eae`, right once, has fewer right answers on that reading, while `9087ec4` still has as many right and fewer wrong. `dopplertrack` was wrong on all 16 runs. The pinned build, `b759c6f` and `1dec7d6` asserted nothing, so each had fewer right answers than `dopplerprint`; `b759c6f` and `1dec7d6` put four cases within 256 m but graded each low. Set L, radio aporee field recordings with a logged place and start ([bench/SET_L.md](bench/SET_L.md)), found no case: of 1,838 recordings, two in a large metro reached the traffic screen, and neither had close traffic on record. Set M, six more New York field recordings from set K's screen, was read on 11-09 with the R2 inputs ([bench/SET_M.md](bench/SET_M.md)). `9087ec4`, `b8abf86` and `11313ba` were each right on one case, Fort Wadsworth, Staten Island, at 23 m and 32 m (high), and so was `dopplerprint` in each of its three regimes, at 19 to 36 m. `b8abf86` and `11313ba`, the same code with and without the null-tail limit, gave the same answer on all twelve runs, and both were wrong at **medium** on one case, 5.9 km off: the curve match stood alone, 0.2 nats over the best of its 48 decoy clocks. `9087ec4` asserted nothing else. `dopplerprint` was wrong at medium on one wrong-date run, and `dopplertrack` was wrong on seven runs and failed on five. Over sets J, K and M, `9087ec4` was right on 3 of 15 cases with no wrong answer, and `dopplerprint`, with its 2023 days ingested by hand, was right on 3 with 3 or 4 wrong, by regime. Without a start the curve match does not run (CHANGELOG) |
| What is not validated? | Any site other than the reference one; large US metros (on sets J, K and M the best build was right on 3 of 15 cases, and `11313ba` was wrong at medium on one, from the curve match alone, an answer the current code reports at low, [bench/SET_M.md](bench/SET_M.md); two further searches for such recordings, sets L and N, found none that passed every screen, [bench/SET_N.md](bench/SET_N.md)); low-traffic or rural sites (all four tried failed); following real passes steeper than 4 Hz/s (real ridges that steep exist in the recordings, but no event follows one, so that side rests on synthetic tests); CONUS-wide searches on real data; recordings outside the US; phone-call or heavily processed audio |
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
layers. Physical aircraft speed and distance are converted separately to the equivalent fitted
coordinates; coarse hypotheses bound the possible distance scales before the observer direction is known.

The fitted still-air curve's centre is also converted to the candidate's moving medium.
With oblique wind it differs from the arrival of sound emitted at geometric closest approach,
and can even precede that emission time. The trace search uses the converted centre for
seeding, joint clocks, calibration, confirmation, refinement and matching detections to
otherwise unheard passes. Geometric CPA arrival timestamps in reports retain their physical
meaning. Moving or undeclared media examine the retained track spans beyond the former time
crops; this can add work, with the existing hypothesis budgets and truncation warnings.
The conversion is exact for constant velocity in a homogeneous medium and uses the local
CPA velocity on a turning track. It changes no measurement or confidence threshold.

For recorded flight tracks, an emission-time solve that fails to converge with Newton's method
retries inside a signed time bracket. A turning aircraft can otherwise make Newton alternate
between distant parts of its path even when the sound has one valid emission time. The retry
uses the same trajectory, atmosphere and Doppler derivative, requires an interior solution and
the existing reception-time residual, and rejects invalid propagation. Successful Newton solves
retain their original results.

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
 decoy clocks  a high or medium trace answer from a --start or creation_time window: the same locate
               again with that window moved by −2, −1, +1 and +2 steps (1 h for --start); the answer
               must lead the best of them by 3 nats, or it becomes low
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
| decoy clocks | listed on a high or medium answer from the trace search when `--start` is given or the answer came from the file's `creation_time` window: `locate` runs again with that clock window moved by −2, −1, +1 and +2 steps (the next of ±3 and ±4 steps when a window would leave `--dates`), and the answer's evidence must lead the best decoy's by ≥ 3 nats. A step is whole hours, at least the window's width plus 10 minutes: 1 h for `--start`. Fewer than four decoys that run fail it |

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

When broad fitted timing intervals reach the shared clock scan's 400-cell limit, continuous
refinement covers the actual spacing between those cells. It no longer confines the optimum to
the nominal 0.25 s neighbourhood of a sample when the samples are farther apart. Narrow timing
windows retain their original evaluation sequence; the scan budget and confidence gates are unchanged.

Trace scoring retains the original least-squares tone fit and also tries a robust emitted-tone estimate
from the median of observed frequency divided by predicted Doppler ratio. The alternative is used only
when it improves the same capped residual objective. This prevents a minority of stray ridge points from
shifting the fitted tone enough to escape the existing outlier trim. Tone bounds, missing-coverage penalties,
the raw RMS diagnostic and the core-match thresholds remain in force.

A still-air fit's reference tone can differ from the aircraft's physical source tone in wind.
For each candidate flight, trace scoring converts the fitted tone and its permitted interval into
source-frequency units using the same path medium and aircraft velocity as its Doppler prediction.
The conversion is exact for straight, constant-velocity motion in one homogeneous medium;
using the local closest-approach velocity on a turning track remains an approximation.
It applies to quick and exact scoring, chance calibration, confirmation, independent clock checks
and refined matches, including the allowed tone change for model-misfit events.
Calm-air intervals stay unchanged. This preserves the existing tolerance instead of discarding
a physically supported windy pass because its source tone lies outside the unconverted interval.

The speed and distance intervals from a still-air fit also require separate wind conversions.
Exact candidate checks now predict both fitted quantities from the aircraft's physical CPA and
compare them with the measured intervals. A common first-order factor could exclude the true
aircraft even when its propagated frequency curve fitted exactly. Before an observer is known,
hypothesis generation uses the corresponding speed scale and a distance interval covering all
CPA directions for the sampled medium. Calm-air arithmetic, search budgets and confidence
thresholds retain their existing behavior. The conversion is exact for constant velocity in
homogeneous wind; using a local tangent and sampled weather on a turning or spatially varying
path remains an approximation.

Between stored Doppler predictions, the search uses a four-point cubic on a finite, monotone stretch,
bounded by the two bracketing predictions. This reduces curvature bias for close, fast passes without
additional emission-time solves or a denser table. Boundaries, turns, plateaus and gaps retain the linear
fallback; missing bracketing samples remain missing.
Each prediction grid now prepares its monotone intervals and cubic differences once, then reuses them
through the repeated recording-clock fits. This costs 16 extra bytes per interval and preserves the
original interpolation values and fallback rules; it does not change the search grid or scoring model.

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
low trace answer that lies inside its radius to **medium**. Where the trace answer lies outside its radius, or the
trace search found nothing, the answer is the curve match's own place at **low**, and the verdict says it comes
from the curve match alone. Its decoy clocks test the recording's clock, not its place: on the right date a place
near the aircraft heard can beat every decoy clock. Before this rule such an answer was medium, and on the
benchmark's set M it put Washington Square 5.9 km off ([Known issues](#known-issues)). It does not stand on one
heard pass: one pass's Doppler curve fixes when and how close it passed, which leaves place and clock free to trade
along its track, and its decoy test checks the clock, not the place. That rule was added after a curve match on one
heard pass graded a marina medium 8 km off on the benchmark's sealed set E (CHANGELOG).
Where it stands more than 3 km from a high or medium
answer, that answer becomes **low**. It never makes an answer high. Where it ran with at least 40 decoy clocks
and stands nowhere, a **medium** trace answer becomes **low**. The medium grade's margin has no calibrated
false-alarm rate. The curve match's decoy test has one: with a wrong date or place, the real window is one of at
least 41 interchangeable clocks, so it stands by chance about once in 41 at most. This rule was added after a
wrong-date run graded medium at a wrong place (CHANGELOG), so its effect on the development recordings is not a
test. A high answer keeps its grade. Where the curve match does not run or has too few decoy clocks, the grade
is the trace search's, except that a medium answer resting on a core match measured past the old line (below)
becomes **low**: nothing calibrated then checks that the extra events did not just give chance more to work with.

A high or medium answer from the trace search then meets its **decoy clocks** when its clock window was narrow:
given by `--start`, or by the file's `creation_time`. `locate` runs again four times with that window moved by −2,
−1, +1 and +2 steps, each into `decoy_clock_<shift>h/` inside the run directory. A step is whole hours, at least
the window's width plus 10 minutes, so no decoy window reaches the true start: 1 h for `--start`, and 2 h for a
one-hour recording's `creation_time` window, which spans the recording. A decoy draws no figures or map and looks
up no place name online. Hours off, no true pass lines up, but the recording, the place and the day's traffic are
the same, so the best decoy shows how much evidence chance fits to this recording's sounds reach there. The answer
must lead it by 3 nats, the medium margin line, or it becomes **low**. The margin gate cannot see this, because it
compares explanations at one clock. The gate was added after a wrong date graded high on `c2c7578`
([Known issues](#known-issues)), so the development rows are not a test of it. On them it lowers that run and keeps
every right answer: the five lead their decoys by 14.0 to 67.8 nats, the wrong date by 1.07 (`ab_c2c7578_dec1`,
`n1_dec1`). The decoys run without the curve match, which the gate does not read. Before sharing the audio
analysis, the four added 4 to 9 minutes to an answer on the development rows, with the same evidence as with
the curve match on (`ab_dec1_dec2`, `n1_dec2`).
On copies of two development recordings given their start as `creation_time` instead of `--start`, both answers
stay high, leading decoys at 2 h and 3 h steps by 13.96 and 7.54 nats, and all six runs at wrong places stay low
(`ct_dec4`). A search of the full dates, with neither or after the `creation_time` window found nothing, runs no
decoys: it already compares every clock in the dates.

All four decoys now borrow the original recording's decoded duration, detections and measured events, including
their traces and spectrograms. Probing, decoding, survey generation, detection and measurement run once; the
large event spectra are shared without copying. Each decoy still checks its own archive availability and loads
its own flight tracks and atmosphere, rebuilding wind-dependent speed gates before searching. Audio warnings,
event IDs, measurement provenance and the independent-event limit are preserved. In each decoy's `result.json`,
`extra.audio_analysis_reused` is `true`, and `decode_s`, `detect_s` and `measure_s` are zero; an ordinary run records
`false` and its actual stage times. The search and confidence rules are unchanged. Overall time savings depend
on how much of a run is spent analysing audio; the historical timings above are not measurements of this reuse.

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
  The bank's inverse-range weights use the aircraft's position at retarded emission time,
  matching the physical amplitude convention of exact trajectory scoring. The received
  frequency and the distance weight therefore describe the same emitted sound.
  Real windows, null calibration and decoy windows all use this same scorer.
- **Tone by type.** For common propeller types a tone the propeller can make (blades × rpm / 60, from typical
  published figures) earns more and other tones less. The weights are divided by where this recording's best
  tones fall with no weighting, and scaled to average one over those tones, so they do not favour every flight.
  Weighing against a flat spread instead credited every flight at every clock on rumbling audio, because the
  rumble sits where the propeller tones are. The decoy clocks are weighted the same way.
- **Search.** Flights vote for (place, clock) cells 200 m apart. The best 16 places are rescored with the exact
  curve (the real trajectory, emission-time solve, modelled wind and sound speed).
  The coarse vote uses the same terrain and microphone position as its reported latitude and longitude.
  A retained CPA queries the atmosphere at that observer and the actual source/receiver heights.
  In wind, its locally straight frequency curve is mapped to the existing bank's equivalent
  speed, distance and heard centre; correcting travel time alone would leave the wrong shape.
  Calm paths retain their original arithmetic. This mapping changes which supported candidates
  reach exact scoring; exact trajectory scores, amplitude weights, nulls and decoy gates retain
  their existing models. It assumes a homogeneous path-averaged medium and local constant velocity.
  The grid bounds the geographic box's ellipsoid projection, keeps a centre point on narrow axes, and
  includes a one-cell fringe clipped onto the boundary. Small boxes and long, thin regions therefore
  retain searchable locations at their edges instead of losing them to a tangent-plane approximation.
  At each selected place,
  positive local peaks in the clock votes, including tied plateaus, supply alternative recording times.
  Exact evidence chooses the clock that reaches spatial refinement; a stronger coarse alias is no longer
  the only timing seed. The original seed is tried first. A later sweep is skipped only when every
  in-window timestamp it would score was already covered on the same clock lattice by successful sweeps.
  Each of the best 4 places stands for every place within 1 km of it, so every point of that disc is scored
  on a 100 m grid before it is climbed to 25 m. A close pass with no energy costs evidence, as in the trace
  search, and one audio event can credit only one flight. This recovers clock alternatives at selected
  places; the existing spatial shortlist still limits which places reach exact scoring.
  If two trajectories initially prefer the same bright tone, the original strongest-tone assignment
  finishes before recovery looks for a distinct, unclaimed peak for the rejected trajectory. A recovered
  peak must clear the existing 6.9-nat heard threshold and timing penalty, with the same coverage, type
  prior and null. Peak qualification includes claimed neighbours, so the shoulder of an owned ridge
  cannot become a new tone simply because the ridge itself is excluded. Harmonic pixel ownership still
  applies, and an original independent match keeps priority over every recovered alternative. Recovery
  subtracts only positive harmonic contributions on already claimed full paths, retaining independent
  partials, all original coverage weights and the original comb normalization. Its remaining evidence
  must still clear the heard threshold. The normalized frequency rows are retained for this check,
  adding one `f32` image the size of the comb without another STFT. Tone profiles are computed only
  for collisions and cached within the exact score; their unmasked maxima bound which profiles can
  improve it. A new claim invalidates pending residual scores before they can be accepted. Real and
  decoy windows use this same recovery.
  Each retained mode with at least two independently heard visits then fits its location and
  shared clock on a fixed set of observed frames. The frames, range weights, approach/recede
  partition and per-flight timestamp offsets stay fixed during this fit, so moving farther from
  a circling aircraft cannot win merely by adding audio to its window. Fractional frequency rows
  fit within the original emitted tone's one-row neighbourhood. Every retained real and decoy
  mode receives the same procedure, including modes outside the expensive evidence-refinement
  budget; disabling refinement still disables it. The ordinary calibrated scorer then evaluates
  the fitted point at that shared clock with its original per-flight timing slack. The point is
  adopted only if each fitted source visit remains independently heard, matched one-to-one by
  track, observed arrival support and tone neighbourhood. Otherwise the original mode remains.
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
  width plus 60 s apart, out to about ±2 hours, on whichever side the UTC days this recording needs cover
  before the published archive ends. A clock that far off cannot be right, so the best decoy is what this
  recording and this sky give by chance. A start known to ±120 s (the default `--clock-tol`) gets 48 decoys
  and ±150 s gets 40, the fewest that can judge. A wider window gets fewer (±180 s gets 34), and so does the
  `creation_time` window of any clip longer than a minute, which also spans the recording's length. Up to
  30 min such a window is still searched, but its verdict can neither stand nor change a grade, and a
  warning says why.
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
  low). A stand it removes no longer conflicts with the trace answer either. The verdict line says how many
  twins tied, and `curvematch.json` lists them under `aliases`.
- **When it runs.** Only when the recording clock is known to 30 min or better and the region holds at most
  200,000 allocated cells, including the enclosing grid and boundary fringe (roughly 8,000 km² for a compact
  region); otherwise it is skipped with a log line. Invalid or unprojectable grids are also skipped before
  allocation. A state-wide search with no start
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
`--offline`, `--include-jets` and `-q/--quiet` belong to `locate` and `fetch`, and go after the subcommand. `detect` needs no data
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
| `--include-jets` | Experimental: include known jets as potential tonal sources; uses separate certified ADS-B caches and the existing acoustic gates |
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
| `--start <ISO>` | — | Exact recording start if known, e.g. `2026-09-08T19:17:30-04:00`. Without a zone it is read as **UTC**. Unix seconds also work. With it, a high or medium answer from the trace search is also checked at four [decoy clocks](#the-gates), which runs the search four more times without the curve match |
| `--clock-tol <S>` | `120` | Finite, nonnegative clock tolerance (s) around `--start` or the file's `creation_time`. The [curve match](#the-curve-match) can judge only a clock window of 300 s or less: with `--start`, 150 s or less. Before 2026-11-07 the default was 180 s, at which a run with `--start` never could |
| `--ignore-metadata-time` | off | Ignore the container's `creation_time` |
| `--max-events <N>` | `12` | Maximum events that drive the pair join; the others can still confirm |
| `--no-curve-match` | off | Skip the [curve match](#the-curve-match); the answer then comes from the trace search alone |
| `--include-jets` | off | Retain known jets in both searches and their decoy clocks. Stable subsonic tones within the existing analysis band are still required; no field validation is claimed |
| `--json` | off | Print the full result JSON on stdout instead of the summary |

```bash
fdop locate clip.m4a --dates 2026-09-08 --near NY
fdop locate clip.mov --dates 2026-09-07..2026-09-09 --near "Oyster Bay, NY" -o runs/oyster
fdop locate clip.wav --dates 2026-09-08 --near "40.87,-73.52,25" --start 2026-09-08T15:17:30-04:00 --clock-tol 60
fdop locate clip.m4a --dates 2026-09-08 --near CONUS --json > result.json
fdop locate clip.m4a --dates 2026-09-08 --near NY --offline        # cached data only
fdop locate clip.mov --dates 2026-09-08 --near NY --audio-stream 1 --audio-channel 0
fdop locate clip.wav --dates 2026-09-08 --near NY --include-jets
```

**Jet matching is opt-in.** `--include-jets` retains known jets through archive parsing,
cross-feed and cross-day reconciliation, trace hypotheses and confirmation, and the
independent curve match. Its real and decoy clocks use the same aircraft policy. Included
jets are treated as potential tonal sources by the existing unheard-pass evidence too;
this assumption can reject a location when nearby jets do not produce usable tones.
The report warns that this mode has no field validation and records the request in the
additive `extra.include_jets` boolean. The detector band, speed limits, measurement limits
and confidence gates are unchanged. Broadband jet noise and supersonic propagation are
not modelled. Ordinary runs continue to use the existing non-jet catalogues. Unknown
types remain eligible in either mode.

**The time window.** Without `--start`, recording starts are searched from local midnight on the first date to
local midnight after the last, using every UTC offset the region spans, DST included. If the file's
`creation_time` falls inside that window, the search first narrows to it, allowing for both start and end
conventions, and a high or medium trace answer found there meets the [decoy clocks](#the-gates). Both the trace
search and, when eligible, the independent curve match try that window. A standing curve match with no trace
answer at its place gives a low answer, so it does not keep that window by itself. The metadata window is
retained only if the complete assessment reaches high or medium confidence, including any required decoy-clock
check. A low or absent answer retries the full date window when it adds available recording start times, so a
chance candidate at an export timestamp cannot block the rest of the dates. If publication limits leave the
same or a narrower available window, the original grade and failed checks are retained. The audio, detections and
measurements are reused. The curve match
retries too if the wider window is still eligible. A curve test from the earlier narrow window cannot adopt,
confirm or overrule an answer in that wider search. Its original `clock_window` and
`judged_search_window` flag are recorded in `result.json` under `extra.curve_match` when it has a verdict.

Archive availability, track loading and weather follow the window actually being tried. A metadata
attempt can therefore run offline with just its required cached UTC day, even when `--dates` includes
uncached surrounding days. If its archive cannot be loaded, or its answer remains unconfirmed, the
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

Detects flyovers using the same recording analysis as `locate`: detections are measured strongest
first within each stage until 24 independent aircraft events survive. If ordinary measurement
finds fewer than two independent aircraft, unmeasured detected harmonics of accepted families
or families with a complete but refused primary ridge are measured strongest first: coarse
survey grouping can put separate moving sources in one family. A sibling of a complete refused
ridge must show distinct observed motion on both sides of the closest approach; a cleaner fit
of the same sound or missing overlap cannot overturn that refusal. The original result remains.
The observed ridges decide whether they are independent, and retries stop once two aircraft
survive. True harmonics still count once, including siblings whose fitted closest-approach times
or emitted tones disagree. These extra measurements retain expanded-search provenance and a warning.
If an aircraft is still missing, a bounded half-second survey retries the recording's
true edges and refused detections. Those candidates are measured before a fully supported short
interior scan looks for any still-missing fast pass. Failed earlier candidates retain their slots
and proposal costs but do not hide sound from that later scan. The observed ridges and detected harmonic families
of accepted aircraft are excluded from that recovery. Every recovered candidate must pass the
unchanged measurement and aircraft checks; warnings identify added candidates and any cap.
Final events are ranked by measured quality and numbered
`e1`, `e2`, ... exactly as in `locate`, so an audit and a location search identify the same measured
passes. This replaces `detect`'s former detector-order output and event numbering.
If a family's primary lost its
ridge around the closest approach, its detected harmonic siblings are tried strongest first until one passes
the existing quality and aircraft gates. This first attempt supplies one result per family;
the missing-aircraft recovery above may add independently measured siblings. Refused fits, non-aircraft fits and duplicate traces of one
pass do not consume the independent-event allowance. Every selected family result and additional sibling attempt remains in `events.json`,
including duplicates and unrecovered refusals. A warning on stderr says when either cap omits candidates, when the audio
looks high-passed, or when the survey image was raised because its spread was under 0.5. A detection found
only on the contrast image of such a recording has `"from_contrast": true` in `events.json`. The command
writes `events.json`, `survey.png`, and `e<N>.png` plus `e<N>_trace.csv` for every fitted event, using
those final event IDs consistently. The default
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
fdop fetch --dates 2026-09-08 --near NY --include-jets # expanded aircraft catalogue
```

This caches the ADS-B day stores that `locate` will need, which is useful before going offline or before
several runs. It prints `<legs> legs, <points> points cached under <cache>`. Like `locate`, it fetches only the
published part of the archive. When the dates run into a UTC day adsb.lol has not published yet, it warns
which days are pending and stops before them. If no published day is left, it exits with an error: rerun after
the release. The `locate` footprint follows the largest retained event's upper distance gate plus
15 km of track context; for wider ranges, prefetch with a sufficiently large `--pad-km`.
The default 30 km prefetch footprint may not cover every `locate` request.
Use matching aircraft flags for offline work: `fetch --include-jets` primes
`locate --include-jets --offline`, while ordinary `fetch` primes ordinary `locate --offline`.
The two cache populations are separate; neither silently substitutes for the other.

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
| `decoy_clock_<shift>h/` | On a high or medium trace answer from a `--start` or `creation_time` window: one run directory per [decoy clock](#the-gates), with its JSON, CSV and report files but no figures or map |
| `curvematch.json` | The curve match, when it ran: its places with every pass's evidence, the best place's twins along the heard flights' tracks (`aliases`), each decoy window's best place and score, the margin over the best decoy and its rank p |
| `detections.json`, `events.json`, `search.json` | Intermediate results. `search.json` keeps the best 10,000 candidates, and `candidates_total` counts them all |

UTC timestamps are rounded to the nearest millisecond, carrying into the next minute or date when
needed. Whole seconds omit the fractional part; rounding never leaves a `:60` seconds field or moves
the reported recording clock back a minute.

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
├── adsb/<YYYY-MM-DD>/releases-main.json              main-only discovery metadata
├── adsb/<YYYY-MM-DD>/raw/*.fdopraw.gz                completed filtered raw feeds for retry
├── weather/                                          Open-Meteo and radiosonde responses
├── geoid/<lat>_<lon>.txt                             NGS geoid height at a region's centre
├── terrarium/<z>/<x>/<y>.png                         elevation tiles
├── usgs_topo/<z>/<y>/<x>.jpg                         basemap tiles for map.png
├── gazetteer/                                        Census 2024 gazetteer zips
└── runs.sqlite                                       the run ledger (FrankenSQLite)
```

Expanded jet-inclusive stores append `+jets` to the feedset, for example
`prod+mlatonly+jets_<S>_<W>_<N>_<E>.fdtrk`. Their metadata must also contain the boolean
`stats.include_jets: true` before an expanded request can reuse them. Renaming an old
store cannot restore jets discarded during its ingestion. This coverage requirement
also applies to offline legacy caches and to partial-feed fallback.

Availability checks look up only the main feed (prod, or staging when prod is absent).
Fetches with MLAT disabled also skip its release lookup. With MLAT enabled, an optional
MLAT lookup failure keeps the successfully discovered main feed and records the error in
progress and final notes, so a usable main cache or archive can still supply the day.
Required main-feed lookup errors remain fatal.

Main-only discoveries use `releases-main.json`. They never replace `releases.json` or
certify that MLAT is absent. A later online request for both feeds retries MLAT unless an
existing full listing is reusable under the established publication rules. Offline reconstruction
can combine main-only metadata with an existing MLAT-only listing without rewriting either.

Completed raw feed checkpoints live under `adsb/<YYYY-MM-DD>/raw/` as
`<feed[+jets]>_<S>_<W>_<N>_<E>.fdopraw.gz`. They retain the filtered reports before
cross-feed reconciliation, including both altitude measurements, duplicate reports and
leg-boundary provenance. A later online `locate` or `fetch` can reuse a completed feed
when another feed failed, including an MLAT feed whose main feed failed. The archive stream
is opened only when no usable checkpoint exists.

When building a day from raw reports, duplicate reconciliation keeps the preferred report's
time, position, motion and position-source flags. A lower-priority duplicate can supply a missing
GNSS or barometric altitude only when its exact coordinates and one shared altitude agree,
and it carries a complete, nonstale altitude pair. This retains observed altitude offsets for
the existing leg and regional barometric corrections, with the contributing feed recorded in
leg provenance. The rule applies to fresh ingestion and raw-checkpoint reconstruction; an
existing processed day store must be rebuilt to recover evidence discarded by older ingestion.

Reuse requires the exact release metadata and ordered assets, spatial/time filter,
unknown/jet policy and midnight-context version. A changed finite geoid reference can
reuse the same checkpoint: finite GNSS heights shift by the old reference minus the new
one before ordinary cross-feed reconciliation and barometric correction. This lets a
retry use a newly available geoid correction without downloading the completed feed again.
Raw barometric heights, missing-altitude values, motion and flight boundaries retain their
original bits. The cached source is unchanged, and identical geoid references bypass the
conversion entirely. All raw floats and boundary times are stored as integer bit patterns,
preserving missing-altitude NaNs and exact timing. A complete empty feed is reusable;
a truncated ingest is never published. This
private cache is written atomically and checked through the gzip footer when loaded.
A missing, incompatible or damaged checkpoint causes a fresh download of that feed.
A checkpoint-write failure warns and keeps the current successful ingest; it does not
change final day-store publication errors.

If no processed full or partial day cache is usable, `--offline` can rebuild it from
completed raw checkpoints and cached `releases.json` or `releases-main.json` metadata.
It validates that each release belongs to the requested day and feed, then applies the same
raw identity checks and geoid conversion as an online retry. A usable prod checkpoint takes priority over staging. Available requested
MLAT is reconciled with the main feed; if requested MLAT is unavailable, a main-only result
keeps its missing-feed warning. MLAT alone cannot supply a day. Existing processed-cache
hits keep their priority.

The rebuilt day is atomically saved for later searches, so this recovery needs a writable
cache. A final day-store write failure remains an error. No network client or archive
stream is opened in offline reconstruction. For recent ended days, the CLI's availability
check recognizes a completed raw main-feed filename paired with valid same-day release
metadata; full checkpoint integrity and query policy are checked during reconstruction.
Metadata alone, temporary files and MLAT-only data do not establish that availability,
and unfinished UTC days remain pending.

These checkpoints add disk usage for the filtered raw reports. They do not save entire
multi-gigabyte archives, resume an unfinished feed across process restarts, or recover
raw information from older processed-only caches. No extra flag is required, and ordinary
completed-day cache hits still take precedence.

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
scan. The binary store format is unchanged.

Newly ingested day stores also retain the original reports needed to join a flight across UTC midnight,
even when neither day's separate segment enters the cached region. Boundary support uses the closest
airborne reports within the existing five-minute cross-day join limit, with their interpolation neighbours;
source endpoints in the existing archive padding also keep their interpolation support. Duplicate
candidates at those timestamps remain available for feed preference. This does not retain hours of
worldwide position samples or bound total process memory. Stored flags and compact boundary metadata
preserve cuts made by spatial filtering and ground or restart boundaries, including a marker after
a feed's last airborne report or in a ground-only companion feed. A midnight join
may bridge a filtering cut only when both day stores certify that their boundary support was retained.

Online multi-day requests refresh older day stores without this coverage marker. Offline requests can
still use those older stores, but warn that missing midnight reports cannot be recovered from them;
the warning remains in the final report, including when it comes from the curve matcher's separate
real/decoy catalogue. Single-day requests continue to reuse older caches. The coverage marker does not
survive spatial/time cropping, which may remove the reports it certifies.

Concurrent searches publish track stores through separate, exclusively created staging files, so one
writer cannot truncate another writer's cache. A load also rejects bytes after its compressed member,
including additional gzip members; a damaged cache goes through the recovery path above.

Weather responses, terrain tiles, geoid heights and file downloads also use private, exclusively
created staging files. Concurrent runs cannot share or truncate one another's in-progress response.
Census gazetteer ZIPs are parsed before reuse; online searches replace an unreadable, malformed or
empty table only after downloading and parsing a valid replacement. A failed download or validation
leaves the published cache intact. Offline searches report the unusable cache path and how to repair
it; a valid cache is reused without any request.

Open-Meteo hourly responses and radiosonde soundings also pass the requested weather checks before
their cached JSON is reused. Online runs refresh an incomplete cached response once through the
existing request/retry policy, and publish the replacement only if it is usable. Offline runs retain
the validation error and normal fallback behavior. A failed refresh leaves the earlier response
intact, including any hours it can still serve. Missing hours or an unusable profile do not create
a failure marker for the whole query; transport and provider errors retain the existing 30-minute
backoff.

For uncached multi-day searches, each UTC day is built and atomically cached as soon as all of its
requested feeds finish, while the remaining days continue downloading. Completed raw feeds pass through
a bounded queue instead of accumulating until the last download. If MLAT fails, a completed prod or
staging feed is still cached under its actual partial feedset; an MLAT-only remainder is never published
as a main catalogue. The online request still reports the feed failure. Offline runs can reuse the
surviving main feed with an explicit missing-MLAT warning. An online retry loads matching completed raw
feeds and downloads the missing ones, then rebuilds them together so their raw altitude and aircraft-type
information can be reconciled correctly. Final day-store cache-write failures remain errors, after other
days receive their save attempts. After publication,
a completed day keeps its actual open cache
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

A streamed download resumes only when its initial response supplied exactly one valid strong HTTP
ETag. Resumed requests send `If-Range`, and the response must carry the same strong tag before any
further bytes are accepted. Changed, missing, weak, malformed or duplicate tags fail the download,
including a same-length replacement or a full response after the server ignores the range. A final
`416` length probe must also repeat the pinned tag. The already streamed prefix cannot be rewound
safely during archive ingestion; rerun to fetch the current object afresh.

A complete download without a strong ETag still works. If it is interrupted after delivering bytes,
or requires another partial response, it fails explicitly before making an unverifiable resume
request. Weak ETags and `Last-Modified` alone do not establish the byte identity required here.
Retries before any bytes have been delivered can still start afresh.

Day caches retain aircraft with unknown types. For data API callers using
`IngestSpec.keep_unknown = false`, that policy is applied after cross-day legs are merged:
a recognized type on either adjoining day identifies the complete joined flight, preserving
both halves of its geometry. A missing or unrecognized designator can be replaced by a known
one; the first recognized type still takes precedence over a later conflicting known type.
Only flight segments that actually join share this metadata. Separate flights of the same
aircraft and flights whose type remains unknown are still excluded by the strict policy.

Within a day's feed merge, the first recognized aircraft type in feed-preference order takes
precedence over an unrecognized designator from another feed. If no feed recognizes the type,
the first nonempty designator is retained and the usual unknown-type policy applies. This does
not change which feed supplies duplicate positions; boundary-only records cannot supply identity.

Malformed scalar entries inside an otherwise valid aircraft trace are skipped individually,
like the existing null and object entries. Valid surrounding reports and ground/new-leg markers
are retained, rather than losing the entire aircraft. Invalid JSON still fails parsing. These
recoveries apply during fresh ingestion; existing day stores are not automatically rebuilt to
restore previously discarded aircraft or improve their cached type metadata.

When neighboring track stores overlap, reports within the existing 0.05 s duplicate interval
prefer non-MLAT positions even if an MLAT report arrived slightly earlier. The merge preserves
the previous flight-leg joins and same-quality choices, including interleaved overlaps from
three or more files. Unjoined legs retain their original point allocation.

Archive ingestion and cached-window slicing retain the original samples bracketing a sparse segment that
crosses the requested region and time window, even when neither sample is inside. The segment must enter
both at the same time. Ingestion does not infer a crossing across a ground report or restart marker;
cached-window slicing does not cross stored flight-leg boundaries. Separate retained sections remain
separate legs, so filtering cannot invent a straight chord through discarded reports. Two neighbouring
samples on each side still anchor interpolation, and the track model's existing gap and geometry checks
still apply. No positions are invented. Older narrow caches that already discarded an aircraft need fresh ingestion to recover it;
the fix can use bracketing
samples in an existing wider cache, but cannot restore samples absent from the stored data.

`locate` and `fetch` reuse the agreed NGS geoid height cached for the region's centre in both online and
`--offline` mode. Offline mode uses the −30 m default only when that height is absent, unreadable or invalid,
and warns when it does. Going offline therefore preserves the altitude reference of a valid cached run.

### Data sources

Weather profiles retain the original hourly rows that bracket the requested time window. Missing
temperature, pressure or winds in unrelated hours no longer discard usable weather for a short recording.
Every retained grid cell must have all required interpolation endpoints. A cell whose hourly
coverage misses the requested window is omitted with a warning while complete cells remain usable.
Different leading or trailing rows are aligned by their selected UTC timestamps. Cells with matching
selected timestamps keep regular-grid interpolation. When usable cells have different hourly phases,
each retains its own temporal brackets and the existing inverse-distance-squared spatial interpolation
and harmonic averaging of sound speed combine them. The report identifies that interpolation choice
separately from missing cells; a fully covered grid remains complete for cache reuse. A wrong location
count or no complete cell still invokes the existing fallback sources and warnings.

Partial cached grids remain usable offline and still trigger an online repair attempt. A failed or
unusable refresh preserves the cached partial response, as does another partial response with no more
complete cells. This counts coverage, not geographic accuracy. New partial responses are cached too.
A window ending after 23:00 UTC requests the following midnight; an offline run needs that covering
response cached. Complete-grid descriptions count retained hours rather than every downloaded hour.

Radiosonde selection first keeps the existing nearby-station blend. If none of those stations
has a usable sounding, it tries the next-nearest stations within 700 km until one supplies usable
profiles. Preferred and recovery attempts share the existing limit of four stations; a working
preferred blend is never mixed with these farther recovery stations. This also works offline
when the alternative profiles are cached. The atmosphere description names the stations actually
used, counts failed attempts, and identifies recovery from an unavailable preferred station.
Missing temporal brackets retain the existing radiosonde interpolation policy.

| Data | Source | Notes |
|---|---|---|
| ADS-B and MLAT tracks | [adsb.lol](https://adsb.lol) `globe_history_<year>` GitHub releases | ODbL. One global archive per UTC day (`prod` about 3.6–3.9 GB, `mlatonly` about 0.3 GB), streamed and filtered to the region; `staging` is used when `prod` is missing |
| Aircraft engine class | `crates/fdop-types/data/icao_engine_class.tsv` | 2,614 ICAO type designators → piston, turboprop, jet, electric or other. Known jets are dropped by default and retained with `--include-jets`; unknown types are kept |
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
- **Archive coverage follows the fitted distance gates.** The `locate` pipeline fetches
  aircraft out to the largest retained event's upper distance gate, with a 5 km minimum
  and the existing additional 15 km of track context. The former 30 km cap could discard
  flights that the search itself permits. Larger ranges now request larger cached
  footprints and can retain more tracks. This removes that cap; the fixed context
  padding is not a proof of complete coverage for arbitrary wind or curved paths.
- **Wind delay is included before the recording-clock join.** After a candidate ground point
  passes the existing geometry gates, its source-to-observer propagation uses the path-averaged
  atmosphere at that observer, with wind in the observer's frame. The same moving-medium travel
  model is used by exact trace scoring. A far lateral pass can therefore remain inside a tightly
  specified recording clock instead of being discarded by a still-air travel-time seed. Calm
  paths first use the original clock arithmetic. The coarse geometry remains approximate;
  search budgets and confidence gates are unchanged.
- **Wind-delayed passes survive the early source-time crop.** The search first keeps its
  existing scan and sampling phase, then also examines the earlier retained track span
  when the atmosphere is windy, varying or unknown. Only an explicitly uniform calm
  medium with the same finite positive sound speed as the search and event fit retains
  the original crop alone. All recovered points still meet the original geometry,
  recording-clock, track-gap and terrain checks and use the same hypothesis budgets.
  This prevents a still-air time estimate from excluding a distant pass before its
  moving-medium travel time is evaluated. It does not replace the approximate spatial
  reach or establish complete terrain coverage. Extra coarse work can be substantial
  for late events on long retained flight tracks; the catalogue's 1200 s pre-padding
  is relative to recording start, not a per-event work limit.
- **Wind-scaled distances survive the spatial shortcuts.** The coarse search retains its
  original padded region and expands it using the upper physical/fitted distance bound
  for the locally sampled medium, including narrow regions between sampling times.
  Additional-flight confirmation uses the raw fitted-distance shortcut only for an
  explicitly uniform matching still-air medium; otherwise exact CPA, speed, distance,
  clock and trace checks decide. This can recover a flight that the corrected gates
  permit but the uncorrected distance excluded. Weather-dependent confirmation can
  require more CPA evaluations. Its existing emission-time windows and the coarse
  terrain approximation remain unchanged; this is not complete coverage of all media.
- **Hillside rejections can reach the observer's exact closest approach.** The initial CPA line
  uses terrain beneath the aircraft. A climb or descent over a slope can therefore imply the
  wrong distance or recording start at a candidate observer, even when the real pass fits. If that point
  passes the coarse geometry and region checks but fails the clock check, and the observer's
  terrain differs, the search retries the nearest exact CPA in its existing 150 s window.
  A geometry rejection can use that same retry even when another point or a later visit by
  that track already supplies a hypothesis. Each source-time sample retains its ordinary
  points and successful fallbacks before retrying rejected geometry. The retry also clips the
  approximate cross-track ray to the requested box, so a narrow region can supply a candidate
  even when the approximate distances all fail. Nearby points with different recording-clock
  evidence remain eligible; proximity alone does not make their hypotheses interchangeable.
  The exact pass must still pass the original clock, speed, distance, track-gap, interior-CPA
  and terrain-clearance gates. Already accepted hypotheses keep their original arithmetic;
  recovered points count against the existing hypothesis budgets, and confidence rules are
  unchanged. The extra exact-CPA work can increase search cost. This retry does not replace the
  approximate terrain projection or guarantee complete coverage of a hillside.
  The native `known_clock_hillside_flyovers_reach_the_hypothesis_join` regression now runs
  through search and refinement. Before the retry, grades +0.1, +0.2 and -0.2 produced no
  hypotheses or location; all three now recover both intended core matches within 0.494 m
  and 0.005 s, with no truncation. The flat control, wrong-clock and physical-gate controls
  pass too. These are synthetic search cases; the reference and field recordings have not
  been replayed.
- **Rejected reports cannot hide a separate supported flight segment.** Track cleaning keeps the
  original longest physically consistent chain, then can recover another supported chain wholly
  before or after it. Every retained interval must be separated by more than the existing 180 s
  gap limit and physically reachable from its neighbours under the same speed/vertical limits.
  Each interval becomes a separate model; interpolation never crosses the hole. The original
  model and existing split keys stay intact; additional models use deterministic `#recoveredN`
  suffixes. Overlapping alternatives and a denser ambiguous winning component remain unresolved.
- **Unusable GNSS reports cannot erase an otherwise supported corrected-height flight.** If
  preferring GNSS-height reports removed other points and leaves no usable track model, the
  builder retries the reports whose barometric heights were already corrected to MSL. The retry
  keeps the same position, velocity and gap checks and requires at least three retained reports
  in each model. It cannot use raw pressure altitude, join a real coverage gap, or replace any
  model the preferred path already built.
- **Missing speed reports do not impose a 200 m/s aircraft limit.** If the established track
  cleaner finds no usable model and every groundspeed report is missing, three consecutive
  positions can establish a faster motion scale. Their adjacent horizontal velocity vectors
  must agree within the existing 25 m/s tolerance and obey the existing gap and vertical-motion
  limits. The inferred consistency envelope is capped at the nominal sound speed. The same
  cleaner then retries once, and velocities still come from the retained positions. Already
  supported tracks and finite speed reports keep their existing treatment; unsupported fast
  turns and fast tracks hidden by a supported rival remain unresolved. This recovers geometry
  for both searches without changing engine selection or the confidence gates.
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
- **Compressed images also use contrast-first recovery.** The swell that compresses an image also
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
  After every existing stage, including the fast banks, a final contrast-first edge scan can recover
  a pass whose short clip cannot cover a full template. It uses the ordinary edge bank's coverage guard,
  the same residual ownership mask and strong line, and only the remaining candidate budget and
  detection slots. Before acceptance, native survey frames must support one coherent Doppler curve
  inside the seed's original frequency/time cell. The check uses the original sparse sample count, so
  denser sampling cannot raise the score merely by adding correlated observations. It searches exact
  frequency-rounding intervals and bounded subframe clock phases, retaining owned samples as zero.
  Earlier detections retain priority; recovered seeds carry `from_contrast`, preserving
  the existing guard on answers that depend on expanded detection. Four fixed 30-second synthetic
  clips now reach physical measurement where the original stage sequence found no detection (Testing).
- **A supplemental bank covers faster subsonic passes.** After the primary, edge and contrast stages, templates
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
  fast-edge PCM recovery assertion still fails in the native baseline: its early 215 m/s case adds
  no fast-edge seed. The contrast-edge change does not repair or weaken that separate assertion (Testing).
- **A shorter survey can supply the missing second aircraft.** A steep fast pass can smear
  across frequency in the ordinary one-second survey. If fewer than two independent aircraft
  survive ordinary measurement, a half-second survey first searches fast edge templates at the
  recording's true ends, using at most 48 seconds of audio from each end. It then retries full
  fast templates around detections that actually failed measurement, within bounded neighborhoods
  using their time scales and the existing ten-second suppression floor. These interior crops
  include complete template support; their seams cannot supply artificial edge detections.
  These earlier recovery candidates are measured before deciding whether the final bank is
  needed. Only accepted measurements own evidence between stages: a failed full-template seed
  remains in the detections and spends its original slots and proposals, but cannot hide a
  valid shorter path or make it an unmeasured harmonic child. If fewer than two independent
  aircraft remain, a final interior bank uses ten-second halves for fast transitions whose natural shortest
  support fits that interval. It scans complete paths on the half-second survey, including
  centers with no ordinary seed and neighborhoods the longer bank already tried. Every scored
  center has actual STFT support on both sides. Tiles cover at most 120 seconds of centers,
  with at most 141 seconds of working audio under the defaults.
  An accepted aircraft keeps its original measurement and owns its
  observed ridge and detected harmonic family, even when its fitted geometry is inaccurate.
  Ownership interpolates only across traced gaps of at most three seconds and never extrapolates;
  occupied samples contribute zero without shortening the scoring denominator. The retry shares
  the remaining candidate allowance and 60 detection slots, uses the same strong line, and sends
  its seeds through ordinary refinement and family measurement. Original and recovered aircraft
  share the 24-event limit. Every retry event counts as measured past the old line, including one
  whose fitted speed falls. Warnings record added candidates and any cut. Recordings with two
  independent aircraft already measured keep their original path. The short interior bank does
  not need an ordinary detection to identify a retry window, but cannot replenish an exhausted
  budget. If the remaining proposal allowance or detection slots stop that bank, a separate
  warning reports that some interior audio was not searched with its short templates.
- **STFT working storage stays bounded as recordings grow.** Frames transform into a reusable block of
  at most 256 columns and 4 MiB of frequency values, then copy into the final row-major spectrogram.
  A band wider than that budget still needs one complete column; parallel FFT scratch is separate.
  The final spectrogram still scales with recording length, but a second complete spectrogram and a
  separate allocation for every frame are no longer retained. Frequency bins and frame centres are unchanged.
- **Robust fits choose their winner with the robust objective.** Multiple starting geometries
  are solved with soft-L1 loss at a 0.5 Hz transition scale, and now compete using that same loss.
  A few large jumps to an unrelated ridge must not select a different aircraft merely by
  reducing ordinary squared error. The existing outlier trim and final linear fit follow
  selection; uncertainty profiles still use their original linear residual sums. No measurement
  gate or confidence threshold changes with this correction.
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
  three-bin interpolation, including a vertex between the selected bin and a neighbouring basin
  boundary; without one, the normalized vertex stands during discovery. A restored vertex stays inside the original
  frequency tolerance, so restoration cannot cross to another ridge or a removed hum, and the original
  SNR/z thresholds stay in force. There is no climb toward a raw maximum several bins away: tried,
  it made Livermore distances worse and raised the evidence of wrong-date runs. No extra full
  spectrogram is retained.
- **Accepted aircraft can improve their frequency and geometry without changing discovery.** After
  the ordinary, free-trace or fitted-continuation path supplies a usable aircraft event, an optional refinement uses only
  qualified peaks whose local three-bin raw profile supports the measured vertex. A displaced normalized
  peak with no local raw vertex cannot bias this fit. The candidate must independently pass every
  original quality gate and aircraft bound on its own retained observations; otherwise the original
  accepted measurement stands. A recovered continuation uses its own spectrum and restored row
  background, including any renewed spectral band, and must also retain continuous support on
  both sides of the closest approach. Refused events keep their existing paths.
  Final coverage, uncertainty and diagnostics use the selected trace and fit together. This adds at most
  one ordinary budget of guided rounds to an accepted event; real-recording runtime has not been measured.
- **Steep passes get a matched window.** A 0.5 s main window stops holding a chirp at 1/win² = 4 Hz/s. Frames
  steeper than that read a second spectrogram, with the window that matches the latest fit's maximum
  received chirp. That maximum occurs d/c before the heard closest approach and is larger than its CPA
  rate by (1 - (v/c)²)^(-3/2); the public CPA-rate readout keeps its existing meaning. There the
  peak integrates along the model's curve, the band is at least 0.25 s of chirp to allow for detection's error,
  and the noise reference spans four times the ridge's half-width (half a main lobe plus half the chirp's smear),
  as the main window's does. Both windows exclude that signal width from the noise estimate while retaining
  the original final frequency tolerance. A real tone's skirts are wider than a pure tone's; at twice, they filled the
  reference once the band narrowed, and later rounds dropped steep frames whose ridge stood 8–14 dB up.
- **A fit needs a ridge through its drop.** Two steady tones at different pitches fit a Doppler curve with nothing
  between them, at an excellent RMS. `drop_traversed` in `events.json` is the share of the central 10–90 % of the
  fitted drop that the trace travels: each continuous stretch adds its net fall, and a stretch ends at a time gap
  or where the trace steps more than 2 Hz off the model's own step. Under 15 % the event is refused. It still
  counts as heard, but it does not use an independent-event slot.
- **A one-sided refusal is traced once more, without the model.** A template of the wrong shape (at the edge
  of the bank, or with speed and distance wrong together) meets the ridge over one stretch and leaves the band
  elsewhere, so the guided trace holds one side of the pass and the gates refuse the fit. When a fit is refused
  and its retained observations cover less than 8 s on either side, the ridge is followed by its own slope from that trace's
  first, strongest and last points, inside the event's analysis window. The free trace is fitted, and the guided
  rounds start again from that fit if it is plausible. The second attempt replaces the first only if every gate
  accepts it; `reseeded` in `events.json` marks it. Side coverage uses the fitter's existing retained timestamps;
  discarded frequency outliers cannot create apparent approach/recede support or block this retry.
  Raw trace points, SNR and central-drop accounting remain unchanged. Accepted fits, and refusals whose retained trace held both sides, are
  measured exactly as before. A wrong fit can pass this way too: a climbing pass in a clip that starts mid-drop
  trades speed against distance, and the retry accepts the wrong pair.
- **A fast fitted ridge can regain missing side or central-drop coverage.** When the fitted speed is
  already beyond the primary bank's 175 m/s limit and a steep fitted ridge fails only the required
  approach/recede span or central frequency drop, measurement can continue that same fit. A missing side
  renews its spectral band and window after free tracing fails. A missing central drop preserves the
  original spectrogram and the final narrowed tracing band, so continuation does not restart discovery
  or lose the ridge it was converging on.
  It continues the same ridge for at most twice the ordinary fitting budget, capped at six rounds. After
  completing the ordinary rounds, it stops when all gates pass or coverage stops growing on the missing
  side. Both eight-second sides must belong to the retained ridge through the fitted CPA: a gap longer
  than the existing three-second continuity limit ends that ridge. Isolated retained points cannot stand
  in for a missing approach. The original recording interval, normalization context, detection identity
  and CPA-drift limit remain in force. An accepted continuation retains its actual spectrum and is marked
  `reseeded`; a failed one preserves the original refusal. Existing accepted measurements keep their
  complete output. The short-fast-pass regressions below check recovered geometry and this preservation.
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
  If fewer than two independent aircraft remain, an unused sibling of a complete refused
  primary can also be measured as a separate event. It must have distinct two-sided observed
  motion from every complete refused member of that family. The primary remains refused;
  compatible motion or insufficient overlap refuses the sibling too, with its trace and fit
  retained for inspection. All extra attempts retain expanded-measurement provenance.
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
- **A narrow region can also lie between sampled moments of a flight.** If consecutive
  closest-approach lines pass on opposite sides of the entire requested box, the search finds
  an intervening line through it, re-evaluating the track and terrain during the search. The
  final partial time interval is included. This retains the regular sampling phase and sends
  every added point through the same speed, gap, terrain, distance and recording-clock gates
  and hypothesis budgets; it does not reduce the timestep across the whole flight catalogue.
- **Terrain is checked across the full cross-track reach.** A hillside can bring a later observer point back
  inside the slant-distance gate after a valley falls outside it. One failed point no longer cuts off that side.
- **A location still needs two events after clock checks.** A candidate pruned to one flyover is removed.
  Newly promoted leaders receive the same clock checks before the winner is chosen, even when many higher
  scoring coincidences fail first. Separate passes by the same aircraft remain supported.
- **Returning aircraft keep distinct physical passes.** A hypothesis's emission time selects the
  nearest interior local closest approach in time, so a closer return elsewhere in the window does not
  replace its pass during pair scoring, clock checks or refinement. Confirmation considers each local
  approach inside its propagation window, extending that window when the event's profiled CPA-time
  uncertainty reaches farther. Both the catalogue query and the local approach scan retain those
  times, with enough scan support to distinguish a true approach from an artificial window edge.
  Existing intervals and sampling stay when they already contain that support. The same timing,
  geometry and trace-fit gates apply.
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

Nearest-pass searches now visit the original two-second lattice outward from the proposed emission
time. After finding an interior closest approach, a direction stops only when every remaining
refinement bracket is strictly farther away in time. The lattice, refinement, tie order and boundary
preference stay unchanged, with constant scratch space. The native track suite passes 36 tests,
including 3,087 comparisons of every result bit against the exhaustive calculation.
`nearest_cpa_benchmark` runs 20,000 queries with a 150 s half-window in each of six alternating-order
trials: exhaustive 256-422 ms, outward 43-107 ms, median paired speedup **5.27x** (range 3.02-6.31x).
This uses the pinned nightly at optimization level 1 on a shared AMD EPYC 9V74 Linux x86-64 host. It is a
closest-approach component measurement; no whole-recording speedup is claimed.

Pair joining now discards duplicate flight/place/clock seeds as they are generated, using bounded
batches of 4,096 input hypotheses and ordered worker chunks. Previously it held every compatible
raw pair before deduplicating: a dense bucket could exhaust memory before the quick-check budget
was considered. The hash index, deduplication keys, active worker chunks and quick survivors still
consume memory; this is not a fixed total-memory bound. The same first representative, candidate
order, spatial and clock cells, and complete comparison are retained. No detection cap, search
budget or confidence line changes.

Complete representatives now reach the quick trace check in groups of at most 4,096 seeds while
their event pair is still being joined. Rejected seeds therefore do not accumulate until the end
of even one large pair. This bounds the completed-output buffer, rather than the complete search:
the join's deduplication keys and active worker chunks still grow with the data. The joined-seed
budget counts rejected seeds and stops at the same complete-pair boundary. Hypothesis clouds are
released before fine scoring, and fine-grid input seeds before confirmation and refinement.
Later stages retain their surviving candidates; the changes impose no additional search cut.

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
the additional per-pair quick-scoring and early-release changes. These measurements also predate
streaming completed outputs into the quick check; that follow-up has native ordered-output,
bounded-delivery, budget and positive-localization regressions, but no new recording measurement.

The exact-curve scorer now builds one conservative ECEF box per shortlisted trajectory and reuses it
across candidate locations, clock alternatives, refinement and decoy windows. The box encloses the full
Hermite curves, including turns outside the reported points. A trajectory proven farther away than the
existing reach limit skips the repeated closest-approach scan; uncertain geometry keeps the original
path. This does not change the reach limit, score formulas, candidate order or confidence thresholds.
The geometry and complete exact-score equivalence regressions now pass in a native core build.
Whole-recording runtime measurements remain pending; no end-to-end speedup is claimed.

Ridge tracing prepares each integration frame's time interpolation once and reuses it across the
frequency profile. Frequency interpolation, summation order, peak qualification and the three-bin
raw-background correction remain unchanged. In the native `ridge_profile_validation` component run,
384 original/current full-profile cases returned identical frequency, SNR and z-score bits, including
fractional timestamps, clipped edges and restored backgrounds. Seven alternating-order trials of
2,400 calls each gave a median paired speedup of **1.69x** (range 1.56-1.79x): original 204-292 ms,
prepared-time 117-164 ms. This used the pinned nightly with optimization level 3, one codegen unit,
real dependencies and the complete original/current trace modules on a shared AMD EPYC 9V74 Linux
host. It measures synthetic ridge-profile calls, not an end-to-end recording or its accuracy.

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

### Wind changes fitted speed and distance by different amounts

The search now uses the exact homogeneous-wavefront geometry when comparing a physical
aircraft with the speed and distance fitted in still air. For an independent synthetic example
with sound speed 320 m/s, aircraft ground speed 180 m/s, headwind 80 m/s and closest distance
3000 m, the equivalent fit at 343 m/s has speed 242.117647 m/s and distance 3321.083220 m.
The former shared factor places the ordinary speed gate's lower bound at 182.0065 m/s,
excluding the true aircraft. Separate conversions retain the original measured intervals.

Three new track regressions check independent moving-wavefront observations, coordinate
rotations and reference shifts, conservative CPA-direction bounds, calm controls and invalid
inputs. Independent numerical reproduction passed 450 waveform observations (maximum
frequency discrepancy 4.843e-7 Hz) and 4332 direction-bound cases. These are arithmetic checks,
separate from the native Rust validation below.

A search regression supplies propagated traces to the actual constant-velocity fitter and
measured-event adapter, then exercises the pair search, confirmation and final reevaluation
at relative and Unix-epoch clocks. It retains ordinary uncertainty intervals and includes
wrong-speed, wrong-distance and wrong-clock controls. Two earlier synthetic fixtures now
use physically fitted geometry instead of the old approximation; their original recovery
assertions and tolerances remain in place.

Native validation resumed successfully after the execution host recovered. With the pinned
`nightly-2026-08-31` LLVM toolchain, all six focused regressions pass, and the complete track
and search modules pass **145 tests, with 4 ignored and no failures**. The actual-fitter
regression recovers both intended aircraft at relative and Unix-epoch clocks, with observer
errors **0.182087 m and 0.182078 m**, zero clock error to six decimal places, and no search
truncation. Wrong-speed, wrong-distance and wrong-clock controls pass too.

That regression's first received sample needs an emission before its original synthetic track
boundary. Extending the generated source track from -100 s to -300 s supplies that observation;
the 90 s half-window, gates and recovery assertions are unchanged. Native validation also
corrected a GNSS-gap fixture that had accidentally relabelled missing corrected reports as
dense GNSS support. It now checks its preferred-only and corrected-only prerequisites before
testing recovery, under the unchanged production rules.

The broader core run encountered the already documented
`curvematch::tests::grazing_passes_are_heard_across_exact_clock_seeds` failure and was interrupted;
it is not a passing full-suite result. At this validation snapshot, workspace Clippy/tests,
release selftest and reference/field replays had not completed. These synthetic trace cases
cover fitting through final search reevaluation; they do not establish real-recording accuracy
or an end-to-end runtime improvement.

### Recover the physical pass when a trace jumps to another ridge

The new `fit::tests::robust_multistart_recovers_a_pass_after_a_terminal_ridge_jump` regression
uses 121 synthetic frequency observations of a 200 Hz source moving at 50 m/s, 3 km from
the observer, with deterministic 0.08 Hz scatter. Its last six observations jump down by
120 Hz. The expected result retains the original pass and rejects those six observations.
A second regression covers clean joint, approach-only and recede-only fits. Existing profile
and uncertainty tests are unchanged.

An independent SciPy reproduction of the solve/trim/refit sequence found the previous
selection returning a false 199.8 m/s pass at 255 m, while robust-objective selection
returned 49.997 m/s at 2999.6 m and retained the 115 uncontaminated observations. These
are numerical reproduction results, not execution of the Rust fitter or a recording replay.
The pinned native build lost its execution host during dependency compilation; the new
Rust regressions, full workspace checks, release selftest and field/reference replays
remain unverified. No real-recording accuracy improvement is claimed.

### Aircraft hidden inside coarse harmonic families

The sibling scan also includes complete primary ridges refused by the aircraft or fit checks.
New regressions synthesize a slow tonal source beside an aircraft and require the aircraft to
remain eligible only when their measured motion differs. A true-harmonic control, including
misleading fitted metadata, must remain refused; missing or one-sided overlap cannot establish
independence. These new regressions have not run: both native compiler backends were killed in
pinned `fsci-stats` before the core test binary was built, then the execution host disconnected.
Their expected recovery is not a measured native or real-recording result.

The survey's coarse harmonic grouping can put two independent aircraft in one family.
When measurement leaves fewer than two independent passes, the shared `detect`/`locate`
path now measures that family's unused detected siblings before scanning for new fast
passes. It retains the original measurements and stops when two aircraft are available.
Extra attempts use the existing quality gates, detection budget and expanded-measurement
provenance; no detection or confidence threshold is lowered.

`observed_siblings_recover_aircraft_hidden_by_coarse_harmonic_grouping` synthesizes
200 and 400 Hz passes at 88 and 100 m/s, both 1,492.442 m away at closest approach.
At source amplitudes 0.075 and 0.05 against the same seeded noise, the ordinary family
path supplies one aircraft; independent sibling measurement supplies two. With the
recording start supplied, the actual search and refinement recover the synthetic observer
within 0.2951 m and 0.3064 m, respectively, with both intended core flights and no
search truncation. These are synthetic results, not field-accuracy measurements.

True harmonics must still count once. Known family members are compared using their
observed ridges even if fitted closest-approach times or frequencies differ. An alternate
seed retains the family links of measurements it duplicates, so changing the strongest
representative cannot create an extra aircraft. Those links permit an observed comparison;
distinct measured motion still remains independent. Controls cover uneven sampling,
partial coverage, displaced fit metadata, alternate seeds and multiple linked families.

On the recovery build before merging the concurrent wind/weather commits, all eight
focused core measurement and family-selection tests passed. Another 45 physics, track
and two-flight search tests passed in the actual core test binary; one existing timing
benchmark was ignored. The exact shared pipeline source, linked to the real core in a
temporary native runner, passed the quiet/loud recovery test. Substituting only the
pre-change `measure_recording` from `1bdb123` made that same test fail with one aircraft
instead of two. The original harmonic pipeline control retained one independent aircraft
and its original measurement, then failed an obsolete whole-record-vector equality:
the feature deliberately retains inspected sibling records. Its replacement checks exact
primary/spectrum preservation, known siblings and provenance, but has not been rerun.

After incorporating `e5cca91`, the combined core test harness compiled successfully.
The normal library rebuild was killed, and the execution host disconnected before the
nine combined family/wind test results or the null-audio result could be read. The earlier
unchanged-source native selftest passed at 1.674 m position error, 75 m radius and 0.020 s
clock error; the final combined selftest remains unrun. Full root compilation was killed
in locked `asupersync 0.5.0`. Workspace Clippy/tests, release CLI selftest and reference/field
replays remain incomplete; the native runners are not a full application-build pass.
The subsequent track/ingest commits through `d1e4014` are preserved, but the final
combined tree has not been run.
Changed Rust ranges passed formatting and whitespace checks before publication; the
workspace-wide formatter still reported existing unrelated differences.

### Fast tracks with missing groundspeed reports

`missing_speed_reports_do_not_erase_a_supported_fast_flyover` covers 215, 245 and 270 m/s
position-only tracks sampled every 30, 10 and 45 seconds, respectively, at two headings
and at zero and Unix epochs. All twelve cases retain their genuine positions, reproduce
position and velocity within 0.001 m and 0.001 m/s, and match received Doppler within
0.001 Hz for a 100 Hz tone. Disabling only the new recovery makes the first case produce
no model. `position_only_fast_recovery_requires_coherent_subsonic_support` covers isolated
corrupt reports, persistent impossible jumps, inconsistent directions, finite speed
reports and preservation of an already-supported competing chain.

All 43 active tests in the actual `physics.rs` and `track.rs` modules passed through a
temporary native runner linked to the real `fdop-types`; one existing timing benchmark
was ignored. This used the configured nightly and Cranelift with optimization disabled.
Whole-core builds using LLVM and Cranelift were killed in the `fsci-linalg` dependency
before project tests. This module validation is not a workspace or release-selftest pass,
and reference/field recordings have not been replayed for this change.

### Failed recovery seeds leave shorter physical curves eligible

`short_recovery_after_refused_legacy_measurement_finds_physical_pass` uses fixed 180-second
synthetic PCM with a 350 Hz pass at 215 m/s and 900 m, heard closest at 80.13 seconds,
at amplitude `0.32 * 10^(-3/20)`, with seeded noise and a steady 250 Hz interferer.
Ordinary measurement supplies no aircraft. The former combined recovery returns a
24-second-half seed whose measurement is one-sided, traverses none of the central drop
and fails the speed gate. Its predicted ridge used to mask the later short bank.

The staged recovery measures that earlier seed before forming the short bank's owners,
retains its detection and proposal costs, and supplies a ten-second-half seed for ordinary
physical measurement. The matching pipeline regression,
`short_window_recovery_releases_refused_legacy_evidence`, requires the refused candidate
to remain counted and reported, the successful short candidate to retain expanded-measurement
provenance, and exactly one independent aircraft. Accepted observed ridges still own their
sound; original proposal, detection and event limits and all measurement gates remain.

The native core regression passes with 350.00314 Hz, 214.99825 m/s, 900.00779 m and
80.12993 seconds, at 0.03317 Hz fit RMSE. Its accepted-ridge control yields no duplicate
candidate, and the budget/coverage checks pass. The exact `measure_recording` function,
its real helper items and the new pipeline test also pass in a temporary native
test binary linked to the same core. Replacing only that function with its pre-change
body fails the same assertion with zero independent aircraft. This tests the actual
measurement orchestration; it is not a full root-crate build or release CLI run.

Four existing pipeline controls also pass in that same native binary: noise, steady tones
and reversed audio produce no aircraft; accepted harmonic families and partial ridges
retain their measurements without duplication; all four missing-second-aircraft cases
pass. Their three search/refinement subcases, with the recording start supplied, recover
the synthetic observer within 1.77001 m, 0.23455 m and 0.05090 m, with both correct core
flights and no truncation. The fixed tests retain their original assertions.

The unchanged `src/selftest.rs` module also passes again against this core in a native
runner: its original 1300-second audio, ffmpeg decode and +/-6-hour clock search recover
the synthetic observer within 1.674 m, with a 75 m radius, 0.020 s clock error and both
intended flights. That selftest uses its own measurement path rather than the recovery
orchestration tested above.

Core all-targets clippy and changed-file formatting pass. The full native workspace build
was again killed with `SIGKILL` in pinned `asupersync` before root compilation, and the
workspace formatting check still finds unrelated pre-existing differences. These are
synthetic results, not evidence of field accuracy.

### Brief interior fast passes can reach physical measurement

`short_interior_fast_recovery_finds_physical_passes` compares the complete earlier recovery
path with the new short interior bank on the same 180-second synthetic PCM. A physical
83 Hz pass at 245 m/s and 700 m, heard closest at 79.37 seconds, produces only refused
ordinary measurements, and the earlier edge/refused-seed recovery produces no usable aircraft.
The ten-second interior template supplies a seed that ordinary family measurement fits at
83.0066 Hz, 244.9895 m/s, 699.9509 m and 79.3696 seconds, with 300 traced points and 91.5%
of the central frequency drop traversed. This is a native synthetic recovery, not a real
recording or a claim of broader field accuracy.

`short_interior_fast_recovery_supplies_a_second_independent_aircraft` adds a separate 450 Hz,
90 m/s pass at amplitude 0.04. Ordinary measurement supplies one independent aircraft, and
the complete earlier recovery path supplies no second one. The new bank supplies the missing
fast pass. With the recording start supplied, actual pair search and refinement recover the
synthetic observer within 0.051 m, with both correct flights as core matches and no funnel cut.
The harmonic PCM control still yields exactly one aircraft. Existing observed-owner,
central-drop-removed, reversed-audio, noise-plus-hum and exhausted-limit controls also pass.

The pipeline retains the existing detection and measurement budgets, records recovered events
as measured past the old line, and requires the same quality and confidence gates. Its matching
two-aircraft bookkeeping regression now passes through the exact-source native orchestration
described above. Full root-crate checks remain blocked: compilation stopped with `SIGKILL`
in pinned `asupersync`. Core all-targets clippy,
the focused native regressions, and formatting of the changed Rust files pass. The workspace
formatting check still reports pre-existing differences in other files.

The unchanged `src/selftest.rs` module separately passed its original 1300-second WAV, ffmpeg
decode, ordinary detection/measurement, wide region and +/-6-hour clock search: 1.674 m error,
75 m radius and 0.020 s clock error, with both intended flights. A temporary runner linked the
actual module against the pinned nightly's unoptimized Cranelift build; this was not the release
CLI or the recovery orchestration. Its snapshot precedes the concurrent `5fb4a17` jet-policy
filter. Subsequent combined-source checks pass the jet-policy regression, all three CPA-time
regressions, the unchanged narrow-clock fingerprints and recovery-limit tests. Full workspace
tests, the release CLI selftest and real-recording replays have not completed.

### Opt-in jet catalogues and complete search wiring

The new regressions exercise compressed jet traces, cross-feed type reconciliation,
certified cache coverage, and adjoining-day geometry whose aircraft type arrives from
another day. An ordinary cache, a renamed old cache, a false marker and a string marker
must all fail an expanded offline request. Certified expanded main-only caches retain
the existing missing-MLAT warning.

The search regression requires synthetic jet-labelled tracks to yield no hypotheses
under the default policy, then recover the known observer within 60 m and its clock
within 1 s when enabled. It also puts an excluded jet before eligible propeller tracks
to verify that filtering preserves original track indices. Pipeline regressions carry
the flag through actual offline cache loading, synthetic audio decoding and measurement,
trace localization, saved result metadata, all four trace decoys, and all 48 curve-match
decoy windows. These use synthesized harmonic tones, not a jet-specific noise model.

**Validation is incomplete.** On the pinned nightly, the two-job LLVM build was killed
with SIGKILL in `fsci-special` and `fsci-stats`; a single-job retry was also killed in
`fsci-special`, before any project test ran. The execution environment then disconnected.
Source review and recovered GitHub content checks completed, and the workspace formatting
baseline already failed on existing files. Those were the validation limits at `5fb4a17`.
The subsequent core jet-policy regression and core all-targets clippy passed as recorded
above; the archive/pipeline regressions, full workspace and release CLI checks, and
reference/field replays remain incomplete. No real-recording accuracy improvement is claimed.

### Broad CPA-time profiles remain eligible for confirmation

`uncertain_cpa_times_keep_physical_confirmers` checks the search-event uncertainty contract with
an unchanged physical trace and recording clock. A third aircraft whose nominal CPA estimate
is early or late must reach exact scoring, remain a core confirmer after actual search/refinement,
and retain the existing wrong-trace, wrong-clock and same-pass exclusions. It runs at zero and
Unix epochs. This is a direct search-input regression, not a claim that measurement produced
the deliberately displaced estimates.

`uncertain_cpa_window_covers_both_observed_sides` separately uses a 560-second physical trace
with both displaced estimates inside its observed support. Its stored flight already survives
the catalogue query; the old local closest-approach window alone excludes the true pass.
`uncertain_cpa_time_boundary_keeps_close_pass_interior` checks a nearby aircraft just inside
the exact time gate, where the scan needs a small margin to recognize an interior approach;
moving it outside the time gate still refuses it. The change retains the existing propagation
approximation and does not establish completeness for arbitrary wind delays. All three
regressions pass. The first two fail against the old windows, and the close-range case fails
without the extra scan support;
the three-flight search recovers the synthetic observer within 0.56 m and its clock within
0.005 s. Five neighboring narrow-window, wind and repeated-pass controls also pass, along
with core all-targets clippy. Native runs use the pinned nightly's Cranelift backend; full
workspace checks, release selftest and real-recording replays have not completed.

### Resume raw ADS-B recovery after the geoid correction changes

Raw-cache regressions cover conversion in both directions, exact reuse at the original
reference, preservation of missing-altitude bits and all unrelated report fields, and
rejection of invalid references or incompatible release/filter policies. An offline
fetch regression rebuilds main plus MLAT feeds from existing checkpoints after a geoid
change and compares the result with fresh ingestion at the new reference. It also checks
that the original checkpoint bytes stay unchanged, preventing cumulative corrections
on later retries. The comparison uses exactly representable reference shifts; arbitrary
shifts retain the raw format's existing f32 altitude precision.

These Rust regressions were authored during an execution-host outage and have not run.
Source review and independent height-reference arithmetic checks completed; workspace
checks, live archive recovery and real-recording validation remain unverified.

### Rebuild interrupted day catalogues offline

Seven focused regressions call the real offline `fetch_window` entry point with cached
synthetic raw feeds and release metadata. Reconstructed results must equal a fresh joint
build, including the main feed's barometric corrections and an initially unknown type
resolved from MLAT. The cases cover prod and staging, ordinary and jet-inclusive policy,
repeat processed-cache hits, existing partial-cache priority, absent/disabled MLAT,
corrupt-prod staging fallback, MLAT-only refusal, mismatched metadata and identities,
and fatal final-publication failures even when missing days are otherwise permitted.

The recent-day regression also calls `published_until`, as both CLI entry points do,
before reconstruction. Complete main checkpoints with same-day metadata reach the offline
fetch for an ended day; metadata alone, MLAT-only data, unfinished temporary files and
wrong-day listings do not. An unfinished day stays pending even with a raw main checkpoint.

Pinned-nightly formatting of all changed Rust files and `git diff --check` pass.
Independent bounded source review found no blocker. A one-job, unoptimized
`cargo clippy -p fdop-data --all-targets --locked -- -D warnings` attempt exited 255
while compiling `synstructure 0.13.2`, before checking `fdop-data`; its log contains
no Rust diagnostic explaining that exit. These new data regressions have not executed.
Full workspace checks, release selftest, live-download timings and field replays remain
unverified for this change.

### Resume incomplete archive days from completed raw feeds

Six new regressions cover the raw checkpoint format and its use by the archive worker.
The retry test uses real synthetic tar ingestion, the shared cache-aware worker path,
the existing day collector, atomic day publication and the normal offline fetch entry point.
Across prod/staging, either failed feed and both jet policies, it requires the completed feed
to be opened once and the failed feed twice. Rebuilt flight data and statistics must equal
a fresh joint ingest exactly, including type resolution and barometric altitude corrections
supplied by the other feed. Truncating an archive must leave no checkpoint for that feed.

The module tests cover exact point/boundary float bits, empty successful feeds, every
release/filter/context identity field, renamed jet-policy files, old versions, CRC damage,
truncation, trailing garbage and replacement while an earlier reader remains open.
A worker regression covers corrupt-cache refetch and repair, then a raw-checkpoint write
failure that still permits the successful feed to become a readable day store.

**Native validation is pending.** At `16f56eb`, source review and GitHub checkpoint
verification had completed, but host loss prevented data checks from starting. Subsequent
pinned formatting passed; the data-crate Clippy attempt described above stopped in dependency
compilation before checking the crate. These codec and retry regressions remain unexecuted.
No live-download timing, full-workspace pass, selftest or field-accuracy improvement is claimed.

### Completed main feeds survive a failed MLAT download

The archive regressions now cover both prod and staging, either completion order, actual atomic
cache publication, offline reuse with a warning, rejection of MLAT-only remnants, and partial
caches failing full-feedset lookups. They retain the bounded-channel publication handshake and
verify that strict feed failures and finalized-day cache-write failures remain errors. The new and updated
tests are authored but unrun because the execution host is unavailable; source review is not
native validation.

### Archive coverage for wide distance gates

`archive_catalogue_keeps_wide_distance_gates_and_track_support` exercises offline day-cache
loading and real track interpolation for flights within 31.25 km and 43.75 km fitted gates
that the old 30 km footprint excludes. It also checks a distant negative control and
unchanged smaller footprints. The regression is authored but has not run: the execution
host is unavailable. Native formatting, compilation, selftest and reference replays remain
unverified for this change.

### Partial weather grids: recovery implemented, native validation pending

`partial_model_grids_recover_complete_columns_without_changing_full_grids` checks
complete-grid equality against the original interpolation, independently calculated partial-grid
wind/slowness weights, missing required endpoints, one-cell recovery, total failure and warning
propagation through preferred, provisional and combined sources. A second test uses an actual
loopback HTTP server to check partial-cache repair, preservation after an unusable replacement
or failure cooldown, and caching for offline reuse. These tests are authored but have not run.

The exact pinned native data build was terminated by SIGKILL in `asupersync 0.6.2`
before compiling `fdop-data`. A subsequent metadata-only Clippy attempt failed on shared
disk exhaustion before project code, then the executor disconnected. Changed-range formatting,
whitespace checks and source review completed. No native weather, real-service, location-accuracy
or full-workspace pass is claimed.

### An interior fast pass supplies the second aircraft needed for a location

`interior_fast_recovery_supplies_a_second_independent_aircraft` synthesizes 180 seconds
of PCM containing a 450 Hz, 90 m/s flyover and a separate 118 Hz, 215 m/s flyover. Ordinary
measurement accepts only the slower aircraft. The shorter targeted retry recovers the
fast pass while honoring the accepted aircraft's observed ridge. Actual flight association,
search and refinement then recover both core flights, with **0.234547 m** position error,
zero clock error, a 75 m reported radius and no truncation. The recovered fast fit is
118.000989 Hz, 214.994167 m/s and 899.983993 m against 118 Hz, 215 m/s and 900 m.

This native core regression and a separate ten-row physical-PCM ladder pass on the pinned
nightly. The ladder restores one previously refused 215 m/s row and checks every new accepted
fit and the one-aircraft-per-source limit. Its five 245 m/s rows remain unrecovered. These
are synthetic capability checks, not real-recording accuracy or confidence calibration.

The separate owner/null/budget control was interrupted after six minutes without an assertion
failure; it is not a pass. A narrower version keeps the physically refused seeds fixed while
removing or reversing their sound, preserving the original refusal and ownership assertions.
That version and the extended root-pipeline integration test remain unrun after executor loss.
The standalone and release selftests, full workspace checks, and reference/field replays
were not completed in this continuation. Production detector code did not change after
the two positive regressions passed. Separated retry windows can repeat surrounding FFT
work, and no end-to-end runtime improvement is claimed.

### Wind-scaled ranges and early source times: native search recovery

**Archived fixture results.** The native results below used the former approximate gate
metadata. The current fixtures use equivalent fitted geometry: the reach case uses a
410 m/s synthetic sound speed and 80 m/s east wind, while the omitted-time-window case
uses 90 m/s crosswind. Their recovery assertions are unchanged; those updated fixtures
have not run natively after the execution-host outage.

`wind_scaled_distances_reach_the_pair_join_and_confirmers` places a 230 m/s flight
12 km laterally from the observer in a synthetic 120 m/s east wind. Its then-used approximate
gates pass, but both preceding raw-distance shortcuts exclude it. The preceding code fails
the first hypothesis assertion. The repair recovers both intended core flights at zero and
Unix epochs within 0.163 m and 0.2 s, with no truncation. Direct confirmation also passes;
wrong distance, wrong clock and an already-used pass remain refused. An undeclared provider
gives the same confirmer score. Both complete still-air result fingerprints are unchanged.

The new regression, the still-air controls and seven adjacent wind, terrain, track-bound and
confirmation regressions pass on the pinned nightly. The edited search file passes rustfmt
and whitespace checks. These synthetic tests do not establish real-recording accuracy or
the runtime cost of admitting more weather-dependent CPA evaluations.

The source-time recovery regression constructs two physical flight traces, including a
20 km lateral pass in 75 m/s wind whose valid closest approach lies before the previous
still-air crop. It requires the existing search/refinement helpers to recover both core
flights with chi below 1, position error below 75 m and clock error below 0.2 s, at both
zero and Unix epochs. It repeats the search with an undeclared custom atmosphere and
checks that a zero hypothesis-storage budget retains the exact count while storing no cloud.
A separate 12-flight control compares every retained calm hypothesis bit for bit, counts
additional atmosphere evaluations, and checks that differing fitting sound speeds disable
the still-air shortcut. A types regression checks the exact uniform-medium declaration.

The source-time assertions now pass natively: the four declared/undeclared-medium runs
recover within 3.918 m and 0.042 s. The twelve-flight cloud comparison and all 30 shared-type
tests pass too. Earlier dependency builds and one subsequent core rebuild received SIGKILL;
a sequential retry completed on the exact pinned toolchain. The radiosonde test, Clippy,
selftest and reference/field replays remain pending at this search commit.

### Hillside recovery survives other valid visits by the same track

`hillside_recovery_survives_other_ordinary_visits` extends two climbing/descending trajectories
with later level returns at incompatible recording clocks. The earlier hillside passes still
explain the same observed curves, but the preceding whole-track guard keeps only the returns:
there are no joined seeds and no location. Retrying rejected geometry per source sample retains
both visits and recovers the synthetic observer within 0.547703 m, with a -0.005003 s clock error
and no truncation. A separate wider-region control keeps the valid hillside point when an
ordinary point lies farther across the same sampling slice.

All three hillside regressions and six neighboring budget/region controls pass natively on the
pinned nightly. Two existing scenes now correctly retain two hypotheses instead of one: the
points lie within 50 m but carry different coarse clocks. The tests require the explicit counts,
exact zero-slack physical validity of every point, the original location and clock bounds, and
the existing wrong-clock, gap, clearance, speed and range refusals. Zero, one and exactly enough
storage budgets report the same total count. This is synthetic search validation; additional
exact evaluations may increase search cost, and no reference or field replay is claimed.

The unchanged production `src/selftest.rs`, compiled against the combined rebuilt core and
type libraries, also passes its real FFmpeg decode → detect → measure → search → refine chain:
1.673779 m error, 75 m radius, +0.019586 s clock error, four measured events and both intended
flights. This validates the engine integration; the full release CLI build is not established
by that standalone entry point.

### A terrain distance rejection no longer hides an otherwise valid pair

The following records the earlier empty-cloud-only retry. The mixed-cloud repair above adds
valid hypotheses in two of its positive scenes; the cloud-preservation and timing comparisons
below describe that earlier version.

`hillside_cpa_survives_an_approximate_distance_rejection` uses two crossing flights about 6 km
above the observer, climbing and descending at 15 m/s, with the normal three-second timing
tolerance and 25% multiplicative distance floor. The actual range lies near the gate's upper
edge. On a 40% planar hillside the preceding source rejects both approximate ranges and never
reaches the valid exact CPAs; both event clouds are empty and there is no location. The bounded
second geometry pass recovers one matching hypothesis per event and, for the 6,650 m lateral
case, locates within 0.351176 m at exactly the known recording clock.

The new native regression passes twelve full-search scenes across three grid phases and four
terrain slopes, including wrong-clock, gap, clearance, speed, range and zero-cap accounting controls.
Fourteen neighboring regressions and test-root Clippy with warnings denied also pass. A separate
comparison with the immutable preceding source preserves every ordered hypothesis field bit
for all 33 originally positive scene pairs; exactly three previously empty slope cases recover.
Flat, +20% and -40% full-search locations and clocks retain their previous values.

Three alternating native timing trials measured each two-event/two-track workload 100 times.
Accepted 20 km scenes measured 0.655 ms versus 0.657 ms before on flat terrain, and 2.171 ms
versus 2.080 ms before on a slope. The newly recovered 50 m slope case takes 0.572 ms versus
0.309 ms before. Deliberately incompatible short-range flights in a 20 km sloping box still
produce no hypotheses but take 1.318 ms versus 0.168 ms before: exact retries cost about 1.15 ms
there. These are component measurements, not whole-search performance claims. Recovery remains
bounded and retains every physical gate; it does not guarantee complete hillside coverage.
The combined production engine selftest retains its prior result. Reference, wrong-date and
field recordings have not been replayed.

### Wind-supported curves reach exact scoring

`wind_bank_stamps_recover_two_supported_pcm_passes` uses a fixed 900-second physical PCM recording,
two orthogonal 190 m/s flights, and wind components of 35 m/s east and north. It keeps the full
221-template production bank and a 400 m-wide region containing 25 active observer cells. The
unchanged exact scorer already hears both aircraft at the true observer, but the preceding coarse
matcher selects shapes with no recording-clock votes anywhere in that region. Replaying the actual
old module confirms zero coarse candidates, zero exact starts and no mode.

Mapping the wind-distorted frequency curve into the existing bank now produces one mode at the
correct synthetic observer cell with both intended aircraft, exactly the known recording clock,
and evidence 8.606058 nats. The separate wrong-clock and pure-noise controls return no modes.
The fixture deliberately omits decoys to isolate foreground reachability and asserts that it
cannot stand as a confident answer without them; this is not a new confidence-calibration result.

Five further native regressions compare the mapping with actual retarded propagation, reject
invalid/relative-supersonic media, preserve calm stamps bit for bit, and check prior-clock
independence and observer-specific atmosphere coordinates/heights. All six pass, as does component
Clippy with warnings denied. The combined actual production engine selftest retains its prior
four-event, two-flight result through real FFmpeg. These checks use synthetic recordings; reference,
wrong-date and field recordings have not been replayed. The mapping is exact for the frequency
shape under its local model, not for the bank's amplitude weighting or curved/changing-medium paths.

### Calibrating the frequency of recovered fast passes

Accepted fitted continuations now receive the same local raw-vertex calibration as ordinary
accepted aircraft. `continued_aircraft_use_raw_frequencies_from_their_own_spectrum` synthesizes
phase using an independent numerical emission-time solve, then runs detection, refinement and
measurement on an early 30-second pass and a late 180-second pass. The early pass renews its
spectrum; the late pass continues on its original spectrum. Both must retain the original
aircraft gates and continuous approach/recede observations.

| Synthetic pass | Tone truth / measured | Speed truth / measured | Distance truth / measured | CPA truth / measured |
|---|---|---|---|---|
| Early, 30 s recording | 118 / 117.994546 Hz | 215 / 215.024854 m/s | 900 / 900.708249 m | 10.13 / 10.130923 s |
| Late, 180 s recording | 83 / 82.998994 Hz | 245 / 245.000374 m/s | 700 / 700.376658 m | 169.37 / 169.370471 s |

With only the continuation calibration disabled, the unchanged test fails on the early pass's
117.855916 Hz tone, outside its 0.05 Hz requirement. The corrected path passes on pinned-nightly
x86-64 Linux. These are synthetic parameter-recovery results, not new field-accuracy evidence.

### Following a detected fast pass through its missing central drop

The fixed 180-second tail recording now reaches an accepted measurement from its actual detected
seed. Previously three guided rounds followed only 11% of the central drop and correctly refused
the event. `fitted_continuation_recovers_a_long_tail_with_a_missing_central_drop` continues that
same fit and narrowed band for three further rounds, without redoing the first three. It observes
91.56% of the drop and retains a continuous ridge through the CPA with at least eight seconds on
each side. All original physical and quality gates remain required.

The following measurements were recorded before the raw-vertex continuation calibration above.

| Quantity | Synthetic truth | Recovered at that change |
|---|---:|---:|
| Emitted tone | 83 Hz | 83.084086 Hz |
| Speed | 245 m/s | 244.897978 m/s |
| Closest distance | 700 m | 698.011988 m |
| Received CPA | 169.37 s | 169.360513 s |

The exact previous core fails the updated tail regression with zero independent aircraft; the
changed core recovers one. Missing central audio, a missing approach and steady asymptotes retain
their original refused results. Existing accepted measurements, including the known inaccurate
short-pass fits, retain identical events and spectra. Short-edge recovery, crop boundaries,
noise/steady/reversed refusals and the faint-drop sensitivity refusal also pass. Core test Clippy
passes with warnings denied. The combined production engine selftest also retains both intended
flights, four events, 1.673779 m error, a 75 m radius and +0.019586 s clock error through real FFmpeg.
These are native synthetic recording/measurement checks; no new
field-accuracy or confidence-calibration result is claimed.

### The recording audit shares the location search's recovered events

The native `detect_uses_locates_recovered_aircraft_and_event_ids` regression exercises the exact
production `detect` path with real FFmpeg/FFprobe and fixed synthetic float WAV files. The original
command misses the accepted aircraft in the 30-second fast-pass case; the shared analysis and
updated command recover it within the existing tone, speed, distance and CPA tolerances. Saved
events must exactly match `locate`'s recording analysis, with consistent survey/event figures and
trace filenames. A noise-only control remains empty. The new command path passes; the original
arm fails the same assertion. Changed-component Clippy also passes with warnings denied.

This is native component validation against the complete production core and locked dependencies.
Full application execution remains blocked by compilation of the unchanged transitive
`asupersync 0.5.0` dependency. No new field-accuracy result is claimed. Like the other media tests,
this regression is opt-in because it requires FFmpeg and FFprobe.

### Recovering the second aircraft needed for a location (2026-11-09)

`short_window_recovery_supplies_a_missing_second_aircraft` combines a 450 Hz, 90 m/s, 900 m
pass with a separate 83 Hz, 245 m/s, 700 m pass in fixed 30-second PCM. At both tested strengths,
the previous recording routine accepts only the slower aircraft. Recovery now supplies two
independent aircraft while retaining the original detections, measurements and spectra exactly
(apart from the established final quality-based event numbering). The fast fit is 245.024776 m/s,
701.63047 m and 19.376120 s received CPA against the 19.37 s truth.

The stronger mixture also passes through actual flight association, search and refinement with
orthogonal synthetic tracks and a known recording start: both intended core matches, **2.315639 m**
position error, **+0.004065 s** clock error, a 75 m radius and no truncation. The exact preceding
recording routine fails the same test because it supplies only one independent aircraft.

Two single-pass controls whose ordinary fits have incorrect geometry remain one aircraft with
unchanged detections and serialized measurements. A genuine 150/300 Hz harmonic-family control
also remains one aircraft with identical events and spectra. The earlier empty-result recoveries
still pass; noise, steady tone and reversed audio remain empty. Recovery retains the original
physical gates, shared candidate/detection limits, 24-independent-event limit and expanded-search
provenance. These native checks execute the complete production recording routine against actual
core modules and locked dependencies. They establish synthetic recovery, not field accuracy.

The combined production engine selftest also passes through real FFmpeg decoding and the original
1300-second synthetic scene: four events, both `ACE391` and `A1DFEB`, 1.673779 m error, 75 m radius
and +0.019586 s clock error, unchanged from the preceding baseline. Full application/workspace
execution remains blocked by the unchanged `asupersync` dependency compilation; reference,
wrong-date and field recordings have not been replayed.

### Earlier fast edge recovery after empty recording measurement (2026-11-09)

Five native checks execute the complete production recording-measurement routine against the
fresh, complete core and locked numerical dependencies. Two fixed 30-second PCM recordings have
zero aircraft on the original routine and exactly one independent aircraft after the shorter
edge survey, with the original one-second image still used for refinement and measurement:

| Synthetic truth: 83 Hz, 245 m/s, 700 m | Recovered tone (Hz) | Speed (m/s) | Distance (m) | Received CPA (s) |
|---|---:|---:|---:|---:|
| CPA 19.37 s | 82.986035 | 245.024777 | 701.630478 | 19.376120 |
| CPA 19.75 s | 83.002345 | 244.998950 | 700.216656 | 19.750013 |

Noise, a steady tone and reversed audio remain at zero detections and zero aircraft in the full
routine. Two already accepted recordings retain byte-identical detections, events, provenance and
warnings against the immutable prior routine. That includes the known inaccurate single-source
fit which a naive supplemental scan had turned into two independent aircraft. The new guard leaves
that existing measurement unchanged; it does not repair its inaccurate geometry. Recovery warnings,
family links, event renumbering, expanded-search provenance and both output caps are checked.

Six focused native core checks also pass, covering recovery, candidate accounting, crop seams,
absolute tail times, and preservation of the existing fast stages and cap. At that stage, the
180-second control returned the local edge crop's coarse detections translated into recording
time but still failed physical measurement. The later central-drop continuation described above
now recovers it from that detected seed. The production engine
selftest retains both intended flights, four events, 1.673779 m error, a 75 m radius and +0.019586 s
clock error, identical to its prior baseline. Core library/test Clippy passes with warnings denied.

These are native audio-analysis component results, including the exact production function and its
repository tests, with no replacement algorithms. They do not establish a new two-flight location,
faint-pass recovery or field accuracy. Full application/workspace validation remains blocked in
unchanged `asupersync`; reference, wrong-date and field recordings were not replayed.

### Local raw vertices improve accepted-aircraft measurements (2026-11-08)

Both documented PCM accuracy failures now pass their unchanged tolerances. Discovery retains its
normalized-ridge fallback, while an optional fit of an already accepted aircraft uses only locally
supported raw vertices. It neither restores the removed raw-peak climb nor changes detection,
quality or aircraft-selection thresholds. Invalid or ineligible refinements retain the original
accepted measurement.

Absolute errors on the existing fixed synthetic fixtures, before → after:

| Fixture | Tone (Hz) | Speed (m/s) | Closest distance (m) | Received CPA time (s) |
|---|---:|---:|---:|---:|
| Harmonic recovery: 118 Hz, 120 m/s, 1500 m, 110 s | 1.321376 → 0.000648 | 2.007916 → 0.000042 | 73.789775 → 0.062019 | 0.538345 → 0.000289 |
| Fast pass: 83 Hz, 245 m/s, 3600 m, 85.61 s | 0.376272 → 0.002579 | 0.764535 → 0.004208 | 46.928168 → 0.536758 | 0.104248 → 0.001233 |

All 36 ordinary measurement cases and all eight trace cases pass in native component execution.
This includes accepted/refused short-pass controls, wrong-seed recovery, noise and reversal refusals,
retained-support gates, and exact eager/deferred trace and full-fit comparisons. The interrupted-fit
reference now explicitly fails the optional refinement and proves the original accepted trace/fit
survives; its existing interruption comparisons remain exact.

With the committed detector correction included, all four contrast-edge tests and the original
747 Hz false-addition regression pass, including real PCM measurement and negative controls.
The unchanged production engine selftest runs through real ffmpeg decoding, detection, measurement,
association, search and refinement: both `ACE391` and `A1DFEB`, four measured events, 1.673779 m
location error inside a 75 m radius, and 0.019586 s clock error.

Execution used all actual core modules and pinned FFT/optimizer dependencies. To fit this executor,
the F-distribution path was compiled from 46 verbatim upstream source items and the unchanged
`bratio.rs`/`types.rs` numerical modules, without substitute numerics. Separately, the complete core
test source passes type checking and native Clippy with `-D warnings` against the original complete
dependency metadata. New code introduces no formatting differences; existing formatting discrepancies
remain. These are component results: full Cargo workspace/CLI execution was blocked by filesystem
exhaustion and dependency compiler kills. Reference, wrong-date and field recordings were not replayed.

### Short contrast-edge curves need continuous physical support (2026-11-08)

The final contrast-edge stage now corroborates each sparse candidate on native survey frames.
One frequency offset inside the original half-row cell and one CPA offset inside the original
half-hop cell must explain both halves. Each half's mean residual evidence is scaled by the
square root of its original sparse sample count; the strong line remains 4.5. This prevents an
increased sampling rate from supplying extra score by itself. Frequency intervals are swept
exactly, with 17 bounded clock phases and O(N log N) work per phase. Candidate ranking, reported
scores, earlier-stage priority, ownership masks and shared caps retain their original rules.

Eight native detector component checks pass with the pinned compiler and actual production
FFT/spectrogram/detector code. The unchanged
`contrast_stage_keeps_every_detection_of_the_survey_image` fails before this repair at 35.5 s,
747.510 Hz and score 4.87718; that false short curve is now refused. The new regression at both
0.5 s and 1 s survey hops retains continuous and fractional-cell ridges while rejecting support
only at sparse sample times and alternating neighbouring peaks that no single curve can follow.

The four existing 30-second cropped PCM fixtures retain byte-identical coarse and refined
detection catalogues. Existing short-edge recovery, hidden-pass recovery, earlier-stage ordering,
negative controls and cap checks also pass. These component checks do not execute measurement
or search, and do not replace the full workspace/CLI checks or real reference/wrong-date replays.
Full core test-source type checking and native Clippy with `-D warnings` also pass against the
original complete dependency metadata. The separate early 215 m/s fast-edge miss remains unresolved.

### Full workspace suite at `c2c7578` (2026-11-08): seven red core tests and their causes

Every test target of the workspace at `c2c7578` was built with the pinned nightly on x86-64 Linux
(`cargo build --locked --workspace --tests`, through RCH), and each test executable was run from its
crate's directory. Doc tests were not run. `fdop-core`: 231 passed, 7 failed, 7 ignored. Every other
target passed: `fdop-data` 75, `fdop-types` 27, `fdop-geo` 26, `fdop-http` 16 and the application
library 32 (the binary target has no tests). On `8be452d` the same run had eight core failures;
`79414aa` fixed `coarse_stamps_feed_a_short_circuit_and_crossing_flight_into_the_match`. `fdop-core`
does not depend on `asupersync`, so `cargo test -p fdop-core --lib` needs no application build; it
took 145 s on four threads.

On `0dae249` (2026-11-09, `cargo test --release --workspace --no-fail-fast` through RCH on x86-64
Linux), four of the seven still fail and every other target passes. `fdop-core`: 266 passed, 4 failed,
8 ignored. The four are the first four rows below: `three_flights_locate_the_observer_and_beat_every_decoy`
now at a margin of 4.979, the refinement control, the grazing pass and the fast edge pass. With the
null-tail limit added (none of the four calls `trace_nats`) they fail with the same values on x86-64
Linux and on macOS arm64, the platform the benchmark binaries are built for: not platform effects.

The curve-match and measurement causes were measured: the earlier behaviour was put back behind a
test-only switch in a scratch tree outside this repository, and the unchanged test was run again.
The contrast row's cause follows from the failing detection, which only `c7c1206`'s stage can
produce. The grazing and fast-edge tests were added with their code unrun and fail on every commit
run here (`0abfbe9`, `a0795de`, `8be452d`, `c2c7578`).

| Red test | Fails on | Cause |
|---|---|---|
| `curvematch::three_flights_locate_the_observer_and_beat_every_decoy` | decoy margin 4.961 nats; needs more than 5 | `83e8796`, then `79414aa` (below) |
| `curvematch::refinement_climbs_from_a_start_beyond_one_move_per_scale` | its control: one move per scale now ends 106 m off with 11.242 nats, above the observer's 11.208 | `83e8796` |
| `curvematch::grazing_passes_are_heard_across_exact_clock_seeds` | the grazing pass has 6.29 nats; needs 6.9 | added by `7bc5b48`; never seen passing |
| `detect::fast_edge_passes_reach_measurement_at_both_recording_ends` | no pass found at 215 m/s, 10.13 s | added by `459079e`; never seen passing |
| `detect::contrast_stage_keeps_every_detection_of_the_survey_image` | an added detection with a 10 s half path | `c7c1206`; passed on `a0795de` (below) |
| `measure::harmonic_sibling_recovers_a_refused_primary_as_one_aircraft_event` | 119.32 Hz and 1,426 m, against 118 Hz and 1,500 m | `c0204f6` (below) |
| `measure::fast_passes_survive_detection_refinement_and_measurement` | the 245 m/s pass at 83.376 Hz; bound 83 ± 0.3 Hz | `c0204f6` (below) |

**The interpolated null (`83e8796`).** With the nearest-node null put back on `162e248`,
`three_flights_locate_the_observer_and_beat_every_decoy`,
`refinement_climbs_from_a_start_beyond_one_move_per_scale` and
`coarse_stamps_feed_a_short_circuit_and_crossing_flight_into_the_match` pass, and only
`alternative_clock_peaks_recover_the_three_flight_recording` fails: the 213 m case `83e8796` was
written for. The interpolated null changes with the observer's place, and that change can outweigh
the data. In the three-flight scene the best point of a 50 m grid is 206 m from the observer. Between
the two, flight AAA's raw score falls from 211.5 to 205.1, but its null's median and tail scale fall
from 14.26 and 22.26 to 13.22 and 20.28, so its nats rise from 9.55 to 10.15. A scratch variant on
`162e248` compared nearby places under one calibration and recalibrated at each accepted point. It
put the three-flight answer 78 m off, the circuit and crossing 141 m off and the clock-alias case 56 m
off. But the three-flight margin fell from 5.13 to 4.967: the best decoy kept its 6.278 nats while the
real window's best place lost the 0.16 nats the null's pull had given it. Its 600 m climb also ended
at 11.193 nats, under the observer's 11.208. `79414aa`, a different fix of the same effect, gives a
4.961 margin here. A place counts as right within 1 km (5 km on some sets) and the margin is one of
the grade gates, so a more exact place that lowers margins can cost asserted answers. The scratch
variant is not proposed.

**The contrast edge stage (`c7c1206`).** The 48 s scene with a 35 dB swell has one tone, a 300 Hz pass
at 22 s. The new stage adds an edge seed there at 35.5 s and 747.5 Hz, with score 4.88 against the
4.5 line: a false detection. The test requires every contrast-stage addition to come from the full
bank. Running the stage only on clips with no frame a full half path from both ends would not exclude
this scene: its frames span less than 48 s, two 24 s half paths.

**The removed climb (`c0204f6`).** With `82f26a9`'s climb put back on `c2c7578`, both measure tests
pass, and four tests added since by `8be452d` and `c2c7578` fail:
`fitted_continuation_preserves_accepted_short_events`,
`fitted_continuation_preserves_refused_late_fast_passes`,
`quality_support_uses_only_times_retained_by_the_actual_solver` and
`short_fast_passes_recover_the_same_detected_ridge_after_refusal`. On `162e248` the climb gives
118.0001 Hz and 1,500.07 m for the sibling, and 82.999 Hz for the 245 m/s pass. `c0204f6` removed it
on set D's evidence (34 of 40 right at 25/50 %, against 33). Its record says both tests fail on
`0abfbe9` with and without the climb. On x86-64 both pass on `0abfbe9`, so that record holds only on
the machine it was measured on. These are synthetic checks on one platform, not field results.

### Curve localization on fixed observations

The original circuit-and-crossing fixture contains three visits by a circling aircraft and a
separate crossing flight. All four were heard, but the score preferred a displaced observer even
when given the true point. Moving farther from the circuit increased the amount of audio scored;
the crossing flight also gained calibrated evidence despite a poorer raw match.

The new local geometry fit compares each candidate mode's locations on the same observations,
then recomputes the existing evidence at the fitted point. The unchanged known-answer regression
`coarse_stamps_feed_a_short_circuit_and_crossing_flight_into_the_match` now passes:

| Synthetic result | Before physical fitting | Final guarded fit |
|---|---:|---:|
| Observer error | 340.026957 m | 35.336527 m |
| Recording-clock error | -2.500 s | -0.500 s |
| Independently heard visits | 4 | 4 |

The original requirements remain under 150 m position error, under 2 s clock error, the crossing
flight and at least two distinct circuit visits. A separate multiple-mode test covers a limited
evidence-refinement budget; it passed before the placement change and is coverage, not a red/green claim.

Extending the existing overlapping-flight PCM fixture through physical fitting caught a source
reassignment: final rescoring swapped two tones. The final implementation preserves the original
mode if its fitted source visits cannot all retain their independent identities and original tone
neighbourhoods. The distinct-tone, absent-source and same-tone variants now pass, alongside all
their original acoustic-assignment assertions.

All ten focused native checks pass on the integrated core through genuine RCH: those three tests,
real/decoy symmetry, alternative clocks, fast crossings and their refusal controls, duplicate-event
protection, per-flight timestamp errors, simultaneous distinct tones, and noise versus decoys.
Core all-target Clippy also passes with warnings denied on the integrated source.
The existing empirical calibration, hearing threshold, confidence gates and radius rules remain
unchanged. These are synthetic curve-matching results; field/reference/wrong-date replays and a
complete CLI/workspace result are not established. The separate grazing-pass hearing failure remains.

### Short fast passes: fitted-ridge recovery

`short_fast_passes_recover_the_same_detected_ridge_after_refusal` runs fixed 30-second PCM
through survey, detection, refinement, family measurement and independent-event selection.
For a 118 Hz, 215 m/s pass at 900 m, the 0, 6, 9 and 12 dB attenuation cases previously supplied
no usable event: the ordinary seed masked the supplemental fast seed, then measurement retained
only about two seconds of approach. A bounded continuation now recovers one independent event
from that original detection in each case, with 8.39–9.89 seconds of retained approach and at
least 19.41 seconds of recede support. Across the four, maximum absolute errors are 0.145 Hz,
0.206 m/s, 3.44 m and 0.0108 s for emitted tone, speed, distance and arrival time. The 0 dB case requires trace
points above the old spectrum's upper frequency limit, proving that the cropped approach returned.

All four new end-to-end measurement regressions pass through genuine RCH on the pinned nightly.
`fitted_continuation_preserves_accepted_short_events` compares complete event output and spectra
for the already successful early 3 dB case and the previously accepted late 3 and 6 dB cases.
`fitted_continuation_preserves_refused_late_fast_passes` retains the late 0, 9 and 12 dB refusals,
including their complete output and spectra. Together with the four recoveries, these checks cover
all ten fixed early/late attenuation scenes.
`fitted_continuation_refuses_noise_tones_and_reversed_passes` checks noise/hum, a steady tone,
and both reversed edge passes through the actual measurement path. The late accepted fits still
have incorrect geometry; preserving them establishes the scope of this recovery, not their accuracy.
The detector and its ownership masks remain unchanged. These synthetic results do not establish
real-recording accuracy or confidence calibration.

The complete early/late attenuation diagnostic caught a new wrong acceptance when continuation
was initially allowed on ordinary-speed fitted curves: a late 245 m/s source was mismeasured at
159 m/s. Recovery is therefore scoped to fits already beyond the existing primary-bank speed
limit, with that late refusal retained as a permanent regression. The four accurate new early
recoveries already satisfy this domain requirement; no accuracy assertion or quality gate is loosened.

Continuing the same fixed scenes also caught a premature acceptance at 9 dB: one isolated retained
point made a fit appear to cover both sides while its continuous ridge began too late. Completing
the ordinary fitting rounds and requiring continuous retained support restores the correct geometry
at both 9 and 12 dB. The continuity and side-span limits remain the existing three and eight seconds;
no fixture-specific threshold or relaxed physical-accuracy bound was introduced.

If a continuation loses its ridge before those rounds finish, it refuses rather than accepting
the preceding fit. `continuation_refuses_a_lost_trace_before_ordinary_rounds_complete` isolates
that exit with injected loss of replacement-window observations after an actual successful fit;
the original non-continuation behavior remains unchanged. This is a failure-path unit check,
separate from the four PCM pipeline tests above.

All five focused checks pass on the final native core, as does core all-target Clippy with
warnings denied. The unchanged production selftest module, freshly linked to the actual
core/types libraries and using FFmpeg, recovers the correct two flights from four events:
1.312094 m position error within a 75 m radius and +0.014154 s recording-clock error.
These results do not establish release-CLI or full-workspace integration. Full application
compilation remains memory-blocked in unchanged `asupersync`; the workspace formatting check
also reports remaining differences. Reference, wrong-date and field recordings were not replayed.

Two existing measurement accuracy failures remain. The harmonic-family fixture still returns
119.32138 Hz and arrival 109.46166 s against 118 Hz and 110 s. In the longer 245 m/s fixture,
independent-event selection still chooses 83.37627 Hz against 83 Hz, outside its unchanged
0.3 Hz bound. Direct continuation-off/on comparisons reproduce both selected results; the
selected events and spectra compare exactly. The longer-pass continuation also recovers an
accurate alternative at 83.03463 Hz, but that alternative does not displace the existing winner.
These are unresolved measurement/selection defects, not passing accuracy checks.

### Fast-pass measurement (2026-11-08): recovery and sensitivity limits

The PCM oracle independently solves the light-time equation for emission phase and 1/r amplitude;
it does not synthesize from the production frequency model. The two geometries have maximum
received chirps of 7.24975 and 10.6545 Hz/s, although their heard-CPA rates are only 3.88727 and
2.95627 Hz/s. The latter values had omitted the short spectrogram needed near the steepest drop.
Selecting the correct window alone was insufficient: its noise estimate also needed to exclude
the full signal lobe, as the ordinary window already does.

Both final native regressions pass: `fast_passes_recover_physical_parameters_from_emission_phase`
and `faint_fast_pass_from_emission_phase_preserves_ridge_refusal`. Every fixed PCM/noise/seed row
retains the same 0.5% tone, 3% speed, 5% distance and 0.5 s arrival-time bounds:

| Synthetic pass | True distance | Before | Matched windows | Acceptance |
|---|---:|---:|---:|---|
| 200 m/s, faint | 3,000 m | 3,171.26 m | 2,993.37 m | Accepted in both; now within the distance bound |
| 200 m/s, strong | 3,000 m | 2,996.37 m | 2,991.20 m | Accepted in both |
| 260 m/s, faint | 10,000 m | 10,445.33 m | 10,058.03 m | Refused in both for insufficient central-ridge support |
| 260 m/s, strong | 10,000 m | 10,000.63 m | 9,996.76 m | Accepted in both |

The initial exploratory requirement that all four rows be accepted failed. Even guidance by the
exact physical curve follows only 13.6963% of the faint 260 m/s pass's central drop, below the
unchanged 15% gate. Its final fitted parameters satisfy all four physical bounds, but the permanent
control requires refusal for that specific missing-ridge reason. It also requires nontrivial exact-guide
support on both sides, so an empty or wholly lost trace cannot satisfy the control. This is a real
faint-signal sensitivity limit, not a claim of four accepted recoveries.

The earlier clean 367 Hz/135 m/s/1,758 m wrong-seed case now recovers directly: 367.01306 Hz,
135.01227 m/s, 1,758.32291 m and arrival 119.99774 s against 120 s. Its old expectation of a
one-sided first refusal is therefore obsolete. The revised route check retains every original
recording, seed, physical bound and negative control: a qualifying first result must be identical
with retry enabled, while a refused first result must still be one-sided and recover through retry.

The original receding-seed case exposed a separate support-accounting defect during validation:
discarded frequency points made a fitted tail appear to have 30.6302 s of approach support when
the robust fitter retained only 7.0302 s. Reusing those original inlier timestamps at all three
quality decisions restores the unchanged 8 s gate and existing free retry. The unchanged PCM
then recovers 249.99170 Hz, 109.95228 m/s, 898.55909 m and arrival 120.00093 s against the known
250 Hz, 110 m/s, 900 m and 120 s. Neither the robust trim nor the retry condition was widened.
An actual-solver regression verifies the rejected-point/retained-support contract on that same
recording and preserves a clean, two-sided control. Short-window noise exclusion is also separate
from peak-selection and raw-interpolation bounds; a dedicated profile regression protects both.

The final native core checks pass for both new emission-phase tests, retained-fit support,
noise-exclusion bounds, the complete original one-sided recovery/refusal matrix, the no-ridge
and stepped-tone matrix, the actual slow-source refusal, nearby-ridge and steep-skirt controls,
and all three short-clip contrast-edge regressions. Core Clippy passes for all targets with
warnings denied. Running the unchanged production `selftest::run` module against the final real
core/types libraries and FFmpeg recovers the correct two flights from four events, with 1.3121 m
position error inside its 75 m radius and +0.01415 s clock error.

That module run does not establish release-CLI, ledger, network or full-workspace integration.
The serialized full application build still hits the 8 GiB memory limit in unchanged
`asupersync` compilation; workspace formatting also retains pre-existing differences. No field,
reference or wrong-date recording replay was available. These synthetic checks establish the
specific recovery and refusal contracts above, not field accuracy or confidence calibration.

### Distinct tones survive overlapping-flight ownership

A fixed 900-second PCM scene contains two simultaneous, similar Doppler paths at 90 and 114.5 Hz,
plus a separate crossing flight at 150 Hz. The original exact scorer credits only two aircraft:
both simultaneous paths select the brighter 90 Hz tone before pixel ownership rejects one.
Recovery finds the other real peak and credits all three, using the same 6.9-nat threshold.
The regression failed with two heard passes before the production change. Controls with the
second source absent or on the same tone still require exactly two heard passes, including
harmonic ownership. An initial implementation exposed a false extra tone on the bright peak's
shoulder; requiring a peak in the complete tone profile fixes that case without changing its test.

A separate physical PCM control checks that a rejected duplicate cannot take a weaker original
flight's evidence. It establishes that the competing tone is recoverable when its real owner is
omitted, then requires the complete previous pass output and evidence to remain identical when
that owner is present. The existing noncolliding-output oracle also compares every reported
floating-point bit and metadata field with recovery disabled. These checks concern acoustic
assignment; they do not resolve parallel-flight position/clock ambiguity or establish field
confidence calibration.

All three focused native core regressions pass together on the pinned nightly through genuine
RCH: independent-tone recovery with absent/same-tone controls, exact original-owner protection,
and the complete noncolliding-output oracle. The ownership check uses individual retained
frequency rows: deleting a whole comb sample also deleted valid independent partials and failed
the unchanged positive test. Per-harmonic subtraction preserves that recovery while refusing
the duplicate. Manual `CombImage` values without `whitened` rows keep the original assignment
and cannot receive additional tone credit.

After integrating the concurrent retarded-time bank-weight correction, the same three recovery
regressions and both physical bank-weight tests pass together in the native core binary (five
tests). This checks the weaker recovered tone against the corrected empirical calibration.
Integrated core all-target Clippy also passes with warnings denied through genuine RCH.
Independent source review, focused formatting, whitespace checks and staged bug-scan review
found no new production issue; existing whole-file formatting differences remain outside this change.

The unchanged `src/selftest.rs` also passed in a native component linked to the actual core,
types and Rayon libraries on the pinned nightly through genuine RCH. Its 1,300-second synthetic
recording went through WAV generation, FFmpeg decode, detection, measurement, search and refinement.
It selected `ACE391` and `A1DFEB`, with 2.365 m position error inside a 75 m radius and 0.0249 s
clock error. This validates the selftest component and the committed detector path; it does not
exercise curve matching or replace a full CLI/workspace build or real-recording replay.

### Retarded curve-bank weights (2026-11-08): physical scoring agreement

The actual pinned-nightly core crate passes both new regressions through genuine RCH:
`bank_path_weights_match_retarded_track_geometry` compares every frequency row and range weight
against the trajectory solver in 24 speed/range/time/position cases, and
`bank_path_scores_match_the_exact_fast_pcm_curve` recovers the same emitted tone and raw score
from a 263 m/s harmonic PCM pass. These are straight trajectories in still air at 343 m/s.

Both tests fail before the repair. The first geometry mismatch is already present at 28 m/s;
the fast PCM gives a bank score of 94.35496 against the physical trajectory's 81.89703 at the
same emitted-tone row. After the repair, the raw scores agree within the unchanged 0.001
tolerance. The existing fast-crossing test also passes: both correct flights, observer within
200 m, clock within 2 s, a standing decoy verdict, and refusal of reversed/steady-tone controls.

That bank-weight correction did not resolve the spatial-evidence bias. The grazing test still
fails its unchanged 6.9-nat hearing requirement:
6.2908 nats after the repair versus 6.3341 before. The short-circuit/crossing-flight regression
selected a point 340.027 m away and clock 2.5 s early while hearing all four passes on that build;
the fixed-observation geometry fit described above now repairs this separate regression.
No complete core/workspace suite, release
CLI selftest, reference/wrong-date replay or field-accuracy improvement is established here.

### Contrast-first recovery for short recordings

The final contrast-edge stage is checked against the complete previous detector stage sequence,
with recovery disabled only in the test oracle. Four fixed 30-second PCM clips combine a faint
300 Hz, 90 m/s pass at 900 m with 20 or 25 dB broadband swells, near either recording edge.
The previous detector finds no seed; the new path must produce one independent aircraft-like
measurement and recover tone, speed, distance and arrival time within fixed physical tolerances.
The regression failed on the old production path before adding the stage.

Additional regressions compare every original detection field and harmonic prefix across 16
physical scenes at caps of 1 and 60, require new seeds to clear the strong threshold, and preserve
the cap and original ordering. Fourteen controls exercise reversed passes, steady tones and
noise swells through survey, detection, refinement, measurement and independent-event selection.
These controls must yield no aircraft-like event. They do not establish field accuracy or
confidence calibration; no reference or wrong-date recording replay is available in this checkout.

All three new tests pass in the actual `fdop-core` test binary on the pinned nightly through
genuine RCH. On `c7c1206`, the four recovered clips' largest absolute errors were 1.79 m in distance,
0.114 m/s in speed and 0.0092 s in arrival time. The matched-window follow-up repeats all three
tests with unchanged assertions; its largest errors are 3.74 m, 0.213 m/s and 0.0164 s respectively.
The same recovery test failed before the
production change. Staged bug-scan findings were reviewed; new warnings concern test assertions
and fixture allocations. The changed lines meet formatting checks; three existing formatting
differences in the separate fast-edge tests remain. This is a focused core result, not a full
workspace or CLI selftest pass.

The separate detector fast-edge regression remains unresolved. Before fitted-ridge continuation,
a native diagnostic measured all
ten fixed early/late fast-pass scenes, including the original seeds that the old regression skipped.
Legacy measurement correctly recovers one scene. Two further early 215 m/s scenes recover only
when the fast bank is run without earlier ownership: all five receding-side samples of their useful
seed are already masked by an ordinary candidate whose measurement fails. The late 245 m/s scenes
remain unrecovered by that diagnostic; some ordinary fits pass quality gates with incorrect geometry.
The fitted-ridge continuation described above now recovers four additional early scenes from the
ordinary detections, preserving the already successful one. Its regression checks physical parameters
as well as acceptance. No ownership threshold, stage priority or existing assertion was changed.

### Midnight flight coverage: native data validation

All 27 tests in the actual production `ingest.rs` and `build.rs` modules pass in a native component
using the real `fdop-types`, pinned nightly and locked dependencies through genuine RCH. Seven new
regressions cover raw two-day tar ingestion through cache save/load, merging and final filtering;
one-report-per-day crossings; ground-only feeds and interleaved boundary reports; natural feed handoffs;
retained filtering cuts; preferred-position duplicates; bounded seam support; and compact ground
events under the unchanged 30-second boundary rule. Both separate legacy ingests drop the positive
crossing fixture, while the new day-cache path retains its complete two- or four-report flight.
The related full `fdop-types` suite passes all 27 tests, and its all-target Clippy check is clean.

The new actual offline-fetch cache-policy test and curve-catalogue warning integration test remain
**unrun**. Full HTTP/data/application compilation was blocked by SIGKILL in unchanged `asupersync 0.6.2`,
both during native code generation and during a separate metadata-only check on the shared 8 GiB
machine. The native component does not contain `fetch.rs`, network adapters or the application pipeline.
Its separate Clippy attempt stopped on unused entry points whose callers are outside the component;
no full-data Clippy pass is claimed.
Changed source files pass focused formatting and whitespace checks; whole-workspace formatting still
reports differences in eight untouched files. Full workspace tests/Clippy, CLI release selftest and
recording replays remain unconfirmed. These data-geometry tests do not establish recording accuracy.

### Confirmation and silent-pass bounds: native output equivalence

The trace search now builds conservative spatial bounds alongside its time index. Confirmation
and unheard-close-pass checks skip a track only when its complete Hermite path cannot reach the
observer under the existing distance gate. This avoids repeated closest-approach scans of distant
flights at every candidate and refined location. The bounds include between-knot excursions;
invalid bounds or query geometry retain the original exact check. Time order, scores, pass
selection, budgets and confidence thresholds are unchanged. The index belongs to one immutable
track catalogue and must be rebuilt after geometry or ordering changes.

All six focused search tests pass in the actual pinned-nightly core binary through RCH:

- `spatial_bounds_keep_grazing_and_unknown_tracks`: grazing boundaries, temporal exclusion,
  malformed bounds and nonfinite query values preserve the required candidates.
- `spatial_bounds_preserve_confirmers_and_unheard_passes`: complete serialized outputs equal
  the same scorer with spatial bounds disabled, with 1,024 distant flights, still and windy air,
  a returning aircraft, a Hermite excursion outside its knot box, shifted observers/clocks,
  same-pass exclusions and heard/silent close passes.
- Existing narrow-circuit confirmation, same-pass exclusion and streamed pair-join regressions
  pass. The existing closer-return test also passes after rebuilding its index following its
  test-only trajectory mutation. It retains every prior assertion and additionally compares
  the full silent-pass list against the bounds-disabled path.

Together with the ridge, spectrogram, trajectory-bound and track-recovery checks in this section, 55
distinct focused core tests passed during this continuation. This is not a complete 228-test
core-suite or workspace pass. Core all-target Clippy also passes with warnings denied:
`cargo clippy -p fdop-core --all-targets --locked -- -D warnings`, through the same pinned-toolchain
RCH configuration. Focused formatting and staged bug scans passed review; whole-workspace
formatting already reported 60 pre-existing diff sections on the untouched checkout.
No whole-recording speedup or accuracy change is claimed, and
CLI selftest plus reference/wrong-date/field replays remain unrun.

### Recover supported motion when another report component is unusable

Track construction now recovers independently supported horizontal or vertical motion after the
existing full-vector report check fails or horizontal metadata is absent. A corrupt climb rate
can no longer automatically discard a valid turn tangent; a missing speed or heading can no
longer automatically discard a supported climb rate. Unsupported components retain the existing
position-derived finite differences. Recovery still requires position support at the same
25 m/s tolerance, including across samples at least 30 s apart, and retains the close-neighbour
requirements for turn support. When both components individually pass but the original joint
check fails, the original fallback remains. Previously accepted full vectors and their
`reported_vel_frac` credit are unchanged; partial recovery adds no credit.

The actual pinned-nightly core crate ran the same 34 track tests before and after the production
change through RCH: **30 passed / 4 failed before; 34 passed / 0 failed after**. The analytic
oracles exercise raw `FlightLeg` construction, Hermite interpolation, emission-time solving and
received Doppler against independently calculated circle and cubic-climb trajectories:

- `sparse_turns_recover_horizontal_motion_despite_bad_climb_rates` covers both turn directions
  and uneven samples. Maximum position error stays below 30 m, versus more than 100 m for the
  old finite-difference fallback. Maximum 200 Hz Doppler error is below one quarter of the old
  error. The first failing original-code case measured 288.23 m and 27.75 Hz.
- `climbing_tracks_keep_vertical_reports_without_horizontal_metadata` covers missing speed,
  missing heading and both, in climbs and descents. Maximum error stays below 0.1 m and 0.01 Hz,
  versus more than 20 m and 0.2 Hz for the old fallback.
- `partial_velocity_recovery_requires_position_support_and_keeps_joint_limit` and
  `accepted_velocity_reports_remain_bitwise_unchanged` protect the joint tolerance, sparse-sample
  support, report credit and previously accepted vectors. Existing corrupt-speed/heading/rate
  tests still reject the corrupt component while requiring preservation of the supported one.

These are synthetic trajectory results, not measured recording accuracy. Independent source
review passed. The CLI selftest and reference/wrong-date/field replays were not run, and no
matching recording/cache was available in this workspace. The selftest also constructs models
directly, so it would not replace the raw-report recovery oracles above.

### Ridge-profile interpolation: native equivalence and component timing

Both `spectro::tests::prepared_time_*` regressions pass against the unchanged scalar bilinear sampler,
covering non-dyadic grids, large timestamp origins, boundaries, invalid coordinates and nonfinite
samples. All six existing trace tests and both existing spectrogram tests also pass. The native
`ridge_profile_validation` run compares full original/current profile outputs bit for bit in 384
cases and in each timed trial; the measured workload and limits are in [Performance](#performance).
The actual core crate compiled with the pinned nightly and locked dependencies through RCH, using
one build job, no debug information and dependency optimization disabled to fit shared memory.
These focused checks do not establish a workspace, CLI selftest or real-recording replay pass.

### Search working set (2026-11-07): focused native integration passes

The pinned `nightly-2026-08-31` full-core test binary passes these three checks through the actual
search code, in addition to the pair-join component measurements in Performance:

- `pair_join_preserves_every_ordered_representative_across_batches`: complete ordered seed equality,
  including multiple batches, worker counts, clock/spatial boundaries and repeated-flight exclusions.
- `quick_pair_budget_counts_rejected_seeds_before_starting_another_pair`: rejected quick seeds still
  consume the joined budget and record the same complete-pair truncation.
- `two_synthetic_flyovers_locate_the_observer`: correct A/B identities, less than 60 m position error,
  less than 1 s clock error, successful refinement and coherent candidate/margin reporting.

The broader core run was stopped after 32 passes, two failures and four declared audio-test ignores
to reproduce the failures individually. Both exact reruns fail without changing their assertions:

- `coarse_stamps_feed_a_short_circuit_and_crossing_flight_into_the_match` hears the crossing flight
  and all three circuit visits, but its selected location is 340.027 m off and its clock is 2.5 s
  early, outside the test's 150 m and 2 s requirements.
- `grazing_passes_are_heard_across_exact_clock_seeds` recovers the within-range trajectory at
  7,999.5 m. Its aligned-seed audibility prerequisite fails at 6.334 nats against the unchanged
  6.9-nat hearing line, before the remaining clock phases run. This does not show geometric loss.

The full data-crate test build, a serial retry and workspace Clippy ended with dependency-compiler
SIGKILLs (`asupersync 0.6.2` for the data tests and `0.5.0` for Clippy). The shared environment
reached its 8 GiB cap during validation; these builds used one job and reduced compiler memory
settings. Thus the new full offline-fetch regression has not run. `cargo fmt --all -- --check`
still fails on files outside these search/data
changes; the three edited Rust files pass focused formatting and whitespace checks. Full workspace
tests, release CLI selftest and reference/field replays were not completed. No full-core, workspace,
release or new recording-accuracy pass is claimed.

### Complete archive ingestion (2026-11-07): native reader component verified

New regressions require both tar end blocks, surface errors reported after the marker, accept zero
padding and short reads across split boundaries, and reject nonzero trailing data. These archive-reader
regressions pass as part of 20 production build/ingest tests in a native component using the unchanged
modules, real `fdop-types`, pinned nightly and exact locked dependencies. The component also verifies
recognized-type precedence across joined days. It does not include the fetch or release-discovery
modules: their offline-cache, sidecar, contiguous-part, unfinished-upload and listing-refresh tests
remain blocked by the full data-crate build. This is not an end-to-end archive replay or workspace pass.

### Conservative trajectory bounds (2026-11-07): native regressions verified

Added geometry regressions for between-knot Hermite excursions, signed ECEF coordinates, grazing
faces/edges/corners and malformed input. The exact-score regression adds 1,024 distant tracks to the
existing synthetic three-flight PCM fixture and compares all returned float bits, counts and serialized
pass metadata against the original unpruned path at three observer/clock combinations. It includes
unchanged silence and chance-calibration handling; it does not assert a speedup.

Independent JavaScript double-precision arithmetic checks covered 38,514 translated/grazing cases and
confirmed the fixture's distant tracks lie outside every tested observer's reach. These are auxiliary
geometry checks. The geometry/fail-open tests and the complete exact-score equivalence regression
now pass in the actual native core test binary with the pinned nightly and locked dependencies.
Workspace checks, CLI selftest and real-recording replays are not established by these focused tests.

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

The `--clock-tol` default change and its follow-up (CHANGELOG) were checked through real RCH with the
pinned nightly. These are focused checks, not a suite pass.

- **On `c5545ca` with the change, on one worker with two build jobs:** the application's `cli::tests::` and
  `pipeline::tests::` passed, 31 of 31, including `the_default_clock_tolerance_lets_the_curve_match_judge`
  and `a_curve_match_short_of_decoys_says_why_and_what_would_judge`. In `fdop-core`,
  `curvematch::tests::the_widest_judged_window_is_where_the_decoys_run_out` and
  `decoys_fill_from_the_side_the_catalogue_covers` passed. Workspace all-target Clippy passed with
  warnings denied. rustfmt reports nothing in the lines this change adds; it reports 10 differences elsewhere
  in `src/pipeline.rs` and `crates/fdop-core/src/curvematch.rs`, the same 10 as without the change.
- **On `e925900` with the change, all 39 `curvematch::tests::`:** 38 passed, including the new one, and
  `alternative_clock_peaks_recover_the_three_flight_recording` (added in `e925900`) **failed**. It found the
  true clock (error +0.000 s) and all three heard flights, but placed the observer 213 m from the truth, over
  its 200 m bound. This change does not touch that search, and the test was not re-run on the curve-match
  changes that followed `e925900`.
- **On `0abfbe9` with the follow-up** (the short-of-decoys warning names the window it ran on and rounds its
  advice inside the line): the application's `cli::tests::` and `pipeline::tests::` passed, 31 of 31,
  including `a_curve_match_short_of_decoys_says_why_and_what_would_judge` with its new cases, and the same
  two `fdop-core` tests passed, on one worker with two build jobs. Workspace all-target Clippy passed with
  warnings denied, on another worker with one. rustfmt reports the same 4 differences in `src/pipeline.rs`
  as without the follow-up, none in its lines. The same three checks passed again on `80b8241` with the
  follow-up, the tree it was committed on, which adds ten upstream commits that do not touch
  `src/pipeline.rs`.
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
| `ADS-B fetch: ...` | Network failure or a missing archive day | Rerun; finished days and completed raw feeds are cached when their writes succeed. Online retries reuse matching raw feeds and download missing ones. A saved partial main day can be reused offline with a missing-feed warning. A day without any main-feed data cannot be searched |
| A download pauses for a minute, then carries on | The server stopped sending with the connection still open. After 60 s with no data, `fdop` reconnects and resumes with an HTTP `Range` request | Nothing. Resumed `206` responses are checked for their byte offset, range boundary and total length before new bytes are accepted. Capped ranges continue until the full object is proved complete; a range's body length is never treated as the archive length. After repeated failures with no new bytes it stops |
| Warning `the ADS-B archive for UTC day(s) ... is not published yet ...: only recording starts up to ... were searched` | The local date runs into a UTC day that adsb.lol has not published yet. Each day appears some hours after it ends | Rerun after the release to search the rest of the date. A recording that ended before the cut was searched in full. With no published part at all, `locate` stops with `no published day covers ...: rerun later` |
| Answer graded **low** | One or more gates failed | Read the gates in `report.html`. A margin marked `provisional, comparison truncated` failed because a comparison was cut: an unchecked rival could still win, so the answer is unconfirmed, not refuted. Otherwise, a small margin means other flights explain the curves almost as well, possibly at nearly the same place: `margin.next_distinct_km` in `result.json` says how far away |
| Margin gate reads `both at their fine-grid cells, because one could not be refined` | Refining the answer or its best rival moved a locating match out of its gates, so the margin compares the two scores from before refinement. A warning gives the refused position | Nothing to change. `margin_top_score` and `margin_rival_score` in `result.json` hold the pair compared; `top_score` is still the answer's refined score |
| An event reads `trace follows only N % of the central frequency drop: no ridge through the CPA` | The fit joins two steady tones with nothing between them, or the pass is too faint to trace through its steepest part | Nothing to change: the event can neither drive nor confirm an answer, though it still counts as heard |
| A search runs for an hour or more | A busy recording and a wide region: every rival is compared, at 50–70 ms of CPU per fine cell, and `fine grid N/M` on stderr shows how far it has got | Narrow `--near` to a county, a point or a box |
| Warning `joined N of M event pairs before the quick-check budget (...)`, `N quick-check survivors > 2000000; scoring the best 2000000` or `confirmation budget: checking 2000000 of N fine candidates; rival comparison is incomplete` | The search outgrew a comparison budget | The margin gate fails and the answer grades **low**; a numerical margin is labelled provisional. Narrow the region or the dates and rerun |
| Warning `ADS-B <day>: no mlatonly data in this catalogue, so aircraft located only by multilateration ... are missing` (or `prod release missing, using staging`) | The cache lacks a feed because its release was unavailable or its download failed | Rerun online to obtain any now-available feeds. Aircraft missing from the data can neither match nor count as unheard passes |
| Disk filling up | Many cached regions and days | Delete old `adsb/<date>/` folders under the cache root |

---

## Limitations

- **The US in practice.** Region parsing, place names, ZIP codes, time zones, radiosondes and the geoid are
  US-specific. ADS-B coverage is global, so a `lat,lon` or box region elsewhere runs, but the place report and
  time-zone handling assume US data.
- **At least two tonal aircraft flyovers**, propeller/rotor sources by default, each heard on both sides of its closest approach, from aircraft that
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
  With fewer than two measured aircraft and budget remaining, a shorter survey revisits refused
  detections' neighborhoods and scans complete ten-second fast paths through the interior.
  An ordinary seed is no longer required for that short bank. Passes that do not clear its
  strong line and measurement gates, or for which no detection budget remains, are still lost.
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

- **On `c2c7578` a wrong date graded high; the decoy-clock gate now grades it low.** Of the 33 development runs
  with no right answer (R2 inputs with a start), the osprey cam's 11-02 recording searched on 10-31 graded high,
  34 km off, with a 6.34-nat margin (`n33_c2c7578`). `8be452d`'s factor for the peak received chirp tipped it
  (`bis_o02_1031`). The factor is correct: the steepest received chirp is steeper than the chirp at the heard
  closest approach by (1 − (v/c)²)^−1.5, and `physics.rs` tests it. Taking it out puts the same recording, searched
  at a wrong place, at high instead (`np_o02_p3`, `n33_gnochirp`). Both wrong answers rest on the same two strong
  passes, so the fault is the high grade, which let chance fits to them through. The
  [decoy-clock gate](#the-gates) lowers this run: at four decoy clocks the same search reaches 17.34 nats against
  its 18.41 (`n1_dec1`). 17 more wrong dates on `c2c7578`, from the cached days, assert nothing (`f17_c2c7578`), so
  they test nothing about the gate. Its line and its decoys were fixed after seeing this one wrong answer, and only
  a sealed set can show what it is worth.
- **Jet matching is experimental.** Known jets are excluded by default; `--include-jets` retains them
  as potential tonal sources throughout both searches. It does not add a jet-specific acoustic model,
  extend the existing frequency/speed envelope, or establish accuracy on real jet recordings.
  The same unheard-pass assumptions apply to included jets, even when their noise is not tonal.
- **The curve match's decoy test checks the clock, not the place.** On the right date the real window holds real
  aircraft tones, so a wrong place can beat every decoy clock. On set M, `b8abf86` and `11313ba` put Washington Square
  5.9 km off at medium from the curve match alone: 21.4 nats, 0.2 over the best of 48 decoy clocks, with six passes
  heard ([bench/SET_M.md](bench/SET_M.md)). The lead over the best decoy does not tell a right place from a wrong one.
  On the benchmark, 25 `fdop` runs so far had a curve match ahead of every decoy, several of them builds of one case.
  The wrong places led by 0.2, 1.9 and 4.1 nats, and the right ones by 1.1 to 44.8: Olympia (set E) by 2.5 and 2.9
  and Winter Park (set I) by 2.0, both medium and right. `9087ec4`'s two right answers on set K came from the curve
  match alone, so dropping that path would cost right answers too. Since `8d672af` was merged, such an answer is
  reported at low, not dropped: on `11313ba`'s runs on record it had been wrong once and never right, and set N,
  registered to test the trade, found no case ([bench/SET_N.md](bench/SET_N.md)). `9087ec4` is older code.
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
- **Trace evidence past what the chance null measures is extrapolated, within a limit.** Below the null's 5 %
  quantile `trace_nats` follows a log-normal tail fitted to two quantiles, where an empirical null of n samples
  supports at most ln(n+1), about 6. The fitted tail stops at ln(n+1) + ln(1000), three orders of magnitude further,
  and never above 25 nats. Without that limit a narrow null gave one chance match the full 25. On `b8abf86` the osprey
  cam on 10-31 was placed 0.12 km from the truth and graded low: one chance match on its +1 h decoy clock carried 25
  trace nats. With the limit it is high, and no null or answer got worse in replays of the development rows and the
  33-null battery ([CHANGELOG](CHANGELOG.md)). The limit was picked by arithmetic on saved runs with these recordings
  in view, so those replays could only have shown harm. Set K's saved runs, read after their seal and used for
  development only, show the same failure. On `b759c6f` and `1dec7d6`, `k_weehawken` was placed 41 m from the
  truth and graded low on its decoy clocks alone: its −2 h decoy had one chance match with 20.4 trace nats against
  a null of 262. First-order arithmetic grades it high under the limit and changes no other set K row; that is not a
  replay. A replay on `b8abf86` and `11313ba` grades it high at 33 m on both, so on the current code the limit is
  not what lifts it. On set M the two gave the same answer on all twelve runs ([bench/SET_M.md](bench/SET_M.md)).
  A match inside the limit can still earn about 6.9 nats more than its null can show. On the older `*_r5`
  runs, one reference event earned 2.3–8.3 nats past ln(n+1) on right and wrong dates alike. Capping at ln(n+1)
  itself in stored candidates moved margins both ways, by up to 3.7 nats, so a tighter limit needs its own replays.
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
  diagnostics now run once for the final accepted trace, instead of in every ridge-refinement round. Guided
  rounds also reuse the same normalized short spectrogram and row background while their exact analysis
  window stays unchanged. Only one short spectrum is retained per run of rounds; a different window rebuilds
  it and a return to the main window releases it. The point fits, final diagnostics and gates are retained.
  These changes have no measured end-to-end timing improvement yet. The short-spectrum reuse regressions
  were added during an execution-environment outage and have not run.
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

**Why exclude jets by default?** The model and field validation were developed for propeller and rotor
tones. Turbofan engines can also produce fan and compressor tones ([NASA](https://ntrs.nasa.gov/citations/19950055972)),
but this does not establish that a recording contains stable tones within `fdop`'s analysis band.
`--include-jets` permits those aircraft to be matched experimentally; it does not fit broadband roar.
Unknown aircraft types are kept in either mode.

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

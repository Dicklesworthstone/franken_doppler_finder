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

| Question | Answer |
|---|---|
| Does it run end to end? | **Yes.** `fdop locate` goes from a media file to a report with a map, a radius and a ZIP code. Every stage is automatic: detection, curve fitting, ADS-B download, weather, terrain, search, geocoding and reporting |
| Has it located a real recording? | **One recording, three ways, one answer.** The recording is the raw upload colsto published with the video: one hour of audio. Three configurations were tried: the right date across the whole state (`--dates 2026-09-08 --near NY`), a three-day window (`--dates 2026-09-07..2026-09-09 --near NY`), and a county (`--near "Nassau County, NY"`). All three pass every gate at **high** confidence and give the same spot: a 292 m radius about **224 m** from the location published in the video (221 m for the county search). Each matched the same three flyovers: a Pilatus PC-12, a Cessna 208 and a King Air 350 |
| Has it been wrong? | **Yes, and so far every committed build has graded its wrong answers low.** On the reference recording, earlier builds named Old Lyme, CT (116 km off) and later Southold, NY for the three-day window (102 km off). In the field tests below, five answers were wrong. A low grade does not mean wrong, though: one build put the state-wide answer 129 m off at high confidence but graded the county search low. The [CHANGELOG](CHANGELOG.md) has every run. Treat **low** as "unconfirmed", whichever way it points. A working-tree experiment that measured more of a 3 h recording's flyovers graded a wrong answer **medium** (103 km off; see [Known issues](#known-issues)), so medium alone does not prove an answer either |
| Is it tested? | **74 unit tests,** all offline: synthetic known-answer checks of detection, fitting, emission-time physics, track models and a two-flyover search, plus parsers, formats and a stalled download resuming over a loopback socket. `fdop selftest` runs the real decode, detect, measure and search path on a synthesised recording and recovers the observer to under 1 m |
| Has it located a recording it was not developed on? | **Once, at the same site.** Nine recordings from public YouTube live cams (2–3 h each, with published locations, and true clocks from the stream timestamps) were searched with only the local date and the state. An osprey cam at the reference site, recorded on a different day with different flights, came out **high**: about 150 m from the cam's geocoded street address, just outside its 147 m radius (an address geocode only approximates where a camera is), with the recording start off by only 0.33 s. The same cam on two other days gave wrong answers graded **low**. An eagle cam at Duke Farms (Hillsborough, NJ) came out in the right township, about 2.2 km from the property's entrance address, graded **low** (the nest position is not published, so a smaller error cannot be scored). At five other sites (railcams in Rochelle, IL, Tucson, AZ, Plant City, FL and Fullerton, CA, and a bird-feeder cam in Ithaca, NY), three answers were wrong and graded **low**, and Plant City and Fullerton gave no location. All but Fullerton held only 0–3 non-jet passes within reach. Fullerton (13 within 3 km) and Duke Farms (16) had more traffic but poor audio. Fullerton's is high-pass filtered at about 125 Hz, which removes or weakens the propeller fundamentals, under broadband wind and road noise; Duke Farms has a wind-saturated microphone. No wrong answer from a committed build was graded above low |
| What is not validated? | Any site other than the reference one; low-traffic or rural sites (all four tried failed); CONUS-wide searches on real data; recordings outside the US; phone-call or heavily processed audio |
| Which code produced these results? | The reference runs `ny9`, `md2` and `nassau3` used the second commit, `d79d90d`: the import plus a 5-line change to evidence calibration, which only matters when a region leaves an event fewer than 20 chance samples to calibrate against (CHANGELOG section 11). `ny10` repeated the state-wide run on `1f321cb` with the same answer. The field tests ran first on working-tree builds made just before `e872b1c` was committed, then all six again on `1f321cb`. The last three ran on a build of the tree at `25f12ff`. Before `1f321cb` the osprey-cam answer was the same point graded **medium**, and the four wrong answers named other towns, also graded **low**. See [CHANGELOG.md](CHANGELOG.md) |
| Is it fast? | **Three measurements on one machine.** The state-wide run took 295 s and the three-day run 440 s on an Apple M5 Ultra, with the ADS-B days already cached. See [Performance](#performance) |

The track interpolation changes after those recorded runs have synthetic regression coverage. The reference
recording and field recordings have not yet been rerun on those changes; the historical distances and grades
above are not a validation of the updated interpolation.

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
| margin | yes | evidence 39.2 nats vs 30.7 for the best different explanation (PC-12 + a PA-32 + a P212, 23.8 km away): 8.5 nats, likelihood ratio about 5153:1 |
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

The same recording with a three-day window (`--dates 2026-09-07..2026-09-09`, run `md2`) gave the same point,
radius and flights at high confidence, with a 7.9-nat margin over a rival 130 km away. Searching only Nassau
County (run `nassau3`) gave a point 221 m from the published one, also high, with the same 292 m radius.

A known-answer check that needs no network:

```console
$ fdop selftest
selftest: PASS | error 0.4 m, radius 75 m, clock error +0.20 s, 4 events, flights ["ACE391", "A1DFEB"]
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
f_pre = f0·c/(c − v)    f_post = f0·c/(c + v)    steepest slope = f0·v²/(c·d)
```

The pitch drop sets `v`, the steepness sets `d`, and the curve's centre sets the heard CPA time `t0r`. Matched to
one flight, one flyover therefore puts the recorder on a line beside that flight's path, each point of it tied to
a different recording clock. Two flyovers, with the real time gap between them, intersect at a point and fix the
recording clock. Detection and fitting use still air at 343 m/s. The search uses the real atmosphere: the
effective speed of sound and the along-path wind for each pass, which rescale the fitted `v` and `d`
consistently.

### The pipeline

```text
 media file ──ffprobe/ffmpeg──► mono f32 @ 4 kHz
      │
      ▼  detect   1 s STFT, 35–1100 Hz ─► 48 bins/octave robust z-map ─► matched-filter bank of
      │           emission-time templates over speed (40–175 m/s) × CPA distance (250 m–12 km);
      │           both halves of a pass must light up, so steady hum and clicks are rejected
      ▼  measure  per event: STFT window ≈ 1/√(steepest slope), model-guided ridge trace, bounded
      │           trust-region fit with multistarts and robust loss, profile-likelihood ranges,
      │           manoeuvre / side-disagreement / structured-residual warnings that widen the ranges
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
          6. refine on a 25 m grid; the 95 % Δχ² region gives the radius (75 m floor)
      │
      ▼
 gates ─► confidence ─► reverse geocode ─► map.png / report.html / report.md / result.json ─► run ledger
```

### The gates

| Gate | Passes when |
|---|---|
| core matches | at least 2 matches locate the observer: each fits its ADS-B prediction with chi ≤ 4, has its own best clock within 4 s, and beats chance by ≥ 2 nats. When an event has fewer than 20 chance samples to calibrate against (a tiny region), chi ≤ 2 stands in for the nats test |
| clock lock | every match's own best clock is within its timing tolerance (clamped to 4–6 s) of the shared clock |
| margin | the answer beats the best *different* explanation by ≥ 6 nats, or no rival survived. A rival is different if it matches a shared event to another flight, shares no match, or lies more than 5 km away |
| no unheard close pass | no tonal aircraft passed much closer than the heard ones without leaving a detection |
| radius | the 95 % radius is ≤ 1,000 m and is not merely a lower bound (the region did not reach the edge of the refinement window) |

**High** confidence needs every gate. **Medium** needs two core matches, clock lock, and a margin of at least
3 nats (or no rival). Everything else is **low**. The full gate details are in `result.json` and `report.html`.

---

## Design principles

1. **Refuse rather than guess.** A single flyover constrains the recorder to a line, not a point, so `fdop`
   refuses it. "No location" is a normal outcome with its own exit code. A low-confidence answer is labelled
   low, and the report shows the alternatives it beat.
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
and takes only `-o` and `-q`. `runs` and `doctor` take `--cache` too.

| Shared option | Meaning |
|---|---|
| `--cache <PATH>` | Cache root for ADS-B day stores, terrain, gazetteer and map tiles. Default: see [Configuration](#configuration) |
| `--offline` | Never use the network; cached data only |
| `-q, --quiet` | Less progress output on stderr |

### `fdop locate <MEDIA>`: the full pipeline

| Option | Default | Meaning |
|---|---|---|
| `<MEDIA>` | required | Audio or video file, anything ffmpeg reads |
| `--dates <D>` | required | Local date(s) of the recording: `YYYY-MM-DD` or `YYYY-MM-DD..YYYY-MM-DD` (also `D0 to D1`, `D0,D1`, `YYYY/MM/DD`, `YYYYMMDD`). The year comes first: `09/08/2026` is refused as ambiguous. At most 15 days |
| `--near <REGION>` | required | Coarse US region; see [Region syntax](#region-syntax) |
| `-o, --out <DIR>` | `fdop_runs/<name>_<unix time>` | Output directory |
| `--start <ISO>` | — | Exact recording start if known, e.g. `2026-09-08T19:17:30-04:00`. Without a zone it is read as **UTC**. Unix seconds also work |
| `--clock-tol <S>` | `180` | Clock tolerance (s) around `--start` or the file's `creation_time` |
| `--ignore-metadata-time` | off | Ignore the container's `creation_time` |
| `--max-events <N>` | `12` | Maximum events that drive the pair join; the others can still confirm |
| `--json` | off | Print the full result JSON on stdout instead of the summary |

```bash
fdop locate clip.m4a --dates 2026-09-08 --near NY
fdop locate clip.mov --dates 2026-09-07..2026-09-09 --near "Oyster Bay, NY" -o runs/oyster
fdop locate clip.wav --dates 2026-09-08 --near "40.87,-73.52,25" --start 2026-09-08T15:17:30-04:00 --clock-tol 60
fdop locate clip.m4a --dates 2026-09-08 --near CONUS --json > result.json
fdop locate clip.m4a --dates 2026-09-08 --near NY --offline        # cached data only
```

**The time window.** Without `--start`, recording starts are searched from local midnight on the first date to
local midnight after the last, using every UTC offset the region spans, DST included. If the file's
`creation_time` falls inside that window, the search first narrows to it, allowing for both start and end
conventions. If that finds nothing, it widens to the full window and says so in the warnings.

### `fdop detect <MEDIA>`: flyovers only, no ADS-B

Detects and measures up to 24 flyovers, with a warning on stderr when the detection or measurement cap cuts
some, and writes `events.json`, `survey.png`, and `e<N>.png` plus `e<N>_trace.csv` for every fitted event. The
default output directory is `fdop_runs/detect`. One line per event goes to stdout:

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
| `detections.json`, `events.json`, `search.json` | Intermediate results |

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
├── terrarium/<z>/<x>/<y>.png                         elevation tiles
├── usgs_topo/<z>/<y>/<x>.jpg                         basemap tiles for map.png
├── gazetteer/                                        Census 2024 gazetteer zips
└── runs.sqlite                                       the run ledger (FrankenSQLite)
```

A cached day store is reused whenever its box covers the request. A larger region on the same day downloads
that day again.

### Data sources

| Data | Source | Notes |
|---|---|---|
| ADS-B and MLAT tracks | [adsb.lol](https://adsb.lol) `globe_history_<year>` GitHub releases | ODbL. One global archive per UTC day (`prod` about 3.6–3.9 GB, `mlatonly` about 0.3 GB), streamed and filtered to the region; `staging` is used when `prod` is missing |
| Aircraft engine class | `crates/fdop-types/data/icao_engine_class.tsv` | 2,614 ICAO type designators → piston, turboprop, jet, electric or other. Jets are dropped; unknown types are kept |
| Weather | [Open-Meteo](https://open-meteo.com) historical forecast (pressure levels) and archive; [Iowa Environmental Mesonet](https://mesonet.agron.iastate.edu) radiosondes | First usable source wins; falls back to a 15 °C standard atmosphere with a warning |
| Terrain | [AWS Terrain Tiles](https://registry.opendata.aws/terrain-tiles/) (Terrarium encoding) | z10 for the coarse search, z13 for the fine search; a tile that cannot be fetched counts as 0 m, with a warning giving the number of such tiles |
| Geoid | [NOAA NGS](https://geodesy.noaa.gov) GEOID API | Converts GNSS heights to MSL; −30 m, with a warning, if unreachable or `--offline` |
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
| `franken_doppler_finder` (`fdop`) | all five | clap, fsqlite, asupersync, rayon, serde_json; fsci-fft, fsci-spatial and frankenpandas in `doctor`'s smoke tests |

Design notes:
- **Detection is a template bank, not peak picking.** On a log-frequency axis, changing `f0` only shifts a
  curve vertically, so one template per (speed, distance) scans every (time, pitch). Scoring takes the
  *minimum* of the approach and recede halves, which rejects flat tones and one-sided transients.
- **The search is flight-centric.** It does not grid the whole region. For each event and each flight, every
  moment of the flight is a candidate CPA: the recorder then sits on the perpendicular to the path through that
  point, at a distance that fits the measured CPA distance, and the moment fixes the recording start. A hash join
  on (east, north, recording start) finds the places two events share in O(N) time.
- **Hypotheses are compact.** Each is 20 bytes (a `u32` track index and four `f32`). The budgets are sized from
  physical memory: a tenth of RAM, clamped to 200 M–2 G hypotheses in total (4–40 GB), so on machines with less
  than 40 GB the floor is more than a tenth.
  A pair-join or fine-stage cut is reported in `funnel.truncated` and the warnings. An event that exceeds its
  hypothesis budget, or falls beyond `--max-events`, drops to confirmer-only, which `funnel.hypotheses` shows;
  a budget demotion is also a warning.
- **Parallel, and repeatable up to exact ties.** rayon runs the STFT frames, the template bank, tracing and
  every search stage. Each FFT is pinned to one worker so it does not nest inside rayon. Null sampling uses a
  fixed hash. Candidates with exactly equal scores can come out in a different order from run to run.
- **Sparse turns retain valid reported velocities.** If a reported tangent disagrees with the finite
  difference of nearby positions, the track builder also checks the position chords against the circular arc
  implied by neighbouring headings. Both checks use the same 25 m/s residual limit; a turn does not disable
  checks for incorrect ground speed, heading or vertical rate.

---

## Performance

Three runs of the reference case, on one machine. Treat them as an order of magnitude, not a benchmark.

| Stage | `ny9`: NY, 1 day (s) | `md2`: NY, 3 days (s) | `nassau3`: Nassau County, 1 day (s) |
|---|---|---|---|
| decode (1 h of audio → 4 kHz mono) | 9.2 | 23.6 | 9.2 |
| detect | 2.5 | 1.4 | 2.4 |
| measure (24 events) | 20.7 | 14.5 | 20.7 |
| ADS-B (UTC days cached) | 1.7 | 2.2 | 1.8 |
| search | 258.8 | 396.9 | 65.8 |
| answer, geocode, map, report | 0.6 | 0.7 | 0.7 |
| **total** | **294.6** | **440.2** | **101.5** |

The state-wide run searched 10,550 track models. All three ran on an Apple M5 Ultra with the ADS-B days already
cached. A first run adds the archive download (about 4 GB per UTC day) and the store build. One local date
usually spans two UTC days. Worst-case search memory is set by the hypothesis budget: a tenth of RAM, clamped to
4–40 GB. A 3 h field recording took 509 s, 205 s of it in measurement.

---

## Testing

```bash
cargo test --workspace --locked                               # 74 unit tests, no network
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
- fit recovery and range coverage;
- Hermite track models against closed forms;
- sparse turning tracks, including unequal sample spacing and heading wraparound, against an analytic circle;
- a two-flyover search, with and without wind, that locates the observer within 60 m and the clock within 1 s;
- junk positions inside an ADS-B leg are dropped before track models are built;
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
   - high-pass filtering (the 80–500 Hz band overlaps speech, and the curve came back after a boost);
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
| `GitHub API refused ... limited to 60/hour, set GITHUB_TOKEN` | Unauthenticated GitHub API quota | `export GITHUB_TOKEN=$(gh auth token)` or any read-only token |
| `no location: no location found: N usable flyover events; need at least two clear propeller flyovers` | Fewer than two usable passes | Look at `survey.png` for faint falling curves the detector missed. One flyover cannot be located; a longer recording may hold more |
| `no location: ...; no ADS-B flight pair explains two events at one place and clock (...)` | Wrong date or region, or the aircraft had no ADS-B | Check `--dates` (local dates) and widen `--near`; try `--ignore-metadata-time` if the file's `creation_time` is wrong |
| `ambiguous location: 'Springfield' matches places in K states` | Bare place name | Add the state: `"Springfield, IL"` |
| `date range ... spans more than 15 days; narrow it` | Range too long | Split it into several runs |
| `unrecognised date '09/08/2026': put the four-digit year first` | The year is not first. `09/08/2026` is 8 September in the US and 9 August in most other countries | Write it year first: `2026-09-08` |
| Warning `detection kept the 60 strongest of N candidate curves` or `measured the 24 strongest of N flyover detections` | A long or busy recording hit a fixed cap (see [Known issues](#known-issues)) | Nothing to change: the weaker flyovers are not used |
| Warning `the answer lies in CT, outside the searched state NY` (or `appears to lie in`, when the state is a gazetteer estimate) | A state search covers the state's bounding box, which takes in parts of its neighbours | If you know the recording was made in the searched state, distrust the answer whatever its grade, unless its radius reaches back across the border. A narrower `--near`, such as a county, takes in less of the neighbours |
| `needs network or a cached file: Census gazetteer ...` | `--offline` before the gazetteer was cached | Run once online, or use a state, point or box region |
| `ADS-B fetch: ...` | Network failure or a missing archive day | Rerun; finished days are cached. A day missing from adsb.lol cannot be searched |
| A download pauses for a minute, then carries on | The server stopped sending with the connection still open. After 60 s with no data, `fdop` reconnects and resumes with an HTTP `Range` request | Nothing. After 9 failed tries that brought no new data it stops with an error ending `body idle for 60s`; rerun, finished days are kept |
| Warning `the ADS-B archive for UTC day(s) ... is not published yet ...: only recording starts up to ... were searched` | The local date runs into a UTC day that adsb.lol has not published yet. Each day appears some hours after it ends | Rerun after the release to search the rest of the date. A recording that ended before the cut was searched in full. With no published part at all, `locate` stops with `no published day covers ...: rerun later` |
| Answer graded **low** | One or more gates failed | Read the gates in `report.html`. A small margin means another flight pair explains the curves almost as well |
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
    halves the event's weight in the search.
  - The search follows each aircraft's real ADS-B path. For misfit events it also allows one change of tone,
    such as an RPM or power change.
- **Detection envelope:** 35–1100 Hz, about 40–175 m/s and 250 m–12 km at detection, with the decode at
  4 kHz.
- **Atmosphere:** a straight-ray, height-averaged speed of sound with horizontal wind only. No refraction.
- **Data volume:** each new UTC day costs about 4 GB of download, and adsb.lol archives cannot be fetched
  partially. adsb.lol has gaps on some days.
- **The HTML report's map needs internet** to load Leaflet and tiles. `map.png` is self-contained.
- **Located real recordings at one site only.** The reference recording, and a public osprey cam at the same
  site on a different day with different flights, both came out high. Eight further field recordings were graded
  low or gave no location: two from the same cam on other days, four from sites with sparse light-aircraft
  traffic, and two from busier sites with poor audio (a high-pass filter under road noise, and a
  wind-saturated microphone). One of those, at Duke Farms, NJ, was low but in the right township. See
  [Honest status](#honest-status).

### Known issues

- Jets are excluded when the ADS-B archive is ingested, so they can neither explain an event nor count as an
  unheard close pass. There is no option to include them.
- Detection and measurement are capped: at most 60 candidate curves, the 24 strongest flyovers measured, and 12
  of those driving the pair join (`--max-events`). The reference recording already hits the first two caps (82
  curves, 32 strong flyovers), and a 3 h recording loses most of its flyovers. Each cut is a report warning.
  Raising the caps does not simply help. A working-tree build that scaled them with duration (180 curves, 72
  measured) was tried on a 3 h osprey-cam recording. Its answer went from wrong and **low** to wrong and
  **medium**, 103 km off, and rested on two weak events that the old cap never measured. The medium grade does
  not yet allow for how many events were searched, so the caps stay until it does.
- Measurement is slow on long traces: 205 s of a 509 s run on a 3 h recording.
- `--near <state>` searches the state's bounding box, which takes in parts of neighbouring states, so a wrong
  (low) answer can land outside the state (Connecticut for a New York search). The report warns when the
  answer's reverse geocode is in another state.
- Helicopter passes (rotor tones, speed changes) can fit with confidently wrong speed and distance. They do
  harm only through false pairings, which the margin gate is meant to catch.

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

**How precise is it?** On the reference recording, the high-confidence answers landed 221–224 m from the
published location, inside their 292 m radius. On the osprey-cam field recording it was about 150 m from the
cam's geocoded street address, just outside its 147 m radius; an address geocode only approximates where a
camera is. Precision depends on how many clean passes there are, how close they were, and the ADS-B cadence. The
radius is the run's own measure of its uncertainty, and is meaningful only for a **high** or **medium**
answer. A **low** answer can be far off; one was 102 km off.

**How long does it take?** A one-day state-wide search of a 1 h recording took about 5 minutes with the data
cached; a three-day window took about 7.5, and a 3 h recording about 8.5. The first run on new dates is
dominated by the archive download.

**Can I search the whole country?** `--near CONUS` is accepted. It downloads the same daily archives but keeps
far more tracks, and the budgets may truncate the search: a pair-join or fine-stage cut appears in the funnel
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
- **adsb.lol** and its volunteer feeders, for the open ADS-B archive (ODbL).
- **Data providers:** the U.S. Census Bureau, USGS The National Map, NOAA NGS, Open-Meteo, the Iowa Environmental
  Mesonet, and AWS Terrain Tiles.
- **Report map:** Leaflet and OpenStreetMap.

## License

MIT with the OpenAI/Anthropic rider. See [LICENSE](LICENSE).

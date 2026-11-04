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
  away, but none was audible. Wind and road noise fill 50–300 Hz, and its 4 detections were trains and vehicles
  (one with a 42 m closest approach).
- **Duke Farms** had 16 passes within 3 km, but a wind-saturated microphone. All four matched aircraft (two core,
  two confirming) were real passes over the property, so the search found the right flights and clock and
  placed them loosely. Only 1 of 4 traces fit cleanly (chi 1.2; the others 10–12), and a C172 passed 604 m from
  the answer unheard. The margin passed at 8.7 nats; the core-match and unheard-pass gates failed, correctly.
- **The osprey cam on 2026-11-02** (15:45–18:30Z) had only 4 passes within 3 km, one of them close: margin
  4.8 nats, 1 of 2 core.

Two dense-traffic sites had noisy microphones, and neither reached high. Every wrong answer from a committed build
is still low: nine across the field tests and builds.

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

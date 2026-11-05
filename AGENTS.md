# AGENTS.md - franken_doppler_finder

> Guidelines for AI coding agents working in this aircraft-Doppler audio geolocation codebase
> (`franken_doppler_finder`; crates `fdop-*`; binary `fdop`).

---

## RULE 0 - THE FUNDAMENTAL OVERRIDE PREROGATIVE

If I tell you to do something, even if it goes against what follows below, YOU MUST LISTEN TO ME. I AM IN
CHARGE, NOT YOU.

---

## RULE 0.1 - VALUE DELIVERY OVER PROCESS: NO PROCESS PORN

The product is one command: `fdop locate <media> --dates ... --near ...` returns a correct location with an
honest radius and confidence, or refuses. Every hour spent on meta-infrastructure is an hour not spent on the
code that finds a fainter curve, fits it better, matches it to the right flight, or refuses a wrong answer.

**Process is never the product unless the user explicitly asks for process work.** Plans, trackers, Agent Mail,
audits, harnesses, dashboards and status reports exist only to support delivery.

### The value test

Before doing non-product work, answer all three:
1. What concrete capability, correctness defect or immediate blocker does it address?
2. Is it the smallest direct action that addresses it?
3. Will its likely value exceed its cost right now?

If any answer is unclear, **do not do the work**. Return to the requested functionality.

### Hard scope limits

- **Real code and real tests come in the same unit of work.** Not allowed:
  - faked tests;
  - mocks or fixtures presented as live proof;
  - weakened assertions or widened tolerances to make a test pass;
  - hard-coded success paths;
  - `todo!()` or `unimplemented!()` in committed production paths.
- **Refusal discipline.** "No location" beats a fabricated one, and a low-confidence answer must say low. But
  refusal-only work never counts as a positive capability.
- **Reviews stay narrow.** A review request authorizes reading the relevant code, reproducing concrete defects,
  and the smallest sound fixes with focused regressions. It does not authorize redesigning adjacent code.
- **No bloat.** Never turn a small repair into hundreds of lines of harness, schema or logging without explicit
  approval.
- **Stay on target.** Do not spawn an agent swarm for a narrow task.

### Mandatory checkpoint and stop rule

At the first sign of scope expansion, state the outcome, the files involved, why it is necessary and the
smaller alternatives. If that cannot establish immediate value in a few sentences, ask first.

---

## RULE NUMBER 1: NO FILE DELETION

**YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.** Even a new file that you yourself
created, such as a test file. You must always ask and receive clear, written permission before deleting a
file or folder of any kind.

---

## Irreversible Git & Filesystem Actions - DO NOT EVER BREAK GLASS

1. **Absolutely forbidden commands:** `git reset --hard`, `git clean -fd`, `rm -rf`, or any command that can
   delete or overwrite code/data, unless the user explicitly provides the exact command and states, in the
   same message, that they understand and want the irreversible consequences.
2. **No guessing:** if there is any uncertainty about what a command might delete or overwrite, stop and ask.
3. **Safer alternatives first:** `git status`, `git diff`, backups, hand-written patches.
4. **Mandatory explicit plan:** even after authorization, restate the command verbatim, list what will be
   affected, and wait for confirmation.
5. **Document the confirmation** in your final response: the authorizing text, the command, the time.

Never stash, revert, overwrite or otherwise disturb another agent's work. Changes you did not make are another
agent's work in progress: treat them as your own.

---

## Git Branch: Canonical `main`, Mirrored to `master`

All work happens on `main`. The only other permitted branch is `master`, an exact mirror of `main`. After
pushing `main`, fast-forward both local and remote `master` to the same commit. Never develop on `master`,
force-push it, or discard unique history to make it match.

## RULE 2: NO GIT BRANCHES. NO GIT WORKTREES. EVER.

**Forbidden:** feature branches, `git checkout -b`, `git switch -c`, `git worktree add`, pushing other refs,
pull requests, scratch clones used to isolate work, and any tool that creates branches or worktrees as a side
effect.

**Instead:** commit to `main` when the work is ready, keep unfinished work in the tree, and coordinate through
Agent Mail reservations.

---

## Project Truth Sources

- **`README.md`** is the user-facing contract: the command reference, region syntax, outputs, gates, data
  sources, limitations and known issues. Keep it in step with the code (see Documentation).
- **The code** is the truth for behaviour. When the README and the code disagree, fix one of them in the same
  commit and say which.
- **`CHANGELOG.md`** is the project history for agents.
- **The method** comes from colsto's video "I discovered a sound that leaks your exact location (and it's
  everywhere)" (https://www.youtube.com/watch?v=UytYrhYLOKI, 2026-11-02). The video's description says no code
  was released; `fdop` is an independent re-implementation.
- **The reference oracle and validation notes** (a Python implementation, method notes and the V1–V4 validation
  ledger) live in the private `doppler-sounds-for-geolocation` agent skill, outside this repository.

---

## franken_doppler_finder - This Project

`fdop` locates where an audio or video recording was made. It detects the Doppler curves of propeller aircraft
in the recording, fits each with an emission-time model, downloads the adsb.lol ADS-B archive for the given
dates, and searches every flight in a coarse US region for the one place and recording clock at which two or
more curves agree. It grades the answer with five gates and writes a report with a map, a radius, and the
nearest town and ZIP code.

| Stage | Where |
|---|---|
| Decode (ffmpeg → mono f32, 4 kHz) | `crates/fdop-core/src/audio.rs` |
| Spectrograms | `crates/fdop-core/src/spectro.rs` |
| Detection: log-frequency template bank | `crates/fdop-core/src/detect.rs` |
| Ridge tracing and measurement | `crates/fdop-core/src/{trace,measure}.rs` |
| Emission-time model and fitting | `crates/fdop-core/src/{physics,fit}.rs` |
| ADS-B track models | `crates/fdop-core/src/track.rs` |
| Search: hypotheses, pair join, fine grid, refine | `crates/fdop-core/src/search.rs` |
| ADS-B ingest and the `.fdtrk` store | `crates/fdop-data/src/{adsblol,ingest,build,fetch}.rs`, `crates/fdop-types/src/store.rs` |
| Weather, terrain, geoid | `crates/fdop-data/src/{weather,terrain,geoid}.rs`, `crates/fdop-types/src/atmo.rs` |
| Regions, gazetteer, geocoding, map, reports | `crates/fdop-geo/src/*` |
| CLI, pipeline, gates, run ledger | `src/{cli,pipeline,ledger,doctor,selftest}.rs` |

### What this project is NOT

- Not a tool for locating other people's recordings without permission. The CLI's permission notice and the
  report's permission warning stay.
- Not a GPS-metadata reader. It ignores location tags and must keep working on stripped files.
- Not a real-time tracker, an ADS-B database or a general acoustic classifier.
- Not a generic geolocation service outside the US: region parsing, geocoding, time zones, radiosondes and the
  geoid are US-specific.

---

## Claims - Never Conflate Them

| Claim | Evidence | Permitted wording |
|---|---|---|
| **Known answer** | Unit tests and `fdop selftest` on synthesised audio | "recovers the synthetic observer within X m" |
| **Reference case** | `fdop locate` on the source video's raw recording against the published answer | "224 m from the published location on the reference recording" |
| **Performance** | A timed run with hardware, region and cache state | "295 s on an Apple M5 Ultra, `--near NY`, one day, ADS-B cached" |

A selftest PASS does not show that real recordings work. One reference case does not establish general
accuracy. A timing is never evidence of correctness. Any number in docs, commit messages or reports cites the
run (`result.json`) or the test that produced it.

---

## Honesty Rules

1. **Never print an answer that bypasses the gates.** Every location carries its radius, confidence and gate
   results.
2. **Refuse, never fabricate.** A single flyover is refused. An ADS-B day missing from the archive is an error.
   The one exception is a trailing day adsb.lol has not published yet: it clips the search window, with a warning.
   Every fallback adds a warning to the report: the standard atmosphere, a default geoid, terrain tiles read as
   0 m, a failed terrain prefetch and a missing basemap. A new fallback must do the same.
3. **Report truncation.** The detection cap (60 candidate curves) and the measurement cap (24 events) put a
   warning in the report when they cut. Pair-join, fine-stage and confirmation cuts appear in `funnel.truncated`
   and the warnings, and any of them fails the margin gate. Their default budgets (20 M, 2 M, 2 M) sit above the
   largest searches estimated from real runs. An event demoted to confirmer-only by its hypothesis budget gets a warning. Events beyond
   `--max-events` only confirm by design (shown in `funnel.hypotheses`). Do not add a silent cut.
4. **Do not overfit the reference case.** There is one reference recording and a handful of field tests. A
   change to a threshold or constant needs a synthetic or metamorphic test that justifies it on its own terms.
   Report its effect on the reference case and the field tests, including when it gets worse.
5. **Report failures as failures.** A wrong or low-confidence result on the reference case is recorded, not
   rerun until it passes.

---

## Data, Privacy And Responsible Use

- **Never commit recordings, real run directories or cache files.** That covers media files, `fdop_runs/`,
  `result.json` from real recordings, `.fdtrk` stores and `runs.sqlite`. `.gitignore` enforces the common
  cases; you enforce the spirit. Tests synthesise their audio.
- **One real location only.** The only real-world location in tests and fixtures is the answer published in the
  source video: 40.876466, −73.524951 (Oyster Bay, NY, 2026-09-08), and the flights the video shows there. Do
  not add anyone else's location, recording or flight history.
- **Field tests on public live cams** (streams with published locations) may be summarised in the docs at town
  level: the site type, the town, the date, the confidence and the error. Never commit their recordings, their
  coordinates or the registrations of the aircraft heard.
- **No personal data** in fixtures, docs, examples or commit messages.
- **Network etiquette.** The HTTP User-Agent (`crates/fdop-http/src/lib.rs`) names the tool honestly; keep it
  that way. Cache every response, respect the GitHub API limit (60/hour without `GITHUB_TOKEN`), and never
  hammer the Census, NGS, Open-Meteo or IEM services in loops or tests.
- **Attribution stays.** Reports carry the data attribution for adsb.lol (ODbL), USGS, OSM, Census and AWS
  Terrain Tiles.

---

## Pins

| Item | Pin |
|---|---|
| Toolchain | `nightly-2026-08-31` (`rust-toolchain.toml`), Rust 2024 edition, resolver 3, one Cargo workspace |
| Async runtime and HTTP | asupersync `0.6.2` with `tls-webpki-roots` (rustls), the only runtime our code uses. fsqlite `0.4.13` brings its own asupersync `0.5.0` internally |
| Numerics | frankenscipy `0.4.0`: `fsci-fft` (STFT), `fsci-opt` (bounded least squares), `fsci-stats` (F distribution), `fsci-spatial` (`fdop doctor` smoke test only) |
| Tables | frankenpandas `0.6.0` |
| Run ledger | fsqlite `0.4.13` (`native`, no default features) |
| External tools | `ffmpeg` and `ffprobe` on `PATH`, plus `sysctl` on macOS to read physical memory. A C compiler for the build (`ring`, bundled SQLite). `yt-dlp` only to fetch the reference recording |

Moving a pin is a deliberate event. Afterwards, run `fdop selftest` and the reference case, and record both
results in the commit message.

---

## Toolchain, Unsafe Code And Dependencies

- **No `unsafe` code.** There is none today; do not introduce it.
- **Library crates do not print**, except test diagnostics and one documented stderr notice when a map renders
  without its basemap (`fdop-geo/src/staticmap.rs`).
- **Banned as direct dependencies:**
  - tokio, hyper, reqwest, axum;
  - rusqlite, sqlx;
  - arrow, parquet, polars;
  - petgraph;
  - chrono and time;
  - Python or other runtimes reached by subprocess.

  arrow, parquet, rusqlite (with its bundled C SQLite) and chrono already arrive transitively through
  frankenpandas. That is accepted, but none of them may become a direct dependency.
- **Layering.**
  - `fdop-types` is the leaf.
  - `fdop-core` depends only on `fdop-types`, the numerics and small utilities (rayon, serde, png), and never
    touches the network.
  - Only `fdop-http` does HTTP; `fdop-data` and `fdop-geo` go through it.
  - The root crate wires everything together.
- **No new crates without approval.**

---

## Workspace Crates

| Crate | Role |
|---|---|
| `fdop-types` | Shared types: geodesy, time, atmosphere, the `.fdtrk` store format (magic `FDOPTRK1`), the ICAO engine-class table |
| `fdop-http` | Blocking HTTP/1.1 client over asupersync and rustls: retries, resumable streaming, atomic downloads |
| `fdop-data` | adsb.lol release discovery, streamed tar ingest, track-store build and cache, weather, terrain, geoid |
| `fdop-core` | Audio, spectrograms, detection, tracing, fitting, physics, track models, search |
| `fdop-geo` | Region parsing, Census gazetteer, reverse geocoding, static maps, HTML/Markdown/CSV reports |
| `franken_doppler_finder` | The `fdop` binary: CLI, `locate` pipeline and gates, run ledger, `doctor`, `selftest` |

---

## Testing Policy

- **Unit tests sit next to the code** and never touch the network. Synthesise audio and tracks with fixed seeds.
- **Known answers:** detection, measurement, fitting, track models and the two-flyover search all have
  synthetic known-answer tests. Extend these rather than adding a framework.
- **Network checks live in examples** (`probe`, `ingest_ny`, `check_oracle`, `live_check`), never in `cargo test`.
- **`fdop selftest` must pass** after any change under `crates/fdop-core` or `src/`.
- **The reference case**, for changes to detection, measurement, fitting or search:

  ```bash
  yt-dlp -x --audio-format m4a -o "ref_raw.%(ext)s" "https://youtu.be/4CxVxuMVkMc"   # keep it outside the repo
  fdop locate ref_raw.m4a --dates 2026-09-08 --near NY -o <outside-the-repo>/ref
  ```

  The upload is the recorder's 1:00:00–2:00:00 window. The published answer is 40.876466, −73.524951. Report the
  distance, the radius, the confidence, the gates and the funnel for each configuration:

  | Configuration | Last recorded result (2026-11-05, the measurement change, run before its rebase onto the heard-range profile, which these runs never reach) |
  |---|---|
  | `--dates 2026-09-08 --near NY` (`r_ny_gc`) | 264 m off, radius 379 m, **high**, margin 12.79 nats at the fine cells against a rival 52 km away, no comparison cut |
  | `--dates 2026-09-08 --near "Nassau County, NY"` (`r_nassau_gc`) | 270 m off, radius 406 m, **high**, margin 14.78 nats after refinement against a rival 33 km away, no comparison cut |
  | `--dates 2026-09-07..2026-09-09 --near NY` (`r_md_gc`) | 263 m off, radius 388 m, **high**, margin 8.44 nats after refinement against a rival 119 km away, no comparison cut |

  All three kept 60 of 82 candidate curves, the detection cap, with a warning. Each measured all 60 from the silence
  line up and 12 passed the quality gates, so the 24 was not reached. On `9087ec4` (`*_b1`) they measured the
  strongest 24 of 42 flyover detections and had 3 usable events; now they have 10 (`funnel.events_used`). All three
  matched N22NY (PC-12) and N501EX (C208) as the core matches, and N93CQ (B350) and an R44 as weaker matches, at chi
  1.29, 0.54, 2.54 and 3.22 in the county search. The pair that seeded the answer is N22NY with the R44 state-wide,
  with N501EX in the county and with N93CQ over three days. In the three-day run the hypotheses of N501EX and the R44
  overflow the budget (warnings), and N501EX is still a core match at the answer. The wrong dates are the other half
  of this check. The same file with `--dates 2026-09-07 --near NY` gives low, 119 km off, core 1 of 3, margin 2.00 at
  the fine cells (`n_ref_on_0907_gc`). On `9087ec4` it was 138 km off, core 2 of 2, 0.60 after refinement
  (`n_ref_on_0907_b1`); 0.61 on the knot-rule commit (`n_ref_on_0907_c1`); 2.95 before the knot rule, 0.05 under the
  medium line. `--dates 2026-09-09 --near NY` gives low, 104 km off, core 0 of 3, margin 0.15 (`n_ref_on_0909_gc`;
  73 km, core 1 of 2, 0.35 on `9087ec4`). A wrong-date medium or high is a regression.
  None of these runs gives a start, so the curve match is skipped in all five. After a change to refinement, the margin
  or trace scoring, also replay the Ithaca feeder (CHANGELOG): a margin that credited only the leader with refinement's
  gain graded its wrong answer medium. On the measurement change both feeder runs are low, 0.19 and 0.99 nats
  (`f_feederwatch_gc`, `n_feeder_on_1101_gc`). These `gc` rows ran on the 32 GiB benchmark machine. The hypothesis
  budget scales with RAM and the comparison budgets do not, so a larger machine has a different funnel and does not
  repeat them: on 256 GiB the three-day run joins 21.1 M points and fills the fine stage's 2 M (`r_md_gh`, still
  running when this was written), against 102,247 fine cells in `r_md_gc`. Replay on the machine that gave the row.

  The first high results were `ny9` (224 m off, margin 8.5 nats), `nassau3` (221 m, 6.8 nats) and `md2` (224 m,
  7.9 nats), all radius 292 m with N93CQ as the confirmer, on the import commit plus the 5-line
  evidence-calibration change described in CHANGELOG section 11. `ny10` repeated the first on `1f321cb`: high,
  224 m off, radius 292 m, margin 7.7 nats, the same flights. Other builds got these configurations wrong or low
  (CHANGELOG reference-case table and Unreleased), so re-run all three and the two wrong dates after any change to
  detection, measurement, search or grading, and update this table and README "Honest status" with the result.

---

## Output Style

- **stdout carries results; stderr carries progress** (`[   12.3s] message`).
- `--json` output and `result.json` field names are an interface; change them deliberately and note it in
  `CHANGELOG.md`.
- **Exit codes:** 0 success; 1 no location, a selftest FAIL (wrong answer or no candidate) or a doctor
  failure; 2 error. Errors print as `fdop: error: ...`.

---

## Compiler Checks

After substantive Rust changes, run these four checks. `dsr quality franken_doppler_finder` runs the same
four; it is the project's CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run --release -- selftest
```

**No GitHub Actions, ever.** Do not add `.github/workflows/` files, Actions badges, or anything that waits on
Actions runs. CI, builds and releases go through dsr: `dsr quality franken_doppler_finder` for the checks, and
`dsr build` / `dsr release` for artifacts.

Use RCH for heavy Cargo work and never bypass the `rch` cargo shim:

```bash
rch exec -- env CARGO_TARGET_DIR="${RCH_TARGET_BASE:-${TMPDIR:-/tmp}}/rch_target_fdop" cargo test --workspace
```

Always base new target dirs on `${RCH_TARGET_BASE:-${TMPDIR:-/tmp}}`, never bare `/tmp`. Before heavy builds on a
laptop, check free disk (`sbh check --need 20G`). Report the exact command and result; if a check did not run,
say so.

---

## Documentation

- **Keep the README's command reference in step.** When you change a CLI flag, default, output file, gate,
  threshold that users see, or environment variable, update `README.md` in the same commit. The command
  reference, region syntax, outputs, gates and Known issues must match the code.
- **Fixing a known issue** means removing it from README "Known issues" in the same commit.
- **CHANGELOG.md:** add user-visible changes under "Unreleased" in the existing style.
- **Comments** document units, frames, sign conventions, thresholds and why they are set where they are. They do
  not restate the code.

---

## Agent Workflow

- **Start.** Read this file and the README sections you will touch, then inspect the tree.
- **While working.** Keep changes scoped. Attach every claim to a test, a run or explicit no-claim language.
- **End.**
  1. Run the applicable checks.
  2. Report exactly what passed and what did not run.
  3. Commit to `main` with a message that names the change and its evidence.
  4. Push, and mirror `master`.

---

## MCP Agent Mail - Multi-Agent Coordination

When Agent Mail is available:
- register for this project path;
- reserve files before editing;
- send start, progress and completion messages;
- release reservations when finished.

Never run `am service restart`, `am service stop`, `am doctor fix`, `am doctor repair` or
`am doctor reconstruct`, and never `kill` an `am` or `mcp-agent-mail` process. If `am` fails, retry once after a
few seconds, then continue without it.

## `ubs` - Bug Scanner

Before committing code: `ubs $(git diff --name-only --cached)`. Fix true positives at the root cause.

## Search Tools

`rg` for targeted text search, `ast-grep` when syntax matters. Avoid scripted rewrites; make focused hand
patches.

---

## Quick Reference

```bash
fdop doctor --network                                    # tools, cache, services
fdop selftest                                            # offline known answer (needs ffmpeg)
fdop fetch --dates 2026-09-08 --near NY                  # pre-cache ADS-B
fdop locate clip.m4a --dates 2026-09-08 --near NY        # the full pipeline
fdop detect clip.m4a                                     # flyovers only, no ADS-B
fdop runs --json                                         # the run ledger
```

- **Cache:** `--cache`, then `$FDOP_CACHE`, `$FDOP_CACHE_DIR`, `$XDG_CACHE_HOME/fdop`, then `~/.cache/fdop`. It
  holds ADS-B day stores, terrain, map tiles, gazetteer, weather and `runs.sqlite`.
- **Runs:** written to `./fdop_runs/<name>_<unix time>/` unless `-o` is given.
- **Data cost:** about 4 GB per uncached UTC day; one local date usually spans two UTC days.
- **`GITHUB_TOKEN`:** optional; raises the GitHub API limit used to list adsb.lol releases.

---

## Current Repository State

The pipeline runs end to end. It has offline unit tests, a selftest and one validated reference case (above).
Field tests on public live-cam recordings are summarised in README "Honest status": the reference site was
located again from a recording on another day, and four sparse-traffic sites failed, graded low or with no
location. On the current code the reference recording grades high in all three configurations above, and margin
does not separate right from wrong (README "Known issues"), so read any grade with the wrong-date runs beside it.
The benchmark's sealed set E has been read: no build beat `dopplerprint`, and `6914ab7` was wrong at medium, from the
curve match alone on one heard pass ([bench/PROTOCOL.md](bench/PROTOCOL.md) Addendum 14). The curve match now raises
an answer only on two heard passes, which is needed, not sufficient (README "Known issues").

Known fixes not landed. Each one changes answers, so it waits until the reference configurations and the
quoted field tests can be re-run, which needs a Mac build:
- The medium grade and the margin do not allow for how many events or how large a region were searched (README
  "Known issues"). Measurement goes past the old line only behind the `measurement line` gate: a medium resting on
  such an event needs a curve match that judged it. Fix the grade itself before raising the detection cap or the
  24 that must pass.

`f717f6f` keeps reported ADS-B velocities on sparsely sampled turns, by checking position chords against the arc
implied by neighbouring headings at the same 25 m/s limit, and `a4df296` stops a wrong heading from moving its own
prediction. Synthetic turning-track tests cover them. The reference configurations and all nine field recordings
have since been replayed on a build containing them (README "Honest status").

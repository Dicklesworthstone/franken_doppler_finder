# Set M: six more New York field recordings, left by set K's per-metro cap

Fixed on 2026-11-09, before any of its audio was fetched or any tool ran on it. It asks set K's question (`SET_K.md`)
with set K's inputs, scoring and rules, except where this file says otherwise. Later changes go in a dated section at
the end; nothing here is edited to suit a result.

## Why

`11313ba` stops the fitted null tail at ln(n+1) + ln(1000) nats. Development replays and arithmetic on set K's saved
runs say it lifts right answers that a single chance match on a decoy clock held low. Development replays are not
tests. Set K's screen passed six New York uploads through every one of its screens, steps 2 to 9. Its step 10 did not
take them only because New York already had 6 cases. They are a sealed test that needs no new search.

**Known before this file was written:**
- the results of sets A to L, and set K's supplementary run;
- for these six uploads, set K's screen records. These are their descriptions (site, date and start), the truths
  fixed by its step 5, its density and airport screens, and step 9's traffic counts (passes near the truth in the
  recording's span);
- the null-tail limit's development replays and set K's arithmetic;
- development replays of `k_weehawken` and its null with `11313ba` and `b8abf86`, started before this file. With
  `11313ba`, `k_weehawken` is high at 33 m. `k_weehawken` is in New Jersey, and step 6 put each of the six more than
  1 km from every set K case.

No audio of the six has been fetched or heard (set K captured only its cases), and no tool has run on them.

## Cases

In set K's row order, all from the channel Cloud Forest Walks, all in the New York-Newark-Jersey City metro, from
2023-07-04 to 2023-09-10 local time, 33 to 72 minutes long, each with a 1 km limit:

| Case | Screen row | Site |
|---|---|---|
| `m_washsq` | 2096 | Washington Square Park, Manhattan |
| `m_highline` | 2099 | The High Line at W 25th St, Manhattan |
| `m_hunterspt` | 2103 | Hunter's Point South Park, Queens |
| `m_ftwadsworth` | 2114 | Fort Wadsworth, Staten Island |
| `m_bryantpark` | 2122 | Bryant Park, Manhattan |
| `m_eastriver` | 2128 | East River Esplanade, Manhattan |

Each has a wrong-date null `m_<site>_n`, as set K's.

## Tools under test

`fdop` builds, the binaries the benchmark machine already holds:

| Build | Why | Binary sha256 |
|---|---|---|
| `11313ba` | `main` when this file was written: the null-tail limit | `04c5dc5b05db893c` |
| `b8abf86` | the same code without the limit | `c40fe55b6757135a` |
| `9087ec4` | the best on sets J and K | `faf2a40e7bcc5f60` |

`11313ba`'s binary is `fdop-tail1`, built from `git archive` of gate object `31b6bfe`. Its code tree is `11313ba`'s;
only `README.md` and `CHANGELOG.md` differ.

Rivals:
- `dt` and `dtfb` exactly as on set K.
- `dopplerprint` in its three regimes, `dp`, `dp48` and `dpdem`, run once each as `dpi`, `dp48i` and `dpdemi`.
  Every day these rows need is from 2023, when adsb.lol published each day as a single `.tar`. Set K found that
  `dopplerprint`'s downloader cannot read such a release. So, as in set K's supplementary run, each needed UTC day
  is ingested by hand first, with `dopplerprint ingest <tar> --out cache/dp/adsb/store --typedb cache/dp/typedb`.
  The day list is each row's span, its start ±2 min to its end, plus the neighbouring day when the span comes
  within 2 h of a UTC midnight. That gives 2023-07-03, 07-04, 07-05 (already in the store), 07-09, 07-10, 07-20, 07-21,
  07-22, 08-05, 08-06, 08-07, 09-02, 09-03, 09-09 and 09-10. No `dopplerprint` row starts until every listed day is
  in the store. A row that still fails for a day not in the store is a harness fault: that day is ingested and the
  row rerun, and both are listed.

Registration closes with this commit. No build or tool is added later.

## Capture, inputs and runs

- **Capture.** As set K: audio with `yt-dlp -f 140` as `audio.m4a`, with its metadata. Both are sha256'd into
  `seal_m.txt`. A file whose whole-file mean is at or below -60 dBFS is void. Nobody listens to a test recording,
  and nothing diagnoses one, until every row is done.
- **Inputs.** R2 as set K: the stated start floored to the minute, ±2 min. A 25 km circle centred 10 km from the
  truth, on the bearing `sha256(case id)[:4] mod 360`. The nulls start 24 h earlier, with the local date one day
  earlier.
- **Runs.** Every registered tool once on every case and null, cases before nulls.
  - `fdop`: `11313ba` first, because its rows build the ADS-B stores, then `b8abf86` and `9087ec4` side by side.
  - `dopplerprint`: `dpi` and `dp48i` with the national DEM aside, then `dpdemi` with it back.
  - `dopplertrack`: `dt`, then `dtfb`.
  - A rerun only for a harness or machine fault, each listed with its cause.

## Scoring and reads

`tools/score.py` gains set M's groups ("M", "MN" and "M+MN") before any set M row runs. All rows are read once,
together, into a new file whose sha256 is recorded. The reads are set K's, for set M alone and for sets J, K and M
summed:
- **More accurate:** a build meets the rule against a rival when it has at least as many correct assertions, no more
  wrong ones (cases and nulls together), and is strictly better on one of the two. For the J+K+M sums,
  `dopplerprint`'s set K counts are its supplementary ones (`dpi`, `dp48i`, `dpdemi`), with the sealed ones beside them.
- **Works near population centres:** right on at least half of the cases, with no wrong assertion on a case or null.
- **Falsifier, for each `fdop` build:** it fires if the build has fewer correct assertions than any rival, or any
  wrong assertion.

Every row of `11313ba` and `b8abf86` is also listed side by side. Their code differs only in the null-tail limit.

## What it can show

Six cases, one recordist, one metro: every case is listed and no significance is claimed. A start time is the
recordist's word, and a wrong one makes a case unsolvable for every tool alike.

Recordings, coordinates and aircraft identifiers stay out of this repository.

## 2026-11-09: capture, runs and result

### Capture and runs

- **Capture.** All six uploads fetched once and sealed in `data/set_m/seal_m.txt` (12 lines). None is void: their
  means are −25.7 to −14.1 dBFS, and they run 33 to 72 minutes.
- **Scorer.** `tools/score.py` gained groups "M", "MN" and "M+MN" before any row ran. Its output on set J and on set K
  (both regimes) was byte-identical before and after the change.
- **Runs.** Regime `mr2` started at 20:15:32Z and finished at 22:13:23Z.
  - All 15 listed days were downloaded and ingested into `dopplerprint`'s store with exit code 0 by 20:38:03Z. No
    `dopplerprint` row failed for a missing day.
  - The national DEM was renamed aside at 20:38:03Z and back at 20:47:15Z, between `dp48i` and `dpdemi`.
  - No row ended in a fault exit code (101, 127, 133, 137 or 143), so no row was rerun.
  - `dt`'s refine step exited 1 on five rows. Both causes are in its own code: a `ValueError` ("no flight near the
    candidate") in its refine, and a pandas `IndexError` on an empty candidate table. They are scored as errors, and
    `dtfb` covers them as registered.

### Result

Read once at 22:15:02Z by `data/set_m/tools/m_read.sh`: `tools/score.py data/set_m/jobs/cases.json runs_m r2`, then
the same on `r2i`, into `runs_m/SCORE_setM_20261109.txt`, sha256 `a114f60932a5488b`. `score.py` prints the rule only
within one regime, so `fdop` against `dopplerprint` was decided from the two tallies by a script fixed and logged before
the read.

| Row | `11313ba` | `b8abf86` | `9087ec4` | `dpi` | `dp48i` | `dpdemi` | `dt` | `dtfb` |
|---|---|---|---|---|---|---|---|---|
| `m_washsq` | **wrong**, 5936 m, medium | **wrong**, 5936 m, medium | declined | declined | declined | declined | **wrong**, 15 km | **wrong**, 15 km |
| `m_highline` | declined | declined | declined | declined | declined | declined | **wrong**, 15 km | **wrong**, 15 km |
| `m_hunterspt` | declined | declined | declined | declined | declined | declined | **wrong**, 24 km | **wrong**, 24 km |
| `m_ftwadsworth` | **right**, 32 m, high | **right**, 32 m, high | **right**, 23 m, high | **right**, 19 m, high | **right**, 19 m, high | **right**, 36 m, high | error | **wrong**, 11 km |
| `m_bryantpark` | declined | declined | declined | declined | declined | declined | error | declined |
| `m_eastriver` | declined | declined | declined | declined | declined | declined | **wrong**, 2816 m | **wrong**, 2816 m |
| `m_washsq_n` | 12 km, low | 12 km, low | 12 km, low | declined | declined | declined | error | declined |
| `m_highline_n` | declined | declined | declined | 13 km, low | 22 km, low | 22 km, low | **wrong**, 26 km | **wrong**, 26 km |
| `m_hunterspt_n` | declined | declined | declined | declined | declined | declined | **wrong**, 23 km | **wrong**, 23 km |
| `m_ftwadsworth_n` | 31 km, low | 31 km, low | declined | **wrong**, 14 km, medium | **wrong**, 14 km, medium | **wrong**, 14 km, medium | **wrong**, 23 km | **wrong**, 23 km |
| `m_bryantpark_n` | declined | declined | declined | declined | declined | declined | error | declined |
| `m_eastriver_n` | declined | declined | declined | declined | declined | declined | error | declined |

"Declined" includes a location that the tool itself graded inconclusive or did not assert; the score file has each
one's distance and grade.

| Tool | Right | Wrong |
|---|---|---|
| `9087ec4` | 1 | 0 |
| `11313ba`, `b8abf86` | 1 | 1 each |
| `dpi`, `dp48i`, `dpdemi` | 1 | 1 each |
| `dt` | 0 | 7 (and 5 errors) |
| `dtfb` | 0 | 8 |

- **More accurate.**
  - `9087ec4` meets the rule against every rival.
  - `11313ba` and `b8abf86` meet it against `dt` and `dtfb` only. Against each `dopplerprint` regime they tie at
    1 right and 1 wrong.
- **Falsifier.** It fires for `11313ba` and for `b8abf86`: each made a wrong assertion on a sealed set. It does not
  fire for `9087ec4`.
- **Works near population centres.** No tool works on set M; that needs 3 of the 6 cases right with no wrong
  assertion.
- **`11313ba` beside `b8abf86`.** Every row is the same apart from wall time. The null-tail limit changed nothing on
  set M.

Sets J, K and M summed (`11313ba` and `b8abf86` ran on set M only):

| Tool | Right | Wrong | Cases right |
|---|---|---|---|
| `9087ec4` | 3 | 0 | 3 of 15 |
| `dopplerprint`, set K's supplementary counts (`dpi`, `dp48i`, `dpdemi`) | 3 | 3, 3 and 4 | 3 of 15 |
| `dopplerprint`, set K's sealed counts (`dp`, `dp48`, `dpdem`) | 2 | 2, 3 and 3 | 2 of 15 |
| `dt`, `dtfb` | 0 | 25 and 26 | 0 of 15 |

On the sums `9087ec4` meets the rule against every rival, on either count of `dopplerprint`. No tool works: none is
right on 8 of the 15 cases.

### Why the current code was wrong on `m_washsq` (read after the seal; for development only)

`11313ba`'s answer there came from the curve match alone. The trace search's own best was not asserted: its
core-match gate found 1 of the 2 matches it needs. The curve match scored 21.4 nats with six passes heard, 0.2 over the best of its
48 decoy clocks; its line is 0.0, so it stood. The decoy test checks the clock, not the place. On the right date the
real window holds real aircraft tones, so a wrong place can beat every decoy. `9087ec4`'s older curve match heard one
pass there, scored 10.2 nats, 7.6 below its best decoy, and did not stand.

The lead over the best decoy does not separate right from wrong. Across the benchmark, 25 `fdop` runs have had a
curve match ahead of every decoy, several of them different builds on one case.
- The wrong places led by 0.2 (`m_washsq`), 1.9 (`e_harbourgate`, set E, medium) and 4.1 nats (`i_winterpark` on
  `a0795de`, low).
- The right places led by 1.1 to 44.8. Among them are Olympia (set E) at 2.5 and 2.9, and Winter Park (set I) on
  `9087ec4` at 2.0, both medium.
- `9087ec4`'s two right answers on set K came from the curve match alone.

So raising the line, or dropping the curve match's own answers, would cost right answers as well as wrong ones. Set M
gives no threshold to change.

### The null-tail limit's development replay of `k_weehawken`

The `b8abf86` control replay of `k_weehawken`, read after this file was registered, gives the same answer as `11313ba`
(high, 33 m; its −2 h decoy has no chance match). So on the current code the limit is not what grades it high. The
"Known before" line above is true but did not say so.

# Set J: cams in large US metros with close traffic, every tool

Fixed on 2026-11-08, before any search for it, at the owner's request. It uses `PROTOCOL.md`'s R2 inputs and scoring,
and set I's rules (`SET_I.md`) except where this file says otherwise. The commit date of this file is the point of the
exercise. Later changes go in a dated section at the end; nothing here is edited to suit a result.

## Why

The owner's question: does `fdop` work in the parts of the contiguous US near population centres, where most people
live? A busy airport next door, or a rural site with almost no aircraft, is not that question. Volunteer recordings
from other metros are not available, so the cases are YouTube live cams that happen to suit, with the site taken from
the operator's own description, as for sets E to I.

Set I answered a narrower question. Its wildlife cams were mostly rural: 20 of the 40 truths that reached its traffic
screen had no pass within 3 km in two hours, and it ended with three cases. Set J keeps set I's screens and adds two:

- the site must be in a metro of at least a million people, in a census tract at urban density;
- no large or medium airport within 5 km.

It also lets each stream's test window be the best of five on the same day, judged from ADS-B alone. That is how a
blind user would do it: record when aircraft are about. Selection still uses no tool's output and no test sound.

**Known before this file was written:** the results of sets A to I, and set I's screen files. No search for set J has
run, and no set J stream has been named, fetched or heard.

## Tools under test

`fdop` builds, each built on the benchmark machine from `git archive` of its commit:

| Build | Why |
|---|---|
| `7e66501` | the benchmark's pinned build |
| `9087ec4` | the best sealed result so far; tied `dopplerprint` on set I |
| `b759c6f` | the decoy-clock gate (high and medium answers must beat four wrong clocks by 3 nats). Its binary is `check_dec4`'s (selftest PASS, sha256 `a59ebecd85f18e45`). That source tar matches `git archive b759c6f` in every file but README and CHANGELOG, which no code includes |
| `1dec7d6` | `main` when this file was written |

A build that cannot be built, or that fails `fdop selftest`, is reported as not run.

Rivals, with `PROTOCOL.md`'s commits and R2 commands, as on sets E, F and I:

- `dopplerprint` as `dp`, `dp48` (its 48 kHz WAV) and `dpdem` (48 kHz plus its regional ground model, Addendum 4).
  `dp` and `dp48` run with no national ground model in their cache.
- `dopplertrack` as `dt`, exactly as on set I: `refine` gets every event after the first two as `--extra`, the
  usage its README shows.
- `dopplertrack` again as `dtfb`, with a fallback fixed here before any run, because on set I every `dt` row ended in
  an error and none in an answer:
  - if `search` writes no candidate row, `dtfb` declines;
  - if `refine` exits non-zero with its "no flight near the candidate" error for an extra event, `dtfb` runs `refine`
    once more with only the first two events and no `--extra`, and that is its answer;
  - otherwise `dtfb`'s row is `dt`'s row.

  `dtfb` reuses `dt`'s spectrogram, events, traces and search, which are deterministic, and adds only that `refine`.
  Both rows are reported. `dt` stays the protocol's row; `dtfb` is the fairer reading of what a user of
  `dopplertrack` would get.

Registration closes with this commit. No build or tool is added later.

## Windows

Five two-hour windows of DVR audio on 2026-11-07, a Saturday, starting at 14:00, 16:00, 18:00, 20:00 and 22:00Z.
Each stream's window is chosen at the traffic step below, from ADS-B alone. YouTube keeps about 120 h of DVR, so the
first window can be fetched until about 2026-11-12T14:00Z. A stream whose DVR no longer reaches its window is not taken.

## Selection, in order

1. **Search.** YouTube's live filter, positions 1 to 40, on 44 queries in this order:
   - set I's 22, in set I's order (`bird feeder live cam` to `zoo live cam`);
   - set F's 12, in `PROTOCOL.md` Addendum 9's order (`train camera` to `bridge live cam`);
   - `park live cam`, `downtown live cam`, `main street live cam`, `town square live cam`, `golf course live cam`,
     `college campus live cam`, `boardwalk live cam`, `duck cam live`, `squirrel cam live`, `live cam usa`.

   Each result list is saved when it is taken. Rows are ordered by position first, then query, and a stream met twice
   keeps its first place, as in set I.
2. **Title screen.** As set I step 2.
3. **Earlier streams.** Out: every stream set I's step 3 put out, set I's three cases, and Boulder County Fairgrounds,
   whose set I capture was taken.
4. **Level.** As set I step 4, at the same instant: 60 s from 2026-11-07T18:00:00Z, qualifying above -60 dBFS. A
   stream set I already levelled keeps set I's result, which was taken the same way at the same instant.
5. **Description screen.** As set I step 5. A stream whose description set I already decided keeps that decision.
6. **Truth.** As set I step 6, including its 5 km rule against sets A to I and the development recordings. A stream
   whose truth set I already fixed keeps that truth.
7. **Metro and density, new.** From the Census geocoder at the truth (vintage `Census2020_Current`, one request per
   truth):
   - the truth lies in a Metropolitan Statistical Area whose 2020 population (`POP100`) is at least 1,000,000;
   - its 2020 census tract has at least 1,000 people per square mile of land (`POP100` over `AREALAND`).

   The metro's name is recorded. Its coordinate is not.
8. **Airport, new.** Out if a `large_airport` or `medium_airport` in OurAirports' `airports.csv` (one download, kept
   with the screen files, its sha256 recorded) lies within 5 km of the truth.
9. **Traffic and window.** As set I step 7, with the 2026-11-07 store that set I's screen built
   (`prod+mlatonly`, CONUS), and the same `cpa_list`. Each of the five windows is counted on its own. A window
   qualifies with at least 6 passes within 3 km from at least 3 aircraft, and at least 2 passes within 1.5 km from at
   least 2 aircraft. The stream's window is the qualifying window with the most passes within 1.5 km, then the most
   within 3 km, then the earliest. A stream with no qualifying window is out.
10. **Cases.** The first 12 streams in row order that pass screens 2 to 9 and whose capture holds, with at most 2 per
    metro. A third stream from a metro is not taken. If fewer pass by the end of the rows, or by the time the
    windows leave the DVR, the set is smaller, and no screen is relaxed.

Fetch faults, retries and streams that cannot be timed are handled as in set I. Every stream reached is listed with
the step it stopped at.

## Capture and seal

As set I, at each case's own window: two hours of DVR audio by `ytlive_fetch.py` (sha256 `ce9d1c3dc9d5`), on the
benchmark machine; cut at the first outage, kept if the clock holds for at least 30 min; the true clock is the first
fragment's First-Frame stamp; a capture whose whole-file mean and peak are digital silence is void.

**Seal.** Nobody listens to a test recording. No tool runs on it, and nothing diagnoses it, until every registered
row is done. Then all are read once, together, by `tools/score.py`, into a new file whose sha256 is recorded.
`score.py` gains only set J's groups and the `dtfb` row (`dt`'s reader, with "no candidate" read as declined),
changed before any set J run starts.

## Inputs and runs

- **Cases.** Case ids are `j_<site>`. R2 as in `PROTOCOL.md`: the start is the true clock rounded down to the minute,
  ±2 min. The area is a 25 km circle whose centre is 10 km from the truth, on the bearing given by the first four
  bytes of `sha256(case id)` modulo 360.
- **Wrong-date nulls.** Each case also runs as `j_<site>_n`, with the same audio and circle, the start 24 h earlier and
  the local date one day earlier. Any assertion there is wrong.
- **Runs.** Every registered tool runs once on every case and every null, with default settings. The cases run before
  the nulls. A rerun is allowed only for a harness or machine fault, and each one is listed with its cause. Set I's
  rule for the macOS allocator crash holds.

## Scoring

As set I. A located answer graded high or medium is asserted. Every `dt` and `dtfb` location is asserted. An asserted
answer within the limit is correct, and outside it is wrong. Every assertion on a null is wrong.

Two reads, both fixed here:

- **More accurate.** A build meets the rule against a rival when it has at least as many correct assertions, no more
  wrong ones (cases and nulls together), and is strictly better on one of the two. The session's falsifier fires for a
  build if, on set J, it has fewer correct assertions than any rival, or any wrong assertion.
- **Works near population centres.** A tool works on this set if it is correct on at least half of the cases and has no
  wrong assertion on cases or nulls. This is reported for every tool.

## What it can show

Correct and wrong counts for every tool on live cams in large US metros, away from busy airports, with close traffic
in a window chosen from ADS-B. With 12 cases or fewer every case is listed and no significance is claimed. The traffic
screen picks windows a blind user could only pick with ADS-B in hand. So a "works" here covers times when traffic is
close, not every hour.

Recordings, coordinates and aircraft identifiers stay out of this repository.

## 2026-11-09: screens, a harness fault and its rerun, and the result

### Screens

Every stream reached, with the step it stopped at, is in the benchmark machine's `data/set_j/screen/screen_final.tsv`
(no coordinates there).

| Step | Outcome | Rows |
|---|---|---|
| 1 | rows | 1,002 |
| 2 | out (title screen, including Go Martha's Vineyard's streams) | 422 |
| 3 | out (set I's outs, its cases and Boulder County Fairgrounds; four development cameras) | 38 |
| 4 | out, mean at or below -60 dBFS | 307 |
| 4 | not taken (fetch failed twice, no audio, or no wall clock) | 71 |
| 5 | out (description screen) | 111 |
| 6 | out (truth and the 5 km and same-site rules) | 11 |
| 7 | out (not in a metro of 1,000,000, or a tract under 1,000 people per square mile) | 37 |
| 8 | out, a large airport within 5 km (4.7 and 4.9 km) | 2 |
| 9 | out, no window qualifies | 1 |
| 10 | candidate | 2 |

The two candidates were Times Square (New York, window 20Z) and the Chicago power-plant cam (window 22Z). Times
Square's capture was not kept: its stream clock held for 260 s, under the 30 minutes the capture rule asks for. The set
is `j_chicagopower` and its null.

### Readings of the rules, made during the screens

- Set I's decisions carry over as step 3 and the steps that say so (levels, descriptions, truths).
- Go Martha's Vineyard's streams are out at step 2, as in Addendum 6 (use needs a written request).
- Streams whose terms allow reproduction only with a prominent attribution are out at step 5, as Addendum 6's Grand
  Sierra Resort (rows 21, 350, 382 and 481). So is the Port Lorain stream (row 461), whose terms say it "should not be
  used for any surveillance or any other lawful or unlawful purpose".
- Two truths were requeried once in Nominatim because the first match was not the place named: Main Street,
  Jonesborough, Tennessee (as "West Main Street, Jonesborough, Tennessee") and Two Harbors, Catalina Island (as
  "Two Harbors, California"). Both keep a 5 km limit.

### A harness fault, and its rerun

The case's window starts at 21:59Z on 2026-11-07 and runs two hours, so with its ±2 min it ends just after midnight
UTC, and its local date in Chicago runs to 06:00Z. `fdop` and `dopplerprint` therefore need the ADS-B archive for UTC
2026-11-08. The rows were launched at 21:21Z on 2026-11-08, before that day was published (it appears at about 03:35Z
the next day). Every `fdop` and `dopplerprint` case row stopped with "not published yet" or "no archive". That is my
scheduling fault, not a property of any tool. `dopplertrack` reads one UTC day and ran; its rows stand.

`requeue_j.sh` waited for the release, renamed the 7 faulted case rows `*_fault_unpub` (kept), and ran the same job
scripts again: the 11-08 release was seen at 03:38:24Z on 2026-11-09, and the rerun ended at 03:58:30Z, with the DEM
renamed aside for `dp` and `dp48` and back for `dpdem`. No rerun hit the error again, and no row ended in a fault exit
code. **Deviation:** for those tools the case rows ran after the null rows, not before.

While monitoring, before the read, I saw two null rows' exit codes (pinned `fdop` 1, `dp` 3). Nothing else was looked
at.

### Result

Read once at 03:59:56Z on 2026-11-09: `tools/score.py data/set_j/jobs/cases.json runs_j r2`, into
`runs_j/SCORE_r2_setJ_20261109.txt`, sha256 `abeddd509a70e088`.

| Row | pinned `7e66501` | `9087ec4` | `b759c6f` | `1dec7d6` | `dp` | `dp48` | `dpdem` | `dt` | `dtfb` |
|---|---|---|---|---|---|---|---|---|---|
| `j_chicagopower` | declined | declined | declined | declined | 14 km, low | declined | declined | **wrong**, 40 km | **wrong**, 40 km |
| `j_chicagopower_n` | declined | declined | declined | declined | declined | declined | declined | **wrong**, 34 km | **wrong**, 34 km |

No tool was right. Every `fdop` build asserted nothing, on the case or the null. `dopplerprint` asserted nothing
(its one location was graded low). `dopplertrack` was wrong on both rows.

- **More accurate.** Every `fdop` build meets the rule against `dt` and `dtfb` (no wrong assertion against two) and
  ties `dp`, `dp48` and `dpdem` (0 and 0 each), so it does not meet it against them. The falsifier does not fire for
  any build on set J.
- **Works near population centres.** No tool works on set J: none is right on its one case.

# Head-to-head benchmark protocol

Is `fdop` more accurate than [`dopplerprint`](https://github.com/Hexay/dopplerprint) and
[`dopplertrack`](https://github.com/mjshiggins/dopplertrack)? This file fixes the cases, inputs and scoring before
any competitor is run on them, so the commit date of this file is the point of the exercise. Changes after that
date are additions in a new section, dated and explained; nothing here is edited to suit a result.

## What was already known when this was written

- `fdop`'s date-and-state results on the reference and field recordings (README "Honest status"). Those are not
  blind. Every run below that gives `fdop` a start time is new.
- The competitors' published results. On the reference recording they are better than `fdop`'s: `dopplertrack` 43 m
  with hand-picked flyovers, `dopplerprint` 86 m nationwide with the start known to ±2 min, against `fdop`'s 271 m
  and 306 m with only the date. They had more input, but the numbers are smaller.
- All three tools were developed on the reference recording, so it says little about new recordings. `dopplertrack`
  was developed on the eagle cam and `dopplerprint` on Livermore. `fdop` was checked against the field recordings.
  Only set E is new to all three.

## Versions

| Tool | Commit | Run as |
|---|---|---|
| `fdop` | `7e66501` | `fdop locate`, `fdop detect` |
| `dopplerprint` | `81fba2c52d00fa647592727a2d5f3f6120f37089` | wheel built from that commit, `dopplerprint locate` |
| `dopplertrack` | `0fd161fb75b069e44d6ab77e02c42dedf93b4f7d` | `uv sync --frozen`, `dopplertrack` subcommands |

Default settings throughout. No setting is tuned to a case or to a result. If a later commit of any tool is tested,
it is reported as a separate run beside these, not in place of them.

## Check first: can we reproduce each tool's own claim?

Before scoring, on the reference recording: `dopplerprint` regional ±5 s (claims 79 m) and nationwide ±2 min (claims
86 m), and `dopplertrack` with its published hand-fitted events (claims 43 m). A failure here means the harness is
suspect. It is investigated before anything else runs, and whatever is found is reported.

## Cases

| Set | Recordings | Truth | Correct within |
|---|---|---|---|
| A | Reference: colsto's raw upload, Oyster Bay, NY, 2026-09-08 | Coordinate published in the video, 40.876466, −73.524951 | 1 km |
| B1 | Osprey cam, Oyster Bay, NY, 3 recordings (2026-10-31, 11-01, 11-02) | Same coordinate | 1 km |
| B2 | Eagle cam, North Fort Myers, FL, 2024-12-28 | Published nest register coordinate | 1 km |
| B3 | Duke Farms, NJ; Sapsucker Woods, NY; Rochelle, IL; Fullerton, CA; Plant City, FL; Tucson, AZ | Town-level or site-level location only | 5 km |
| C | Wrong dates: A on 09-07 and 09-09; Oyster Bay 11-01 on 10-31; Sapsucker Woods on 11-01; Duke Farms on 11-02 | Any location is wrong | never |
| D | Livermore, CA, 40 single flyovers (Pathak 2026, Zenodo 23424278, CC BY 4.0) | ADS-B values in the dataset's own results table | see below |
| E | New recordings, chosen by the rule below | Published coordinate | 1 km |

Site coordinates other than the reference stay out of this repository, under the project's data rules. Results name
towns and distances.

**Set E rule.** Public US live cams with audio and an exact published coordinate, which none of the three projects
has used. The list is committed before any audio is downloaded and before any tool runs. Every recording on the list
is scored, including those that turn out to have no flyovers.

## Inputs

The same information goes to every tool that can take it.

**R2, the primary regime: start known, area known.** The start is the true clock rounded down to the minute, ±2 min.
The area is a circle of radius 25 km whose centre is 10 km from the truth, on the bearing given by the first four
bytes of `sha256(case id)` modulo 360, so nobody chooses it. Wrong-date cases use the same circle and a start shifted
by whole days. True clocks come from the stream fragment timestamps of each capture. The eagle cam is an archive with
no such timestamps, so it uses YouTube's published stream start, 14:21:16 UTC, which is within 3 s of the clock implied
by `dopplertrack`'s published matched flights.

```bash
fdop locate F --dates LOCAL_DATE --near LAT,LON,25 --start T --clock-tol 120 -o OUT
dopplerprint locate F --start T --start-tolerance 2m --near LAT,LON,25 --yes --json --out OUT
# dopplertrack takes no start time. It gets the UTC day and the circle's bounding box.
dopplertrack spectrogram F rec.npz
dopplertrack detect rec.npz --band 60 300 --top 10 --out detected.json
dopplertrack search rec.npz --events E1 E2 --traces T --bbox S N W E --spacing 200 --out results.csv
dopplertrack refine rec.npz results.csv --events E1 E2 --traces T --bbox S N W E --extra E3 E4 E5 --out fine.csv
```

`E1` to `E5` are `detect`'s top five, in its order. Its README says these fits do not yet stand in for hand fits on the
reference recording. Choosing windows by hand would put a person, and here someone who has seen the truth, in the
loop, so its hand mode runs only in the reproduction check above, with its author's own events.

**R3: start known, no area.** `dopplerprint` without `--near`; `fdop --near CONUS` with the same start and tolerance.
`dopplertrack` needs a box and does not run here.

**R1: date and state only.** `fdop --dates D --near STATE`. `dopplerprint` requires a start, so it runs with the local
day's midpoint and ±12 h if it accepts that; otherwise it is recorded as unable to take these inputs. `dopplertrack`
does not run here: its published searches are at most 150 km across.

## Scoring

Each run is one of: **located** (it prints a location as its answer), **declined** (`fdop` exit 1; `dopplerprint` exit
2 or 3), or **error**.

A located answer is **asserted** when the tool stands behind it: `fdop` high or medium; `dopplerprint` status found
with confidence high or medium; `dopplertrack` every answer, because it has no way to decline or to grade.

- **Correct assertion:** asserted and within the case's limit.
- **Confident wrong:** asserted and outside the limit, or asserted at all on set C.
- **Unasserted location:** `fdop` low, `dopplerprint` found at low. Reported with its distance, and it counts as
  neither.

Reported for every run: outcome, distance, grade, wall time (load noted; a timing is not evidence of correctness) and
whether the truth lies inside the region the tool claims (`fdop`'s radius; `dopplerprint`'s 90 % region).

**The claim this benchmark can support.** On R2, over sets A, B and C, and then again on set E alone: `fdop` is more
accurate than a competitor only if it has at least as many correct assertions, no more confident wrongs, and is
strictly better on one of the two. Median distance over the cases both tools located is reported beside that, and is
reported even when it favours the competitor. A smaller median error for a competitor on those cases is a loss in
precision and will be said to be one. With this few recordings, every case is listed and no significance is claimed.

**Set D.** Automatic per-flyover measurement: the share of the 40 passes each tool finds unaided, and for those, median
absolute error in speed, closest distance and time of closest approach against the dataset's ADS-B values, with the
paper's two methods alongside. `fdop` refuses to locate from one flyover by design, so on location over this set it
scores 0 of 40 located and 0 confident wrong. `dopplerprint`'s location results over this set are run and reported the
same way. That is a loss in coverage for `fdop`, and is reported as one.

## Rules for running

- Each tool, case and regime runs once. A rerun is allowed only for a harness fault or a machine failure, and each one
  is listed with its cause. A wrong answer is never a reason to rerun.
- If a result looks like a harness error that hurts a competitor, it is investigated with the same care as one that
  hurts `fdop`.
- `fdop` fixes made after this file do not change these results. A better `fdop` is a new run beside them, and it faces
  set E recordings it has not seen.

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

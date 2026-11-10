# Set N: field recordings that log their own place and start time, from a new search of large US metros

Fixed on 2026-11-10, before its search. It asks set K's question (`SET_K.md`) with set K's inputs, scoring and rules,
and set M's `dopplerprint` regime (`SET_M.md`), except where this file says otherwise. Later changes go in a dated
section at the end; nothing here is edited to suit a result.

## Why

On set M, `main` (`11313ba`) was wrong at medium on one case, from the curve match alone: it stood 0.2 nats over the
best of 48 decoy clocks, 5.9 km from the site. The curve match's decoy test checks the recording's clock, not its
place. Across 25 benchmark runs its lead over the best decoy did not separate right places from wrong ones
(README "Known issues"). An answer from the curve match alone has been wrong once on `main`'s runs, and has given
`9087ec4` two right answers on set K. So whether to keep that path is a trade only a sealed set can test.
`8d672af` is `main` with that path graded low. Set K's screen pool is used up: set M took the last six uploads that
passed it. Set N searches again, with queries set K did not use.

**Known before this file was written:**
- the results of sets A to M, and set K's supplementary run;
- for `11313ba` on sets J and K, development replays begun at 23:46Z on 2026-11-09, some rows finished;
- two synthetic probes of the curve match with its region moved 2 to 5 km off the observer. Neither can show a wrong
  place beating the decoys: at the decoy clocks one repeated the real sky every 1000 s, and the other turned the same
  flights about the observer, which keeps every range. Either way the decoys matched as well as the real window;
- set K's searches, screens and lists of the channels it reached (Cloud Forest Walks and The Ambient Archive gave
  all of sets K and M).

No set N query has been run, no upload screened for it, and no audio of any upload set K did not take has been
fetched.

## Tools under test

`fdop` builds:

| Build | Why | Binary sha256 |
|---|---|---|
| `11313ba` | `main` when this file was written | `04c5dc5b05db893c` |
| `8d672af` | `main` with an answer from the curve match alone graded low | `38d493272cd31859` |
| `9087ec4` | the best on sets J, K and M | `faf2a40e7bcc5f60` |

`11313ba`'s binary is set M's `fdop-tail1`. `8d672af` is a commit object, not on `main`. Its parent is `1f7b612`
(`main`, the same code as `11313ba`), and it changes `src/pipeline.rs` only: where the trace search's answer is low
or absent and the curve match stands elsewhere (`CurveCall::Adopts`), the curve match's place is reported at low, not
medium, and its verdict says why. It is built on the benchmark machine from `git archive` of that object, and passed
`fdop selftest` there.

Rivals, as on set M: `dt` and `dtfb`, and `dopplerprint` in its three regimes run once each as `dpi`, `dp48i` and
`dpdemi`. Each UTC day a `dopplerprint` row needs (its span, the start ±2 min to the end, plus the neighbouring day when
the span comes within 2 h of a UTC midnight) that adsb.lol published as a single `.tar` is ingested by hand first, as on
set M. Days published in parts are fetched by `dopplerprint` itself. No `dopplerprint` row starts until every hand
ingest has ended. A row that fails for a missing day is a harness fault: that day is ingested and the row rerun, and
both are listed.

Registration closes with this commit. No build or tool is added later.

## Selection, in order

1. **Search.** YouTube's ordinary search (`yt-dlp ytsearch40:`), positions 1 to 40, on the 78 queries of
   `data/set_n/tools/queries_n.txt` (sha256 `8211719b5bbf964b`), in this order:
   - `<city> field recording`, then `<city> park ambience`, for the principal city of each of the next 25 metros after
     set K's by 2020 population, as listed: Sacramento, Pittsburgh, Austin, Las Vegas, Cincinnati, Kansas City,
     Columbus Ohio, Indianapolis, Cleveland, San Jose, Nashville, Virginia Beach, Providence, Jacksonville, Milwaukee,
     Raleigh, Oklahoma City, Memphis, Richmond Virginia, Louisville, Salt Lake City, New Orleans, Hartford, Buffalo,
     Birmingham Alabama;
   - `<city> soundscape` for set K's 25 cities, in set K's order;
   - `park soundscape usa`, `city park birdsong recording`, `binaural park recording`.

   Each result list is saved when taken. Rows are ordered by position first, then query; an upload met twice keeps
   its first place. An upload among set K's rows is out: set K screened it under these rules.
2. **Length.** As set K.
3. **Description screen.** As set K, with set K's readings of its rules. The date is from 2023 on and at least two
   days before this screen runs.
4. **Channels.** After the search rows, the whole upload lists of Cloud Forest Walks and The Ambient Archive, then of
   each other channel with an upload that passes step 3, in the order the channels are met, are screened by steps 2
   and 3, once. Uploads among set K's rows are out, as in step 1.
5. **Truth.** As set K.
6. **Place.** Out within 5 km of a case of sets A to M or a development recording. Out within 1 km of an earlier
   set N pass, so each site gives one recording.
7. **Metro and density.** As set K (set J's step 7).
8. **Airport.** As set K (set J's step 8).
9. **Traffic.** As set K: at least 2 passes within 1.5 km from at least 2 aircraft in the recording's own span,
   counted by set I's `cpa_list` on stores `1dec7d6`'s `fdop fetch` builds in the screen's own cache.
10. **Cases.** Every upload that passes steps 2 to 9, in row order, at most 6 per metro and 24 in all. Set K's and
    set M's cases do not count toward the caps. If fewer pass, the set is smaller and no screen is relaxed.

Every upload reached is listed with the step it stopped at.

## Capture, inputs and runs

- **Capture.** As set K: audio with `yt-dlp -f 140` as `audio.m4a`, with its metadata, both sha256'd into
  `seal_n.txt`. A file whose whole-file mean is at or below -60 dBFS is void. Nobody listens to a test recording, and
  nothing diagnoses one, until every row is done.
- **Inputs.** R2 as set K. Case ids are `n_<site>`, and each has a wrong-date null `n_<site>_n`: the same audio and
  circle, the start 24 h earlier and the local date one day earlier.
- **Runs.** Every registered tool once on every case and null, cases before nulls.
  - `fdop`: `11313ba` first, because its rows build the ADS-B stores, then `8d672af` and `9087ec4` side by side.
  - `dopplerprint`: the hand ingests, then `dpi` and `dp48i` with the national DEM aside, then `dpdemi` with it back.
  - `dopplertrack`: `dt`, then `dtfb`.
  - A rerun only for a harness or machine fault, each listed with its cause.

## Scoring and reads

`tools/score.py` gains set N's groups ("N", "NN" and "N+NN") before any set N row runs. All rows are read once,
together, into a new file whose sha256 is recorded. The reads are set M's, for set N alone and for sets J, K, M and N
summed (`11313ba` ran on sets M and N only, and `8d672af` on set N only; each is summed over the sets it ran on):
- **More accurate**, **works near population centres** and the **falsifier** for each `fdop` build, as set M defines
  them.
- **`8d672af` beside `11313ba`**, row by row. Their answers differ only in grade, and only where the curve match
  stands alone.
- **The variant's landing.** `8d672af`'s change is landed on `main` if, on set N, it asserts nothing wrong and has
  no fewer correct assertions than `11313ba`. If no set N row tells them apart, it is landed only if no run of
  `11313ba`'s code on record (its set M rows, the sets J and K development replays, and the 15 development and 33
  no-answer runs) has a right answer that came from the curve match alone. Otherwise it is not landed, and the trade
  is reported.

## What it can show

Correct and wrong counts on however many cases pass. Every case is listed with its metro and channel, and no
significance is claimed. A start time is the recordist's word, and a wrong one makes a case unsolvable for every tool
alike. Few runs reach the curve match alone, so set N may not separate `8d672af` from `11313ba` at all.

Recordings, coordinates and aircraft identifiers stay out of this repository.

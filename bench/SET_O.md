# Set O: one public open microphone at Jasper Ridge, recorded hour by hour over two days

Fixed on 2026-11-10, before any of its audio exists. It asks set M's question (is an `fdop` build more accurate than
the rivals?) with set K's inputs and scoring and set N's tools, at one site, except where this file says otherwise.
Later changes go in a dated section at the end; nothing here is edited to suit a result.

## Why

Sets L and N found no public recording that passed every screen, so no sealed set is left from uploads. A live
open microphone whose operator publishes its place gives recordings whose start the benchmark takes itself, as many
as the stream runs. The Locus Sonus sound map lists 47 live streams, 6 in the US. None is in a dense tract of a metro
of 1,000,000 or more. This set takes the one at Jasper Ridge Biological Preserve (Stanford University, near
Woodside, CA), "jasper ridge — birdcast". It sits between Palo Alto and San Carlos airports, near the Woodside VOR.
`fdop` hears propeller aircraft only, so the airline flows into SFO and SJC do not help it; whether enough
propeller traffic passes is for step 3 to say.

So this set says nothing about population centres. It tests, at one site, whether `fdop` is more accurate than
`dopplerprint` and `dopplertrack` on recordings none of them has seen. It is also the first sealed test of `8d672af`
(an answer from the curve match alone is graded low; on `main` since `247e260`) beside `11313ba`, and of `main` as
it stood when this file was written.

**Known before this file was written:**
- the results of sets A to M, set N's screens (no case), and the development replays of `11313ba` on sets J and K;
- the Locus Sonus list of active streams, fetched at about 01:10Z on 2026-11-10, with each stream's map point;
- 12 s of this stream, fetched once to read its format: Icecast, mp3, 44.1 kHz, mono, about 75 kbit/s, and about
  5 s of buffered audio sent on connecting. Nobody listened to it and nothing analysed it.

No ADS-B has been looked up near the site, no traffic counted, and none of this set's audio exists.

## Tools under test

`fdop` builds: set N's three, `11313ba` (`fdop-tail1`, `04c5dc5b05db893c`), `8d672af` (`fdop-8d672af`,
`38d493272cd31859`; merged into `main` by `247e260`) and `9087ec4` (`faf2a40e7bcc5f60`), and `e7e2a8a`, `main` when
this file was written: six commits after `247e260` that change measurement, search, weather and HTTP. `e7e2a8a` is
built from `git archive` on the benchmark machine before any row runs. If it does not build, or `fdop selftest` fails,
that is reported and it has no rows.
Rivals, as set N: `dt`, `dtfb`, and `dopplerprint` as `dpi`, `dp48i` and `dpdemi`. Each UTC day a `dopplerprint` row
needs that adsb.lol published as a single `.tar` is ingested by hand first; days published in parts are fetched by
`dopplerprint` itself. No build or tool is added later.

## Capture

From this file's commit until 2026-11-11T22:00:00Z (14:00 at the site, so that no row reads UTC 2026-11-12), the
benchmark machine reads
`https://locus.creacast.com:9443/jasper_ridge_birdcast.mp3` in consecutive connections of at most 3,600 s each,
and of at most the time left before the end. Each connection's bytes are saved as received (mp3, not re-encoded),
one file per connection. A connection that fails or ends early is followed by the next after 60 s. The client
names itself `fdop-bench/0.1`. A file's start is the UTC time on the benchmark machine's clock (its NTP offset is
logged when the capture starts) read just before its connection opens. The stream's own delay and the buffered
seconds sent on connecting are not removed: R2's ±2 min is meant to cover them. Every file is sha256'd into
`seal_o.txt` when the capture ends. Nobody listens to a file, and nothing diagnoses one, until every row is done.

## Selection, in order

1. **Length.** A file of 20 minutes or more, by `ffprobe` (set K's step 2).
2. **Truth.** The stream's map point in the Locus Sonus list fetched before this file, kept on the benchmark
   machine (`data/set_o/site.json`, sha256 `7a426e46869a1642…`), with a 1 km limit, as for a named site with
   coordinates. Ground height from USGS EPQS. The map point is the operator's word, as a start time is a recordist's.
3. **Traffic.** As set K: at least 2 passes within 1.5 km from at least 2 aircraft in the file's own span, from its
   start to its start plus its length, counted by set I's `cpa_list` on stores `1dec7d6`'s `fdop fetch` builds in the
   screen's own cache.
4. **Cases.** Every file that passes steps 1 to 3, in time order, at most 6 per local date at the site
   (America/Los_Angeles) and 16 in all. If fewer pass, the set is smaller and no screen is relaxed.

Set K's place, metro and airport screens do not apply: there is one site. Its metro and its tract's density are
reported.

## Inputs and runs

- **Inputs.** R2 as set K: the start floored to the minute, ±2 min; a 25 km circle whose centre is 10 km from the
  truth on bearing sha256(case id) mod 360, so each case has its own circle. Case ids are `o_<UTC start as
  YYYYMMDD_HHMM>`. Each case has a wrong-date null `o_..._n`: the same audio and circle, the start 24 h earlier and
  the local date one day earlier. A file whose whole-file mean is at or below -60 dBFS is void, as set K.
- **Runs.** Every registered tool once on every case and null, cases before nulls, in set N's order: `fdop`
  `11313ba` first (its rows build the stores), then `8d672af`, `9087ec4` and `e7e2a8a` side by side;
  `dopplerprint`'s hand ingests, then `dpi` and `dp48i` with the national DEM aside, then `dpdemi` with it back; `dt`,
  then `dtfb`. Rows may run in batches: a case and its null run once adsb.lol has published every UTC day they read
  and every earlier file has been screened, so the selection is the same whenever rows run (the last day is
  2026-11-11, expected about 03:35Z on 11-12). A rerun only for a harness or machine fault, each listed with its cause.

## Scoring and reads

`tools/score.py` gains set O's groups ("O", "ON" and "O+ON") before any set O row runs. All rows are read once,
together, into a new file whose sha256 is recorded. The reads:
- **More accurate** and the **falsifier** for each `fdop` build, as set M defines them, on set O alone. "Works near
  population centres" is not read: the site is not one.
- **`8d672af` beside `11313ba`**, row by row. They can differ only in grade, and only where the curve match stands
  alone. Each row where they differ is reported with whether its place was right.
- **`e7e2a8a` beside `8d672af`**, row by row: each row where the answer or grade differs, and whether each place
  was right.
- Sums over sets J, K, M and O for each tool, as set M's sums, with set O marked as one site.

## What it can show

Correct and wrong counts for every tool on up to 16 hours of one microphone. Its recordings share one place, so they
are not independent tests of place: they test each tool against other places and other clocks at this site. No
significance is claimed. A delayed stream or a misplaced map point would make a case unsolvable for every tool
alike.

Recordings, coordinates and aircraft identifiers stay out of this repository.

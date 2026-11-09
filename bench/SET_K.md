# Set K: uploaded field recordings that log their own place and start time, in large US metros

Fixed on 2026-11-08, before its search, at the owner's request. It reads the same question as set J (`SET_J.md`) with
a different source, and uses `PROTOCOL.md`'s R2 inputs and scoring and set J's rules except where this file says
otherwise. Later changes go in a dated section at the end; nothing here is edited to suit a result.

## Why

The owner's question is whether `fdop` works near US population centres. Live cams with real sound at public sites in
large metros are scarce: set J's search reached 1,002 streams and kept two, and its pool count found about six urban
sites with no large or medium airport within 5 km. The owner suggested videos whose description happens to give what
a test needs. Some field recordists post long stationary recordings with the site, the date and the start time written
in the description. Set K takes those.

**Known before this file was written:**
- the results of sets A to I, and set J's screens; set J is unread, apart from two null exit codes seen while
  monitoring (pinned `fdop` 1, `dp` 3);
- an after-the-fact table of sets A to I's R2 results by set J's metro and airport screens (exploratory, every result
  in it already read);
- a sizing probe: 12 queries (`field recording soundscape usa` and similar), 352 uploads, counted only. Three
  descriptions were read in full. One was The Ambient Archive's, whose 20 uploads were then listed with flags only:
  all 20 give coordinates, a date and a clock time; 9 run 20 minutes or more.

No audio of any set K video has been fetched or heard, no ADS-B has been looked up for one, and no truth computed.

## Tools under test

`fdop` builds, each built on the benchmark machine from `git archive` of its commit: set J's four (`7e66501`,
`9087ec4`, `b759c6f`, `1dec7d6`) and `3c51eae`, `main` when this file was written (source tar sha256
`1c8eccb144f2937a`). A build that cannot be built, or that fails `fdop selftest`, is reported as not run.

Rivals exactly as on set J: `dp`, `dp48`, `dpdem`, `dt` and `dtfb`, with set J's `dtfb` fallback.

Registration closes with this commit. No build or tool is added later.

## Selection, in order

1. **Search.** YouTube's ordinary search (`yt-dlp ytsearch40:`), positions 1 to 40, on 53 queries in this order:
   `<city> field recording`, then `<city> park ambience`, for each principal city of the 25 largest metros in 2020
   (New York, Los Angeles, Chicago, Dallas, Houston, Washington DC, Philadelphia, Miami, Atlanta, Boston, Phoenix,
   San Francisco, Riverside, Detroit, Seattle, Minneapolis, San Diego, Tampa, Denver, Baltimore, St. Louis, Orlando,
   Charlotte, San Antonio, Portland Oregon); then `ambient park sounds`, `urban field recording`,
   `city soundscape recording`. Each result list is saved when taken. Rows are ordered by position first, then query;
   an upload met twice keeps its first place.
2. **Length.** Out if under 20 minutes.
3. **Description screen.** From the upload's metadata. It passes only if the description gives all of:
   - the date of the recording and its local start time to the minute. A chapter mark, the upload time or "at sunset"
     is not a start time. A start with no zone takes the site's zone on that date;
   - the place: coordinates, or a named public outdoor site (park, plaza, street, beach). Out: a home or yard,
     indoors;
   - a site in the contiguous US;
   - one stationary take. Out: a walk or moving camera, added music or narration, a compilation, mix, loop, montage or
     "extended" version;
   - no ban on reuse, as set I's step 5;
   - a date from 2023 on, and at least two days before this screen runs.
4. **Channels.** Each channel with an upload that passes step 3 has its whole upload list screened by steps 2 and 3,
   once, after the search rows, in the order the channels are met. The Ambient Archive goes first, because the probe
   found it.
5. **Truth.** The description's coordinates, with a 1 km limit. Without coordinates, the named site from Nominatim
   as in set I (1 km for a site, 5 km for a neighbourhood or town). Ground height from USGS EPQS.
6. **Place.** Out within 5 km of a case of sets A to J or a development recording. Out within 1 km of an earlier
   set K pass, so each site gives one recording.
7. **Metro and density.** Set J's step 7 (a metro of at least 1,000,000; a tract of at least 1,000 people per square
   mile).
8. **Airport.** Set J's step 8 (no large or medium airport within 5 km).
9. **Traffic.** The recording's own span, from the stated start to the start plus its length, counted by set I's
   `cpa_list` on a store of its UTC day or days that `1dec7d6`'s `fdop fetch` builds around the truth (40 km) in the
   screen's own cache. It qualifies with at least 2 passes within 1.5 km from at least 2 aircraft, set J's closer
   test. Out otherwise.
10. **Cases.** Every upload that passes steps 2 to 9, in row order, at most 6 per metro and 24 in all. If fewer pass,
    the set is smaller and no screen is relaxed.

Every upload reached is listed with the step it stopped at.

## Capture and seal

Each case's audio is fetched once on the benchmark machine with `yt-dlp -f 140` (AAC, 44.1 kHz) as `audio.m4a`, with
its metadata, and both are sha256'd in `seal_k.txt`. A file whose whole-file mean is at or below -60 dBFS is void.
Nobody listens to a test recording, and nothing diagnoses one, until every registered row is done. Then all are read
once, together, by `tools/score.py`, into a new file whose sha256 is recorded. `score.py` gains only set K's groups,
changed before any set K run.

## Inputs and runs

- **Cases.** Ids are `k_<site>`. The true clock is the stated start in UTC. R2 as in `PROTOCOL.md`: that clock
  rounded down to the minute, ±2 min; a 25 km circle centred 10 km from the truth, on the bearing given by the first
  four bytes of `sha256(case id)` modulo 360.
- **Wrong-date nulls.** `k_<site>_n`: the same audio and circle, the start 24 h earlier and the local date one day
  earlier. Any assertion there is wrong.
- **Runs.** As set J: every registered tool once on every case and null, default settings, cases before nulls. A
  rerun only for a harness or machine fault, each listed with its cause.

## Scoring

As set J, with both of its reads ("more accurate" and "works near population centres"), for set K alone and for J and
K together (summed from the two reads). Every case is listed with its metro and channel.

## What it can show

Correct and wrong counts for every tool on stationary field recordings in large US metros, away from busy airports,
with close traffic in the recording's span. The start time is the recordist's word. Nothing here checks it, because
that would need the audio and the ADS-B together. A stated time that is wrong makes a case unsolvable for every tool
alike. One recordist can supply several cases; the cap of 6 per metro bounds that, and the channel is listed.

Recordings, coordinates and aircraft identifiers stay out of this repository.

## 2026-11-09: screens, capture, readings and result

### Screens

The searches were taken on 2026-11-08 from 22:15Z. Every row, with the step it stopped at, is in the benchmark
machine's `data/set_k/screen/screen_final.tsv` (no coordinates there).

| Step | Outcome | Rows |
|---|---|---|
| 1 | rows: 2,028 from the 53 searches, then 110 from channel lists | 2,138 |
| 2 | out, under 20 minutes | 1,057 |
| 3 | out, no recording date from 2023 on (mechanical) | 912 |
| 3 | out, no start time (mechanical) | 46 |
| 3 | out, read: a walk with a handheld gimbal 51, another walk or moving take 6, no start to the minute 2, a ban on reuse 3, a loop 1, indoors 1, a home 1, not one spot 1 | 66 |
| 3 | not taken | 1 |
| 5 | truths fixed, all with a ground height | 56 |
| 6 | out, within 1 km of an earlier set K pass (none within 5 km of an earlier set's case) | 25 |
| 7 | out, a park tract under 1,000 people per square mile | 11 |
| 8 | out | 0 |
| 9 | out, no pass within 1.5 km | 6 |
| 10 | not taken, New York already had 6 cases | 6 |
| 10 | case | 8 |

The cases, in row order, with their recordist: `k_riversidesouth`, `k_tribeca`, `k_weehawken`, `k_sakurapark`,
`k_pier35` and `k_bowlinggreen` in New York (Cloud Forest Walks), `k_lastatepark` and `k_boyleheights` in Los Angeles
(The Ambient Archive). Recordings run 30 to 71 minutes.

### Capture

All 8 were captured once and none is void (whole-file means -29.7 to -12.8 dBFS). `seal_k.txt` (16 lines) has sha256
`6ddf04b54cbc38bb`. The first attempt used the benchmark machine's Homebrew `yt-dlp` (2026.03.17), which YouTube
refused with HTTP 403 on the first case, twice; its logs are kept. The capture then used set J's `yt-dlp` (2026.10.26,
with deno for YouTube's player check), and that case and the other seven were fetched once each.

### Readings of the rules, made during the screens

These were decided while the screens ran, before any audio was fetched or any ADS-B looked up for an upload that
passed step 3:

- "All rights reserved" alone is not a ban on reuse (step 3); an explicit "may not be used ... without written
  permission" is.
- A fixed tripod that pans is one stationary take; a handheld gimbal walk is not.
- Venue sound at a public event (a July 4 fireworks show heard from a public viewing area) is not added music.
- Step 6's 1 km same-site rule compares with earlier set K uploads that passed step 6, as set J's did.
- Park tracts can have fewer than 1,000 residents per square mile; they fail step 7 like any other tract. Nothing was
  relaxed.
- Step 10's caps were applied before the traffic lookup: an upload past its metro's sixth case is not taken
  whatever its traffic, which gives the same cases as capping after.
- The traffic tool first read only `prod+mlatonly_*` stores; 2023 days have no MLAT-only release and fdop builds a
  `prod_*` store there. The first site (a 2023 day) was built but not counted. The tool was stopped by its PID during
  the second site's fetch, fixed to read any store, and restarted once that fetch had run to its end. Both sites were
  counted from the stores the first attempt built; nothing was fetched twice.
- `dp`'s chain waits for set J's harness-fault rerun to finish, because both rename the same DEM cache aside.
- A void capture would still have taken its place under the caps; none was void.

### Runs

Regime `kr2` started at 23:34:48Z on 2026-11-08 and ended at 13:10:49Z on 2026-11-09 (`runs_k/launch.log`). Every
row has an exit code and none ended in a fault code (101, 127, 133, 137 or 143). The national DEM was renamed aside at
03:59:59Z for `dp` and `dp48` and back at 12:17:01Z for `dpdem`, as `launch_k.sh` says. From about 03:20Z the
benchmark machine's USB volume ran at about 11,700 operations of 4 KB a second, from a directory listing by another
session on that machine. That slowed rows. No set K job runs under a time limit.

### Result

Read once at 13:46:15Z on 2026-11-09: `tools/score.py data/set_k/jobs/cases.json runs_k r2`, into
`runs_k/SCORE_r2_setK_20261109.txt`, sha256 `522b3834fcc45557`.

| Row | pinned `7e66501` | `9087ec4` | `b759c6f` | `1dec7d6` | `3c51eae` | `dp` | `dp48` | `dpdem` | `dt` | `dtfb` |
|---|---|---|---|---|---|---|---|---|---|---|
| `k_riversidesouth` | 2719 m, low | **right**, 137 m, medium | 77 m, low | 77 m, low | 137 m, low | **right**, 44 m, medium | **right**, 55 m, medium | **right**, 44 m, medium | **wrong**, 8654 m | **wrong**, 8654 m |
| `k_lastatepark` | declined | declined | 29 km, low | 29 km, low | 29 km, low | declined (20 km, high) | declined (18 km, medium) | declined (18 km, medium) | **wrong**, 22 km | **wrong**, 22 km |
| `k_boyleheights` | 32 km, low | 31 km, low | 143 m, low | 143 m, low | 330 m, low | declined (19 km, high) | declined (21 km, high) | declined (21 km, high) | **wrong**, 20 km | **wrong**, 20 km |
| `k_tribeca` | declined | declined | 256 m, low | 256 m, low | 14 km, low | error | error | error | **wrong**, 28 km | **wrong**, 28 km |
| `k_weehawken` | 6692 m, low | **right**, 20 m, medium | 41 m, low | 41 m, low | **right**, 33 m, high | error | error | error | **wrong**, 32 km | **wrong**, 32 km |
| `k_sakurapark` | 9721 m, low | 9626 m, low | 12 km, low | 12 km, low | 12 km, low | error | error | error | **wrong**, 11 km | **wrong**, 11 km |
| `k_pier35` | declined | declined | declined | declined | declined | error | error | error | **wrong**, 5538 m | **wrong**, 5538 m |
| `k_bowlinggreen` | 30 km, low | 30 km, low | 30 km, low | 30 km, low | 5738 m, low | error | error | error | **wrong**, 1113 m | **wrong**, 1113 m |
| `k_riversidesouth_n` | 3231 m, low | 4806 m, low | 4995 m, low | 4995 m, low | 10 km, low | **wrong**, 11 km, high | **wrong**, 11 km, high | **wrong**, 11 km, high | **wrong**, 6008 m | **wrong**, 6008 m |
| `k_lastatepark_n` | declined | declined | 6793 m, low | 6793 m, low | 6780 m, low | declined (8241 m, low) | declined (28 km, medium) | declined (28 km, low) | **wrong**, 26 km | **wrong**, 26 km |
| `k_boyleheights_n` | 19 km, low | 26 km, low | 27 km, low | 27 km, low | 27 km, low | error, then on rerun declined (18 km, medium) | **wrong**, 4962 m, high | **wrong**, 4924 m, high | **wrong**, 29 km | **wrong**, 29 km |
| `k_tribeca_n` | declined | declined | 4621 m, low | 4621 m, low | 4629 m, low | error | error | error | **wrong**, 13 km | **wrong**, 13 km |
| `k_weehawken_n` | 3946 m, low | 3554 m, low | 3207 m, low | 3207 m, low | 3211 m, low | error | error | error | **wrong**, 31 km | **wrong**, 31 km |
| `k_sakurapark_n` | 11 km, low | 9313 m, low | 13 km, low | 13 km, low | 9315 m, low | error | error | error | **wrong**, 23 km | **wrong**, 23 km |
| `k_pier35_n` | declined | declined | declined | declined | declined | error | error | error | **wrong**, 1777 m | **wrong**, 1777 m |
| `k_bowlinggreen_n` | 3202 m, low | 3429 m, low | 3426 m, low | 3426 m, low | 3385 m, low | error | error | error | **wrong**, 7776 m | **wrong**, 7776 m |

A distance is from the truth, and every case's limit is 1 km. "declined (20 km, high)"
means `dopplerprint` exited 2 or 3, which `PROTOCOL.md` counts as a decline, and its output still held a best location at
that distance and grade. A decline is never asserted.

| Tool | Right on cases | Wrong on cases | Wrong on nulls | Errors |
|---|---|---|---|---|
| pinned `7e66501` | 0 | 0 | 0 | 0 |
| `9087ec4` | 2 | 0 | 0 | 0 |
| `b759c6f` | 0 | 0 | 0 | 0 |
| `1dec7d6` | 0 | 0 | 0 | 0 |
| `3c51eae` | 1 | 0 | 0 | 0 |
| `dp` | 1 | 0 | 1 | 10 |
| `dp48` | 1 | 0 | 2 | 10 |
| `dpdem` | 1 | 0 | 2 | 10 |
| `dt` | 0 | 8 | 8 | 0 |
| `dtfb` | 0 | 8 | 8 | 0 |

`dp`'s counts take its rerun row (below).

### `dopplerprint`'s errors

Ten rows of each `dopplerprint` regime are errors: the five cases `k_tribeca`, `k_weehawken`, `k_sakurapark`,
`k_pier35` and `k_bowlinggreen`, and their nulls. Each ended with `dopplerprint`'s message that the day's release
"doesn't list usable .tar parts". Its fetcher reads only releases split into `.tar.aa`, `.tar.ab`, ... parts, and those
days' adsb.lol releases are one `.tar` each. Its `docs/DATA.md` describes `dopplerprint ingest <archive>` for such a file.
The protocol runs every tool with its default settings, so these rows stay errors. They are a limit of
`dopplerprint`'s download, not of its method, and nothing is known about how it would do on those five cases.

`dp`'s `k_boyleheights_n` first ended with "http status: 504" from GitHub while downloading the day's release. That is a
service fault, not `dopplerprint`'s answer. After the read, `data/set_k/tools/k_rerun_dp504.sh` renamed the row's files
`*_fault_504` (kept), renamed the DEM aside as `launch_k.sh` does, and ran the same job script, which runs only rows with
no exit code. The rerun ran from 13:52:52Z to 14:00:35Z on 2026-11-09 (`runs_k/RERUNS_kr2.log`) and declined, with a
best location 18 km off at medium. Only that row was scored again, with `score.py`'s own functions. No other row was
rerun. The texts "504" in `dp`'s `k_weehawken` and `dp48`'s `k_riversidesouth` logs are not HTTP statuses.

### Reads

- **More accurate** (cases and nulls together).
  - `9087ec4` (2 right, none wrong) and `3c51eae` (1 right, none wrong) meet the rule against every rival: `dp`
    (1 right, 1 wrong), `dp48` and `dpdem` (1 right, 2 wrong each), and `dt` and `dtfb` (none right, 16 wrong each).
  - The pinned build, `b759c6f` and `1dec7d6` asserted nothing. They meet the rule against `dt` and `dtfb` only.
- **Falsifier.**
  - For the pinned build, `b759c6f` and `1dec7d6`, it fires on set K: each has fewer correct assertions than `dp`,
    `dp48` and `dpdem`.
  - For `9087ec4` and `3c51eae`, it does not fire.
  - No `fdop` build made a wrong assertion, on a case or a null.
- **Works near population centres.** No tool works on set K. That needs at least 4 of the 8 cases right with no wrong
  assertion. The best was `9087ec4`, with 2.

Sets J and K together (sets J and K summed; `3c51eae` ran on set K only):

| Tool | Right | Wrong |
|---|---|---|
| `9087ec4` | 2 | 0 |
| `3c51eae` | 1 | 0 |
| pinned, `b759c6f`, `1dec7d6` | 0 | 0 |
| `dp` | 1 | 1 |
| `dp48`, `dpdem` | 1 | 2 each |
| `dt`, `dtfb` | 0 | 18 each |

The reads come out the same as on set K alone. No tool works: none is right on 5 of the 9 cases.

### What the low grades held back (read after the seal; for development only)

`b759c6f` and `1dec7d6` gave the same answer on every row. They placed four cases within 256 m of the truth and graded
each low:

| Case | Distance | Gate that failed |
|---|---|---|
| `k_weehawken` | 41 m | decoy clocks: −3.05 nats. The −2 h decoy scored 27.04 against 23.99; the gate needs +3. |
| `k_riversidesouth` | 77 m | margin: 1.25 nats over a rival with different flights 1.1 km away. |
| `k_boyleheights` | 143 m | margin: 4.25 nats over a rival with different flights 0.5 km away. The curve match also failed. |
| `k_tribeca` | 256 m | margin: 2.57 nats over a rival 14.2 km away. |

The nearest of their seven null locations was 3.2 km off, also graded low.

`3c51eae` graded `k_weehawken` high and right at 33 m. On `k_riversidesouth` and `k_tribeca` it failed the core-match
gate.

These rows are now read. A change suggested by them has to be shown on development recordings and the null battery,
and set K cannot test it.

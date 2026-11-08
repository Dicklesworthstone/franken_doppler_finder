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

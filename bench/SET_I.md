# Set I: wildlife and nature cams with close traffic, every tool, no hearing screen

Fixed on 2026-11-08, before any search for it, after the owner approved a new sealed set whose cases can be solved.
It uses `PROTOCOL.md`'s R2 inputs and scoring. The commit date of
this file is the point of the exercise. Later changes go in a dated section at the end; nothing here is edited to suit
a result.

## Why

Every sealed set so far was evidence-starved.

- **Sets E and F** were chosen without aircraft data. Seven of set F's ten recordings had at most two aircraft within
  3 km (`SET_G.md`).
- **Sets G and H** were chosen on traffic, around the busiest low-altitude cells. That put them in cities. Of about 900
  streams, most sent digital silence, and two cases were left. On both, the true passes were faint: they scored under
  every registered build's detection floor, and chance windows beat them (`SET_H.md`).
- **Every live cam among the development recordings is a wildlife cam**: the Oyster Bay osprey nest, the North Fort
  Myers eagle nest, Duke Farms and Sapsucker Woods. Their microphones are there for the wildlife's sound.

Set I takes its streams from wildlife and nature searches, whose sites are mostly away from city noise. It then keeps
those with close traffic in the test window, from ADS-B alone. This aims at cases a blind search can solve, without
listening to any of them.

**No hearing screen.** Three reasons:

- fdop's tonal oracle is fdop's own front end, and its chance read was biased (`SET_H.md`, 2026-11-07).
- Band power heard one of the 10 pilots whose hearing read ran in sets G and H (Deerfield Beach, read against its
  close passes' own chance lines).
- The centred band-power statistic of session bead `bb-m9r` needs a set that was not chosen on sound at the truth.

So every tool runs here, the rivals included, and no screen uses any tool's output or any test sound.

**Known before this file was written:** the results of sets A to H, and nothing about any set I stream. No search for
set I has run, and no set I stream has been named, fetched or heard.

## Tools under test

`fdop` builds:

| Build | Why | Binary sha256 (bench) |
|---|---|---|
| `7e66501` | the benchmark's pinned build | `af1ef5d79eae80461352ad301fb2cbd158e86c3e580faa308d6575b587995d89` |
| `9087ec4` | the best sealed result so far (set E R2, 1 correct, 0 wrong; Addendum 14) | `faf2a40e7bcc5f6088e30ef1a948a5b3cbff40a566d71d13cefcaf067b6e7293` |
| `a0795de` | `main` when this file was written | `90839bf4c906002ea9f043bffbcfe472de314160545e48a62cb7ec4795788943` |

Each binary is built on the bench machine from `git archive` of its commit and passes `fdop selftest`.

Rivals: `dopplerprint` as `dp`, `dp48` (its 48 kHz WAV) and `dpdem` (48 kHz plus its regional ground model, Addendum 4).
`dopplertrack` runs as `dt`. All run with `PROTOCOL.md`'s commits and R2 commands, as on sets E and F. `dp` and `dp48`
run with no national ground model in their cache, as their set E and F R2 rows did.

Registration closes with this commit. No build or tool is added later. A build that cannot be built, or that fails
`fdop selftest`, is reported as not run.

## Window

**Test window:** two hours of DVR audio from 2026-11-07T18:00:00Z, a Saturday. There is no pilot window. YouTube keeps
about 120 h of DVR, so the window can be fetched until about 2026-11-12T18:00Z. A stream whose DVR no longer reaches it
is not taken.

## Selection, in order

1. **Search.** YouTube's live filter, positions 1 to 40, on each of these queries in this order:
   1. `bird feeder live cam`
   2. `bird cam live`
   3. `osprey cam live`
   4. `eagle cam live`
   5. `owl cam live`
   6. `hawk cam live`
   7. `falcon cam live`
   8. `heron cam live`
   9. `nest cam live`
   10. `wildlife cam live`
   11. `nature cam live`
   12. `nature center live cam`
   13. `wildlife refuge live cam`
   14. `audubon live cam`
   15. `pond cam live`
   16. `marsh cam live`
   17. `wetland live cam`
   18. `farm cam live`
   19. `barn cam live`
   20. `forest cam live`
   21. `garden live cam`
   22. `zoo live cam`

   Each result list (position, video id, channel, title) is saved when it is taken. Rows are ordered by position first,
   then by query, so that no query takes the set: every query's first result comes before any query's second. A
   stream met twice keeps its first place.
2. **Title screen.** `SET_G.md`'s rules, read on the title and channel. Out:
   - outside the 48 contiguous states, which the traffic screen's store covers;
   - not one fixed camera of one site;
   - excluded operators;
   - news, music and compilations.
3. **Earlier streams.** Out: every stream that was a case in sets A to H, every development recording, and every
   stream whose audio any tool or diagnostic read in sets G and H (their pilots).
4. **Level.** As `SET_H.md` step 4: 60 s from 2026-11-07T18:00:00Z, fetched as below. The stream qualifies if ffmpeg's
   `volumedetect` `mean_volume` is above -60 dBFS. That number is all that is taken from it. The same fetch tests
   whether the DVR reaches the window.
5. **Description screen.** As `SET_G.md` step 3:
   - a named public site, or the site of a named organisation or business, one fixed camera;
   - no ban on recording in the stream's own description, and no private residence;
   - no radio, scanner, music or pre-recorded sound, and no stream whose operator says it has no sound.

   One stream per site: a later stream whose truth is within 1 km of an earlier pass is the same site.
6. **Truth.** As `SET_G.md` step 4: an OpenStreetMap feature, or the operator's published address placed by Nominatim
   or the Census geocoder. The limit is 1 km for a single building, nest site, pier or marina, and 5 km where the
   operator names no building. Ground height comes from USGS's elevation service. The truth is fixed before the
   traffic screen, and its coordinate stays out of this repository. A truth within 5 km of a case of sets A to H, or
   of a development recording, is out: those places are where the tools were made and tested.
7. **Traffic, test window, ADS-B alone.** As `SET_G.md` step 5. The archive is 2026-11-07's adsb.lol release, in the
   store `fdop fetch` builds (`prod+mlatonly`; the store drops jets). The closest approaches at the truth come from the
   scratch `cpa_list` (source sha256 `23053ac3badc621e`). A pass is one closest approach with a slant distance of at
   most 3 km. The case qualifies with at least 6 passes from at least 3 aircraft. **At least 2 of those passes must be
   within 1.5 km, from at least 2 aircraft.** Every tool needs two flyovers, and a pass at half the distance is about
   6 dB louder.
8. **Cases.** The first 10 streams in the order above that pass screens 2 to 7 and whose capture holds. If fewer pass
   by the end of the rows, or by the time the window leaves the DVR, the set is smaller, and no screen is relaxed.

A stream that cannot be fetched, whose DVR does not reach the window, or whose capture fails the outage rule, is not
taken, and the next in order takes its place. A fetch fault (HTTP 400 or 404, a yt-dlp or ffmpeg error) is retried
once. A stream that cannot be timed, or has no from-start format, is not retried. Every stream reached is listed with
the step it stopped at.

## Capture and seal

As `SET_G.md`:

- **Capture.** The test window is fetched by the doppler skill's `scripts/ytlive_fetch.py` (sha256 `ce9d1c3dc9d5`). It
  runs on the benchmark machine, which is on the owner's home network.
- **Outages.** Cut at the first outage, kept if the clock holds for at least 30 min.
- **Clock.** The true clock is the First-Frame stamp of the first fragment.
- **Silence.** A capture whose whole-file mean and peak are digital silence is void.
- **Seal.** Nobody listens to a test recording. No tool runs on it, and nothing diagnoses it, until every registered
  row below is done. Then all are read once, together, by `score.py`, into a new file whose sha256 is recorded.

## Inputs and runs

- **Cases.** Case ids are `i_<site>`. R2 as in `PROTOCOL.md`: the start is the true clock rounded down to the minute,
  ±2 min. The area is a 25 km circle whose centre is 10 km from the truth, on the bearing given by the first four
  bytes of `sha256(case id)` modulo 360.
- **Wrong-date nulls.** Each case also runs as `i_<site>_n`, with the same audio and circle, the start 24 h earlier and
  the local date one day earlier. No aircraft of that day made the recording, so any assertion there is wrong, as for
  set C.
- **Runs.** Every registered tool runs on every case and every null, with default settings, once. The cases run
  before the nulls. A rerun is allowed only for a harness or machine fault, and each one is listed with its cause.
  Addendum 23's rule for the macOS allocator crash holds: a row that crashes there twice is that tool's error.

## Scoring

As `PROTOCOL.md`. A located answer graded high or medium is asserted; every `dopplertrack` location is asserted, as
before. An asserted answer within the limit is correct, and outside it is wrong. Every assertion on a null is wrong.

A build meets the rule against a rival when it has at least as many correct assertions, no more wrong ones (cases and
nulls together), and is strictly better on one of the two. The session's falsifier for the claim that `fdop` is more
accurate fires for a build if, on set I, it has fewer correct assertions than any rival, or any wrong assertion.

## What it can show

Correct and wrong counts for every tool on live wildlife and nature cams with close traffic, which are not all live
cams. With 10 cases or fewer, every case is listed and no significance is claimed. If no tool is right on any case,
set I cannot separate the tools either, and that is reported as it is.

Session bead `bb-m9r`'s statistic may be run on set I's captures only after every registered row has been read. It
changes no row here.

Recordings, coordinates and aircraft identifiers stay out of this repository.

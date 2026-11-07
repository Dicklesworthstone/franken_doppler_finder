# Set G: a sealed set chosen so that it can be solved

Fixed on 2026-11-06, before any search for it, at the owner's request. This file is separate from `PROTOCOL.md`,
which the benchmark machine's agent keeps. It uses that file's inputs and scoring. The commit date of this file is the
point of the exercise. Later changes go in a dated section at the end; nothing here is edited to suit a result.

## Why a new set

Sets E and F could not measure accuracy. On set F every tool got 0 right (`PROTOCOL.md`, Addendum 19). Seven of its ten
recordings had at most two aircraft within 3 km in 1.2 to 2 hours. At the two that had traffic, the audio held no trace
of it that a validated oracle hears. That was shown with injection controls, tested for a stream lag of up to two
minutes, and found again by a second, independent method; session log
`brennerbot_sessions/fdop-more-accurate-20261104/analyses/LOG.md`, 2026-11-06 08:48:55Z, 16:40:33Z and 17:00:22Z. Sets
E and F were chosen without consulting any aircraft data, which made them fair, but mostly unsolvable.

Set G keeps that fairness: no screen uses any tool's output or the test recording's sound. It adds two screens, so that
each case can be solved in principle:

- **Traffic:** aircraft passed close to the camera, from the ADS-B archive alone.
- **Hearing:** aircraft can be heard at that camera, from a separate pilot window of the same stream, by a method that
  is no tool's detector.

It also holds one known-unsolvable case, to test that a tool declines when there is nothing to hear.

## Builds under test, registered here

`fdop` builds, each built from `git archive` of its commit:

- `7e66501`, the benchmark's pinned build;
- `6914ab7`;
- `981fad1`, the last build registered for set F;
- `37a156b`, `main`'s newest code commit when this file was committed.

Registration closes with this commit. No build is added later. If a build cannot be built or fails `fdop selftest`, it
is reported as not run.

## Windows

- **Test window:** two hours of DVR audio from 2026-11-05T18:00:00Z.
- **Pilot window:** the same stream, two hours from 2026-11-04T18:00:00Z.

adsb.lol's archives for both days are out.

## Selection, in order

1. **Places, from ADS-B alone.** In 2026-11-05's adsb.lol archive (`prod+mlatonly`, the store `fdop fetch` builds),
   every track (the store drops jets) is sampled every 5 s within the test window. Samples at or below 3,000 m
   altitude are binned into 0.05° cells. A cell's score is its number of distinct tracks. For the 60 cells with the
   highest scores, in score order, the place is the 2024 Census Gazetteer place (incorporated or CDP) whose internal
   point is nearest the cell's centre. A place already taken is skipped. The tool is the scratch example `traffic_rank`;
   its source and hash are kept with the screen files.
2. **Search.** For each place in rank order, YouTube's live filter on `<place name> <state> live cam`, positions 1 to
   20. Each query's result list (position, video id, channel, title) is saved when it is taken, and the selection
   follows those files. Rows are taken in place order, then position. A stream met twice keeps its first position.
3. **Description screen.** As in `PROTOCOL.md` Addenda 9 and 13:
   - in the US, at a named public site, one fixed camera;
   - no ban on recording in the stream's own description, and no private residence;
   - no radio, scanner or pre-recorded sound, and no stream whose operator says it has no sound;
   - one stream per site.

   Out: every stream in sets A to F and the development recordings, and the streams of every operator out there for
   its terms (Virtual Railfan, LIVE Trains, Duluth Harbor Cam, RailStream, EarthCam, and Addendum 6's list).
4. **Truth.** An OpenStreetMap feature or the operator's published address. The limit is 1 km for a single building,
   station, marina or pier, and 5 km where the operator names no building. It is fixed before any screen below. Its
   coordinate is kept with the screen files and is not committed.
5. **Traffic screen, test window, ADS-B alone.** At the truth (ground height from USGS's elevation service, plus fdop's
   microphone height), the closest approaches of every track during the test window. These come from the scratch
   `cpa_list`, as for set F's oracle. A pass is one closest approach with a slant distance of at most 3 km. The case
   qualifies with at least 6 passes from at least 3 distinct aircraft.
6. **Level.** 60 s from 2026-11-05T18:00:00Z, fetched as below. It qualifies if ffmpeg's `volumedetect` `mean_volume`
   is above -60 dBFS. That number is all that is taken from it. The same fetch tests whether the stream's DVR reaches
   the test window.
7. **Hearing screen, pilot window only.** The pilot window is captured as below. Its passes come from 2026-11-04's
   archive, at the truth. The band-level method is run on it unchanged: source `c584bebcf39c`, binary `e238bc01c6e3`,
   rule in the session log at 2026-11-06T16:28:24Z. That is band power at each closest approach against the stream's own
   level at chance times shifted by +397, -631, +211 and -173 s, over passes within 15 km. The pilot's close passes are
   those within 3 km. The case qualifies with at least 4 close passes and at least 5 chance lines, and either:
   - at least 2 close passes, and at least 40 % of them, above the chance lines' 90th percentile; or
   - a mean chance percentile of at least 0.70.

   Nobody listens to the pilot, and no tool runs on it.
8. **Solvable cases.** The first 10 streams, in the order above, that pass screens 3 to 7. If fewer pass by the end
   of the 60 places, the set is smaller, and no screen is relaxed.
9. **The one unsolvable case.** The first stream in the same order that passes screens 3, 4 and 6 and has no track at
   all within 15 km during the test window. If none is met, it is the first such stream from YouTube's live filter on
   `train camera`, positions 1 to 120. It skips step 7.

A stream that cannot be fetched, whose DVR does not reach a window, or whose capture fails the outage rule, is listed
as not taken, and the next in order takes its place. Every stream reached is listed with the step it stopped at.

## Capture and seal

- **Capture.** The test window is fetched by the doppler skill's `scripts/ytlive_fetch.py` (sha256 `ce9d1c3dc9d5`, at
  `je_private_skills_repo` `8d8ffaad`). It runs on a machine on the owner's home network, because YouTube refuses the
  data-centre host.
- **Outages.** As in `PROTOCOL.md` Addendum 9: cut at the first outage, kept if the clock holds for at least 30 min.
- **Clock.** The true clock is the First-Frame stamp of the first fragment.
- **Silence.** A capture whose whole-file mean and peak are digital silence is void.
- **Seal.** Nobody listens to a test recording. No tool runs on it, and nothing diagnoses it, until every registered
  build's R2 run on every case is done. Then all are read once, together.

## Inputs, runs and scoring

- **Inputs.** R2 as in `PROTOCOL.md`: the start is the true clock rounded down to the minute, ±2 min. The area is a
  25 km circle whose centre is 10 km from the truth, on the bearing given by the first four bytes of `sha256(case id)`
  modulo 360. Case ids are `g_<site>`.
- **Runs.** `fdop locate` with default settings, once per build and case. A rerun is allowed only for a harness or
  machine fault, and each is listed with its cause.
- **Scoring.** As in `PROTOCOL.md`. A located answer graded high or medium is asserted. An asserted answer within the
  limit is correct; outside it, confident wrong.
- **Rivals.** `dopplerprint` and `dopplertrack` may run later with the same inputs, as separate rows. They change no
  `fdop` row.

## What it can show

Each registered build's correct and confident-wrong counts. These cover live cams where aircraft passed within 3 km and
could be heard at the site, which is not all live cams. A later build is more accurate than an earlier one only if it
has at least as many correct assertions, no more confident wrongs, and is strictly better on one of the two. With this
few cases every case is listed and no significance is claimed. On the unsolvable case the right outcome is no
assertion, and an assertion there is scored like any other.

Recordings, coordinates and aircraft identifiers stay out of this repository.

## 2026-11-06, the screens and the one run

### Result

Set G has **no solvable case**. Its one case is the unsolvable one, and on it every registered build declined:

| Build | Binary (sha256) | Outcome | Wall time |
|---|---|---|---|
| `7e66501` | `3f52bf10dec34fa3` | declined: 0 usable flyover events | 182 s |
| `6914ab7` | `b00978baa982386b` | declined: 0 usable flyover events | 150 s |
| `981fad1` | `ed7b8547362138e1` | declined: 0 usable flyover events | 151 s |
| `37a156b` | `855a7b6d48c583ad` | declined: 0 usable flyover events | 165 s |

The case is `g_mackinawcity`, the Riviera Motel, Mackinaw City, MI (`Mf_qId_7mlM`, level -38.4 dB, limit 1 km, R2
circle bearing 141°). Each build has 0 correct and 0 confident wrong. That is all set G shows: no registered build
asserts where no aircraft passed. It cannot compare accuracy, because no case could be solved.

### How the screens ended

All 312 search rows from the 47 places were screened in order (13 of the 60 cells mapped to a place already taken).

| Stopped at | Streams | Why, mostly |
|---|---|---|
| 3, title and description | 239 | excluded operators (55 on the title), not a camera of one site, outside the US, bans, radio feeds, music, private homes, a second stream at a site |
| 4, truth | 4 | no OpenStreetMap feature, and no published address that fixes the site within its limit |
| 5, traffic | 30 | fewer than 6 passes within 3 km |
| 6, level | 32 | 28 at or near digital silence (26 at -91.0 dB), one at -71.3 dB; 3 not taken (the fetch failed twice) |
| 7, pilot and hearing | 6 | 5 heard nothing; Carlsbad's pilot not taken (its clock held 875 s, under the outage rule) |
| 9, the unsolvable case | 1 | Riviera Motel |

The five that reached the hearing screen (band-level method; n close passes, k above the chance lines' 90th
percentile, m their mean chance percentile): St. Augustine Beach pier n 19, k 0, m 0.35; Mersea, Treasure Island n 20,
k 2, m 0.48; Fremont Street (LV Tattoo) n 15, k 0, m 0.42; South Ponte Vedra (Serenata) n 40, k 3, m 0.51; Venice
boardwalk, North Cam n 43, k 2, m 0.53. Robbie's, Islamorada, met before the Riviera Motel, had no track within 15 km
but was at -91.0 dB. Every row's step and reason are kept with the screen files.

### Readings of the rule, given here because the rule did not settle them

- **Same site.** From row 171 on, a later stream whose truth is within 1 km of an earlier description pass is the same
  site (Las Vegas City Hall, three Reno city cameras, Hollywood Beach Theatre). Earlier rows were judged by name.
- **Truth.** Where Nominatim matched nothing, or the wrong place, the published address was used, placed by Nominatim
  or by the Census geocoder. Every replaced first match is kept with the truth file. A lighthouse the camera looks at
  takes 5 km, as Addendum 13 does for a small feature in view.
- **Hearing.** n counts the close passes that bband could score. The chance lines come from passes within 15 km, as
  the rule says, where set F's lists used 5 km.
- **Retries.** A fetch fault (HTTP 403 or 404) was retried once.

### A build fault, caught before any run

The first build of the four shared one target directory, and the `git archive` trees carried old file times, so cargo
reused `7e66501`'s artifacts: the other three came out byte-identical to it. The identical hashes gave this away. All
four were rebuilt after touching each tree's files. Each binary's debug information names its own tree, and each passes
`fdop selftest`. The rebuilt `7e66501` reproduces its first hash. Only the rebuilt binaries ran.

### Found after the screens, and not used by set G

fdop's tonal oracle was run on the five pilots afterwards, as a diagnostic: the detector's own matched filter at each
ADS-B pass, against the same chance lines. It hears St. Augustine Beach pier at 8 of 19 close passes, mean percentile
0.79, where the band-level method heard none. South Ponte Vedra came out at 6 of 40, 0.67; the other three show
nothing. So the band-level screen misses propeller tones that fdop's method can use, and set G's "no solvable case" is
partly a property of that screen. Set G is not changed; `SET_H.md` is a new set.

## 2026-11-07, a correction to the section above

The tonal counts above took their chance lines from passes out to 15 km. At these sites the tonal score's chance level
is higher for passes within 3 km than for those beyond. Against only the close passes' own chance lines, St. Augustine
Beach pier is 2 of 19, 0.54, and South Ponte Vedra 2 of 40, 0.47: neither is heard. So that section's conclusion,
that the band-level screen misses tones fdop's method can use, is not shown. The details are in `SET_H.md`, its
2026-11-07 section.

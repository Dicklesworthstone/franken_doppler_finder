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

## 2026-11-08, the screens and the runs

### Result

Set I has **three cases**, not ten, and one of them was solved. At Winter Park all three `dopplerprint` variants were
right, about 3.2 km off within the 5 km limit, graded high. `9087ec4` was right there too, 1.3 km off, graded medium.
No tool asserted an answer on Monterey or Homosassa, or on any of the three wrong-date nulls.

- `9087ec4` ties `dopplerprint`, with one correct and no wrong each, so by the rule above it is not more accurate. It
  meets the rule only against `dopplertrack`, which errored on every row.
- The pinned build declined every row.
- `a0795de`, `main` when this file was written, placed Winter Park 32 km off and its null 22 km off, both at low, so
  not asserted.
- The pinned build and `a0795de` each have fewer correct assertions than `dopplerprint`, so the session's falsifier
  fires for both.
- `9087ec4` is the knot-rule build, whose development nulls already include a wrong-date medium (README Known issues).
  Its tie here does not revive the development claim.

| Tool | `i_monterey` (1 km) | `i_winterpark` (5 km) | `i_homosassa` (5 km) | The three nulls | Correct, wrong |
|---|---|---|---|---|---|
| `fdop` `7e66501` | declined, 69 s | declined, 73 s | declined, 97 s | declined | 0, 0 |
| `fdop` `9087ec4` | declined, 109 s | **correct**, 1,327 m, medium, 158 s | declined, 60 s | declined | 1, 0 |
| `fdop` `a0795de` | declined, 533 s | 32 km, low, 18,606 s | declined, 111 s | `i_winterpark_n` 22 km, low (10,529 s); two declined | 0, 0 |
| `dp` | 6,754 m, medium, inconclusive: declined | **correct**, 3,184 m, high, 18 s | inconclusive: declined | inconclusive: declined (12 km and 22 km at medium) | 1, 0 |
| `dp48` | 6,748 m, low, inconclusive: declined | **correct**, 3,161 m, high, 21 s | 33 km, medium, inconclusive: declined | inconclusive: declined (5.4 km medium, 23 km high, 22 km low) | 1, 0 |
| `dpdem` | 12 km, high, inconclusive: declined | **correct**, 3,161 m, high, 32 s | 33 km, medium, inconclusive: declined | inconclusive: declined (5.6 km medium, 23 km high, 22 km low) | 1, 0 |
| `dt` | error, refine rc 1 | error, refine rc 1 | error, refine rc 1 | error, refine rc 1 | 0, 0 |

**How `dopplerprint` was scored.** Where its status is "inconclusive", it declined by its own exit code. The scorer
counts that as a decline, as on sets E and F (`PROTOCOL.md` Addendum 19). At Winter Park the truth lies outside each
variant's own 90 % region.

**The read.** All rows were read once, together, by `tools/score.py`, at 2026-11-08T13:34:04Z. This came after the
fault inventory below. The output went to `runs_i/SCORE_r2_setI_20261108.txt` on the benchmark machine, sha256
`1240928f48ae10df58718e36e0cb17f7d556628282bf4929bc190b4db7f97327`. Wall times are from a shared host. `a0795de`
took 5.2 h at Winter Park, which is a busy cell.

With three cases, every case is listed and no significance is claimed. Set I's test audio and outcomes have now been
read, so no change made from here can be scored on set I as a gain. Its captures are development material from now on.

### How the screens ended

The 22 searches ran on 2026-11-08 from 03:22 to 03:24Z and gave 877 rows, 499 of them distinct streams. Every row's step
and reason are kept with the screen files on the benchmark machine.

| Stopped at | Streams | Why, mostly |
|---|---|---|
| 2, title | 175 | outside the US or the 48 states (the UK 17, Alaska 9, Israel 9, Kenya 8, South Africa 7, Canada 7 and others), not one fixed camera of one site, excluded operators |
| 3, earlier streams | 8 | 4 the cameras of development recordings (FeederWatch, Duke Farms, the Oyster Bay osprey, the North Fort Myers eagle), 4 read in sets G or H or a case of an earlier set |
| 4, level | 180 | 139 at or below -60 dB (103 at -91.0 dB); 41 not taken: 37 failed their fetch twice (HTTP 404 27, HTTP 400 6, the fetch tool's missing-header error 1, and 3 whose retry found no wall clock), 4 could not be timed |
| 5, description | 91 | site not named or not fixable within its limit (23), private residence (23), recorded sound or music (14, 9 of them Red Squirrel Studios' Finnish loops), a second stream at a site (14), bans on recording or use (8), outside the 48 states (6), a camera that pans or zooms (3) |
| 6, truth | 5 | no OpenStreetMap feature for the named site and no published address (Great Spirit Bluff, Castle Rock, the National Conservation Training Center, the Kentucky Equine Adoption Center, Kincaid Lake) |
| 7, traffic | 36 | 20 with no pass within 3 km in the two hours, 15 with 1 to 5, and Port Tobacco River Park with 6 passes from 2 aircraft |
| 8, capture | 1 | Boulder County Fairgrounds: 97 passes within 3 km, but its capture has 20 outages and its clock holds 20 s. Set H's pilot of the same site failed the same way (420 s) |
| case | 3 | below |

Three cases, not ten: the rows ran out first. The archive's store keeps no jets, and most wildlife cams are rural. Of
the 40 truths that reached the traffic screen, 20 had no pass within 3 km in two hours.

| Case | Stream (row) | Site and limit | Traffic in the window | Capture |
|---|---|---|---|---|
| `i_monterey` | `w3LjpFhySTg` (117) | Monterey Bay Aquarium, Kelp Forest exhibit, an underwater camera; 1 km | 6 passes within 3 km from 5 aircraft; 5 within 1.5 km from 4 | 120.1 min, no outage |
| `i_winterpark` | `pe4xqyzhrK8` (212) | Winter Park Eagle Cam, Winter Park, FL (a town); 5 km | 18 from 15; 4 from 4 | 93.8 min, cut at its one outage; 181 stamp jumps of 1 to 10 s |
| `i_homosassa` | `nbF12irhVys` (231) | Homosassa headspring, above-water camera, Homosassa Springs Wildlife State Park, FL; 5 km | 9 from 8; 3 from 3 | 95.8 min, cut at its one outage |

Each capture's true clock is its first fragment's First-Frame stamp, 17:59:56 to 17:59:59Z, so each R2 start is
17:59:00Z (±2 min), and each null's is 2026-11-06T17:59:00Z. The captures are sealed by sha256 on the benchmark
machine. Nobody listened to them, and no tool or diagnostic read them before the registered rows below.

### Readings of the rule, given here because the rule did not settle them

- **Retries.** A fetch fault (HTTP 400, 403 or 404, a yt-dlp or ffmpeg error, or the fetch tool's own error on a
  response without its sequence header) was retried once, 38 streams in all. One passed on its retry (row 50, an HTTP
  403, -38.5 dB), and stopped at its description. A stream whose fragments carry no wall clock was not retried. Three
  streams were refused that way on their retry, after a fetch fault on the first try.
- **Description.** A town the operator names as the nest's place is its site, with the 5 km limit (Barnegat Light,
  Bluff City, Seaside, Sandpoint, Decorah, Winter Park, Trempealeau, Independence, Johnson City, Two Harbors). A named
  building or facility takes 1 km. A site named only as a region (the West End of Catalina Island, the Missouri
  Ozarks, the Blue Ridge foothills) is out. The YouTube location tag is the operator's statement of a place (Sandpoint).
  An underwater camera passes, as in set H (Monterey's kelp forest). A camera the operator says pans or zooms is out;
  nothing else about the camera could be checked. A later stream at the same named site as an earlier pass is the same
  site, even where the earlier one stopped at a later step (the International Wolf Center, NCTC), because its truth
  would be the same.
- **Truth.** Nominatim's first match is taken when it is the place named. Twice it was not, and the place named was
  taken instead, before the traffic screen ran on it. For Trempealeau, WI, the county came first and the village is
  named. For Fraser Point, a Fraser Point in Lake County came first, and the description names Santa Cruz Island; the
  next query's one match lies on the island. Where a first query found nothing, one more was tried, without the town or
  in the operator's spelling: NCTC, Fraser Point, Weston Power Plant, Hog's Breath Saloon, Kincaid Lake. Goose Pond's
  Visitor's Center, which Nominatim does not have, is placed from the address on the Indiana DNR page that its
  description links, by the Census geocoder.
- **Traffic.** A pass counts if its reception time is inside the two hours. No stream was stopped by the 1.5 km rule
  alone: every one that stopped at traffic had fewer than 6 passes or fewer than 3 aircraft.
- **Capture.** The traffic screen counts the whole window, so some of Winter Park's and Homosassa's passes fall after
  their cuts.

### The runs, and the one fault

Regime `ir2` started at 2026-11-08T04:25:23Z. Every registered tool ran once on the three cases and then on the three
nulls, with default settings. The pinned `fdop` (`7e66501`) ran first, and its rows built each case's ADS-B store.
Beside it ran `dopplertrack`, and `dopplerprint`'s three variants in one chain with the national DEM renamed aside,
as for sets E and F. Then `9087ec4` and `a0795de` ran.

- **One row was rerun, for a network fault.** `a0795de` on `i_winterpark_n` stopped at 10:04:16Z with `fdop`'s error
  code 2. Its ADS-B fetch of 2026-11-06's mlatonly release got HTTP 504 from GitHub. A rerun at 10:09:58Z failed the
  same way at 10:12:18Z. At 10:13Z every GitHub release download tried from the benchmark machine's network returned
  504, including one from an unrelated repository (`cli/cli`), while github.com itself answered 200. Both failed runs
  are kept, renamed `_fault_http504` and `_fault_http504b`. The row was run once more when the file served again
  (10:34:23Z to 13:29:52Z).
- **No other row has a fault.** No row has exit code 101, 127, 133, 137 or 143, or no exit code. No row's error log
  has a line of a network, disk or kill fault.
- **What was seen before the read.** The watch on the last chain printed each `a0795de` row's exit code as the row
  ended (`fdop`'s 1 means no location). A fault check at 06:40Z saw `dopplertrack`'s refine step exit 1 with a pandas
  IndexError on every row, as on sets A to F. Neither could change a row, since every build and command was fixed
  before the runs.

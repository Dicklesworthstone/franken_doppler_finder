# Set H: set G's search, extended, with a second hearing method

Fixed on 2026-11-06, before any search for it, after set G found no solvable case (`SET_G.md`, its 2026-11-06
section). Everything not changed here is as in `SET_G.md`. The commit date of this file is the point of the exercise.
Later changes go in a dated section at the end; nothing here is edited to suit a result.

## Why

Set G's screens left no solvable case, for two reasons. Of the 39 streams whose level was taken, 28 sent digital
silence or close to it. And the hearing screen's one method, band power, is blind to a propeller's tones under
broadband noise. After set G's screens were done, fdop's tonal oracle heard St. Augustine Beach pier's pilot at 8 of
19 close passes, where band power heard none. Set H keeps set G's screens. It accepts either method at the hearing
step and searches further down the traffic ranking.

**Known before this file was written:** the tonal oracle's result on set G's five pilots. Under step 8 below, St.
Augustine Beach pier qualifies and the other four do not. No test audio of any set H stream has been fetched, and no
tool has run on one.

## Builds under test

The same four as set G, and the same binaries: `7e66501` (sha256 `3f52bf10dec34fa3`), `6914ab7` (`b00978baa982386b`),
`981fad1` (`ed7b8547362138e1`) and `37a156b` (`855a7b6d48c583ad`). No build is added.

## Windows

The same as set G: test, two hours from 2026-11-05T18:00:00Z; pilot, two hours from 2026-11-04T18:00:00Z. YouTube keeps
about 120 h of DVR, so pilots can be fetched until about 2026-11-09T18:00Z and tests until about 2026-11-10T18:00Z. A
stream whose DVR no longer reaches a window is not taken.

## Selection, in order

1. **Places.** `traffic_rank` as in set G (same store, window and settings), top 400 cells. Each cell maps to its
   nearest 2024 Gazetteer place as in set G, and a place already taken, set G's 47 included, is skipped. The order is
   set G's 47 places in their order, then the new places in rank order.
2. **Search.** As set G: YouTube's live filter, positions 1 to 20, each result list saved when taken. Set G's saved
   lists stand for its places.
3. **Title screen.** Set G's rules, read on the title and channel: excluded operators, sets A to G, the development
   recordings, outside the US or the store, not one fixed camera of one site, news, music and compilations. Set G's
   decisions stand for its rows.
4. **Level.** As set G step 6, taken here, before the description, because it needs no location. Set G's levels stand.
5. **Description screen.** As set G step 3, with one stream per site: a later stream whose truth is within 1 km of an
   earlier pass is the same site. Set G's decisions stand.
6. **Truth.** As set G step 4: an OpenStreetMap feature, or the operator's published address placed by Nominatim or
   the Census geocoder. It is fixed before the screens below. Set G's truths stand.
7. **Traffic.** As set G step 5.
8. **Hearing, pilot window only.** As set G step 7, with the same pass lists, chance shifts, close passes and
   thresholds. One change: a stream qualifies if **either** method qualifies.
   - **Band power:** set G's method, unchanged (`bband` `e238bc01c6e3`). Set G's outputs stand.
   - **Tonal oracle:** fdop's own detector's matched filter along each pass's ADS-B Doppler curve. The deep list is
     `deeplist` (binary `7be0b011`, source `475aafd1da57`, cap 400), then `oracle2` (binary `a9773d32`, source
     `db5f2f00a069`), window 20 s. The score is `raw`, read as set G's reader reads `best`. It passed its control on
     the development recordings (session log 2026-11-06T05:05:25Z: kept passes above the chance lines' 90th percentile,
     19 of 19). It runs on every pilot, set G's five included.

   Nobody listens to a pilot, and no registered build runs on one.
9. **Solvable cases.** The first 10 streams in the order above that pass screens 3 to 8. If fewer pass by the end of
   the places, or by the time the pilot window leaves the DVR, the set is smaller, and no screen is relaxed.
10. **No unsolvable case.** Set G's `g_mackinawcity` is the one.

A stream that cannot be fetched, or whose capture fails the outage rule, is not taken, and the next in order takes its
place. Every stream reached is listed with the step it stopped at.

## Capture, seal, runs and scoring

As set G. Case ids are `h_<site>`. Each test window is fetched once and sealed, as in set G. Every registered build's R2
run on every case finishes before any result is read. Then all are read once, together.

## Fairness, and what it can show

The tonal oracle is fdop's own front end, aimed with the true ADS-B passes. Selecting on it picks recordings where an
fdop-style tonal method has signal at the true answer. Between fdop builds that is fair: all four share that front end,
and it reads no build's answer and no test audio. For `dopplerprint` and `dopplertrack` it is not, so set H has no rival
rows.

It shows each registered build's correct and confident-wrong counts on live cams where aircraft passed within 3 km
and could be heard at the site. A later build is more accurate than an earlier one only if it has at least as many
correct assertions, no more confident wrongs, and is strictly better on one of the two. With this few cases, every
case is listed and no significance is claimed.

Recordings, coordinates and aircraft identifiers stay out of this repository.

## 2026-11-06, the screens and the runs

### Result

Set H has **two solvable cases**, not ten: the places ran out first. Every registered build declined both, as all four
declined set G's unsolvable case:

| Build | Binary (sha256) | `h_staugustinebeach` | `h_deerfieldbeach` |
|---|---|---|---|
| `7e66501` | `3f52bf10dec34fa3` | declined, 165 s | declined, 164 s |
| `6914ab7` | `b00978baa982386b` | declined, 369 s | declined, 597 s |
| `981fad1` | `ed7b8547362138e1` | declined, 373 s | declined, 694 s |
| `37a156b` | `855a7b6d48c583ad` | declined, 405 s | declined, 667 s |

Every run exited 1 with the same verdict: "0 usable flyover events; need at least two clear propeller flyovers". Each
build has 0 correct and 0 confident wrong on the three cases, `g_mackinawcity` included, so by the rule above no build
is more accurate than another. All eight runs finished before any was read, and all were read once, together, at
2026-11-06T22:45Z. Wall times are from a shared host.

- `h_staugustinebeach`: St. Augustine Beach pier, Surf Station North Pier Cam (`Q6eZVkUKFxo`, set G row 18, level
  -39.1 dB, set G's truth and its 5 km limit, R2 circle bearing 25°).
- `h_deerfieldbeach`: Constitution Park and Arboretum, Deerfield Beach, FL (`ngMbVpBhpe4`, row 905, level -43.5 dB,
  limit 5 km, bearing 171°). Its pilot qualified on the tonal oracle alone (n 7, k 4, m 0.92; band power n 7, k 0,
  m 0.65). Its test clock held all 7205 s, with no outage.

### How the screens ended

The 215 new places (ranks 61 to 400) were all searched, giving rows 313 to 910. Every row's step and reason are kept
with the screen files.

| Stopped at | Streams | Why, mostly |
|---|---|---|
| 3, title | 309 | outside the US (91) or the 50 states (11), not a camera of a site (74), an excluded operator (67), not one fixed camera (25) |
| 4, level | 223 | 195 at or below -60 dB (177 at -91.0 dB); 28 not taken: 9 could not be timed or had no from-start format, 19 failed their fetch twice (HTTP 400 or 404, yt-dlp, audio that would not open) |
| 5, description | 44 | radio or scanner feeds (10), a nest site not given (9), music or recorded sound (6), a second stream at a site (6), bans or terms (4), private (3), outside the US or the store (3), a site that cannot be fixed (3) |
| 6, truth | 0 | |
| 7, traffic | 14 | fewer than 6 passes within 3 km |
| 8, hearing | 7 | 4 heard by neither method; 1 with no close pass in the part of its pilot kept (Nashville); 2 pilots not taken under the outage rule (Boulder County Fairgrounds, clock held 420 s; San Jose City Hall, 1505 s) |
| 9, case | 1 | Deerfield Beach |

The new pilots heard (n close passes, k above the chance lines' 90th percentile, m their mean percentile; band power,
then tonal): PortMiami 8, 1, 0.65 and 8, 0, 0.37; U. S. Steel Irvin Plant 35, 2, 0.40 and 35, 0, 0.44; Pier 39 31, 3,
0.57, and every tonal score 0; Bryant Park 99, 13, 0.55 and 99, 3, 0.40; Deerfield Beach as above. Lower Broadway,
Nashville kept the first 3695 s of its pilot (one 16 s outage), and no aircraft passed within 3 km in that time.

### Readings of the rule, given here because the rule did not settle them

- **Retries.** A fetch fault (HTTP 400 or 404, a yt-dlp or ffmpeg error) was retried once. A stream whose fragments
  carry no wall clock, or that has no from-start format, was not taken without a retry: that is the stream, not the
  fetch. One HTTP 400 (row 716) was missed in the first retry round, retried alone afterwards, and failed again.
- **Description.** An underwater camera passes (rows 314 and 732). A rescue's camera passes where the rescue publishes
  its address (627). A private room at a sanctuary that is private, with no published address, is out (644). A stream
  whose site cannot be fixed within its limit stops here, so no stream stopped at the truth.
- **Truth.** The first match is taken when it is the place named, even where a park's polygon reaches past the limit
  from its point (Silver Springs State Park, 5.3 km; it stopped at traffic).
- **Outage rule on a pilot.** The capture is cut at its first outage and kept if the clock holds 30 minutes, as for a
  test capture.
- **Caches.** Each build kept one cache root: new and empty for its first case, and reused by its second. The rule asks
  for no more, and every run declined on its audio alone, before any downloaded data could matter.

### Found after the runs, and not used by set H

With every outcome read, fdop's tonal oracle was run on the two test captures, with pass lists from the test window's
ADS-B. It hears St. Augustine at 7 of 18 close passes (m 0.78) and Deerfield at 6 of 10 (m 0.91; band power 5 of 10,
0.74), so the aimed filter finds the aircraft on the test hours too. But each pass is faint. The true passes' best
matched-filter scores are 1.48 to 2.01 and 2.05 to 2.49, and no registered build keeps a candidate under 3.5 (`7e66501`
none under 4.5). The oracle and `deeplist` are built on `446a2bf`, whose survey and score are those of all four builds:
its `detect.rs` is `981fad1`'s and `37a156b`'s, `6914ab7`'s but for comments, and `7e66501`'s scoring refactored. In
both captures the best of the chance lines (2.21 and 2.79) beats every true pass, and a blind search has to beat the
best window of the whole recording. The detector's candidate list was empty on both, even with a cap of 400. On each of
the 10 pilots in sets G and H that had a close pass and were heard, the best chance line scores at least as high as the
best true pass. At St. Augustine, South Ponte Vedra and Deerfield the true passes stay under 3. At PortMiami and the
Irvin Plant they reach 3.5 and 4.1, with chance lines up to 6.6. At Bryant Park, Fremont Street, Treasure Island and
Venice they pass 5, and chance lines reach 8.8 to 10.6. At a busy site a shifted window can hold another aircraft.
Pier 39 scored 0 everywhere.

So set H's cases are solvable in the sense its rule fixed: aircraft passed, and fdop's own filter, aimed with the true
passes, finds them above chance. They are not solvable by a method that must first find single flyovers blind, and
every registered build does that. A method that adds faint evidence over many passes under each candidate place might
solve them. Nothing here shows that it would.

Set H's test audio and outcomes have now been read. No change made from here can be scored on set H as a gain, and its
captures are development material from now on.

## 2026-11-07, a correction: the tonal read's chance lines

### What was wrong

The hearing read (step 8) sets each close pass's score against chance lines, and takes the chance lines from every
pass in the pass list, out to 15 km. Most of those passes are more than 3 km away. But on the same audio, the chance
lines of passes within 3 km can score differently from those of passes beyond it. For the tonal oracle, at St.
Augustine Beach and Deerfield Beach the near ones score higher (pilot medians 1.75 against 1.58, and 2.27 against
1.49), so a close pass looked heard when it scored only as a close pass scores by chance. At the busiest sites the far
ones score higher (Bryant Park 4.87 against 5.45, Fremont Street 2.71 against 4.39), which works against close passes
instead. Band power splits at some sites too. On Deerfield Beach's pilot its near chance lines score far lower (1.49
dB against 4.20), which hid that pilot's close passes.

The rule is not changed, and set H's result stands as the rule's result. What it shows changes.

### The same reads, against the close passes' own chance lines

Here the chance lines are only those of passes within 3 km (their shifted copies). Everything else is as in step 8
(k above the chance lines' 90th percentile, of n close passes; m their mean percentile):

| Capture | Tonal, as read | Tonal, close chance lines | Band power, as read | Band power, close chance lines |
|---|---|---|---|---|
| St. Augustine Beach pier, pilot (set G) | 8 of 19, 0.79 | 2 of 19, 0.54 | 0 of 19, 0.35 | 0 of 19, 0.34 |
| Deerfield Beach, pilot | 4 of 7, 0.92 | 2 of 7, 0.52 | 0 of 7, 0.65 | 1 of 7, 0.75 |
| St. Augustine Beach pier, test | 7 of 18, 0.78 | 1 of 18, 0.52 | 2 of 18, 0.58 | 2 of 18, 0.57 |
| Deerfield Beach, test | 6 of 10, 0.91 | 0 of 10, 0.54 | 5 of 10, 0.74 | 5 of 10, 0.80 |

Against each pass's own shifted lines alone (up to four; passes with fewer than three left out), the tonal mean
percentiles are 0.43, 0.54, 0.47 and 0.45 in the same order, where chance is 0.5. So the tonal oracle hears neither
site, on the pilot or on the test hour. South Ponte Vedra's 6 of 40 (0.67, `SET_G.md`) becomes 2 of 40, 0.47.

Read this way, St. Augustine Beach pier would not have qualified: the tonal result that set H was made for does not
hold. Deerfield Beach would have qualified on band power instead (n 7, m 0.75, 28 chance lines), and band power hears
its test hour too. No other pilot read in sets G or H qualifies on either method under this read. So Deerfield Beach is
the one case where any method hears the aircraft, and that method is band power, which fdop does not use.

The tonal method's control (step 8) ran on the development recordings, whose passes are loud. It showed that the
method can hear a loud pass. It did not test whether its chance level is fair at a quiet site.

### Checked where nothing can be heard

The split is in the tonal score, not in the sound at these sites. The tonal oracle was run with St. Augustine's and
Deerfield's test pass lists on `g_mackinawcity`'s test audio, where none of those aircraft can be heard, so every line
is chance. The near lines still score higher (medians 1.66 against 1.46, and 1.67 against 1.47). Read as step 8 reads,
both lists qualify as heard (6 of 18, 0.79; 2 of 10, 0.73). Against the close passes' own chance lines, both are out
(1 of 18, 0.63; 1 of 10, 0.53). Band power on the same audio splits little and qualifies neither (0.41 and 0.41 as
read, 0.44 and 0.51 against close chance lines), so its 0.80 on Deerfield's own test audio is not the read's doing.

### What this changes above

- "Found after the runs": its tonal counts (7 of 18 and 6 of 10) carry the same bias. Its statement that the aimed
  filter finds the aircraft on the test hours is wrong for the tonal filter. It holds for band power at Deerfield
  Beach. Its comparisons of best scores stand: against only the close passes' chance lines, the best is still 2.15
  against the true passes' 2.01 at St. Augustine, and 2.79 against 2.49 at Deerfield. Its "10 pilots in sets G and H
  that had a close pass and were heard" are the 10 whose hearing read ran, not 10 heard above chance.
- "So set H's cases are solvable in the sense its rule fixed": true only in the letter of the rule. fdop's tonal
  filter, aimed with the true passes, does not find either case's aircraft above chance.

### Adding evidence over places, tried

The last paragraph of "Found after the runs" asked whether adding faint evidence over many passes under each
candidate place might solve these cases. It was tried as a development experiment, with its statistic and its
falsifier fixed before it ran. At the true place and at each of 489 places on a 2 km grid over the case's R2 circle,
every pass within 3 km of the place was scored by the tonal oracle, along with 16 time-shifted copies of each. Each
pass's z score was taken against all the copies at that place, and the z scores were summed and divided by the square
root of the number of passes. A place was scored if it had at least 4 passes and 40 copies. At the true place the sum
is 0.71 at St. Augustine (18 passes) and 0.47 at Deerfield (10). Of the scored places more than 3 km from the truth,
68 of 191 and 170 of 382 score as high, the best 3.97 and 3.79. That is chance, as the read above predicts, and the
idea is refuted on these cases. Its null case, `g_mackinawcity`, could not be scored: no place in its circle had a
pass within 3 km, and the ADS-B store holds one pass within 80 km of the site in the two hours.

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

# Head-to-head benchmark protocol

Is `fdop` more accurate than [`dopplerprint`](https://github.com/Hexay/dopplerprint) and
[`dopplertrack`](https://github.com/mjshiggins/dopplertrack)? This file fixes the cases, inputs and scoring before
any competitor is run on them, so the commit date of this file is the point of the exercise. Changes after that
date are additions in a new section, dated and explained; nothing here is edited to suit a result.

## What was already known when this was written

- `fdop`'s date-and-state results on the reference and field recordings (README "Honest status"). Those are not
  blind. Every run below that gives `fdop` a start time is new.
- The competitors' published results. On the reference recording they are better than `fdop`'s: `dopplertrack` 43 m
  with hand-picked flyovers, `dopplerprint` 86 m nationwide with the start known to ±2 min, against `fdop`'s 271 m
  and 306 m with only the date. They had more input, but the numbers are smaller.
- All three tools were developed on the reference recording, so it says little about new recordings. `dopplertrack`
  was developed on the eagle cam and `dopplerprint` on Livermore. `fdop` was checked against the field recordings.
  Only set E is new to all three.

## Versions

| Tool | Commit | Run as |
|---|---|---|
| `fdop` | `7e66501` | `fdop locate`, `fdop detect` |
| `dopplerprint` | `81fba2c52d00fa647592727a2d5f3f6120f37089` | wheel built from that commit, `dopplerprint locate` |
| `dopplertrack` | `0fd161fb75b069e44d6ab77e02c42dedf93b4f7d` | `uv sync --frozen`, `dopplertrack` subcommands |

Default settings throughout. No setting is tuned to a case or to a result. If a later commit of any tool is tested,
it is reported as a separate run beside these, not in place of them.

## Check first: can we reproduce each tool's own claim?

Before scoring, on the reference recording: `dopplerprint` regional ±5 s (claims 79 m) and nationwide ±2 min (claims
86 m), and `dopplertrack` with its published hand-fitted events (claims 43 m). A failure here means the harness is
suspect. It is investigated before anything else runs, and whatever is found is reported.

## Cases

| Set | Recordings | Truth | Correct within |
|---|---|---|---|
| A | Reference: colsto's raw upload, Oyster Bay, NY, 2026-09-08 | Coordinate published in the video, 40.876466, −73.524951 | 1 km |
| B1 | Osprey cam, Oyster Bay, NY, 3 recordings (2026-10-31, 11-01, 11-02) | Same coordinate | 1 km |
| B2 | Eagle cam, North Fort Myers, FL, 2024-12-28 | Published nest register coordinate | 1 km |
| B3 | Duke Farms, NJ; Sapsucker Woods, NY; Rochelle, IL; Fullerton, CA; Plant City, FL; Tucson, AZ | Town-level or site-level location only | 5 km |
| C | Wrong dates: A on 09-07 and 09-09; Oyster Bay 11-01 on 10-31; Sapsucker Woods on 11-01; Duke Farms on 11-02 | Any location is wrong | never |
| D | Livermore, CA, 40 single flyovers (Pathak 2026, Zenodo 23424278, CC BY 4.0) | ADS-B values in the dataset's own results table | see below |
| E | New recordings, chosen by the rule below | Published coordinate | 1 km |

Site coordinates other than the reference stay out of this repository, under the project's data rules. Results name
towns and distances.

**Set E rule.** Public US live cams with audio and an exact published coordinate, which none of the three projects
has used. The list is committed before any audio is downloaded and before any tool runs. Every recording on the list
is scored, including those that turn out to have no flyovers.

## Inputs

The same information goes to every tool that can take it.

**R2, the primary regime: start known, area known.** The start is the true clock rounded down to the minute, ±2 min.
The area is a circle of radius 25 km whose centre is 10 km from the truth, on the bearing given by the first four
bytes of `sha256(case id)` modulo 360, so nobody chooses it. Wrong-date cases use the same circle and a start shifted
by whole days. True clocks come from the stream fragment timestamps of each capture. The eagle cam is an archive with
no such timestamps, so it uses YouTube's published stream start, 14:21:16 UTC, which is within 3 s of the clock implied
by `dopplertrack`'s published matched flights.

```bash
fdop locate F --dates LOCAL_DATE --near LAT,LON,25 --start T --clock-tol 120 -o OUT
dopplerprint locate F --start T --start-tolerance 2m --near LAT,LON,25 --yes --json --out OUT
# dopplertrack takes no start time. It gets the UTC day and the circle's bounding box.
dopplertrack spectrogram F rec.npz
dopplertrack detect rec.npz --band 60 300 --top 10 --out detected.json
dopplertrack search rec.npz --events E1 E2 --traces T --bbox S N W E --spacing 200 --out results.csv
dopplertrack refine rec.npz results.csv --events E1 E2 --traces T --bbox S N W E --extra E3 E4 E5 --out fine.csv
```

`E1` to `E5` are `detect`'s top five, in its order. Its README says these fits do not yet stand in for hand fits on the
reference recording. Choosing windows by hand would put a person, and here someone who has seen the truth, in the
loop, so its hand mode runs only in the reproduction check above, with its author's own events.

**R3: start known, no area.** `dopplerprint` without `--near`; `fdop --near CONUS` with the same start and tolerance.
`dopplertrack` needs a box and does not run here.

**R1: date and state only.** `fdop --dates D --near STATE`. `dopplerprint` requires a start, so it runs with the local
day's midpoint and ±12 h if it accepts that; otherwise it is recorded as unable to take these inputs. `dopplertrack`
does not run here: its published searches are at most 150 km across.

## Scoring

Each run is one of: **located** (it prints a location as its answer), **declined** (`fdop` exit 1; `dopplerprint` exit
2 or 3), or **error**.

A located answer is **asserted** when the tool stands behind it: `fdop` high or medium; `dopplerprint` status found
with confidence high or medium; `dopplertrack` every answer, because it has no way to decline or to grade.

- **Correct assertion:** asserted and within the case's limit.
- **Confident wrong:** asserted and outside the limit, or asserted at all on set C.
- **Unasserted location:** `fdop` low, `dopplerprint` found at low. Reported with its distance, and it counts as
  neither.

Reported for every run: outcome, distance, grade, wall time (load noted; a timing is not evidence of correctness) and
whether the truth lies inside the region the tool claims (`fdop`'s radius; `dopplerprint`'s 90 % region).

**The claim this benchmark can support.** On R2, over sets A, B and C, and then again on set E alone: `fdop` is more
accurate than a competitor only if it has at least as many correct assertions, no more confident wrongs, and is
strictly better on one of the two. Median distance over the cases both tools located is reported beside that, and is
reported even when it favours the competitor. A smaller median error for a competitor on those cases is a loss in
precision and will be said to be one. With this few recordings, every case is listed and no significance is claimed.

**Set D.** Automatic per-flyover measurement: the share of the 40 passes each tool finds unaided, and for those, median
absolute error in speed, closest distance and time of closest approach against the dataset's ADS-B values, with the
paper's two methods alongside. `fdop` refuses to locate from one flyover by design, so on location over this set it
scores 0 of 40 located and 0 confident wrong. `dopplerprint`'s location results over this set are run and reported the
same way. That is a loss in coverage for `fdop`, and is reported as one.

## Rules for running

- Each tool, case and regime runs once. A rerun is allowed only for a harness fault or a machine failure, and each one
  is listed with its cause. A wrong answer is never a reason to rerun.
- If a result looks like a harness error that hurts a competitor, it is investigated with the same care as one that
  hurts `fdop`.
- `fdop` fixes made after this file do not change these results. A better `fdop` is a new run beside them, and it faces
  set E recordings it has not seen.

## Addendum, 2026-11-04, before any scored run and before any set E audio was downloaded

**Recordings dropped for their operator's terms.** Virtual Railfan's published policy says recording its content is
"strictly prohibited" without prior consent. Rochelle, Plant City and Tucson in set B3 are its streams, and they are
not run for this benchmark unless consent is obtained. This was decided from the policy, with no competitor run on
them, and they are listed as not run. Set B3 is now Duke Farms, Sapsucker Woods and Fullerton.

**Set E, fixed here.** The search was YouTube's live filter on five queries (train camera, Virtual Railfan, airport,
harbor, marina). A cam qualifies if it is in the US, has an audio track, is at a named public site, is new to all three
projects, and states no ban on recording. Private residences are out, and so are Virtual Railfan, LIVE Trains and
Duluth Harbor Cam, for their stated terms. "Exact published coordinate" in the rule above is relaxed to a named site:
only one of these publishes a coordinate. The limit stays 1 km, because each site is a single building, station or
marina. No audio was heard, and no ADS-B or aircraft data was consulted, to make this list.

| Case | Site | Video |
|---|---|---|
| `e_olympia` | Olympia-Lacey Centennial Station, WA | `PftkGaoFl_0` |
| `e_fortmadison` | Spring Creek Model Trains, Fort Madison, IA | `mexGMd6-8ik` |
| `e_oxnard` | Axis Air FBO, Oxnard Airport, CA | `MfqAgiqY4-0` |
| `e_fridayharbor` | Port of Friday Harbor, WA | `pr2p_uvmXh8` |
| `e_aransas` | Aransas Pass Harbor Park, TX | `Mn3vY8bordE` |
| `e_sheboygan` | Sheboygan marina, WI | `lu-YXDumeps` |
| `e_colchester` | Champlain Marina, Colchester, VT | `WYgjhPHIw1k` |
| `e_porthenry` | Bridgeview Harbour Marina, Port Henry, NY | `Bf7ewHyVP5I` |
| `e_bayfield` | Pikes Bay Marina, Bayfield, WI | `O8kPCPZEFE4` |

**Capture.** Two hours of each stream's DVR audio from 2026-10-31T17:00Z, with its true clock from the fragment
timestamps. If a stream's DVR does not reach back that far, 2026-11-03T17:00Z instead, and the case says so. A stream
that cannot be captured at either time is listed as not captured. Set E runs on R2, R3 and R1, with R2 scored as
above.

**Set D truth.** The Livermore recorder position is not published. It is recomputed here from the dataset's tracks and
offsets, not taken from `dopplerprint`'s fit. The dataset does not publish each audio file's start time, so time of
closest approach is scored only if it can be recovered from the dataset's own timestamps. Otherwise that column is
dropped and this file says so.

## Addendum 2, 2026-11-04, after the reproduction check and before any scored run

### What the reproduction check found

| Claim | Run | Result |
|---|---|---|
| `dopplertrack` 43 m, with its published events | `search`, then `refine` | Reproduced: the same coordinate to the last digit, 43 m |
| `dopplerprint` nationwide ±5 s, 116 m in its notes | `locate` on the WAV its own case uses | Reproduced: found, high, 116 m, both true flights, decoy margin 29.94 (its notes: 29.9) |
| `dopplerprint` nationwide ±2 min, 86 m in its notes | `locate` on that WAV, `prod` feed | Not reproduced: it declines, with the right spot ranked first (40 m off) by a margin of 0.5, under its limit of 3. A run on the `staging` feed its case uses is pending and goes in a later section |
| `dopplerprint` regional ±5 s, 79 m | Cannot be run through `locate` | This figure has its flyover positions supplied (`oracle_events = true`), as its README says. `locate` gives 648 m on our file and 646 m on its WAV, from one of the two flights |

So the harness reproduces both tools where a reproduction is possible. "Regional ±5 s (claims 79 m)" in the section
above was this file's mistake: that number is not what `locate` does. The first attempt at `dopplertrack`'s check was a
harness fault of ours and says nothing about it: `extract-region` found 0 traces because the archive we gave it is split
into tar parts, which it does not read. It was rerun with `fetch-day`, as its README does.

### Input encoding changes `dopplerprint`'s answer

The same upload, with no `--near`, in three encodings:

| Audio | ±5 s | ±2 min |
|---|---|---|
| Opus-sourced 48 kHz WAV (its own case) | found, high, 116 m | declined, right spot first (40 m), margin 0.5 |
| AAC 44.1 kHz (our file, and how every B and E capture is encoded) | declined; candidates 3,880 km and 282 km off | **found, high, 314 km off** |
| The same AAC resampled to 48 kHz | not run | declined, right spot first (610 m), margin 0.2 |

The third row is what matters here: resampling alone moves it from a confident wrong answer to a narrow decline, so
some of this is sample rate rather than codec. A 44.1 kHz file is a normal input, and its README's own example is an
`.m4a`, so the runs fixed above stand and the claim below is tested on them. But a tool should not lose a benchmark over
something one resample would fix without that being visible, so two more sets of runs go beside them:

- `dopplerprint` runs R2 a second time on every recording resampled to 48 kHz, with nothing else changed. If `fdop`'s
  result against it differs between the two, the report says so and gives both.
- All three tools run R2 on the reference in its own WAV as well (`a_ref_opus`), reported beside `a_ref` and not counted.

### Other changes

- **Fullerton is dropped.** RailStream's terms license personal use "and not to download (other than page caching) or
  modify it, or any portion of it, except with expressed written consent." They are about its site and the capture came
  from its YouTube stream, but the wording is close enough to Virtual Railfan's to be treated the same way. Set B3 is now
  Duke Farms and Sapsucker Woods.
- **Case ids**, which set the bearings: `a_ref`, `b1_osprey_1031`, `b1_osprey_1101`, `b1_osprey_1102b`, `b2_eagle`,
  `b3_duke`, `b3_sapsucker`, wrong dates `c_ref_0907`, `c_ref_0909`, `c_osprey_1031`, `c_sapsucker_1101`, `c_duke_1102`, and
  set E as listed. Wrong dates and `a_ref_opus` use their parent's circle.
- **Truth.** Each named site is an OpenStreetMap feature, or the operator's published street address geocoded by
  OpenStreetMap, fixed from those alone. `e_fortmadison` has no building: its operator gives a railroad milepost in town. Its
  limit becomes 5 km, like set B3.
- **Outages.** A capture with a stream outage is cut at the first one, where its clock stops holding. If under 30 minutes
  remain, that window counts as not captured and the second window is used. This was set before any audio was heard.
- **Set E as captured.** Eight recordings, all from the first window. Six run the full 2 h; `e_fortmadison` is cut to
  49.7 min and `e_oxnard` to 53.1 min. **`e_colchester` is not captured**: its clock holds for 27.9 min at the first window
  and 23.6 min at the second.
- **Medians** leave out set C, where a distance from the true place means nothing, and any run where a tool declined.
- **R1.** A state is not an area `dopplerprint` accepts (`--radius` is at most 50 km), so it gets the day and no area.
- **Flags that are not settings.** `dopplerprint` also gets `--keep-raw --max-cache-gb 200` (its default of 20 GB would
  refuse the downloads) and `fdop` also gets `--json`. Neither changes a search.

### Set D, fixed before its runs

- **Recorder position.** Recomputed from the dataset's tracks and published ground offsets alone. 37 passages fit one
  position with a residual RMS of 2.1 m. The other 3 fit a second position at 31 m, from 3 distances and 2 unknowns, so
  that one is weak and the report marks it.
- **Clock.** Recoverable. Sliding each clip's UTC-stamped frequency curve over its own spectrogram puts clip time zero
  5.0 s before `received_start_utc` (median of 40). The time column is scored on the 37 passages with the well-fitted
  position. The 3 others put closest approach outside some clips, so they have no time truth.
- **Finding a pass.** A tool finds a passage when one of its events has its received closest approach within 15 s of the
  truth. That event's speed and distance are compared with the dataset's ADS-B values. Also reported: how often its
  top-ranked event is the match, and its events per clip, because more events find more by chance. On the 3 passages with
  no time truth, the top-ranked event stands for the tool.
- **`dopplertrack`'s band.** It runs at 60-300 Hz as in R2, and also at 40-450 Hz, its spectrogram's whole range, because these
  aircraft go above 300 Hz.
- **Location.** `dopplerprint` and `fdop` both run R2 inputs on each clip, limit 1 km. The section above predicts 0 of
  40 located for `fdop`; running it tests that. `dopplertrack` needs two flyovers in a recording and has no run here.

## Addendum 3, 2026-11-04, with R2 and set D detection running

The only scored result read before this was written is `dopplerprint`'s `a_ref` in R2: declined. Everything below adds
runs for `dopplerprint` or makes a claim against it harder, so that result cannot have helped `fdop` here.

### Correction: the 86 m reproduces

Addendum 2 said `dopplerprint`'s nationwide ±2 min figure of 86 m was not reproduced. That check had left out the last
input its notes depend on, the national ground model.

| Its WAV, ±2 min, no `--near` | Result |
|---|---|
| `prod` feed, no ground model | declined, right spot first (40 m), margin 0.5 |
| `staging` feed, no ground model | declined, right spot first (40 m), margin 0.5 |
| `staging` feed, national ground model | **found, high, 86 m, both true flights, decoy margin 40.89** (its notes: 86 m, 40.9) |

So the harness reproduces this figure too, and the ground model is what moves it. `locate` neither downloads nor builds
that file. Its `USAGE.md` documents it as optional: `<cache>/dem/5070/conus_1000m.u16`, made by a separate command that
needs the `prep` extra. Without a ground model it takes the ground as flat at the local geoid height, and says so.

### `dopplerprint` also runs with a ground model

`fdop` and `dopplertrack` fetch terrain themselves by default. `dopplerprint` as installed does not, so every `dopplerprint`
run fixed so far is on flat ground. That is what a new user gets and those runs stand, but beating only them would say
little. So its strongest configuration found so far also runs, on every case:

| Runs | Audio | Ground model |
|---|---|---|
| `r2_dpdem` | 48 kHz, as in `dp48`; `a_ref_opus` in its own WAV | 30 m over the bounding box of the case's circle, passed with `--dem` |
| `r3_dpdem`, `r1_dpdem` | The same | `conus_1000m` in its cache, built by the command in its docs |
| `sd_dpdem` | The clips, already 48 kHz | 30 m over a box holding all 40 circles, passed with `--dem` |

- All are built with `dopplerprint.prep.dem` from Terrarium tiles, the elevation source `fdop` and `dopplertrack` read.
- The 30 m step is that tool's default. A ground model that fails to build leaves its case pending, not an error of
  `dopplerprint`'s.
- `a_ref_opus` in `r3_dpdem` is its WAV with the national model on the `prod` feed `locate` downloads, which shows
  whether the feed matters to the 86 m.
- The national file goes into the scored cache only after `r3_dp` and `r1_dp` finish, because the nationwide path reads it
  by itself. The jobs refuse to run in the wrong order. `--near` runs do not read that path, so R2 and set D are unaffected.

**The claim gets harder.** On R2, `fdop` is more accurate than `dopplerprint` only if the rule under Scoring holds against
each of `dp`, `dp48` and `dpdem`, over sets A, B and C and again on set E alone. A win over only some of them is reported
as exactly that. Addendum 2 asked only for a difference between `dp` and `dp48` to be shown.

### Reruns so far

- `dopplertrack`, R2: our script overwrote its own variable and passed a run name as the box's north bound, so `fetch-day`
  exited 2 and no `search` ran. No answer existed. The chain was stopped, its directory kept as `r2_dt_fault_n_clobbered`,
  and it restarts from the first case.

## Addendum 4, 2026-11-04, before any ground-model run

Addendum 3 says every ground model comes from Terrarium tiles. For the 30 m models that would have handicapped
`dopplerprint`: `prep.dem` reads Terrarium at zoom 7, about 0.9 km per pixel, and its docs use that only for the national
file. So `r2_dpdem` and `sd_dpdem` use the tool's default sources, as its docs do for a region: USGS 3DEP 1″ bare earth,
then Copernicus GLO-30. `conus_1000m` stays on Terrarium, as its docs say. The tools therefore read different elevation
data: `fdop` Terrarium at zooms 10 and 13, `dopplertrack` Terrarium at zoom 12, `dopplerprint` the sources above. Each uses
its own.

## Addendum 5, 2026-11-04, with R2 running: five of set E's captures have no sound

### Void cases

Over each whole file, the mean and the peak of these captures are both -91 dB, which is digital silence:
`e_aransas`, `e_bayfield`, `e_fridayharbor`, `e_oxnard`, `e_sheboygan`. So is the uncaptured `e_colchester`, at both windows.
The same capture tool got sound from the other three, whose means are -36.0 dB (`e_olympia`), -34.9 dB (`e_fortmadison`) and
-19.3 dB (`e_porthenry`), so these streams carry a silent track.

The rule above asked for "an audio track" and nothing checked that the track had sound. That is this file's mistake. It
came to light because `dopplerprint` reported no passes at all on these five, and `fdop`'s event counts had been read
when the level was measured. The test itself is the level alone.

No answer on a silent file is evidence about a tool. These cases are **void for every tool in every regime**: they stay
in the table marked void, they leave the counts and the medians, and R3 and R1 do not run on them.

### Replacements, under a rule fixed before any search

Three recordings say little about "set E alone", so five more are added.

- **Search.** YouTube's live filter, in its own order, on: train camera, railroad live, train station live, airport live,
  airport camera, harbor, marina.
- **Qualifying.** As in the first addendum: in the US, at a named public site, new to all three projects, no ban on
  recording, no private residences. Virtual Railfan, LIVE Trains, Duluth Harbor Cam and every cam already considered for set E
  or B stay out.
- **Sound.** About 60 s of the stream's audio. It qualifies if its mean level is above -60 dB. That number is the only thing
  taken from the audio: nobody listens to it, no tool runs on it, and no spectrogram, ADS-B or aircraft data is consulted.
- **Selection.** The first five to qualify, taking the queries in the order above. They are listed in a later section before
  capture.
- **Capture.** Two hours of DVR audio from 2026-11-04T17:00Z with the true clock from the fragment server, by the same tool.
  The outage rule is unchanged. A stream whose DVR does not reach that far is listed as not captured, and the next to qualify
  takes its place.
- **Truth and limits.** As before: an OpenStreetMap feature or the operator's published address, 1 km for a single building,
  station or marina, 5 km where the operator gives no building.
- **Runs.** R2, R3 and R1 as for the rest of set E, including the `dp48` and `dpdem` runs, once that day's ADS-B archive is out.
  `fdop` stays pinned at `7e66501`.

"Set E alone" in the claim now means the three recordings with sound and these replacements.

## Addendum 6, 2026-11-04, the five replacements, before their capture

Listed before capture, as Addendum 5 requires. No tool has run on these streams, nobody has listened to them, and no
spectrogram, ADS-B or aircraft data was consulted.

| Case | Site | Video | Mean level | Limit |
|---|---|---|---|---|
| `e_tehachapi` | West Cable, Tehachapi, CA (a milepost on the Mojave Subdivision) | `8GmYBH2jH5w` | -30.7 dB | 5 km |
| `e_kelso` | City of Kelso railcam, Kelso, WA (a milepost in town) | `TLi5ZJgXOYI` | -45.3 dB | 5 km |
| `e_sagharbor` | Foster Memorial Beach, Sag Harbor, NY (a beach, no building) | `p1IbYzNwWDQ` | -58.8 dB | 5 km |
| `e_harbourgate` | Harbourgate Marina, North Myrtle Beach, SC | `SmCHAs7oQYM` | -28.8 dB | 1 km |
| `e_lakeshore` | Lake Shore Railway Museum, North East, PA | `DFOARJuSfwQ` | -41.8 dB | 1 km |

**How the rule was applied, including where this goes beyond Addendum 5's wording:**

- **Depth.** The rule set no depth. Positions 1-40 of each query were read in query order and gave four. Positions 41-120 were
  then read in the same order, and the first stream to qualify there is `e_lakeshore`. A strict reading, finishing each query
  before the next, could have picked differently.
- **Terms.** Each stream's own YouTube description was read. Wider site terms were not, with one exception: EarthCam's terms page
  could not be retrieved, so its cams are left out rather than assumed to allow recording. A bare copyright notice is not
  counted as a ban, so `e_sagharbor`, which carries one, stays in. "Use by permission only", "strictly prohibited" and the like
  are counted, and they apply to that operator's other cams in the lists. Reno's permission to reproduce only with a prominent
  attribution is counted as a ban too.
- **Radio.** Added here: a stream whose operator says its sound is a radio or scanner feed is out, because it carries no sound
  of the place, which is why the silent files were voided. This was not in Addendum 5. The five above do not say what their
  sound is, and that is not checked further: the level says only that something is there.
- **Level.** The 60 s were taken by the capture tool from 2026-11-04T17:00:00Z, so the same step tests whether the DVR reaches
  that time. Those files stay on the bench disk, unread apart from the level.

**Screened in order and not taken:**

| Stream | Why not |
|---|---|
| Greenville Jct, ME, and Boston and Maine Live's other cams | use by permission only |
| Golden Spike Tower, North Platte, NE | -77.3 dB |
| Truckee Railroad Museum, CA | duplication strictly prohibited |
| Lehigh Gorge Scenic Railway, PA | names a railway, not where the camera is |
| Miami rail cam, FL | names no particular station |
| Caveman Aviation, Anchorage, AK | a hosted show, no fixed site named |
| Grand Sierra Resort, Reno, NV | reproduction prohibited without a prominent attribution |
| Mitchell Airport, Milwaukee, WI | reproduction strictly prohibited |
| DFW Airport, TX, third-party feed | video and radio from third parties, not on site |
| Lake Hood, Anchorage, AK, and StreamTime Live's other cams | duplication strictly prohibited |
| Auburn Airport, CA | operator says the sound is CTAF radio |
| EarthCam: Midway, Chicago, IL; Lincoln Harbor, NJ | terms not retrievable |
| Harbor Island (S), San Diego, CA | tower radio; rebroadcast needs a paid license |
| Edgartown Harbor, MA | use needs a written request |
| Channel Islands Harbor, Oxnard, CA | private property, and not live sound |
| Sag Harbor Bay, NY | -66.4 dB |
| Long Wharf, Sag Harbor, NY | -68.4 dB |
| Bay Street, Sag Harbor, NY | -72.7 dB |
| Holy Childhood steeple, Harbor Springs, MI; Oceanside Harbor, CA; Waukegan Harbor, IL; Redondo Beach Harbor, CA; Grand Marais Harbor, MN; Neenah Harbor, WI; Harbour Town Clubhouse, SC; Blue Harbor Resort, Sheboygan, WI; HarborWalk, Destin, FL; Friday Harbor Airport, WA; Tahoe Donner Marina, CA; Choptank Marina, MD; Dagmar's Marina, WA; Hubbard's Marina, FL; Lake Carroll West Marina, IL; Sutter's Marina, NY; Shipyard Cove Marina, WA; Brown's Wharf, ME | -91.0 dB, digital silence |
| Manitowoc Marina, WI; Norris Lake, TN | no fragments at that time |
| Barefoot Marina | stream not readable |

Set E's streams are mostly silent: of the 27 levels taken here, 18 were digital silence. So were 5 of the first 8 captures.
That is a fact about these cams, and it is why the search ran as deep as it did.

## Addendum 7, 2026-11-05, a later `fdop` build, before it runs on any case here

`9087ec4` is `7e66501` plus 13 commits that change the code, each with its CHANGELOG entry. Most are search, fit,
measurement, calibration and physics fixes. The last two add:
- a curve match, and dropping ADS-B points without GNSS height between GNSS points (`069c971`);
- leaving terrain spike pixels out of the observer's ground height (`9087ec4`).

It is run as a separate tool, `fdop-9087ec4`, beside the pinned `7e66501`, as "Rules for running" requires.
Nothing below changes a pinned result.

- **Runs.** R2, all of sets A, B, C and E as listed for R2 (including `a_ref_opus`, `b2_eagle` and the replacements),
  with the same commands, inputs and `--clock-tol` as the pinned `fdop` jobs, once each. R3 and R1 follow if time
  allows, in that order, and are reported however they come out. A harness fault or a machine failure is the only
  reason to rerun, as before.
- **Scoring.** Unchanged: asserted means high or medium, the same limits, the same claim against each competitor.
  The new build's asserted answers include those raised to medium by the curve match.
- **What is a test of it.** Sets A, B and C are not. The changes were developed on those recordings with R2 inputs, and
  their results on this build were read before this addendum. Set E is, with one disclosure:
  - no development run used set E audio;
  - the pinned build's and the competitors' R2 results on set E's three original cases were read on 2026-11-04: the
    pinned `fdop` declined all three, and `dopplerprint` located one. Why was not examined, and nothing in this build
    was chosen from them;
  - the replacements' runs for the pinned build and the competitors had started, and none of their results was read
    before this addendum was committed;
  - the terrain change came from spike pixels found in development runs. Set E's site list was known by then, and
    nothing about those sites was used.

  The new build's claim is made on set E alone, with the five replacements reported apart from the three original
  cases as well as with them. The A+B+C rows are reported as development rows.
- **Machine.** The bench machine, as for the pinned runs, one run at a time in its own chain. Its results depend on the
  machine's memory, so runs on other machines are not scored.
- **Known before it runs.** The development recordings were also run 42 times with no right answer: 20 at wrong
  dates and 22 at places 90 km or more off. One of them graded **medium**: the osprey cam's 10-31 recording searched on
  11-01. Dropping ADS-B points without GNSS height caused it (README Known issues). This build can therefore assert a
  wrong answer, which is stated here so that no set E result is read without it.
- **Later builds.** Set E's results on this build are read once its R2 runs on set E are all done. A later build is
  tested on set E only if its own addendum, naming its commit, is committed before then. Each such build runs as its
  own tool and is reported. None replaces another's row, and the claim is not moved to whichever build did best.

## Addendum 8, 2026-11-05, a second later `fdop` build, before any later build's set E result is read

`6914ab7` is `9087ec4` plus documents and one commit to the curve match alone: `refine_point` scans the 1 km disc each
start stands for on a 100 m grid, then climbs at each scale until it stops improving, instead of one 5x5 move per scale.
The decoy windows run the same code. No trace-search, grading or gate code changed. It runs as its own tool,
`fdop-6914ab7`, beside the pinned `7e66501` and `fdop-9087ec4`, under every rule of Addendum 7: same runs, inputs,
scoring and machine. Neither earlier build's row changes.

- **When.** Committed before any set E result of `9087ec4` or of this build was read. `9087ec4`'s R2 runs on set E's
  five replacements had finished, unread, and its R3 run on the first of them had started; its R2 runs on the three
  original set E cases had not started.
- **This delays Addendum 7's read.** Addendum 7 reads `9087ec4`'s set E rows once its set E R2 runs are done. They are
  now read once both builds' set E R2 runs are done, so that neither build's addendum is written knowing the other's set
  E result.
- **What is a test of it.** As in Addendum 7: set E is, and sets A, B and C are development rows. Its change was made on
  development recordings with R2 inputs.
- **Gate before this addendum.** The 15 development rows and the 42 runs with no right answer, R2 inputs, on `069c971`
  plus this change, on the controlling Mac. Sets A, B and C: 5 correct and 0 wrong (4 and 0 without it). The osprey
  cam's 11-02 recording goes from low, 14.18 km off, to medium, 0.22 km off, with the curve match standing; the other
  four right answers keep their grade and place, and Sapsucker Woods is still declined. None of the 42 changed grade,
  and the curve match stands on none (largest margin -0.18). `cargo fmt`, `clippy`, the workspace tests and
  `fdop selftest` pass on the committed code. That evaluation binary lacks `9087ec4`'s terrain change, so osprey 11-02 and the
  null with the largest margin were run again on the committed code, and both matched to the hundredth.
- **Known before it runs.** The wrong-date medium recorded in Addendum 7 (the osprey cam's 10-31 recording searched on
  11-01) is unchanged: still medium from the trace search, and the curve match does not stand there (-5.65). Falsifier 2
  therefore fires for this build's development claim as well, and set E is its only test.
- **Both builds are reported.** Each build's row stands as run, and neither is chosen after the fact.

### Reruns so far (continued)

- `fdop-9087ec4`, R2, `c_ref_0909` (a development row): the process got SIGTERM (exit 143) at 05:17:39Z, 224 s in,
  during the curve match, and wrote no `result.json`. Two other agents with processes on that machine checked their own
  commands and sent none then, no Claude or Codex session log on the controlling Mac shows a kill in that window, and
  the sender was not found. The faulted files are kept as `c_ref_0909_fault_term*`, and the case is run once more from
  the job file's own line (`tools/match/rerun_c_ref_0909_9087ec4.sh`, logged in the run directory's `RERUNS.log`).

## Addendum 9, 2026-11-05, set F: a second sealed set, fixed before any search for it

Set E can test only builds whose addendum came before its read (Addenda 7 and 8). Set F is for builds after that. It is
larger, because eight cases say little. It is chosen by rule, captured and sealed before any tool runs on it.

- **Search.** YouTube's live filter, in its own order, on: train camera, railroad live, train station live, airport
  live, airport camera, harbor, marina, pier live, beach live cam, lake live cam, river live cam, bridge live cam.
  Positions 1-40 of every query are read in query order, then positions 41-120 in the same order. YouTube's order
  changes by the hour and by account, so each query's full result list (position, video id, channel, title) is saved to
  a file when it is taken, and the selection follows those files.
- **Qualifying.** As in Addenda 5 and 6: in the US, at a named public site, new to all three projects, no ban on
  recording in the stream's own description, no private residences, and no stream whose operator says its sound is a
  radio or scanner feed. Out, with their operators' other cams: Virtual Railfan, LIVE Trains, Duluth Harbor Cam,
  RailStream, EarthCam (terms not retrievable), and every operator Addendum 6 screened out for its terms. Every stream
  already considered for set B or E stays out, whatever its level was then.
- **Sound.** 60 s of the stream's audio from 2026-11-05T17:00:00Z, fetched by the capture tool, as in Addendum 6. It
  qualifies if its mean level, ffmpeg's `volumedetect` `mean_volume` over that file, is above -60 dBFS. That number is
  the only thing taken from the audio: nobody listens to it, no tool runs on it, and no spectrogram, ADS-B or aircraft
  data is consulted.
- **Selection.** The first ten to qualify, in search order. They are listed in a later addendum before capture, with
  their case ids and every stream screened and why it was not taken.
- **Capture.** Two hours of DVR audio from 2026-11-05T17:00Z by the same tool. The outage rule is unchanged: the audio
  is cut at the first outage and a case is kept if its clock holds for at least 30 min. A stream whose DVR does not
  reach 17:00Z, or whose clock holds for less, is listed as not captured, and the next to qualify takes its place. A
  capture whose whole-file mean and peak are digital silence is void for every tool, as in Addendum 5.
- **Truth and limits.** As before: an OpenStreetMap feature or the operator's published address, 1 km for a single
  building, station or marina, 5 km where the operator gives no building. Each is fixed in the listing addendum, before
  capture.
- **Inputs.** Made by the rule in the R2 section, so none is drawn by hand. The true clock is the capture's first
  fragment's First-Frame stamp, as for set E. R2's start is that clock rounded down to the minute, with ±2 min. Its
  circle has a 25 km radius, and its centre is 10 km from the truth, on the bearing given by the first four bytes of
  `sha256(case id)` modulo 360. R3 and R1 are made as they were for set E.
- **Runs.** R2, then R3 and R1 as time allows and in that order, for every tool and variant that ran on set E (`dp`,
  `dp48`, `dpdem`, `dopplertrack`, pinned `fdop` `7e66501`), once 2026-11-05's ADS-B archive is out. The same goes for
  each later `fdop` build registered below.
- **Registration.** `fdop-9087ec4` and `fdop-6914ab7` are registered now. Any other `fdop` build is registered only by
  an addendum naming its sha, committed before registration closes. Registration closes at 2026-11-06T03:00:00Z, or when
  the first set F run of any tool starts, whichever is first. No build is added after that.
- **Sealed.** No set F result of any tool is read, and no set F recording is diagnosed, until every registered build's
  R2 runs on set F are done. Then all are read at once and reported, losses included.
- **What it tests, and how it is reported.** The claim stated before Set D, on set F alone, holding against each of
  `dp`, `dp48` and `dpdem` as Addendum 3 requires, and against `dopplertrack`, for each registered build separately. The
  report gives the number of `fdop` builds registered and scores every one. The best of them is not presented as
  `fdop`'s result: each build passes or fails the claim by its own row.
## Addendum 10, 2026-11-05, a harness fault in the chains' watchdog, its fix and the reruns

Written before any set E result of `9087ec4` or `6914ab7` was read.

### The fault

Each chain had a watchdog meant to stop that chain if the boot disk or swap got tight. It stopped processes with `pkill
-f` on a pattern, and the patterns matched other chains' processes too: every process of a tool's binary
(`fdop-9087ec4`, `fdop-7e66501`, `venv-dp/bin`), and job names that are substrings of other job names (`r3_fdop.sh` is
part of `e2_r3_fdop.sh`). Swap use crossed the line twice, nine watchdogs fired, and four of them stopped runs in other
chains:

| Time (UTC) | Watchdog | What it stopped |
|---|---|---|
| 05:17:12 | `e2_r2_dt` | its own chain: `dopplertrack`'s R2 on `e_harbourgate` (traces step) |
| 05:17:20-35 | `capx_t0`-`t2` (an old experiment) | nothing: their pattern names only that experiment's binary |
| 05:17:39 | `e2_r1_fdop_9087ec4` | `9087ec4`'s R2 `c_ref_0909`, and its R3 chain on set E's replacements (`e_tehachapi`) |
| 05:17:39 | `sd_fdop` | the pinned `fdop` R3 chain (`c_osprey_1031`) |
| 06:21:29 | `sd_dpdem` | the `dopplerprint` R1 chain (`b3_sapsucker`) |
| 06:22:10-12 | `e2_r1_fdop_6914ab7`, `r1_fdop_6914ab7` | `6914ab7`'s R2 chains (`a_ref`, `a_ref_opus`, `e_kelso`) and its R3 chain on the replacements (`e_tehachapi`) |

A stopped job script did not stop its chain: the chain went on to its next script, so the rest of a stopped regime never
ran. Addendum 8's note on `c_ref_0909` said the sender of its SIGTERM was not found. It was the `e2_r1_fdop_9087ec4`
watchdog, at the same second.

### The fix

The watchdog now gets its chain's loop process and stops that loop and every process under it, and nothing else
(`lib.sh` and `tools/start_chains.sh`; the old copies are kept as `*.pre_guardfix`). A test on dummy processes
(`tools/match/guardfix/test_guard.sh`) shows a firing guard stops its loop, the loop does not go on to its next script,
and a process the old pattern would have matched keeps running. The criterion (5 GB of boot disk, 6 GB of swap growth)
is unchanged. The watchdog left running on `9087ec4`'s R2 chain still had the old pattern, so it was stopped at
06:35:51Z; that chain ran on without a watchdog.

### Reruns

Every stopped run's files are kept under `<name>_fault_guard` (or `_fault_term` for `c_ref_0909`), and the run is done
once more from its job file's own line. The R2 chains run first, in regime `fix1` from 06:41:14Z: `6914ab7`'s R2 on sets
A, B, C and E, its R2 and then R3 on the replacements, `dopplertrack`'s R2 on the replacements from `e_harbourgate`'s
traces step, and `9087ec4`'s R3 on the replacements. The pinned `fdop` R3 chain and `dopplerprint`'s R1 chain restart
from the stopped case once the R2 chains are done. No run that finished is repeated.

### What was seen while finding it

Diagnosing this meant reading exit codes in set E's run directories. `fdop` exits 1 when it gives no location. So one
set E result was seen early: `fdop-6914ab7`'s R2 run on `e_tehachapi` exited 1, no location. No other set E exit code,
output or result was read. Nothing about either build or the benchmark was changed after it.
## Addendum 11, 2026-11-05, register `fdop-ce02222` for set F; the guards stop the chains again

Written before any set F audio was captured or any tool ran on it, and before any set E result of a later build was read
(the one exit code disclosed in Addendum 10 aside).

### The build

`ce02222` is `6914ab7`'s code with one change: a trace answer graded medium becomes low when the curve match ran with at
least 40 decoy clocks and stands nowhere. It was written after `nd_o31_1101` fired falsifier 2 for `9087ec4` and
`6914ab7` (Addendum 8). Its reason on its own terms is the decoy test: with a wrong date or place, the real window is
one of at least 41 interchangeable clocks, so it stands about once in 41 at most. Set F's R2 inputs have the right date,
so there the rule can only cost it: a right answer that the trace search grades medium becomes low where the curve match
judged and does not stand.

### How it was chosen

There were two candidates: `ce02222`, and `32b7834`, which is `ce02222` plus build C's front end with a gate of its own.
Build C's front end measures down the detection list from the silence line until 24 events pass the quality gates. The
gate lowers a medium that rests on an event the old line would not have measured, unless a curve match with enough decoy
clocks judged it. Before any run of either, the rule was sent to `projects-bf` at about 06:58Z: register at most one of
them, the one with the most correct asserted development answers among those that assert none of the 42 no-right-answer
runs at medium or high, and the smaller change on a tie.

Both assert none of the 42, and both give five correct asserted development answers and no wrong one. `ce02222`, the
smaller change, is registered. `32b7834` is not, whatever it does later.

### What they did on runs they were developed on

All in-sample, with R2 inputs.

- `ce02222`'s rows are its rule applied to `6914ab7`'s saved runs (`disc_dev`, `disc_nulls`). The rule reads only the
  grade, the curve verdict and the decoy count in `result.json`, so this is exact.
- `32b7834`'s rows are runs of the binary `6dd7b74` (sha256 `94bff95e`) on an Apple M5 Ultra. `32b7834` differs from it
  in one test assertion only.

| Row | `6914ab7` | `ce02222` | `32b7834` |
|---|---|---|---|
| reference recording | high | high | high, 0.25 km |
| osprey cam 10-31 | high | high | high, 0.09 km |
| osprey cam 11-01 | high | high | high, 0.11 km |
| osprey cam 11-02 | medium (curve match's answer) | medium (curve match's answer) | high, 0.22 km |
| Duke Farms | high | high | high, 0.96 km |
| sapsucker cam | no location | no location | no location |
| 42 no-right-answer runs asserted | 1 (`nd_o31_1101`, medium) | 0 | 0 |

`32b7834` grades `nd_o31_1102` low by the curve-match rule, where build C graded it medium: the curve match judged it
and does not stand (margin -5.17).

The reference replays have no start, so the curve match does not run on them. `ce02222` keeps `6914ab7`'s grades there.
On `32b7834`, every grade is unchanged from `b1`. It has three right answers, each high: 270, 264 and 263 m from the
site. Four runs have wrong answers, each low: the reference recording on two wrong dates, and the Ithaca feeder
recording on its own date and on a wrong one.

### What set F can and cannot show for it

Set F scores `ce02222` by its own row, as Addendum 9 requires. It cannot show whether the rule lowers wrong answers on
recordings it has not seen, since R2 has the right date. Its R3 and R1 inputs can.

### The guards stopped the chains again

At 07:57:22Z the swap had grown to 13,988 MB from a baseline of 7,979, and the boot disk had 5 GB free. The guards, as
fixed in Addendum 10, each stopped only their own chain, and so all four regime `fix1` chains stopped. The interrupted
steps are kept as `<step>_fault_guard2`:

- `e2_r2_dt`'s `e_harbourgate_detect`
- `e_tehachapi` in both `e2_r3_fdop_6914ab7` and `e2_r3_fdop_9087ec4`
- `r2_fdop_6914ab7`'s `b3_duke`

Two steps that Addendum 10's guard stopped before its fix are kept as `<step>_fault_guard`: `r3_fdop`'s `c_osprey_1031`
and `r1_dp`'s `b3_sapsucker`.

Regime `fix3` started at 16:06:58Z, with three chains at once instead of four:

- `6914ab7`'s R2, running its eight set E steps first, then the full job, which skips them. The first step is a file of
  the job's own lines.
- `dopplertrack`'s set E R2.
- The set E R3 of both builds, then the pinned `fdop` R3 and `dopplerprint` R1.

Each step runs on its own, so order changes no result. A second watchdog follows chain 3 to its last script.

## Addendum 12, 2026-11-05, a fourth later `fdop` build, `3164272`, for set F

`3164272` is `6914ab7` plus two commits that change behaviour, and documents:

- `ce02222`, in `src/pipeline.rs`: a medium answer becomes low when the curve match ran with at least 40 decoy windows
  and stands nowhere, including when it found no place. High answers, the calls where the curve match stands, and runs
  where it did not run are unchanged.
- `3164272`, in `crates/fdop-core/src/curvematch.rs`: the curve match scores every place with the range heard half
  the time at 1, 2 and 4 km and keeps the best, in the real window and in every decoy window alike. Before, it was
  fixed at 2 km.

It is registered for set F as `fdop-3164272`, under every rule of Addendum 9, beside `fdop-9087ec4`,
`fdop-6914ab7` and Addendum 11's `fdop-ce02222`. Set E's read is not delayed: set E tests only the builds of
Addenda 7 and 8.

- **When.** Committed at 16:08Z, before set F's levels are taken (17:02Z) and before any set F run of any tool.
  No set E or set F result was read by the agent that made this build.
- **What is a test of it.** Set F. Every recording below is a development recording, run with R2 inputs.
- **The rule it had to pass.** It was written at about 06:30Z, before the evaluation started (06:41Z):
  - on the 42 runs with no right answer, no grade reaches medium or high that was not medium or high before, and the
    curve match stands on none;
  - on the 15 development rows, no right answer is lost and no wrong one is asserted;
  - and the curve-match margin rises on at least 3 of the 5 right development rows, or the curve match stands at the
    reference site.
- **Outcome**, full `locate` runs on the controlling Mac with the profile on `50708aa`'s code.
  - **Right rows.** 5 correct and 0 wrong. High: the reference recording at 250 m, the osprey cam at 90 m and 110 m,
    and Duke Farms. Medium: the osprey cam's 11-02 at 240 m. Sapsucker Woods is still declined. Every grade is
    unchanged, and so is every distance except the osprey cam's 11-02 (220 m before).
  - **Margins.** The margin over the best decoy rose on the osprey cam's three days, from +9.23, +13.29 and +1.14 to
    +22.24, +21.36 and +5.29. It is unchanged at Duke Farms. On the reference recording it went from -3.95 to -4.32.
  - **No-answer runs.** None of the 42 stands: the largest margin is -0.47 (-0.18 on `6914ab7`), with 20 rising and 18
    falling. The only medium among them is the wrong-date `nd_o31_1101`, which was already medium on `6914ab7`.
  - **The gate.** Its effect is read exactly from each run's `result.json`. It lowers `nd_o31_1101` to low and keeps
    all five right answers.
  - **The committed binary.** Rerun on `nd_o31_1101` (low, curve -5.46) and on the osprey cam's 11-02 (medium, curve
    +5.29); both matched the evaluation.
  - **Gates.** `cargo fmt`, `clippy` and `fdop selftest` (2.4 m) pass on the committed tree; the workspace tests give
    214 passed, 0 failed and 2 ignored.
- **A set the rule first named and then dropped.** The rule as first written also required no wrong answer on four
  railcam recordings: three Virtual Railfan streams and one RailStream stream. Their operators' terms prohibit
  recording, which is why the first addendum and Addendum 2 dropped them from set B3. `6914ab7` was run on them once,
  at 06:26Z, before this was pointed out; all four declined. At 06:50Z, before any result of this build existed, they
  were removed from the rule and from all use. This build never ran on them, and nothing else stands in for them, so
  no development data looks like set F's cams.
- **Known before it runs.** Set F's R2 inputs have the right date, so the gate's benefit on wrong dates cannot show
  there. Set F shows mostly its cost: a right medium from the trace search, lowered where the curve match does not
  stand. No development row has one. Its benefit could show on set F only as a medium at a wrong place, and none of
  the 22 development runs at wrong places had one. The profile was suggested by an offline rescoring of stored
  places and chosen on development data, so set F is its only test.
- **Every build is reported.** This makes four registered `fdop` builds: `9087ec4`, `6914ab7`, `ce02222` and
  `3164272`. Each passes or fails the claim by its own row, and none is chosen after the fact.
## Addendum 13, 2026-11-05, set F's ten cases, before their capture

Listed before capture, as Addendum 9 requires. No tool has run on these streams, nobody has listened to them, and no
spectrogram, ADS-B or aircraft data was consulted. The only thing taken from their audio is each 60 s level.

| Case | Site | Video | Mean level | Limit |
|---|---|---|---|---|
| `f_kingman` | Kingman Railroad Museum, Kingman, AZ (the host of a BNSF Seligman Subdivision railcam) | `h8-J3JGU7g4` | -20.8 dB | 1 km |
| `f_chehalis` | Lewis County Historical Museum, Chehalis, WA | `YV8hzUJCp88` | -34.1 dB | 1 km |
| `f_santamonica` | Santa Monica Beach and Pier, Santa Monica, CA (no camera building given) | `v97JpT3ZA0w` | -56.3 dB | 5 km |
| `f_easthampton` | Main Beach, East Hampton, NY | `C92aPUuBWA4` | -48.3 dB | 5 km |
| `f_ftlauderdale` | Elbo Room, Fort Lauderdale, FL | `bM0GplM_vPY` | -21.0 dB | 1 km |
| `f_sunnyisles` | Sunny Isles Beach, FL (the city; no building given) | `bi7B4EmyHHs` | -41.9 dB | 5 km |
| `f_brooksfalls` | Brooks Falls, Katmai National Park, AK | `J7ZrIDvqlic` | -31.9 dB | 5 km |
| `f_charlevoix` | the Bridge Street drawbridge, Charlevoix, MI | `yWIHgMW2yVQ` | -14.2 dB | 5 km |
| `f_dossin` | Dossin Great Lakes Museum, Belle Isle, Detroit, MI | `L73DrCZHJ8A` | -31.0 dB | 1 km |
| `f_bergenfield` | CP10 on the CSX River Line, Bergenfield, NJ (a control point in town) | `yDSZcjG4x0Y` | -16.9 dB | 5 km |

If a capture fails the outage rule, the first replacement is `f_porthuron`: the St. Clair River waterfront camera in
Port Huron, MI (`SrL74LPoXF8`, -58.8 dB, 5 km). Any further replacement is found by taking levels further down the same
order.

**Truth.** Each truth is an OpenStreetMap feature:

- the museum buildings, the pier, the bar, the waterfall and the movable bridge;
- for East Hampton, the building at the operator's published address;
- for Sunny Isles Beach, Bergenfield and Port Huron, the town.

The coordinates are kept on the bench disk and are not committed.

**How the rule was applied, including where this goes beyond Addendum 9's wording:**

- **Depth.** The first description screen covered search-order rows 1-272 and passed 40 streams. A second reading left
  36, and 7 of those qualified at the level step. After those levels were read, the screen went on down the same saved
  order, rows 273-362, under the same rules, and their passes were leveled in order.
- **A qualifier taken out for its truth.** Gasparilla Marina, Placida, FL (`h3T6vjwktHg`, -35.8 dB) qualified but is
  out. OpenStreetMap has no feature for it. Its published address, on a road through two communities, geocodes to points
  about 5 km apart (OpenStreetMap's road segments and the Census 2020 address ranges), so its truth cannot be fixed
  within its 1 km limit. This was decided after its level and before any capture.
- **The same-site rule.** The first screen left out a second stream at a site it had already passed (12 streams). It was
  applied to rows 273-362 only after their levels had been taken. It puts out the riffles camera 100 yards below Brooks
  Falls (`z7_GhJeFxQI`), which had qualified at -32.1 dB, and the Riviera Motel's second stream, never leveled. It also
  puts out Ponquogue Beach Pavilion, which had failed its level.
- **Sound.** As with the radio feeds, a stream is out when its operator says the sound is a pre-recorded loop, or that
  there is none. Its sound is not of the place.
- **Site.** A camera that names only what it looks at is out when that is a lake, river or bridge several kilometres
  long. That does not say where the camera is. A small feature it looks at, a town drawbridge or a waterfall, is kept
  with the 5 km limit.
- **Private.** A camera run by an individual, looking out over a park or a river with no public site hosting it, is
  taken as a likely private home.

**Screened and not taken.** 63 of the 362 rows read passed the description screen. The other 299:

- 91 are outside the US.
- 90 are from excluded operators.
- 40 were considered for set B or E.
- 19 are not one fixed live camera.
- 17 name a site that does not fix where the camera is, or no particular place.
- 14 are a second stream at a site already passed.
- 12 carry radio or no sound of the place.
- 11 have a ban or use restriction.
- 4 are likely private homes.
- 1 does not say which side of the border it is on.

A second reading of the first 40 passes put out four more: a gated community's marina, a likely private home, and two
streams under use restrictions. Every row's decision and reason is saved with the search files. The streams that reached
the level step and were not taken:

| Stream | Why not |
|---|---|
| Tyrone History Museum, PA | the capture tool cannot time it: its fragments carry no wall clock |
| Sunset Beach Pier, NC | the fetch failed (HTTP 404) |
| Napili Kai Beach Resort, HI | an outage inside the 60 s; -73.1 dB |
| Ponquogue Bridge, Hampton Bays, NY | -69.5 dB |
| Ponquogue Beach Pavilion, Hampton Bays, NY | -64.0 dB, and the same site as Ponquogue Bridge |
| Tiana Beach, Hampton Bays, NY | -61.0 dB |
| Antelope Point, Lake Powell, AZ | -87.3 dB |
| Embassy Suites, Miami International Airport, FL; Fort Myers Beach, FL; Wailea Beach Resort, HI | -85.5 dB |
| Mendenhall Glacier, Juneau, AK | -90.3 dB |
| Hobby Airport, Houston, TX; Rockport Harbor, ME; Boot Key Harbor, FL; Robbie's Marina, FL; Galveston Yacht Basin, TX; Key West Bight, FL; Alton Marina, IL; Surfside Beach Pier, SC; Jacksonville Beach Pier, FL; Navarre Beach Pier, FL; Dania Beach Pier, FL; Flagler Beach Pier, FL; Fairhope Pier, AL; Huntington Beach Pier, CA; Redondo Beach Pier, CA; Pavilion Park, Myrtle Beach, SC; Chatham Fish Pier, MA; Murdoch's Bathhouse, Galveston, TX; El Porto, Manhattan Beach, CA; Southernmost Beach Resort, Key West, FL; Mirror Lake, Lake Placid, NY; Round Lake, Charlevoix, MI; Hart Hall, SUNY Oswego, NY; South Beach, South Haven, MI; Lake Nighthorse, Durango, CO; Coldwater Science and Learning Center, Mount St. Helens, WA; Back Door Saloon, Houghton Lake, MI; The Cove, Leland, MI; The Edge Hotel, Clearwater Beach, FL; Alexandria Bay, NY | -91.0 dB, digital silence |
| Gasparilla Marina, Placida, FL | truth not fixable within its limit (above) |
| Brooks River riffles, Katmai National Park, AK | the same site as Brooks Falls (above) |

As in set E, most of these cams are silent: of the 52 levels read, 39 were below -60 dB, 30 of them digital silence.

**Inputs and capture.** Inputs are made by Addendum 9's rule once the clocks are known. The capture of the ten listed
takes two hours of DVR audio from 2026-11-05T17:00:00Z and starts at 19:01Z, after that window ends. The second network
user on this site holds its downloads from 19:00Z until the capture is done.

### Bench notes since Addendum 11

- At 16:14Z a build of `fdop-ce02222` on the bench machine took the swap to 9.7 GB. Memory was 45% free, so most of that
  swap was idle pages. The watchdogs of regime `fix3`, with a 3.7 GB baseline, were within 0.1 GB of stopping all three
  of its chains. The build was stopped. At 16:17Z the same three watchdogs were restarted with a fresh baseline; their
  disk floor is unchanged. No chain stopped, and the restart is in `RERUNS_guard.log`.
- The registered binaries were built from `git archive` of their commits on the bench machine, and both pass `fdop
  selftest` (2.4 m): - `fdop-ce02222`: sha256 `d93bfb38cbda4703d9154c93279b3dd3854a24795016e1bb00d975f01ca969f2` -
  `fdop-3164272`: sha256 `c21322b8045751d89491bbde653c81ceacaf75ed039342536dd4c55b410a7cb9`

## Addendum 14, 2026-11-05, set E's R2 read; a fifth later `fdop` build, `32d4e1a`, for set F

### Set E, R2

The seal condition of Addenda 7 and 8 was met at about 18:40Z: every set E R2 run of every tool and registered build
had finished (`fdop-9087ec4` and `fdop-6914ab7` 8 of 8, `dopplertrack` 8 of 8). All were then read at once with
`score.py jobs/cases.json runs r2`; the output is kept on the benchmark machine as `runs/SCORE_r2_setE_20261105T1842Z.txt`.
Eight cases count; the five void ones (Addendum 5) are out for every tool.

| Tool | Correct asserted | Wrong asserted | Rows |
|---|---|---|---|
| `dopplerprint` (`dp`) | 1 | 0 | Olympia, 63 m, high |
| `dp48` | 1 | 0 | Olympia, 63 m, high |
| `dpdem` | 1 | **1** | Olympia, 82 m, high; Tehachapi, **14 km, medium** (5 km limit) |
| `dopplertrack` | 0 | 0 | an error on all eight (below) |
| `fdop` `7e66501` (pinned) | 0 | 0 | |
| `fdop-9087ec4` | 1 | 0 | Olympia, 66 m, medium |
| `fdop-6914ab7` | 1 | **1** | Olympia, 66 m, medium; Harbourgate, **7.97 km, medium** (1 km limit) |

- **The claim.** No `fdop` build meets it on set E. `fdop-9087ec4` ties `dp` and `dp48`, one correct and no wrong
  each, where the claim needs it strictly better on one count. It beats `dpdem`, which has a wrong medium, and
  `dopplertrack`, which asserted nothing.
- **The falsifiers** (WS question of record). Falsifier 3, a sealed set with fewer correct asserted answers than a
  rival or any wrong one, fires on set E for pinned `fdop` (0 correct, against 1) and for `fdop-6914ab7` (a wrong
  medium). It does not fire on set E for `fdop-9087ec4`. That does not clear `fdop-9087ec4`: falsifier 2 already fired
  for its development claim and for `fdop-6914ab7`'s, on the wrong-date `nd_o31_1101` (Addenda 8 and 11). Falsifier 1
  fires for pinned `fdop`, which has 1 correct answer on A+B+C.
- **`fdop-6914ab7`'s wrong medium.** At `e_harbourgate` the trace search detected nothing in two hours. The curve
  match stood +1.86 nats over the best of 48 decoy clocks, on one heard pass of one aircraft, at a clock 110 s from the
  true one, and the answer took its place. That pass was a chance fit, read from the ADS-B track: the aircraft was
  287 m from the place at the fitted clock and 9.76 km from the truth at the true clock, so the site did not hear it.
  It is not a slide along the track either: a clock 110 s early needs a place behind the truth along the heading, by
  the 3.2 km flown in that time, and this one is 7.9 km ahead of it. `fdop-9087ec4` gave no location there (its curve
  match did not stand, −0.58). `ce02222` has `6914ab7`'s curve match, and so this stand. `3164272`'s heard-range
  profile changes the curve match: on `32d4e1a`, which has it, the curve match does not stand there (−0.17, no pass
  heard; a local diagnostic, in-sample). `ce02222` and `3164272` both keep the path that produced the wrong medium.
  On set F's R2 either can assert a curve-match place on one heard pass.
- **`dopplertrack`'s errors.** All eight are its own refine step failing (`IndexError`) on a candidate table that its
  search wrote with a header and no rows: it found no candidate. Its inputs were the same R2 traces as before, 1.3 to
  5.2 MB each, fetched after 04:19Z on 11-05. It is a failure in its own code, not in the harness, and it is scored as
  the error it is.
- **What set E tests.** Little, for `fdop`. Its detector found nothing at all on four of the eight cases
  (`e_harbourgate`, `e_lakeshore`, `e_sagharbor`, `e_porthenry`) and 9 candidates each on two (`e_kelso`,
  `e_fortmadison`), none usable. Only `e_olympia` (4 events used) and `e_tehachapi` (10) gave the search anything to
  work with (`fdop-9087ec4`'s R2 runs). `dopplerprint` asserted only `e_olympia` as well. It declined five of the others
  and gave `e_kelso` and `e_harbourgate` low. One right answer each is a count on about two cases, not a measure of
  accuracy.
- **Development rows on the benchmark machine** (A+B+C, 12 cases, in-sample): `fdop-9087ec4` 4 correct and 0 wrong,
  meeting the rule against all four rivals there. Pinned `fdop` gives 1 and 0. `fdop-6914ab7`'s A+B+C runs are still
  running.

R3 and R1 of set E are still running and are read as they finish.

### The build

`32d4e1a` is `c6e5800`'s code, which is `3164272` plus the measurement change (`b0a9424`), with one more change in
`src/pipeline.rs`: a curve match that stands on fewer than two heard passes no longer raises an answer. It does not
adopt its own place, and it does not confirm a low trace answer. The answer stays low, or there is none. Asserted trace
answers are settled as before. Heard passes are counted, not aircraft, so one aircraft heard twice counts as two.
One pass's Doppler curve fixes when and how close the aircraft passed. The curve match searches a window of clocks, so
place and clock trade along the track at the flight's speed. The trace search asks two core events of a medium for the
same reason. `e_olympia` shows the trade on a real recording: six of its eight best places hear only its first flight,
each where that flight was at that place's clock, up to 147 s and 10.8 km off, and its second flight separates them. A
metamorphic test on exact curve scores shows the trade, shows a crossing flight breaking it, and shows a flight on a
parallel track, the same way at the same speed, leaving it, so the rule is needed, not sufficient. `projects-bf` found
the mechanism and proposed the test. `e_harbourgate` motivated the rule by its grade, not by this mechanism: its pass
was a chance fit (above). One pass is too little for either reason.

It is registered for set F as `fdop-32d4e1a`, under every rule of Addendum 9, beside `fdop-9087ec4`, `fdop-6914ab7`,
`fdop-ce02222` and `fdop-3164272`. Its binary is built on the benchmark machine from `32d4e1a`'s tree, sha256
`a31d1a57e2335f0e0abc3f7fdb509dd3f0015c802fa6e610f96ef4c99b7200fa`. The evaluation below used binary `bb8bdce` (sha256
`cf86b51a`), an unpushed commit object. Its source differs from `32d4e1a` in the Markdown files, in tests, in comments,
in the wording of this rule's gate and verdict text, and in moving the no-location answer into a function. `32d4e1a`'s
own binary gives the same rows: `b1_osprey_1031`, `np_o31_p2` and `e_harbourgate` equal `bb8bdce`'s in every
`result.json` field but run time and paths. The first run of the first two took the -30 m geoid default in an NGS
outage (below) and differed for that reason; they were run again for this comparison, and both runs are kept
(`x_32d4e1a`, `x2_32d4e1a`). This is a software comparison on in-sample rows, not a graded result.

`b3828e6`, `5ab7f48` and `e13f2d2` reached `main` while `32d4e1a` was being checked. The first changes the ADS-B cache
lookup, the second the margin and the third the curve match: exact scoring stays inside each clock window, a sparse
crossing pass survives the track filter, and `verdict` needs two heard passes that keep positive value, so on `main` a
curve match does not stand on one pass at all. Neither of the last two messages claims a test pass or a replay.
`32d4e1a` contains none of the three, and none is registered for set F. It is merged into `main`, not rebased, so its
sha is public and its tree is the one the evaluation below read. The merge keeps `e13f2d2`'s form of the rule and drops
the one in `src/pipeline.rs`, which could no longer be reached, so `main`'s code is not `fdop-32d4e1a`'s. On `main` a
one-pass curve match can no longer veto an asserted answer or keep a medium, and `fdop-32d4e1a`'s still can (CHANGELOG).

- **When.** Written after set E's R2 was read, and because of it: `e_harbourgate` is the case. Set E is therefore not a
  test of this build. Set F is. Committed before registration closes (2026-11-06T03:00:00Z) and before any set F run
  of any tool. No set F result exists, and no set F audio has been listened to or run through any tool.
- **The rule it had to pass**, logged at 18:54Z, before its binary existed. It said "heard flights"; the code counts heard
  passes, as it did then, and the words here follow the code:
  - on the 42 runs with no right answer (R2 inputs, local), no grade is medium or high, and every grade equals
    `edf1a09`'s (runs `nm`);
  - on the 15 development rows, no right answer is lost and no wrong one is asserted, and every grade equals
    `edf1a09`'s (runs `fm`);
  - its effect on every recorded `result.json` is read exactly. The rule reads only the trace grade, the curve
    verdict and its heard count;
  - `cargo fmt`, `clippy`, the workspace tests and `fdop selftest` pass.
- **Outcome.** All four held, on `bb8bdce`. Of the 42 runs with no right answer, none is medium or high and every grade
  equals `nm`'s: 33 in `nh` (26 low, 7 no location, no curve match standing) and 9 among the development rows. The 15
  development rows are identical to `fm`'s in grade, place, margin and curve call: 5 right, all high, none wrong. The
  census finds one stand on fewer than two heard passes in the whole record, `e_harbourgate` under `6914ab7`, and none
  on this build, where that row gives no location (`eh`). Two `nh` rows differ in numbers and not in grade, and both
  are the geoid (below): `np_o02_p3` by 5 cm and `np_o02_p1`'s curve margin from -1.35 to -1.287. On the final source,
  `fmt` and `clippy -D warnings` pass and the workspace tests give 220 passed, 0 failed, 2 ignored. `selftest` passes
  on `bb8bdce` (2.4 m) and on `32d4e1a` (2.4 m). `r_nassau` repeats `r_nassau_gc` exactly. The rule carried the other
  six reference, wrong-date and feeder runs over, since they give no start and the curve match cannot run; replays of
  them on 256 GiB had not finished at this commit and are not read here.
- **Known before it runs.** On set F's R2 this rule can only lower answers. It costs a right answer where the curve
  match alone, on one heard pass, would have placed a recording within its limit. No development row has one. It
  helps only where such a place is wrong, as at `e_harbourgate`, and on this build's own curve match it does not
  stand there, so no recorded row changes. The curve match does not run on R3, where CONUS needs about 360 million
  cells against a limit of 200,000, or on R1, which gives no start. There this build answers as `c6e5800`'s code does.
  Set F's R2 is also the first sealed test of the measurement change (`b0a9424`), which no other registered build has.
- **Every build is reported.** This makes five registered `fdop` builds: `9087ec4`, `6914ab7`, `ce02222`, `3164272`
  and `32d4e1a`. Each passes or fails the claim by its own row, and none is chosen after the fact.

### Bench notes since Addendum 13

- `score.py` has a set F group (Addendum 9's claim on set F alone, void cases out), added before any set F result
  existed; a dry run on empty run dirs showed every row pending.
- Set F's capture started at 19:01:23Z, after the 17:00-19:00Z window closed. Four streams had an outage. Addendum 9's
  outage rule cuts each at the first one and keeps it if its clock holds for 30 min: `f_kingman` holds 75.9 min,
  `f_brooksfalls` 92.1 and `f_bergenfield` 87.1, and all three are kept.
- **`f_sunnyisles` is out.** Its clock holds 7.2 min, so it counts as not captured. By Addendum 13, `f_porthuron` takes
  its place, captured from the same DVR window at 20:21:47Z to 20:24:40Z, before any set F run. It had an outage too and
  its clock holds 74.5 min. Its whole-file mean is -49.6 dB and its peak -13.4 dB, so it is not digital silence. Set F
  stays at ten cases.
- **The geoid.** The NGS geoid API can answer a request with the value of another made at the same moment, and it echoes
  the coordinates it was asked for, so a response cannot show it. In a check of four simultaneous requests on 11-05, a
  point in Georgia came back with a Pennsylvania point's height and error. `fdop` asks NGS once per run and does not
  cache the answer. Among the benchmark's 125 `fdop` rows, on 60 pairs of case and region, no two rows of a pair
  differ, so no crossing shows there, though a pair with one row cannot show one. The local evaluation below ran
  several lanes at once and has two: see the outcome. A lookup can also hang: around 21:10Z on 11-05 two local rows
  waited out their timeouts and took the -30 m default, with its warning. On `b1_osprey_1031` that 0.58 m kept the
  grade and took the radius from 349 m to 575 m. Set F runs as registered, and each run's log keeps the value it used.
- The builds and the workspace tests of `bb8bdce` and `32d4e1a` ran on the benchmark machine with direct cargo, beside the
  chains. Each cargo step waited for free space on the boot disk, 9 GB at first and 5.7 GB by the end as swap took the
  difference, and each stopped itself above the chains' guards, under 8 GB at first and 5.3 GB by the end, or over
  13,500 MB of swap. Two runs of the checks stopped themselves, and what was left of them was run again on the same
  tree. The chains' guards stop at 5 GB and at 6,000 MB of swap over their baseline, and neither fired on these.
- The hypothesis budget scales with the machine's RAM and the comparison budgets do not, so a run on a larger machine
  is not a repeat of a run here. Every set F run is on this 32 GiB machine. `AGENTS.md` now says so beside the reference
  table. Of the local replays on 256 GiB, only `r_nassau` was read against this machine's row: it fits the budget on both
  machines and came out identical.
- The bench's USB volume dropped off this machine at 22:10:27Z on 11-05 and came back about 22:24Z, which revoked every
  file open on it. The job shells were held for about 4 minutes. Five rows were in flight, and none had written a
  result. `r2_fdop_6914ab7/c_osprey_1031` panicked with rc 101 at its next log line. `e2_r3_fdop_6914ab7/e_tehachapi`,
  `r1_fdop/b1_osprey_1031`, `r3_fdop_9087ec4/b1_osprey_1101` and `r3_fdop_6914ab7/b1_osprey_1101` were certain to do the
  same, and were stopped at 22:28:54Z. All five are kept as `*_fault_usb` and run again under the same commands, as an
  infrastructure fault, in the chains of regime `fix4`, whose guards keep the 9,618 MB swap baseline of 16:17Z through
  a new optional argument to `watchdog`. The chains went on to their next scripts early, so R1 of `9087ec4` and `6914ab7`
  started before their R3 finished, and no row reads another. Afterwards every `result.json` on the volume parsed, and
  no row had ended between 21:50Z and the drop. Set F's capture folders, clock files, jobs, `cases.json` and
  `truth_f.json` were all present, with folder sizes in line with their clock lengths. No set F audio was opened. The
  clock files hold no sizes or hashes, so that is not a byte-level check.

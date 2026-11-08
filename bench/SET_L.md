# Set L: radio aporee field recordings with a logged place and start time, in large US metros

Fixed on 2026-11-08, before its search, while set K runs. It asks set J's and set K's question with a third source,
and uses `PROTOCOL.md`'s R2 inputs and scoring and set K's rules except where this file says otherwise. Later changes
go in a dated section at the end; nothing here is edited to suit a result.

## Why

Set K's screens kept 8 uploads, 6 in New York and 2 in Los Angeles, from two recordists. That cannot say much about
US population centres in general. radio aporee ::: maps is a public sound map of field recordings. Its recordings are
mirrored on the Internet Archive (collection `radio-aporee-maps`), each with the recordist's map point (latitude and
longitude), the recording's local date and time to the minute, its IANA time zone, its licence and the original audio
file. So each recording's metadata states what a test needs, and the probe below counted 198 recordists in the
Americas.

**Known before this file was written:**
- the results of sets A to I; set J's and set K's screens and set K's capture levels; sets J and K are unread, apart
  from set J's two null exit codes noted in `SET_K.md`;
- a sizing probe of three Internet Archive calls. Two were searches of `radio-aporee-maps`, counted only: 1,838 items
  dated 2023-01-01 to 2026-11-06 in an `America/*` zone, 748 of them in a contiguous-US zone; of those 748, 145 have
  an item size over 100 MB; 109 give a start at :00 or :30; by year, 322 from 2023, 236 from 2024, 133 from 2025 and
  57 from 2026. The third call read the metadata of one recording from Japan, to learn the fields.

No US item's title, description, coordinates or audio has been seen, no ADS-B has been looked up for one, and no
truth computed.

## Tools under test

As set K: `fdop` builds `7e66501` (pinned), `9087ec4`, `b759c6f`, `1dec7d6` and `3c51eae` (`main`'s code when this
file was written), the binaries set K runs; and `dp`, `dp48`, `dpdem`, `dt` and `dtfb` with set J's `dtfb` fallback.
No build or tool is added later.

## Selection, in order

1. **Search.** The Internet Archive's `advancedsearch` on
   `collection:radio-aporee-maps AND date:[2023-01-01 TO 2026-11-06] AND timezone:America*`, every result, saved
   when taken. Rows are ordered newest recording first (the item's `date`), then by identifier.
2. **Zone.** Out unless the item's `timezone` is a contiguous-US zone: `America/New_York`, `America/Detroit`,
   `America/Kentucky/*`, `America/Indiana/*`, `America/Chicago`, `America/Menominee`, `America/North_Dakota/*`,
   `America/Denver`, `America/Boise`, `America/Phoenix` or `America/Los_Angeles`.
3. **Metadata.** From the item's metadata (`archive.org/metadata/<id>`, one call per item, saved). Out if any of:
   - it gives no latitude and longitude;
   - its longest audio file runs under 10 minutes. Set K asked for 20. The lower floor admits more recordings (the
     one recording read in the probe ran 4.4 minutes), and step 9 still asks for close traffic inside the recording
     itself;
   - its licence (`licenseurl`) is neither a Creative Commons licence nor a public-domain mark;
   - its start minute is :00 or :30. The probe found those minutes about four times as often as chance; they are
     taken as rounded, not read off a clock.
4. **Description.** The title and description, read. It passes only if they describe one stationary take at a
   public outdoor site. Out: a walk, or a recording from a moving car, boat, bicycle or train; indoors, including
   stations and malls; a home, yard or balcony; added music or narration; a compilation, montage or composed piece;
   a description that bans reuse, as set I's step 5.
5. **Truth.** The item's latitude and longitude, with a 1 km limit. Ground height from USGS EPQS.
6. **Place.** Out within 5 km of a case of sets A to K or a development recording. Out within 1 km of an earlier
   set L pass.
7. **Metro and density.** Set J's step 7.
8. **Airport.** Set J's step 8.
9. **Traffic.** Set K's step 9: the recording's own span, from its start to the start plus its length. At least 2
   passes within 1.5 km from at least 2 aircraft.
10. **Cases.** Every item that passes steps 2 to 9, in row order, at most 6 per metro and 24 in all. If fewer pass,
    the set is smaller and no screen is relaxed.

The start is the item's local date and time in its own zone, converted to UTC. Every row is listed with the step it
stopped at.

## Capture and seal

Each case's original audio file (the item's audio file marked `source: original`; if there are several, the longest)
is downloaded once from the Internet Archive on the benchmark machine. Every tool is given the same WAV made from it:
`ffmpeg -i <original> -map 0:a:0 -c:a pcm_s16le audio.wav`, with rate and channels kept. The original, the WAV and
the saved metadata are sha256'd in `seal_l.txt`. Set K's void rule, listening rule and single read apply.

## Inputs and runs

As set K, with ids `l_<site>` and wrong-date nulls `l_<site>_n`. The local date given to each tool is the start's own
date in the item's zone.

## Scoring

As set K, for set L alone and for sets J, K and L together (summed from the separate reads). Every case is listed with
its metro and a recordist label (R1, R2, ... in row order). The recordists' account names, often personal names, stay
on the benchmark machine.

## What it can show

Correct and wrong counts for every tool on stationary field recordings by many recordists in large US metros, away
from busy airports, with close traffic in the recording's span. The start is the recordist's word, as in set K; step
3 drops only the times that look rounded.

Recordings, coordinates and aircraft identifiers stay out of this repository.

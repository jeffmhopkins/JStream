# Feature tour: a demo project with a show

A project that tours what JStream is and does, run by a show
([ADR-0210](../../docs/adr/0210-shows-a-run-order-with-bookmarks.md)). It doubles as a
fixture for the show timeline: it uses every part of a show at least once.

Everything in it is drawn by JStream itself (text, shapes, backdrops, a starfield light,
fog, a spectrum, a waveform, MIDI visuals), so it opens on any machine with no media
files and no camera. The only device it touches is desktop audio, which the show's audio
track brings up during two chapters.

## Open it

**File › Open project…** and pick `feature-tour.json`. Opening a project replaces the one
that is open, and JStream saves into this file from then on, so copy it somewhere first if
you want to keep this one clean.

## Run it

Open the Show panel, pick **Feature tour** and press Play, or press `Ctrl+Alt+P`.

| Key | MIDI note | Does |
|---|---|---|
| `Ctrl+Alt+Space` | 36 | Continue |
| `Ctrl+Alt+[` / `Ctrl+Alt+]` | 37 / 38 | Back / Skip |
| `Ctrl+Alt+1` … `Ctrl+Alt+8` | | Go to bookmark 1 to 8 |
| `Ctrl+Alt+0` | | Go to the Thanks bookmark |
| `Ctrl+Alt+.` | | Stop the show |
| `Ctrl+Alt+L` / `Ctrl+Alt+B` | 39 (lower third) | Toggle the lower third / live bug overlay, with or without a show |
| `Ctrl+Alt+E` | | Play the Energy ramp sequence |
| `Ctrl+Alt+K` | | Fire the impulse shake in Motion |

Every bookmark carries talking points in its note.

## The Feature tour show

| Time | Chapter | What it exercises |
|---|---|---|
| 0:00 | Holding | a loop (Pre-show) that Continue leaves at once; a non-pausing bookmark inside it |
| 0:20 | Welcome | a per-clip slow fade; a sequence cue plays Title build (item cues and a lane) |
| 0:32 | Scenes & keyframes | the keyframe track: three keyframes loaded with a Move |
| 0:50 | 3D space | camera keyframes (push in, orbit, rise, home) on the keyframe track |
| 1:14 | Motion | event cues fire an impulse motion |
| 1:30 | Channels & sequences | a sequence cue plays Energy ramp; an event cue sets the Glow channel |
| 1:48 | Audio & MIDI | a bookmark that continues by itself after 15 s; an audio clip with fades |
| 2:06 | Effects | a non-pausing bookmark; item show and hide event cues |
| 2:22 | Agents & MCP | a sequence cue plays Agent typing |
| 2:40 | Shows | a stinger cut on the clip and on the bookmark |
| 2:58 | Thanks | a loop (Outro) that finishes its pass on Continue; the end goes to Pre-show |

Overlay tracks put the lower third over three chapters and the live bug over all of them,
with fades. A second show, **Lightning tour**, plays six of the same scenes straight
through and loops, with the Next monitor live: one project, several shows, shared scenes.

## Changing it

The file is in the canonical form the engine saves. `cargo test -p jstream-graph --test
feature_tour` checks that it loads with the engine's kinds, is saved unchanged, and that its
shows name only scenes, keyframes, items, sequences, channels, sources and presets it
holds. After editing it by hand, rewrite it in canonical form with
`FEATURE_TOUR_WRITE=1 cargo test -p jstream-graph --test feature_tour`.

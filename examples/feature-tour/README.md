<!-- @doc
id = "EXAMPLE-feature-tour"
title = "Feature tour example"
kind = "guide"
status = "active"
review_every = "90d"
-->

# Feature tour: a demo project with a show

A project that tours what JStream does, run by a show
([ADR-0210](../../docs/adr/0210-shows-a-run-order-with-bookmarks.md)). Real screenshots of
the app sit in scenes as image sources, and the show loads keyframes that move the scene
camera onto one part of a screenshot at a time, with a spotlight and a short callout. Two
chapters are the feature itself rather than pictures of it: the Keyframes chapter moves one
scene between three of its keyframes, and the Motion chapter runs five motion modifiers.
The project doubles as a fixture for the show timeline: it uses every part of a show at
least once.

It needs no camera and no network. The only device it touches is desktop audio, which the
show's audio track brings up in three chapters. The 3D chapter shows stills that JStream
rendered of the cockpit project in [StreamingScenes](https://github.com/jeffmhopkins/StreamingScenes),
and that project's DAW screens drawing a simulated Bitwig session; it doesn't load the
cockpit itself.

<a id="open"></a>
## Open it
<!-- @section
-->

**File › Open project…** and pick `feature-tour.json`. Keep the `media/` and `fonts/`
folders beside it. Opening a project replaces the one that is open, and JStream saves into
this file from then on, so copy the folder somewhere first if you want to keep this one
clean.

<a id="run"></a>
## Run it
<!-- @section
-->

It needs a build with the show runner and the Show panel, which arrive in the next
pre-release. Open the Show panel, pick **Feature tour** and press Play, or press
`Ctrl+Alt+P`. Every bookmark's note is a short script for whoever presents it.

| Key | MIDI note | Does |
|---|---|---|
| `Ctrl+Alt+Space` | 36 | Continue |
| `Ctrl+Alt+[` / `Ctrl+Alt+]` | 37 / 38 | Back / Skip |
| `Ctrl+Alt+1` … `Ctrl+Alt+8` | | Go to bookmark 1 to 8 |
| `Ctrl+Alt+0` | | Go to the Thanks bookmark |
| `Ctrl+Alt+.` | | Stop the show |
| `Ctrl+Alt+L` / `Ctrl+Alt+B` | 39 (lower third) | Toggle the title lower third / the status tag, with or without a show |
| `Ctrl+Alt+K` | | Fire the impulse in the Motion chapter |

<a id="show"></a>
## The Feature tour show
<!-- @section
-->

| Time | Chapter | What it exercises |
|---|---|---|
| 0:00 | Pre-show | a loop that Continue leaves at once; a non-pausing bookmark inside it |
| 0:20 | Welcome | a slow fade on the clip; a sequence cue plays the title build; a camera keyframe on a slow drift; an audio clip |
| 0:32 | The studio | four camera keyframes over a screenshot, each with a spotlight and a callout |
| 1:01 | What you make | a bookmark that plays through |
| 1:13 | Keyframes | one flat scene and its three layout keyframes, loaded with a Move; one item has its own delay |
| 1:39 | Motion | event cues fire an impulse modifier; an audio clip feeds the audio modifier |
| 1:55 | Sequences & channels | camera keyframes over the Sequence panel |
| 2:18 | Shows | a stinger on the clip and on the bookmark |
| 2:58 | 3D cockpit | camera keyframes over two cockpit renders and the cockpit's DAW screens |
| 3:27 | DAW & web pages | a sequence cue plays a card build |
| 3:43 | Keys & MIDI | a bookmark that continues by itself after 12 s |
| 4:01 | Effects | a bookmark that plays through |
| 4:19 | Plugins, cameras and going live | camera keyframes over screenshots and panels |
| 5:01 | Under the hood | a sequence cue plays a card build |
| 5:15 | Thanks | a loop that finishes its pass on Continue; a lane drives the Glow channel; the end goes to Pre-show |

Overlay tracks put a lower third over the opening of most chapters, one over the title,
and a "Rendered in JStream" status tag over the whole tour, with fades. A second show,
**Lightning tour**, plays the title, Keyframes, the 3D cockpit and Thanks straight through and
loops, with the Next monitor live: one project, several shows, shared scenes.

<a id="look"></a>
## Look
<!-- @section
-->

The tour uses the app's own palette and type: the colours of `crates/jstream-ui/style.css`
and IBM Plex Sans and Mono, which are in `fonts/` under the SIL Open Font License
(`fonts/OFL.txt`), as the app bundles them
([ADR-0077](../../docs/adr/0077-bundle-ibm-plex-fonts.md)).

<a id="changing"></a>
## Changing it
<!-- @section
-->

The file is in the canonical form the engine saves. `cargo test -p jstream-graph --test
feature_tour` checks that it loads with the engine's kinds, is saved unchanged, and that its
shows name only scenes, keyframes, items, sequences, channels, sources and presets it
holds. After editing it by hand, rewrite it in canonical form with
`FEATURE_TOUR_WRITE=1 cargo test -p jstream-graph --test feature_tour`.

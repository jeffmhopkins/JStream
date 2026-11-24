<!-- @doc
id = "ADR-0210"
title = "Shows: a saved run order of scenes, with bookmarks, loops and Continue"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0210: Shows: a saved run order of scenes, with bookmarks, loops and Continue

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
An operator running a live programme today switches scenes by hand, by key or by MIDI pad,
and starts sequences ([ADR-0184](0184-sequences-lanes-and-triggers.md)) the same way. There
is no saved order for a whole programme: nothing says "this scene, then that one, wait for
me here, then the next", and nothing can put the studio back where a programme would be at a
given point after a mistake or a skipped segment.

What a programme does is already expressible as actions: `scene.switch` puts a scene on
program with a transition preset, `scene.set_keyframe` loads a keyframe, `timeline.play`
plays a sequence, `item.show` and `channel.switch` set state. What is missing is an object
that places those actions in time, a clock that stops where the operator wants it to, and
one button that moves the programme on.

The word "timeline" is taken: on the wire a timeline is a sequence (`timeline.*`,
ADR-0115, ADR-0184). The new object needs another name.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It adds protocol commands, fields
and an event, minor and additive ([R-API-001](../RULES.md#R-API-001)), and a collection file
version with its migration ([R-API-003](../RULES.md#R-API-003)). It changes no product
scope, licence, security or privacy posture, compatibility promise or spending: a show adds
no new way of doing anything, only a saved order of the actions that exist.

<a id="decision"></a>
## Decision

1. **The object is a show**, `show.*` on the wire and "Show" in the UI. The word "timeline"
   stays out of its API.

2. **A collection holds any number of shows; one runs at a time.** All of them share the
   collection's scenes, sequences and bindings. A show is a collection object:
   `show.set` stores a whole show (new, or in place of the one with its `show_id`),
   `show.delete` removes it, `state.get` lists `shows`, and the collection file saves them.
   Each is one undo step. Running a show is studio state, like the program scene: never an
   edit, never undone, never saved. At most 64 shows (`MAX_SHOWS`).

3. **Tracks.** A show (`ShowInfo`) lays its content out in show time, in milliseconds:
   - the **program track** (V1): scene clips end to end with no gaps, so a clip's start is
     derived from the clips before it (ripple) and only its length is stored. A clip's start
     puts its scene on program through its `preset_id`, which wins over the scenes' own in
     and out; omitted, the usual order applies;
   - **overlay tracks** (V2 and up, at most 8): scene clips anywhere, gaps allowed, none
     overlapping on one track, drawn over program for their span with optional fades;
   - the **keyframe track** (K): markers that switch program to a keyframe scene of the
     scene on program, as a Move with the marker's preset timing;
   - **sequence tracks** (S, at most 8): cues that run one action a timeline's cue may run
     (`Action::cue_problem`), except `scene.switch`, which only the program track does. A
     cue that runs `timeline.play` is drawn as a clip as wide as the sequence;
   - **audio tracks** (A, at most 8, optional): clips that bring one source's fader up to a
     level for their span, with fades, moving the fader as a sequence lane does, so the
     mixer tells the truth and a hand on the fader takes over (latest wins). With no audio
     track, a show changes nothing about audio.

   Every element (clip, track, marker, cue, bookmark, loop) has an ID unique within its
   show. A clip naming a scene, source, sequence, keyframe or preset the collection doesn't
   hold is kept and does nothing, as a timeline's cue is, so deleting a scene never shifts
   a show. `ShowInfo::problem` is the one shape check every implementation shares.

4. **Bookmarks** are named points in show time, pausing (the default) or non-pausing, in
   time order with no two at one time. A pausing bookmark stops the show until Continue
   (one at a clip's start puts that clip on program as it stops, so the operator waits on
   what comes next), and may continue by itself after `continue_after_ms`. Each has an "arrive with" preset
   for jumps to it, a name, and show notes for the operator: Markdown, up to 16 KiB, which
   the Run view shows in an expandable panel. The show's own `preset_id` is the
   default for jumps and loop wraps.

5. **Continue is one bindable action**, `show.continue`: waiting at a pausing bookmark, or where `show.pause` held it, it
   plays on; with a spot armed it jumps there; off script it goes to the next clip; in a
   loop it leaves as the loop says; playing, it jumps to the next pausing bookmark. It is
   ignored during a transition, so a double press can't skip a bookmark. Its siblings, all
   bindable to a key, a MIDI trigger or called over MCP, are `show.play` (from the start or
   a bookmark, or rehearsed on preview), `show.stop`, `show.pause` (hold the play head
   where it is until Continue, or Play of the same show, plays on), `show.back`, `show.skip`,
   `show.go_to` (one bookmark of one show, acting only while that show runs),
   `show.go_to_number` (bookmark 1 to 8 of the running show, by order, so a pad row needs no
   rebinding), `show.seek` (jump the play head, for scrubbing and agents) and `show.arm`.
   `show.delete` takes the bindings that name the show with it, as other deletes do.

6. **A jump applies state and skips one-shots.** After a jump, the studio is as playing to
   that point would have left it: the program clip's scene and the last keyframe, the
   overlay and audio clips that span the point, items shown or hidden and channels switched
   by earlier cues, and each spanning sequence sought to its place. One-shot cues (a sting,
   a source restart) are not replayed. Only what the show placed is worked out; what the
   operator did by hand is left alone. A cue is state or one-shot as
   `Action::cue_state` says. A jump into a video seeks it; audio a jump cuts off fades over
   about 0.3 s.

7. **Loops** are spans of show time with a name, a wrap preset and how Continue leaves
   them (`at_once`, the default, or `finish_pass`). Once inside, the show wraps from the end
   to the start until a jump outside or Continue leaves. A bookmark at a loop's start is
   inside it and pauses once on entry, not on every wrap; one at its end is outside it.
   Loops don't overlap or nest, and run at least 100 ms.

8. **Media dropped on a show becomes real scenes.** A video, image or audio file dropped on
   a track becomes a scene with a media source (in a "Show media" folder), placed as a clip.
   Video and audio take the file's length; images take the show's `image_ms`, 10 s by
   default. This is UI over existing edits; the format needs nothing more.

9. **Seamless and timed clips** are worked out per clip: a clip is timed when its scene
   holds a video or audio file. Playing through, Continue, a jump to a clip's start and a
   loop wrap must be seamless; scrubbing and jumps into the middle of a video may hitch.

10. **End of show** is a per-show `end`: `stop` on the last scene (the default), `loop` to
    the first bookmark (or the start), or `go_to` a named bookmark. A show never ends the
    stream.

11. **Overlays are items of an internal show scene** that sits one level above program: the
    renderer draws program (transitions included) as its implicit bottom layer and its
    nested-scene items over it, and outputs draw it in place of program. It is an ordinary
    scene of the collection, made by the engine, never listed, selected, edited, put on
    program or used as a source. The collection file saves which scene it is in a top-level
    `show_scene` marker; a marker naming a scene the file doesn't hold is dropped on load.
    Overlays also work with no show running: `overlay.show`, `overlay.hide` and
    `overlay.toggle` put a scene over program on the show scene's top layer, or take it
    down, through its fade, from a key, a pad or MCP, since a client can't name the show
    scene's items. They are runtime state, and `studio.get` lists the scenes up as
    `overlays`.

12. **What runs is reported, not saved.** `studio.get` reports the running show in `show`
    (`RunningShow`: its play head, `playing`, `paused` or `off_script`, the bookmark it
    waits at and how long, the loop and pass, an armed spot, and whether it rehearses).
    `show.changed` says what it did (`started`, `paused`, `continued`, `jumped`, `looped`,
    `off_script`, `ended`, `stopped`). A scene switched by hand while a show runs takes it
    off script: it waits where it was, and Continue goes on to the next clip.

13. **Run and Edit views.** The Show panel has an Edit view (the tracks) and a Run view (a
    big Continue, Now and Next, bookmark buttons, clocks). The Run view's Next monitor is a
    per-show `next_monitor`: `live`, `still` (the default) or `off`; program is never slowed
    to draw it. While a show runs, a click on the ruler arms a spot for the next Continue
    (`show.arm`) instead of jumping program; a bookmark's button or key jumps at once. The
    Lock that stops editing is the panel's own state, not part of the show. Rehearse runs
    the show on preview, leaving program, the channels and the mixer alone.

14. **The file.** Collection file `schema_version` 22 adds `shows` and `show_scene`; a
    version 21 file with either, or with a `show.*` action in a binding, is refused, as
    version 21 refused it. `PLUGIN_SCOPES` places every `show.*` command as never granted
    to a plugin, as timelines are, until a later decision says which class may.

<a id="consequences"></a>
## Consequences
- A programme becomes a saved, shareable object an operator or an agent can build over the
  protocol and run with one button, and everything already bound to actions keeps working
  during it: a show is one more operator, not a mode.
- Implementations share one shape check (`ShowInfo::problem`) and the parity fixture holds
  a show, so the engine's graph and the mock store and refuse shows identically.
- The engine and the mock answer the `show.*` run actions as not implemented until the
  engine's show runner lands; `studio.get` omits `show` until then. The runner, the jump
  state resolver ("the state after a jump equals the state after playing there", with
  property tests), overlay toggles, media seeking, the panel and MCP guidance build on these
  types without another change to them.
- The format leaves out labels on shows, a clip's media in-point and stinger clips; each
  can be added later as an optional field without a file version that refuses older files.
- A show runs one at a time. Two operators on one engine see the same play head and either
  can Continue, which falls out of running it in the engine.

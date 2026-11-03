<!-- @doc
id = "ADR-0096"
title = "Items can be locked, and a preview stream can send PNG for a screenshot"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0096: Items can be locked, and a preview stream can send PNG for a screenshot

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The maintainer approved two more OBS habits for the editing UI. OBS has a lock on each
source row: a locked source can't be dragged, resized or turned in the preview, but it
can still be selected and changed in its properties. OBS also takes a screenshot of the
program, a scene or a source and saves it as a PNG.

JStream has no lock. A lock only the browser remembered would not reach another client
or survive a new machine, and a person who locks a backdrop expects it locked wherever
they open the collection, so it belongs in the collection.

`video.snapshot` returns a PNG, but only of the program mix, at most 1280 pixels on its
longest side, and it draws every capture (display, window, webcam) as nothing. Preview
streams (ADR-0045) already draw the program, any scene or any source with its captures,
at up to 1920 × 1080, but only as H.264 or JPEG, which lose detail a screenshot should
keep.

<a id="decision"></a>
## Decision
1. **A lock is item state: `item.set_locked` (protocol 0.71).** It takes an `item_id`
   and `locked`, and its inverse sets the old value. `ItemInfo.locked` reports it,
   omitted while false. It belongs to the item, like its transition settings, so it
   takes no `scene_id` and a keyframe scene doesn't override it. `item.remove`'s
   inverse locks the item again, the collection file stores it (omitted while false,
   so a collection with no locked item saves as before and needs no schema bump), and
   the mock, the
   activity log and `jstream edit item.set_locked` know it. Plugin class `edit`, as the
   other item edits.
2. **The engine never enforces a lock.** It is an editing aid, as in OBS: every edit
   still applies to a locked item, so the Inspector, the menus, undo and a script work
   as before. A client doesn't start a canvas drag, resize, turn or crop handle on a
   locked item. The JStream UI shows a lock on each row and a Lock check in the item
   menu.
3. **A preview stream can send PNG (`png` in `preview.subscribe`'s `codecs`, protocol
   0.71).** Every PNG frame is a keyframe, encoded with `video.snapshot`'s encoder. The
   UI takes a screenshot by opening a one-frame PNG stream of the program, a scene or a
   source at the canvas's size (at most 1920 × 1080), saving its first picture as a
   file, and closing the stream. Captures show in it, as in any preview. An engine
   older than 0.71 refuses the codec's name; the JStream UI never meets one, since the
   handshake refuses a client newer than the engine.
4. **`font.list`'s exposure, stated plainly** (ADR-0092 left it implicit). Any client
   that may read, and any plugin granted the `read` class, sees each installed font's
   absolute path. A font in the user's own font folder names their home folder, and
   so usually their account name. It stays `read`, like `capture.targets`, which shows
   such a plugin more (window titles), so a plugin that draws text can offer the
   fonts. `plugin.list` is never granted for a different reason: it holds the person's
   own choices for each plugin and where each package is, which one plugin has no need
   to learn about another (ADR-0069 S2). A path that isn't UTF-8 is left out of the
   list, since a lossy copy of it would name another file.

<a id="consequences"></a>
## Consequences
- A lock follows the collection to every client and machine, and undo covers it.
- A script can still move a locked item. That is deliberate: the lock protects against
  a stray drag, not against a decision.
- A collection file with a locked item doesn't open in a build older than this one,
  which refuses the unknown field rather than drop the lock on its next save.
- A screenshot costs one preview picture, drawn on the preview thread, so it never
  touches an output. It is limited to 1920 × 1080; a larger canvas is scaled down.
- A PNG stream is paced like any preview, and each picture is a lossless encode on
  the one preview thread every session's previews share. The UI asks for 1 fps and
  closes after the first picture, so it costs one encode; a client that kept a `png`
  stream open at 30 fps would slow every other preview, so a client should ask for
  `png` only for a still.
- `video.snapshot` keeps its contract; the PNG codec doesn't change it.

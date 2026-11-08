//! Design guidance for agents that build what goes on screen, appended to the server
//! instructions (`server.rs`), so every client reads it before its first edit.
//!
//! A live test (#1018-#1021's thread) found agents could build any graphic the tools
//! allow and still make poor ones: flat full-saturation colours, thin type, boxes that
//! didn't fit their text, nothing aligned. The tools can't fix taste, so this says what
//! a broadcast designer would: where things go, which sizes, which faces and colours,
//! and to look at the result before calling it done. It names the `backdrop` and `text`
//! settings that make graphics look finished, the `shape` and `timer` kinds, how to
//! group, scroll and animate, how to use 3D placement and blend modes, how to layer
//! light by role, use the light and framing filters (bloom, glow, vignette), mask, and
//! build reflections on glossy surfaces (#1062), how to build a 3D set with a scene camera
//! (ADR-0094) and its depth of field (ADR-0108), which transition to use (including a
//! `stinger`), when to build a graphic as HTML in a `browser` source, animated on one
//! clock, or an audio visualiser on `jstream.levels()` (ADR-0124), and how to write a
//! custom shader the Filters window's shader editor opens (#1232). Numbers are for a
//! 1920×1080 canvas and scale with it.

/// The guidance, as one paragraph of the server instructions.
pub(crate) const DESIGN: &str = "\
Designing what goes on screen: people judge graphics by broadcast standards, so build \
them like a broadcast designer. Read the canvas size from `state.get` (1920×1080 unless \
changed; the numbers here are for that size and scale with it). \
Layout: keep text and logos inside the title-safe area, 5% in from every edge (96 px \
left and right, 54 px top and bottom). Align elements to shared left edges and \
baselines, space them in multiples of 8 px, and leave room around them; don't centre \
everything or fill every corner. \
Backgrounds: start a scene from a `backdrop` source, a generated studio backdrop \
(`style` `canvas`, `muslin` or `paper`) with `color` and a darker `accent` of the same \
hue; a flat full-frame `color` source looks unfinished. Use one backdrop source for the \
scenes that share a look, so a `move` transition matches it instead of sliding it away. \
Shapes: draw plates, bars, chips, pills, badges, dots and dividers with a `shape` \
source whose `width` and `height` are its item's box: `radius` rounds a rectangle (half \
its height makes a pill), `shape` `ellipse` in a square box is a circle, `fill` \
`linear` or `radial` blends `color` into `color2` (keep a gradient subtle, two shades of \
one hue, such as #16325c to #0b2545 at `angle` 90), and `border_width` and \
`border_color` outline it. A `color` source fills its own size, so one placed in a box \
of another aspect is letterboxed by the default `fit` mode; use a shape. \
Type: a `text` source with `face` `bold` or `condensed_bold` for headlines and \
`regular` for supporting lines. At 1080p, headlines 56-72 px, supporting lines 32-40, \
tickers and captions 26-32; at most two sizes in one graphic. Uppercase headlines read \
better with `letter_spacing` 1-3. Text over a picture needs a soft shadow \
(`shadow_color` black at about 60% alpha, `shadow_y` 2-4, `shadow_blur` 8-12) or a plate \
behind it. \
Colour: one base colour, one accent and neutrals. Prefer deep, slightly muted colours \
(a news red like #c8102e, a navy like #0b2545) to pure primaries like #ff0000, and \
off-white text (#f4f4f2) to pure white on large plates. Check contrast: light text on \
dark plates or dark on light, never mid on mid. \
A lower third: a plate shape whose left edge sits at the safe area (x 96-120) and \
bottom about 120 px above the canvas bottom, about 100-130 px tall for a headline with \
an optional smaller line under it; an accent strip 8-12 px wide on its left; text inset \
24-32 px from the plate's edge and centred on it vertically. Give the plate a \
`drop_shadow` filter (blur about 20, offset 4-8, black at about 50%). \
A ticker: a full-width bar shape with a label plate at its left, above the text in the \
stack; the text source holds the headlines with a separator such as •, is wider than \
the bar (repeat the headlines if not), and has a `scroll` filter with `speed_x` -80 to \
-120 (negative moves left) and `gap_x` about 80 so the end doesn't run into the start. \
A countdown or clock: a `timer` source (`mode` `countdown` with `duration` in seconds \
and an `end_text` such as LIVE, `stopwatch`, or `clock` with a `clock_format` and an \
optional IANA `time_zone`), styled like `text`. Its frame keeps one size whatever it \
shows, so size its item once from `source.sizes`; editing its look keeps it counting, \
and changing `mode` or `duration` restarts it. To run it again from the start at the same \
length, change `restart` to any other number. \
A graphic of several parts that should read as one (a scoreboard, a framed camera \
with its name bar): build it in its own scene, place that scene with a `scene` source, \
crop the item to the graphic's bounds, and put `rounded_corners` and `drop_shadow` in \
the item's effects (`filter.add` with owner `item`), which clip and shadow it as one. \
Fit boxes to content: a new text source draws at its own size, so read it from \
`source.sizes` (or a `source.size_changed` event) and give its item that size, rather \
than guessing a box; with the default `fit` mode a wrong box shrinks or offsets the \
text. \
A look no built-in filter makes: write it as a custom shader, an effect pack from \
`pack.write` that a person can then open and edit in the Filters window (Edit shader…). \
Give a new pack ID `custom-<word>` (not one `pack.list` has) one effect `shader` whose \
`shader` is `shader.wgsl`. In that file put a `// @name <Name>` line and one \
`// @param <id> <type> = <default> \"Label\"` line per setting, matching the manifest's \
`[[effect.param]]` list: `float 0..1 = 0.5`, `int 0..64 = 8`, `bool = true`, \
`color = #ff9a3cff` or `enum soft|hard = soft`. The shader defines \
`@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>`, reads the input with \
`src(in.uv)` (premultiplied linear) and each setting as `p.<id>`. \
Motion: animate with keyframe scenes. Build the graphic's off state as a scene (a \
plate 10 px wide or cropped to nothing, text at opacity 0), add its on state with \
`scene.create_variant`, override geometry, crop and opacity per item there, make a \
`move` transition preset (`transition.create`, 500-800 ms, `ease_out`) and switch \
between them with it. Reveal text with a crop that opens with its plate rather than a \
fade over it, and stagger the parts with `item.set_transition` `delay_ms` (0, 120, \
200). To lead with a part in one move and trail with it in another, give each keyframe \
its own order with `item.set_timing` (the keyframe's `scene_id`, the item, and a \
`timing` of `delay_ms`, `easing` and `duration_ms`), which applies only to switches into \
that keyframe. Easings: `ease_out` for arrivals, `ease_in` for exits, `hold` for a jump \
at a set time, and `{\"cubic_bezier\": {x1, y1, x2, y2}}` (y up to 3) for an overshoot. \
Items with no partner in the other scene appear and disappear by their \
transition's `appear` and `disappear` (`slide`, `scale` or `fade`). Keep moves under a \
second and ease them; a cut beats a slow move. A variant that overrides an item's \
geometry stores the whole geometry, so nudging x there also fixes its size and angles \
against later edits to the item in the parent scene; finish an item's size and angles \
in the parent first, and override geometry only on the items that move. \
A variant can override an item's effects too (`item.set_effects`): keep the same \
effects in the same order and change only their numbers or colours (a blur radius, a \
colour grade), and a `move` blends them with the item; another list, or another mask \
scene or image, cross-fades it instead. \
Depth and blending: a keyframe variant can override an item's 3D placement, so a `move` \
interpolates it: `tilt_deg` (top edge away), `turn_deg` (right edge away), `depth_px` \
and `perspective_px` (about 1200-2000 for a natural look). A plate that flips down \
from `tilt_deg` -90 to 0, or a card that turns in from `turn_deg` 70, reads as a \
designed move; keep angles under 90 and pair them with opacity. `item.set_blend` sets \
how an item composites: `screen` or `additive` for light leaks, glints and glows over \
the picture, `multiply` for shadows and textures, `overlay` for a tinted wash; most \
items stay `normal`. Opacity is a blend's amount: below 1 it mixes what is below the \
item with the blended result, so a half-strength `overlay` is the mode at 0.5. \
Opacity goes up to 4 (400%): above 1 it multiplies the item's \
light without covering more, so an `additive` glow or a blurred, masked reflection can \
be brighter than its source draws it. \
Light and framing filters: `bloom` makes the bright parts of any picture give off \
light, an opaque camera included (`threshold` about 0.7, `intensity` 0.5-1.5, `radius` \
16-40); `glow` lights the transparent area around a shape (a logo, text, a keyed \
person), so on an opaque picture it shows only past its edge, as an item effect. As \
item effects, `glow`, `drop_shadow` and `outline` draw past the item's box. A \
`vignette` tints the edges towards its `color` (`blend` `normal`) or darkens by it \
(`multiply`), as an ellipse that follows the frame or a `round` circle around \
`center_x`, `center_y`; `strength` is the linear share of the colour, so a dark one \
looks half as bright at the edge near 0.8. Its looks: subtle (`strength` 0.55, `radius` \
0.5, `softness` 0.7), cinematic (0.85, 0.3, 0.7) and spotlight (1, 0.15, 0.45, \
`shape` `round`). \
Layering: build a scene as separate layers by role (the background, what glows or \
emits light, the light it throws onto nearby surfaces, and the foreground frame or \
set in front of it), each with its own mask and blend, so lighting and reflections \
stay consistent when one of them changes. Light is `additive` (or `screen`): glows, \
flares, beams, sparks. Matter is `normal`: smoke, objects, plates, people. An effect \
that mixes both (an explosion, a fire) reads as solid only when its matter is drawn \
normally and just its bright core is added. Masks: `image_mask` cuts an item by an \
image's alpha or luma (`mode`), and `scene_mask` by a scene's picture, so a mask can \
be drawn with `shape` sources instead of a file: a `linear` fill from white to black \
is a ramp with `mode` `luma`, or from white to transparent with the default `alpha`. \
A file setting, such as an `image` source's `file` or `image_mask`'s `image`, takes an \
asset reference, `{\"type\": \"asset\", \"root\": \"absolute\", \"path\": \"/path/to/pic.png\"}` \
(or `root` `collection` with a path relative to the collection file's folder), not a bare \
path. \
Reflections on glossy surfaces (a desk, a floor, glass, a screen): reflect the \
composite the viewer sees, never each layer on its own, or the layers won't line up. \
Put what is reflected in one scene or group and place it again with a `scene` source, \
with `flip_v` on and `fit_mode` `stretch`, its top edge on the surface's edge nearest \
the reflected thing, so each surface mirrors from its own edge; squash its height to \
suit the viewing angle (flatter for a surface seen at a low angle) and give it a small \
`blur` in its effects, more for a rougher surface. A mask drawn for the canvas would be \
stretched over that item's squashed box, not the canvas, so put the flipped item in a \
group of its own, place the group full-frame and unflipped, and put the `image_mask` \
(or `scene_mask`) on that outer placement's effects, with `item.set_blend` `additive` \
and the opacity there. Mask the specific surfaces that would shine, each with a \
fresnel ramp (stronger toward the edge seen at a grazing angle), never one broad sheen \
over the whole picture. A glossy edge or rim is the same composite again, unflipped, \
with a wider blur, an edge mask and `additive`. Keep a reflection on glass at a low \
opacity (often 5-15%): tune it in JStream, and check it over the brightest and the \
darkest background it will sit on. \
Blending space: `canvas.set_blending` picks the space every mode mixes in, for the \
whole project. `srgb`, the default, mixes as a browser and design tools do, so a design \
made in HTML or a design tool keeps its look when rebuilt natively, and soft veils, \
shadows and glows keep the strength they had there. `linear` mixes light as it adds, \
which suits glows, light leaks and fades over camera footage, but makes a faint white \
veil about twice as bright as in a browser. Choose it once, before tuning opacities, \
since switching changes every blended item's look; `state.get` reports it as `blending`. \
3D sets: `scene.set_camera` gives a scene a camera (`position`, `target`, `fov_deg`, \
`roll_deg`), and then every item sits in one 3D space: `geometry.center` is its x and \
y, `depth_px` its z (toward the viewer), and its rotation, `tilt_deg` and `turn_deg` \
turn it about its centre; its own `perspective_px` is ignored. The canvas is the plane \
z = 0 seen from straight in front: the framing camera looks at the canvas centre \
(`target` x 960, y 540, z 0 at 1080p) from `position` z = (height / 2) / tan(fov / 2), \
about 1304 at the default 45°, and through it a flat item lands exactly on its 2D \
box, so start there and move the camera. A floor is an item with `tilt_deg` 90 placed \
low, a wall one pushed back with negative `depth_px`; items draw far to near. Move \
the camera with a keyframe scene that overrides it (`scene.set_camera` on the \
variant) and a `move` transition, which glides the camera with the items. A 2D \
scene's keyframe may have a camera of its own too: a `move` into it starts from the \
framing camera, so a flat 2D layout tilts or pushes into 3D and back; keep the 2D \
side's items flat (no tilt, turn or depth), or the move fades. Depth of \
field: `focus_px` is the distance along the view that stays sharp and `blur_px` (0-64) \
how soft the far background gets; things nearer or farther blur in proportion. A soft \
backdrop behind a presenter is `blur_px` 4-8 focused on the presenter's distance; pull \
focus with two keyframe scenes that differ only in `focus_px` and a `move` transition. \
A camera can move on its own: `scene.set_camera_motion` gives it up to 8 modifiers, the \
kinds and clocks of `item.set_motion`, on `position_x`/`_y`/`_z`, `target_x`/`_y`/`_z`, \
`fov`, `roll`, `focus` or `blur`; a slow `wiggle` of 4-8 px on the position is a \
hand-held camera. To follow the pointer over a screen capture, add a `follow_pointer` \
filter to the effects of an item showing a `display` or `window` source (`filter.add` \
on the `item` owner); `scene.set_camera_follow` is a shortcut that does the \
same. The scene's camera glides after the pointer at an angle (`yaw_deg`, \
`pitch_deg`), keeps the capture filling the frame, keeps the pointer inside \
`safe_margin_pct` (default 10% a side) and, with `blur_px` above 0, focuses where it \
looks; a 2D scene gets a camera for it. `zoom` (1-4) is its resting zoom; \
`scene.zoom_camera` steps it live by 1.25 a step, and is what a zoom hotkey binds. \
Turning the filter off returns the camera to where it was. Camera motion and the \
follow belong to the scene's family: set them on the parent, not a keyframe scene. \
Put \
graphics that must stay flat (a lower third, a ticker) in a 2D scene above a `scene` \
source of the 3D set. \
Transitions between scenes: `fade` for calm changes, `move` when the scenes share \
items, and `stinger` for a branded wipe. A stinger preset (`transition.create`, kind \
`stinger`, about 800-1200 ms) names a `scene` it plays over the switch and a `cut_ms` \
(about half the duration) when the picture under it cuts. Build that scene like any \
graphic: full-height bars, panels or a logo plate that together cover the canvas at \
the cut. Each of its items comes in by its own `item.set_transition` `appear` effect \
and `delay_ms`, holds over the cut and goes by its `disappear` effect after it, so \
`slide` panels close in from the nearest edges and open out again (staggered by \
40-80 ms with `delay_ms`, or by stacking order with the preset's `stagger_ms`), and \
`scale` plates pop in and out; for a one-way sweep or anything more choreographed, use \
an HTML stinger. Make sure the bars cover the whole canvas at `cut_ms`, check \
with `video.snapshot` mid-switch, and make it the scenes' `transition_in` \
(`scene.set_transitions`) or name it in the switch. An HTML stinger is a `browser` \
source with `restart_on_show` on, so it plays from the start each time; the reload \
takes a page load, during which the stinger is transparent (never the end of its \
previous run), so start and end its animation transparent, keep it light (inline, \
no remote assets) and leave a few hundred ms before `cut_ms`. \
HTML graphics: for a layout the built-in kinds can't draw (rich type, icons, tables, \
SVG, CSS gradients, blend modes or 3D transforms, choreographed CSS animation), make a \
`browser` source and give it the whole page in `html`, its CSS and scripts inline. Set \
its `width` and `height` to the canvas and place its item full-frame, or to the \
graphic's bounds; the default `css` makes the page transparent, so draw only the \
graphic and leave `body` without a background. Fonts and images can come from https \
URLs (such as Google Fonts), not from local files. Animate the entrance with CSS \
`@keyframes` that run once on load, with `animation-fill-mode: both`. Continuous motion \
(a sway, a drift, a pulse, a scan) belongs on items, not in pages: `item.set_motion` \
gives an item up to 8 modifiers (`wiggle`, `oscillate` or `drift` on `x`, `y`, `scale`, \
`rotation`, `tilt`, `turn`, `depth`, `opacity` or a crop edge), which the engine draws \
each frame from one clock, so every item, mask and nested scene showing the same \
modifiers moves in step. Put a sway on a group's item to move the group and its masks as \
one, and give a keyframe scene its own list (`scene_id`) to calm or quicken it across a \
`move`. Use `clock` `shown` for motion that should start fresh each time the item is \
shown. For a one-shot jolt or shake (an impact, a kick), add an `impulse` modifier \
(`amplitude`, `frequency_hz`, `decay_per_s`, `duration_ms`) on `clock` `triggered` and \
fire it with `item.trigger_motion`, instead of stepping the item call by call; fire a \
group's item to shake the group and its masks together. \
For lights, meters or a logo that react to the music, use an `audio` modifier \
instead of a page that fakes the beat: it follows the program mix (or a bus, `audio`, or \
a source, `audio_source`) by `level`, `peak`, `onset` or a `band` (`low_hz`..`high_hz`, \
such as 40..120 for a kick), with `attack_ms` and `release_ms`, mapped from `floor_db` \
to `ceiling_db` onto `from`..`to`. On `opacity`, `from` -0.6 and `to` 0 dims a light to \
40% between beats. It hears only while an output runs. \
For an audio visualiser that the built-in `spectrum` and `waveform` kinds can't draw (a \
scrolling spectrogram, a custom meter), build an HTML page on `jstream.levels()`: set the \
`browser` source's `page_api`, `page_api_audio` and `page_api_bus` (the bus it shows, \
never a microphone bus it doesn't), and in the page listen for `levels` events, each \
`{peak: [l, r], rms: [l, r], bands}` with 64 bands from 20 Hz to 20 kHz, lowest first, \
each 0 to 1. A spectrogram draws each reading as one column of a canvas, shifting the \
canvas left by a column first; on a `null` reading (no output runs, or the bus is gone) \
it draws nothing. The page gets at most one reading per frame of its `fps`, and only \
while an output runs. Keep continuous motion \
subtle: an ambient sway or drift of a few px and a fraction of a degree reads as real, \
and more reads as unsteady; anything fast and thin (a streak, a line) must move less \
per frame than its own length or it flickers. \
A timed sequence (a build-up of reveals, a run of hits at set times, an intro then a \
loop) belongs in a timeline, not in a page: `timeline.set` stores in a scene a \
`timeline_id` (unique in the collection), a `duration_ms`, a `loop` (`once`, `loop`, \
or a `range` with `from_ms` and `to_ms` for an intro then a loop) and `cues` in time order, \
each `{at_ms, action}` running `scene.switch` (a keyframe scene to move to), \
`item.show`, `item.hide`, `item.toggle`, `item.trigger_motion`, `values.set`, \
`source.restart` (a page or video from its start) or another timeline's `play`, `stop` \
or `seek`. `timeline.play` runs it on the engine's clock, exact to the frame, and a \
hotkey can bind it. A cue whose item is gone fails without stopping the timeline, so \
after a first play read `timeline.cue_fired` in `events` for errors. \
Group the parts of one effect (its keyframe scenes, the hotkeys that switch to them, \
its timelines) under one label: `scene.set_meta` labels a keyframe scene, \
`hotkey.set_labels` a hotkey and a timeline's own `labels` the timeline, and \
`label.members` lists everything carrying the label in one call, so later you change \
them together, such as every keyframe's `scene.set_transitions` in one batch. \
Changing `html` \
reloads the page, which replays the entrance and blanks it for a moment, so put text \
that changes during a show in a `text` source over it. If creating the source fails \
because no browser is available on this system, build the graphic from the built-in \
kinds instead. \
Check your work: after each step take a `video.snapshot` and look at it critically \
(alignment, overlap, legibility, crowding, anything off the safe area) and fix what's \
wrong before reporting. When a scene is on program, build changes on another scene or \
on preview (`studio.set_preview`) and switch when they're right.";

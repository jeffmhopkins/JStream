//! Design guidance for agents that build what goes on screen, appended to the server
//! instructions (`server.rs`), so every client reads it before its first edit.
//!
//! A live test (#1018-#1021's thread) found agents could build any graphic the tools
//! allow and still make poor ones: flat full-saturation colours, thin type, boxes that
//! didn't fit their text, nothing aligned. The tools can't fix taste, so this says what
//! a broadcast designer would: where things go, which sizes, which faces and colours,
//! and to look at the result before calling it done. It names the `backdrop` and `text`
//! settings that make graphics look finished, the `shape` kind, how to group, scroll
//! and animate, how to use 3D placement and blend modes, how to build a 3D set with a
//! scene camera (ADR-0094), which transition to use (including a `stinger`), and when
//! to build a graphic as HTML in a `browser` source. Numbers are for a 1920×1080 canvas
//! and scale with it.

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
A graphic of several parts that should read as one (a scoreboard, a framed camera \
with its name bar): build it in its own scene, place that scene with a `scene` source, \
crop the item to the graphic's bounds, and put `rounded_corners` and `drop_shadow` in \
the item's effects (`filter.add` with owner `item`), which clip and shadow it as one. \
Fit boxes to content: a new text source draws at its own size, so read it from \
`source.sizes` (or a `source.size_changed` event) and give its item that size, rather \
than guessing a box; with the default `fit` mode a wrong box shrinks or offsets the \
text. \
Motion: animate with keyframe scenes. Build the graphic's off state as a scene (a \
plate 10 px wide or cropped to nothing, text at opacity 0), add its on state with \
`scene.create_variant`, override geometry, crop and opacity per item there, make a \
`move` transition preset (`transition.create`, 500-800 ms, `ease_out`) and switch \
between them with it. Reveal text with a crop that opens with its plate rather than a \
fade over it, and stagger the parts with `item.set_transition` `delay_ms` (0, 120, \
200). Items with no partner in the other scene appear and disappear by their \
transition's `appear` and `disappear` (`slide`, `scale` or `fade`). Keep moves under a \
second and ease them; a cut beats a slow move. \
Depth and blending: a keyframe variant can override an item's 3D placement, so a `move` \
interpolates it: `tilt_deg` (top edge away), `turn_deg` (right edge away), `depth_px` \
and `perspective_px` (about 1200-2000 for a natural look). A plate that flips down \
from `tilt_deg` -90 to 0, or a card that turns in from `turn_deg` 70, reads as a \
designed move; keep angles under 90 and pair them with opacity. `item.set_blend` sets \
how an item composites: `screen` or `additive` for light leaks, glints and glows over \
the picture, `multiply` for shadows and textures, `overlay` for a tinted wash; most \
items stay `normal`. \
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
variant) and a `move` transition, which glides the camera with the items. Put \
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
takes a page load, and until the new page paints the stinger shows the last frame \
of its previous run, so start and end its animation transparent, keep it light \
(inline, no remote assets) and leave a few hundred ms before `cut_ms`. \
HTML graphics: for a layout the built-in kinds can't draw (rich type, icons, tables, \
SVG, CSS gradients, blend modes or 3D transforms, choreographed CSS animation), make a \
`browser` source and give it the whole page in `html`, its CSS and scripts inline. Set \
its `width` and `height` to the canvas and place its item full-frame, or to the \
graphic's bounds; the default `css` makes the page transparent, so draw only the \
graphic and leave `body` without a background. Fonts and images can come from https \
URLs (such as Google Fonts), not from local files. Animate the entrance with CSS \
`@keyframes` that run once on load, with `animation-fill-mode: both`. Changing `html` \
reloads the page, which replays the entrance and blanks it for a moment, so put text \
that changes during a show in a `text` source over it. If creating the source fails \
because no browser is available on this system, build the graphic from the built-in \
kinds instead. \
Check your work: after each step take a `video.snapshot` and look at it critically \
(alignment, overlap, legibility, crowding, anything off the safe area) and fix what's \
wrong before reporting. When a scene is on program, build changes on another scene or \
on preview (`studio.set_preview`) and switch when they're right.";

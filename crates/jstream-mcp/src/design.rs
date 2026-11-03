//! Design guidance for agents that build what goes on screen, appended to the server
//! instructions (`server.rs`), so every client reads it before its first edit.
//!
//! A live test (#1018-#1021's thread) found agents could build any graphic the tools
//! allow and still make poor ones: flat full-saturation colours, thin type, boxes that
//! didn't fit their text, nothing aligned. The tools can't fix taste, so this says what
//! a broadcast designer would: where things go, which sizes, which faces and colours,
//! and to look at the result before calling it done. It names the `backdrop` and `text`
//! settings that make graphics look finished, the `shape` kind, and how to group,
//! scroll and animate. Numbers are for a 1920×1080 canvas and scale with it.

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
Check your work: after each step take a `video.snapshot` and look at it critically \
(alignment, overlap, legibility, crowding, anything off the safe area) and fix what's \
wrong before reporting. When a scene is on program, build changes on another scene or \
on preview (`studio.set_preview`) and switch when they're right.";

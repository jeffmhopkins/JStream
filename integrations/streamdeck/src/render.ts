/**
 * Key images, drawn as SVG: a glyph for what the key does on a background that lights
 * up while what it controls is on (a scene on program, an item shown, a source muted, a
 * sequence playing, an effect on, an output live, a channel on). The key's title, which
 * the person can change in Stream Deck, names the target.
 */

export type Glyph = "scene" | "keyframe" | "eye" | "eye-off" | "speaker" | "speaker-off" | "play" | "stop" | "effect" | "record" | "stream" | "power" | "bolt" | "pause" | "continue" | "back" | "skip" | "bookmark";

/** How a key looks. */
export type Look =
	/** What the key controls is on. */
	| "on"
	/** Starting or stopping, or waiting to start. */
	| "busy"
	/** Off. */
	| "off"
	/** The key's target isn't set, or no longer exists. */
	| "unset"
	/** Not connected to JStream. */
	| "offline";

/** Lit colours, by what lights them. */
export const COLORS = {
	program: "#d93636",
	preview: "#2f9e44",
	shown: "#2f9e44",
	muted: "#d93636",
	playing: "#e8890c",
	effect: "#3b7ddd",
	live: "#d93636",
	on: "#2f9e44",
	keyframe: "#8e5bd6",
	show: "#2f9e44",
	action: "#5c6370",
} as const;

const OFF_BG = "#1f2329";
const OFF_FG = "#c9ced6";
const DIM_FG = "#5c6370";
const BUSY = "#e8890c";

export const GLYPHS: Record<Glyph, string> = {
	scene: '<rect x="34" y="40" width="76" height="50" rx="6" fill="none" stroke="FG" stroke-width="8"/><path d="M58 104h28" stroke="FG" stroke-width="8" stroke-linecap="round"/>',
	keyframe: '<path d="M72 30l34 34-34 34-34-34z" fill="none" stroke="FG" stroke-width="8" stroke-linejoin="round"/>',
	eye: '<path d="M26 64c12-20 28-30 46-30s34 10 46 30c-12 20-28 30-46 30S38 84 26 64z" fill="none" stroke="FG" stroke-width="8" stroke-linejoin="round"/><circle cx="72" cy="64" r="12" fill="FG"/>',
	"eye-off": '<path d="M26 64c12-20 28-30 46-30s34 10 46 30c-12 20-28 30-46 30S38 84 26 64z" fill="none" stroke="FG" stroke-width="8" stroke-linejoin="round"/><path d="M36 100l72-72" stroke="FG" stroke-width="8" stroke-linecap="round"/>',
	speaker: '<path d="M32 52h18l24-20v64L50 76H32z" fill="FG"/><path d="M88 46c8 10 8 26 0 36M100 36c14 16 14 40 0 56" fill="none" stroke="FG" stroke-width="7" stroke-linecap="round"/>',
	"speaker-off": '<path d="M32 52h18l24-20v64L50 76H32z" fill="FG"/><path d="M88 50l24 28M112 50L88 78" stroke="FG" stroke-width="8" stroke-linecap="round"/>',
	play: '<path d="M52 34l46 30-46 30z" fill="FG" stroke="FG" stroke-width="6" stroke-linejoin="round"/>',
	stop: '<rect x="44" y="36" width="56" height="56" rx="6" fill="FG"/>',
	effect: '<path d="M72 26l9 27 27 9-27 9-9 27-9-27-27-9 27-9z" fill="FG"/><path d="M106 88l4 10 10 4-10 4-4 10-4-10-10-4 10-4z" fill="FG"/>',
	record: '<circle cx="72" cy="64" r="30" fill="FG"/>',
	stream: '<circle cx="72" cy="64" r="10" fill="FG"/><path d="M52 44a28 28 0 0 0 0 40M92 44a28 28 0 0 1 0 40M38 30a48 48 0 0 0 0 68M106 30a48 48 0 0 1 0 68" fill="none" stroke="FG" stroke-width="7" stroke-linecap="round"/>',
	power: '<path d="M72 30v32" stroke="FG" stroke-width="8" stroke-linecap="round"/><path d="M52 44a30 30 0 1 0 40 0" fill="none" stroke="FG" stroke-width="8" stroke-linecap="round"/>',
	pause: '<rect x="46" y="36" width="18" height="56" rx="4" fill="FG"/><rect x="80" y="36" width="18" height="56" rx="4" fill="FG"/>',
	continue: '<path d="M36 36l34 28-34 28zM74 36l34 28-34 28z" fill="FG" stroke="FG" stroke-width="4" stroke-linejoin="round"/>',
	back: '<rect x="38" y="36" width="12" height="56" rx="3" fill="FG"/><path d="M104 36L58 64l46 28z" fill="FG" stroke="FG" stroke-width="4" stroke-linejoin="round"/>',
	skip: '<rect x="94" y="36" width="12" height="56" rx="3" fill="FG"/><path d="M40 36l46 28-46 28z" fill="FG" stroke="FG" stroke-width="4" stroke-linejoin="round"/>',
	bookmark: '<path d="M48 30h48v68L72 82 48 98z" fill="none" stroke="FG" stroke-width="8" stroke-linejoin="round"/>',
	bolt: '<path d="M80 26L44 72h24l-6 34 38-48H76z" fill="FG" stroke="FG" stroke-width="4" stroke-linejoin="round"/>',
};

/** The key image for `glyph` in `look`, lit with `color`, as a data URL Stream Deck takes. */
export function keyImage(glyph: Glyph, look: Look, color: string = COLORS.on): string {
	return `data:image/svg+xml;charset=utf8,${encodeURIComponent(keySvg(glyph, look, color))}`;
}

/** The key image as SVG text. */
export function keySvg(glyph: Glyph, look: Look, color: string = COLORS.on): string {
	let bg = OFF_BG;
	let fg = OFF_FG;
	let ring = "";
	switch (look) {
		case "on":
			bg = color;
			fg = "#ffffff";
			break;
		case "busy":
			ring = `<rect x="5" y="5" width="134" height="134" rx="18" fill="none" stroke="${BUSY}" stroke-width="10"/>`;
			break;
		case "unset":
		case "offline":
			fg = DIM_FG;
			break;
	}
	const badge =
		look === "offline"
			? '<circle cx="118" cy="26" r="12" fill="#d93636"/><path d="M118 19v9M118 32v1" stroke="#fff" stroke-width="4" stroke-linecap="round"/>'
			: look === "unset"
				? '<circle cx="118" cy="26" r="12" fill="#5c6370"/><path d="M114 22a4 4 0 1 1 4 5v2M118 33v1" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round"/>'
				: "";
	return (
		`<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">` +
		`<rect width="144" height="144" rx="20" fill="${bg}"/>${ring}` +
		`<g transform="translate(0 -6)">${GLYPHS[glyph].replaceAll("FG", fg)}</g>${badge}</svg>`
	);
}

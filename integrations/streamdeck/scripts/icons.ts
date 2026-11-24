/**
 * Writes the plugin's images from the key glyphs in src/render.ts: each action's list
 * icon (white on transparent) and default key image, the category icon, and the
 * plugin icon as SVG (scripts/icons-png.mjs turns that into the PNGs Stream Deck asks for).
 * Run: node --experimental-strip-types scripts/icons.ts
 */
import { mkdirSync, writeFileSync } from "node:fs";

import { GLYPHS, type Glyph, keySvg } from "../src/render.ts";

const root = "io.github.jeffmhopkins.jstream.sdPlugin/imgs";
const actions: Record<string, Glyph> = {
	show: "continue",
	scene: "scene",
	keyframe: "keyframe",
	visibility: "eye",
	mute: "speaker",
	sequence: "play",
	effect: "effect",
	output: "stream",
	channel: "power",
	action: "bolt",
};

const icon = (glyph: Glyph) =>
	`<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40" viewBox="18 18 108 108">${GLYPHS[glyph].replaceAll("FG", "#ffffff")}</svg>\n`;

for (const [name, glyph] of Object.entries(actions)) {
	mkdirSync(`${root}/actions/${name}`, { recursive: true });
	writeFileSync(`${root}/actions/${name}/icon.svg`, icon(glyph));
	writeFileSync(`${root}/actions/${name}/key.svg`, keySvg(glyph, "off") + "\n");
}

const mark = (size: number, bg: boolean) =>
	`<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 144 144">` +
	(bg ? '<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#3b7ddd"/><stop offset="1" stop-color="#8e5bd6"/></linearGradient></defs><rect width="144" height="144" rx="30" fill="url(#g)"/>' : "") +
	'<path d="M84 30v58a22 22 0 0 1-44 0" fill="none" stroke="#ffffff" stroke-width="16" stroke-linecap="round"/>' +
	'<path d="M98 52l22 14-22 14z" fill="#ffffff"/></svg>\n';

mkdirSync(`${root}/plugin`, { recursive: true });
writeFileSync(`${root}/plugin/category-icon.svg`, mark(28, false));
writeFileSync(`${root}/plugin/marketplace.svg`, mark(256, true));

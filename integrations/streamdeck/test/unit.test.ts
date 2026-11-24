/** The plugin's pure parts: lists, labels, key images, fingerprints and discovery answers. */
import assert from "node:assert/strict";
import { test } from "node:test";

import { Collector, SERVICE } from "../src/jstream/discovery.ts";
import type { StateSnapshot } from "../src/jstream/protocol.ts";
import { effectKey, findEffect, listEffects, parseEffectKey } from "../src/jstream/studio.ts";
import { normalizeFingerprint, pairingRefusal, urlHost } from "../src/jstream/tls.ts";
import { fitTitle, lists, outputLabel, wantsOn } from "../src/lists.ts";
import { keyImage, keySvg } from "../src/render.ts";

const STATE: StateSnapshot = {
	rev: 3,
	scenes: [
		{
			scene_id: "scn_1",
			name: "Main",
			items: [
				{ item_id: "itm_1", source_id: "src_cam", visible: true, effects: [{ filter_id: "flt_frame", kind: "vignette", enabled: true }] },
				{ item_id: "itm_2", source_id: "src_mic", visible: false },
			],
			filters: { video: [{ filter_id: "flt_grade", kind: "lut", enabled: false }] },
			timelines: [{ timeline_id: "tl_1", name: "Lights up" }, { timeline_id: "tl_2" }],
		},
		{ scene_id: "scn_1_wide", name: "Main, wide", variant_of: "scn_1" },
	],
	sources: [
		{ source_id: "src_cam", name: "Camera", filters: { video: [{ filter_id: "flt_lut", kind: "pack.lut", enabled: true }] } },
		{ source_id: "src_mic", name: "Mic", audio: { enabled: true, muted: true }, filters: { audio: [{ filter_id: "flt_gain", kind: "gain", enabled: true }] } },
	],
	outputs: [
		{ output_id: "out_rec", kind: { type: "record", path: "C:\\Videos\\show.mp4" } },
		{ output_id: "out_yt", kind: { type: "rtmp", target: { type: "generic", url: "rtmp://a.rtmp.youtube.com/live2" } } },
	],
	channels: [{ channel_id: "power", name: "Vehicle power" }, { channel_id: "x" }],
	shows: [{ show_id: "evening", name: "Evening show", bookmarks: [{ bookmark_id: "start", name: "Start" }, { bookmark_id: "demo" }] }],
	transitions: { presets: [{ preset_id: "cut", name: "Cut" }], default: "cut" },
	hotkeys: [
		{ chord: "Ctrl+Digit1", action: { type: "scene.switch", scene_id: "scn_1" } },
		{ chord: "F9", action: { type: "output.start", output_id: "out_yt" } },
	],
};

test("the lists name every target a key can pick, from the live state", () => {
	const l = lists(STATE) as Record<string, any>;
	assert.deepEqual(l.scenes, [{ value: "scn_1", label: "Main" }]);
	assert.deepEqual(l.keyframes, [{ value: "scn_1_wide", label: "Main, wide", group: "Main" }]);
	assert.deepEqual(l.items.scn_1, [
		{ value: "itm_1", label: "Camera" },
		{ value: "itm_2", label: "Mic" },
	]);
	assert.deepEqual(
		l.sources.map((s: any) => s.group),
		["Other sources", "With audio"],
	);
	assert.deepEqual(l.sequences, [
		{ value: "tl_1", label: "Lights up", group: "Main" },
		{ value: "tl_2", label: "tl_2", group: "Main" },
	]);
	assert.deepEqual(
		l.outputs.map((o: any) => o.label),
		["Record: show.mp4", "Stream: a.rtmp.youtube.com (out_yt)"],
	);
	assert.deepEqual(
		l.channels.map((c: any) => c.label),
		["Vehicle power", "x"],
	);
	assert.deepEqual(
		l.hotkeys.map((h: any) => h.label),
		["Ctrl+Digit1: switch to Main", "F9: output.start out_yt"],
	);
	assert.deepEqual(l.shows, [{ value: "evening", label: "Evening show" }]);
	assert.deepEqual(l.bookmarks.evening, [
		{ value: "start", label: "Start" },
		{ value: "demo", label: "demo" },
	]);
	assert.deepEqual(l.transitions, [{ value: "cut", label: "Cut" }]);
	assert.deepEqual(lists(undefined), {});
});

test("effects are listed from sources, scenes and items, and found again by key", () => {
	const effects = listEffects(STATE);
	assert.deepEqual(
		effects.map((e) => e.label),
		["Camera › pack.lut", "Mic › gain", "Scene Main › lut", "Main › Camera › vignette"],
	);
	const item = effects[3];
	assert.equal(effectKey(item.owner), "item:itm_1");
	assert.deepEqual(parseEffectKey("item:itm_1"), { type: "item", item_id: "itm_1" });
	assert.deepEqual(parseEffectKey("scene:scn_1"), { type: "scene", scene_id: "scn_1" });
	assert.equal(parseEffectKey("output:out_1"), undefined);
	assert.equal(parseEffectKey("nonsense"), undefined);
	assert.equal(findEffect(STATE, "scene:scn_1", "flt_grade")?.enabled, false);
	assert.equal(findEffect(STATE, "scene:scn_1", "flt_lut"), undefined);
});

test("a key's press follows its mode", () => {
	assert.equal(wantsOn("toggle", true), false);
	assert.equal(wantsOn(undefined, false), true);
	assert.equal(wantsOn("on", true), true);
	assert.equal(wantsOn("off", false), false);
});

test("long names break onto two lines", () => {
	assert.equal(fitTitle("Main"), "Main");
	assert.equal(fitTitle("Lower third left"), "Lower\nthird left");
	assert.equal(fitTitle("Supercalifragilistic"), "Supercalifragilistic");
	assert.equal(fitTitle(undefined), undefined);
});

test("outputs without a path or URL still get a name", () => {
	assert.equal(outputLabel({ output_id: "out_1", kind: { type: "record" } }), "Record (out_1)");
	assert.equal(outputLabel({ output_id: "out_2", kind: { type: "rtmp", target: { type: "youtube" } } }), "Stream: YouTube (out_2)");
	assert.equal(outputLabel({ output_id: "out_3", kind: { type: "virtual_cam" } }), "Virtual camera");
});

test("key images light up in the colour given, and mark offline and unset keys", () => {
	assert.match(keySvg("scene", "on", "#d93636"), /fill="#d93636"/);
	assert.doesNotMatch(keySvg("scene", "off", "#d93636"), /#d93636/);
	assert.match(keySvg("eye", "offline"), /#d93636/, "the offline badge is red");
	assert.match(keySvg("eye", "busy"), /stroke="#e8890c"/);
	assert.ok(keyImage("bolt", "off").startsWith("data:image/svg+xml;charset=utf8,%3Csvg"));
});

test("fingerprints are compared in one form", () => {
	const fp = "3F:A1:07:C2:9B:44:E8:15:6D:0A:B3:72:C9:5E:21:8F:D4:66:0B:E7:13:A9:5C:F0:28:7D:B1:4E:93:C6:0F:52";
	assert.equal(normalizeFingerprint(fp.toLowerCase()), fp);
	assert.equal(normalizeFingerprint(fp.replaceAll(":", " ")), fp);
	assert.equal(normalizeFingerprint("3F:A1"), undefined);
	assert.equal(urlHost("fe80::1"), "[fe80::1]");
	assert.equal(urlHost("studio.local"), "studio.local");
	assert.match(pairingRefusal(401, ""), /No pairing code is open/);
	assert.match(pairingRefusal(403, "wrong code; 4 attempts left"), /4 attempts left/);
});

test("discovery pieces DNS-SD answers together across packets", () => {
	const c = new Collector();
	const instance = `Studio PC.${SERVICE}`;
	assert.deepEqual(c.add([{ name: SERVICE, type: "PTR", data: instance }], "192.168.1.20"), []);
	const found = c.add(
		[
			{ name: instance, type: "SRV", data: { port: 4456, target: "studio.local" } },
			{ name: instance, type: "TXT", data: [] },
		],
		"192.168.1.20",
	);
	assert.deepEqual(found, [{ name: "Studio PC", host: "192.168.1.20", port: 4456, hostname: "studio.local" }]);
	// An A record for the target wins over the packet's source address.
	const again = c.add([{ name: "studio.local", type: "A", data: "10.0.0.5" }], "192.168.1.20");
	assert.equal(again[0].host, "10.0.0.5");
	// Other services are ignored.
	assert.deepEqual(new Collector().add([{ name: "_http._tcp.local", type: "PTR", data: "x._http._tcp.local" }], "1.2.3.4"), []);
});

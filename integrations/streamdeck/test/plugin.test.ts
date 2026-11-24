/**
 * The built plugin (`npm run build`) run as Stream Deck runs it, against a stand-in for
 * Stream Deck's socket and a real `jstream-daemon --lan`: a key appears offline, the
 * property inspector pairs it, the key then shows its scene, and pressing it switches
 * program and lights it.
 *
 * Runs only when JSTREAM_DAEMON names a built daemon (see daemon.test.ts).
 */
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { after, before, test } from "node:test";
import { WebSocketServer, type WebSocket } from "ws";

import { call, fingerprint, lanHost, lanPort, skip, startDaemon, stopDaemon, until } from "./daemon.ts";

const PLUGIN = "io.github.jeffmhopkins.jstream";
const DIR = `${PLUGIN}.sdPlugin`;

let server: WebSocketServer;
let plugin: ChildProcess | undefined;
let socket: WebSocket | undefined;
let globalSettings: Record<string, unknown> = {};
const received: any[] = [];

before(async () => {
	if (skip) return;
	assert.ok(existsSync(`${DIR}/bin/plugin.js`), "run `npm run build` first");
	await startDaemon();
	server = new WebSocketServer({ port: 0, host: "127.0.0.1" });
	await new Promise((r) => server.once("listening", r));
	server.on("connection", (ws) => {
		ws.on("message", (data) => {
			const m = JSON.parse(data.toString());
			received.push(m);
			if (m.event === "registerPlugin") socket = ws;
			if (m.event === "getGlobalSettings") {
				ws.send(JSON.stringify({ event: "didReceiveGlobalSettings", payload: { settings: globalSettings }, id: m.id }));
			}
			if (m.event === "setGlobalSettings") globalSettings = m.payload;
		});
	});
	const info = {
		application: { font: "", language: "en", platform: "windows", platformVersion: "10.0", version: "7.1.0.1" },
		plugin: { uuid: PLUGIN, version: "0.1.0.0" },
		devicePixelRatio: 2,
		colors: {},
		devices: [{ id: "dev1", name: "Stream Deck", size: { columns: 5, rows: 3 }, type: 0 }],
	};
	const port = (server.address() as { port: number }).port;
	plugin = spawn(process.execPath, ["bin/plugin.js", "-port", String(port), "-pluginUUID", PLUGIN, "-registerEvent", "registerPlugin", "-info", JSON.stringify(info)], {
		cwd: DIR,
		stdio: "ignore",
	});
	await until("the plugin to register", () => socket !== undefined, 10000);
});

after(() => {
	plugin?.kill();
	server?.close();
	stopDaemon();
});

const send = (m: unknown) => socket!.send(JSON.stringify(m));
const last = (event: string, context = "ctx1") => [...received].reverse().find((m) => m.event === event && m.context === context);
const image = () => decodeURIComponent(String(last("setImage")?.payload?.image ?? ""));
const toInspector = (type: string) => [...received].reverse().find((m) => m.event === "sendToPropertyInspector" && m.payload?.type === type);

test("a scene key pairs from its property inspector, then switches program and lights up", { skip }, async () => {
	const key = { action: `${PLUGIN}.scene`, context: "ctx1", device: "dev1" };
	const payload = { controller: "Keypad", coordinates: { column: 0, row: 0 }, isInMultiAction: false, settings: { scene_id: "scn_b" } };
	send({ event: "willAppear", ...key, payload });
	await until("the key drawn offline", () => image().includes("#d93636") && image().includes('fill="#1f2329"'));

	// The property inspector opens, probes JStream and pairs with the code JStream shows.
	send({ event: "propertyInspectorDidAppear", ...key });
	send({ event: "sendToPlugin", ...key, payload: { type: "hello" } });
	await until("the connection status", () => toInspector("connection")?.payload.paired === false);
	send({ event: "sendToPlugin", ...key, payload: { type: "probe", host: lanHost(), port: lanPort } });
	await until("the fingerprint", () => toInspector("probed") !== undefined || toInspector("failed") !== undefined, 8000);
	assert.equal(toInspector("probed")?.payload.fingerprint, fingerprint);
	const { code } = await call({ kind: "action", action: { type: "pairing.start" } });
	send({ event: "sendToPlugin", ...key, payload: { type: "pair", host: lanHost(), port: lanPort, fingerprint, code, name: "Stream Deck (test)" } });
	await until("paired", () => toInspector("paired") !== undefined || toInspector("failed") !== undefined, 8000);
	assert.equal(toInspector("failed"), undefined, JSON.stringify(toInspector("failed")?.payload));
	assert.equal(globalSettings.fingerprint, fingerprint);
	assert.match(String(globalSettings.token), /^[0-9a-f]{64}$/);

	// Connected, the key shows its scene's name, unlit.
	await until("the key titled", () => last("setTitle")?.payload?.title === "Main", 8000);
	assert.ok(image().includes('fill="#1f2329"') && !image().includes("#d93636"), "the key is unlit and online");
	// The inspector's lists come from the live state.
	await until("the lists", () => toInspector("lists")?.payload.lists.scenes?.length === 2);
	assert.deepEqual(toInspector("lists")!.payload.lists.transitions.map((t: { label: string }) => t.label).slice(0, 1), ["Cut"]);

	// Pressing the key switches program, and the key lights red.
	send({ event: "keyDown", ...key, payload });
	await until("the key lit", () => image().includes('fill="#d93636"'));
	const studio = await call({ kind: "query", query: { type: "studio.get" } });
	assert.equal(studio.program_scene, "scn_b");
	assert.equal(last("showAlert"), undefined, "the press didn't fail");

	// A scene key with a keyframe puts program on the scene at that keyframe.
	await call({ kind: "action", action: { type: "scene.switch", scene_id: "scn_a" } });
	await until("the first key unlit", () => !image().includes('fill="#d93636"'));
	const wide = { action: `${PLUGIN}.scene`, context: "ctx2", device: "dev1" };
	const widePayload = { ...payload, coordinates: { column: 1, row: 0 }, settings: { scene_id: "scn_b", keyframe_id: "scn_b_wide" } };
	send({ event: "willAppear", ...wide, payload: widePayload });
	await until("the keyframe key titled", () => last("setTitle", "ctx2")?.payload?.title?.replace("\n", " ") === "Main, wide", 8000);
	send({ event: "keyDown", ...wide, payload: widePayload });
	let at: { program_scene?: string; keyframes?: { keyframe_id: string }[] } = {};
	for (let i = 0; i < 80 && !(at.program_scene === "scn_b" && (at.keyframes ?? []).some((k) => k.keyframe_id === "scn_b_wide")); i++) {
		await new Promise((r) => setTimeout(r, 50));
		at = await call({ kind: "query", query: { type: "studio.get" } });
	}
	assert.equal(at.program_scene, "scn_b");
	assert.ok((at.keyframes ?? []).some((k) => k.keyframe_id === "scn_b_wide"), JSON.stringify(at.keyframes));
	assert.equal(last("showAlert", "ctx2"), undefined, "the keyframe press didn't fail");
	assert.deepEqual(toInspector("lists")!.payload.lists.sceneKeyframes, { scn_b: [{ value: "scn_b_wide", label: "Main, wide" }] });
});

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

import { call, events, fingerprint, heard, lanHost, lanPort, skip, startDaemon, stopDaemon, until } from "./daemon.ts";

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
const image = (context?: string) => decodeURIComponent(String(last("setImage", context)?.payload?.image ?? ""));
const toInspector = (type: string) => [...received].reverse().find((m) => m.event === "sendToPropertyInspector" && m.payload?.type === type);

test("a scene key pairs from its property inspector, then switches program and lights up", { skip }, async () => {
	const key = { action: `${PLUGIN}.scene`, context: "ctx1", device: "dev1" };
	const payload = { controller: "Keypad", coordinates: { column: 0, row: 0 }, isInMultiAction: false, settings: { scene_id: "scn_b" } };
	send({ event: "willAppear", ...key, payload });
	await until("the key drawn offline", () => image().includes("#d93636") && image().includes('fill="#1f2329"'));

	// The property inspector opens, probes JStream and asks it to pair; the person
	// presses Allow on JStream's screen (ADR-0235).
	send({ event: "propertyInspectorDidAppear", ...key });
	send({ event: "sendToPlugin", ...key, payload: { type: "hello" } });
	await until("the connection status", () => toInspector("connection")?.payload.paired === false);
	send({ event: "sendToPlugin", ...key, payload: { type: "probe", host: lanHost(), port: lanPort } });
	await until("the fingerprint", () => toInspector("probed") !== undefined || toInspector("failed") !== undefined, 8000);
	assert.equal(toInspector("probed")?.payload.fingerprint, fingerprint);
	const from = events.length;
	send({ event: "sendToPlugin", ...key, payload: { type: "ask", host: lanHost(), port: lanPort, fingerprint, name: "Stream Deck (test)" } });
	// Asked again while the first waits (Connect pressed twice), the plugin keeps the one.
	send({ event: "sendToPlugin", ...key, payload: { type: "ask", host: lanHost(), port: lanPort, fingerprint, name: "Stream Deck (test)" } });
	const asked = await heard("pairing.asked", from);
	assert.equal(asked.ask.name, "Stream Deck (test)");
	await call({ kind: "action", action: { type: "pairing.answer", ask_id: asked.ask.ask_id, allow: true } });
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

test("a show's Play key toggles: it plays, pauses, and continues past a bookmark", { skip }, async () => {
	await call({
		kind: "batch",
		edits: [
			{
				type: "show.set",
				show: {
					show_id: "toggle",
					name: "Toggle",
					program: [
						{ clip_id: "one", scene_id: "scn_a", length_ms: 30000 },
						{ clip_id: "two", scene_id: "scn_b", length_ms: 30000 },
					],
					bookmarks: [{ bookmark_id: "bm_two", name: "Two", at_ms: 30000 }],
				},
			},
		],
	});
	const key = { action: `${PLUGIN}.show`, context: "ctx-show", device: "dev1" };
	const payload = { controller: "Keypad", coordinates: { column: 2, row: 0 }, isInMultiAction: false, settings: { show_id: "toggle", command: "play" } };
	send({ event: "willAppear", ...key, payload });
	await until("the show key titled", () => last("setTitle", "ctx-show")?.payload?.title === "Toggle", 8000);
	await until("the show key drawn", () => image("ctx-show") !== "");
	// How the Play key looks with no show running.
	const idle = image("ctx-show");
	const state = async () => (await call({ kind: "query", query: { type: "studio.get" } })).show?.state as string | undefined;
	// Presses the key, waits for the show to be `want`, then for the key to redraw for it,
	// so the next press acts on what the key shows.
	const press = async (want: string) => {
		const before = image("ctx-show");
		send({ event: "keyDown", ...key, payload });
		let at: string | undefined;
		for (let i = 0; i < 80 && at !== want; i++) {
			await new Promise((r) => setTimeout(r, 50));
			at = await state();
		}
		assert.equal(at, want);
		await until("the key redrawn", () => image("ctx-show") !== before);
		assert.equal(last("showAlert", "ctx-show"), undefined, "the press didn't fail");
	};

	// Not running: it plays. Playing: it pauses. Paused: it continues.
	await press("playing");
	await press("paused");
	await press("playing");
	// Waiting at a pausing bookmark: it continues from there.
	const playing = image("ctx-show");
	await call({ kind: "action", action: { type: "show.go_to", show_id: "toggle", bookmark_id: "bm_two" } });
	for (let i = 0; i < 80 && (await state()) !== "paused"; i++) await new Promise((r) => setTimeout(r, 50));
	await until("the key waiting", () => image("ctx-show") !== playing);
	await press("playing");
	const program = async () => (await call({ kind: "query", query: { type: "studio.get" } })).program_scene as string;
	assert.equal(await program(), "scn_b", "it went on from the bookmark, not from the start");

	// Restart plays it from its start; Next jumps to the next bookmark and waits there.
	const other = (context: string, command: string, column: number) => {
		const k = { action: `${PLUGIN}.show`, context, device: "dev1" };
		const p = { ...payload, coordinates: { column, row: 0 }, settings: { show_id: "toggle", command } };
		send({ event: "willAppear", ...k, payload: p });
		return () => send({ event: "keyDown", ...k, payload: p });
	};
	const restart = other("ctx-restart", "restart", 3);
	const next = other("ctx-next", "skip", 4);
	await until("the Next key titled", () => last("setTitle", "ctx-next")?.payload?.title === "Toggle", 8000);
	restart();
	for (let i = 0; i < 80 && (await program()) !== "scn_a"; i++) await new Promise((r) => setTimeout(r, 50));
	assert.equal(await program(), "scn_a", "restarted from the start");
	assert.equal(await state(), "playing");
	next();
	for (let i = 0; i < 80 && (await program()) !== "scn_b"; i++) await new Promise((r) => setTimeout(r, 50));
	assert.equal(await program(), "scn_b", "Next went to the bookmark");
	assert.equal(await state(), "paused");
	assert.equal(last("showAlert", "ctx-restart"), undefined);
	assert.equal(last("showAlert", "ctx-next"), undefined);
	await call({ kind: "action", action: { type: "show.stop" } });
	await until("the plugin sees the show stopped", () => image("ctx-show") === idle);

	// With no show picked, a key follows the selected show: here the one JStream's Show
	// panel has open (`show.open`), among two.
	await call({
		kind: "batch",
		edits: [{ type: "show.set", show: { show_id: "other", name: "Other", program: [{ clip_id: "o", scene_id: "scn_b", length_ms: 30000 }], bookmarks: [] } }],
	});
	await call({ kind: "action", action: { type: "ui_settings.set", key: "show.open", value: "toggle" } });
	const selected = { action: `${PLUGIN}.show`, context: "ctx-selected", device: "dev1" };
	const selectedPayload = { ...payload, coordinates: { column: 0, row: 1 }, settings: { command: "play" } };
	send({ event: "willAppear", ...selected, payload: selectedPayload });
	await until("the selected key titled", () => last("setTitle", "ctx-selected")?.payload?.title === "Toggle", 8000);
	send({ event: "keyDown", ...selected, payload: selectedPayload });
	let running: { show_id?: string } | undefined;
	for (let i = 0; i < 80 && running?.show_id !== "toggle"; i++) {
		await new Promise((r) => setTimeout(r, 50));
		running = (await call({ kind: "query", query: { type: "studio.get" } })).show;
	}
	assert.equal(running?.show_id, "toggle", "played the show open in JStream");
	assert.equal(last("showAlert", "ctx-selected"), undefined);
	// Opening the other one there moves the key to it, once the running show stops.
	await call({ kind: "action", action: { type: "show.stop" } });
	await call({ kind: "action", action: { type: "ui_settings.set", key: "show.open", value: "other" } });
	await until("the selected key follows", () => last("setTitle", "ctx-selected")?.payload?.title === "Other", 8000);
});

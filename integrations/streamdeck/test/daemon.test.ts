/**
 * The plugin's connection against a real `jstream-daemon --lan`: finding it, probing the
 * certificate, pairing with a code, connecting over the pinned TLS listener with the
 * device token, reading the studio, and the actions and edits each key sends, with the
 * state they light up from.
 *
 * Runs only when JSTREAM_DAEMON names a built daemon, for example:
 *   cargo build -p jstream-daemon
 *   JSTREAM_DAEMON=../../target/debug/jstream-daemon npm test
 */
import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { JStreamClient } from "../src/jstream/client.ts";
import { discover } from "../src/jstream/discovery.ts";
import { Studio, effectKey, findEffect } from "../src/jstream/studio.ts";
import { FingerprintMismatch, askToPair, pair, probeFingerprint } from "../src/jstream/tls.ts";
import { call, events, fingerprint, heard, lanHost, lanPort, skip, startDaemon, stopDaemon, until } from "./daemon.ts";

before(async () => {
	if (!skip) await startDaemon();
});
after(stopDaemon);

test("the plugin finds JStream on the network by its mDNS announcement", { skip }, async () => {
	// The daemon announces `_jstream._tcp.local` with `--lan` (ADR-0233). Several daemons
	// may answer on a shared network; this one is the one on its own port.
	const found = await discover(3000);
	const mine = found.find((f) => f.port === lanPort);
	assert.ok(mine, `found ${JSON.stringify(found)}, not port ${lanPort}`);
	assert.ok(mine.name.length > 0);
	assert.ok(mine.host.length > 0);
});

test("a Stream Deck pairs over the network and drives JStream", { skip }, async (t) => {
	const host = lanHost();

	// The certificate offered is the one JStream prints.
	assert.equal(await probeFingerprint(host, lanPort), fingerprint);

	// Pairing refuses a certificate other than the one the person confirmed.
	const { code } = await call({ kind: "action", action: { type: "pairing.start" } });
	const wrong = fingerprint.replace(/^../, fingerprint.startsWith("00") ? "01" : "00");
	await assert.rejects(pair(host, lanPort, wrong, code, "Stream Deck"), FingerprintMismatch);

	// The code trades for a device token, which opens the network listener.
	const paired = await pair(host, lanPort, fingerprint, code, "Stream Deck");
	assert.match(paired.token, /^[0-9a-f]{64}$/);

	const client = new JStreamClient("Stream Deck (test)");
	t.after(() => client.close());
	const studio = new Studio(client);
	client.setEndpoint({ host, port: lanPort, fingerprint, token: paired.token });
	await until("the studio", () => studio.state !== undefined && studio.studio !== undefined);
	assert.equal(client.status.state, "connected");
	assert.deepEqual(
		studio.state!.scenes.map((s) => s.name),
		["Intro", "Main", "Main, wide"],
	);

	// Scene: switching moves program, which a scene key lights from.
	await client.action({ type: "scene.switch", scene_id: "scn_b" });
	await until("program on Main", () => studio.studio?.program_scene === "scn_b");

	// Keyframe: placing one is reported in the studio.
	await client.action({ type: "scene.set_keyframe", scene_id: "scn_b", keyframe_id: "scn_b_wide" });
	await until("the keyframe placed", () => (studio.studio?.keyframes ?? []).some((k) => k.keyframe_id === "scn_b_wide"));

	// Source: toggling an item hides it.
	const visible = () => studio.state!.scenes.find((s) => s.scene_id === "scn_b")!.items!.find((i) => i.item_id === "itm_bg")!.visible !== false;
	assert.equal(visible(), true);
	await client.action({ type: "item.toggle", item_id: "itm_bg" });
	await until("the item hidden", () => !visible());

	// Mute: an edit, read back from the source's audio.
	await client.batch([{ type: "source.set_audio", source_id: "src_mic", muted: true }]);
	await until("the source muted", () => studio.state!.sources.find((s) => s.source_id === "src_mic")!.audio?.muted === true);

	// Effect: off, then read back.
	const blur = () => findEffect(studio.state!, effectKey({ type: "source", source_id: "src_bg" }), "flt_blur");
	assert.equal(blur()?.enabled, true);
	await client.batch([{ type: "filter.set_enabled", owner: { type: "source", source_id: "src_bg" }, filter_id: "flt_blur", enabled: false }]);
	await until("the effect off", () => blur()?.enabled === false);

	// Sequence: playing it lists it in the studio, stopping it removes it.
	await client.action({ type: "timeline.play", timeline_id: "tl_loop" });
	await until("the sequence playing", () => (studio.studio?.timelines ?? []).some((t) => t.timeline_id === "tl_loop"));
	await client.action({ type: "timeline.stop", timeline_id: "tl_loop" });
	await until("the sequence stopped", () => !(studio.studio?.timelines ?? []).some((t) => t.timeline_id === "tl_loop"));

	// Hotkey action: the bound action runs as it is.
	const hotkey = studio.state!.hotkeys!.find((h) => h.chord === "Ctrl+Digit1")!;
	await client.action(hotkey.action);
	await until("program on Intro", () => studio.studio?.program_scene === "scn_a");

	// Output: its state is read at connect, and labelled by what it records to.
	await until("the output's state", () => studio.outputs.get("out_rec") === "stopped");

	// Revoking the device closes the connection, and it isn't let back in.
	await call({ kind: "action", action: { type: "device.revoke", device_id: paired.deviceId } });
	await until("the connection refused", () => client.status.state === "error" && /Pair again/.test(client.status.message), 8000);
	assert.equal((client.status as { retryInMs?: number }).retryInMs, undefined, "a refused token is not retried");
	client.close();

	// A certificate other than the paired one isn't retried either.
	const pinned = new JStreamClient("Stream Deck (test)");
	t.after(() => pinned.close());
	pinned.setEndpoint({ host, port: lanPort, fingerprint: wrong, token: paired.token });
	await until("the certificate refused", () => pinned.status.state === "error" && /certificate has changed/.test(pinned.status.message), 8000);
	assert.equal((pinned.status as { retryInMs?: number }).retryInMs, undefined, "a changed certificate is not retried");
});

test("a Stream Deck that asks to pair is let in when the person allows it", { skip }, async (t) => {
	const host = lanHost();
	// Denied on JStream's screen, the ask is refused.
	let from = events.length;
	const denied = askToPair(host, lanPort, fingerprint, "Stream Deck (asks)");
	const asked = await heard("pairing.asked", from);
	assert.equal(asked.ask.name, "Stream Deck (asks)");
	await call({ kind: "action", action: { type: "pairing.answer", ask_id: asked.ask.ask_id, allow: false } });
	await assert.rejects(denied, /Denied in JStream/);
	assert.equal((await heard("pairing.ask_ended", from)).outcome, "denied");

	// Allowed, it gets a device token that connects.
	from = events.length;
	const allowed = askToPair(host, lanPort, fingerprint, "Stream Deck (asks)");
	const again = await heard("pairing.asked", from);
	await call({ kind: "action", action: { type: "pairing.answer", ask_id: again.ask.ask_id, allow: true } });
	const paired = await allowed;
	assert.match(paired.token, /^[0-9a-f]{64}$/);
	assert.equal((await heard("pairing.ask_ended", from)).outcome, "allowed");
	const client = new JStreamClient("Stream Deck (asks)");
	t.after(() => client.close());
	client.setEndpoint({ host, port: lanPort, fingerprint, token: paired.token });
	await until("connected", () => client.status.state === "connected", 8000);
	await call({ kind: "action", action: { type: "device.revoke", device_id: paired.deviceId } });
});

test("show keys run a show, and the studio says when it plays and waits", { skip }, async (t) => {
	const seeded = call({
		kind: "batch",
		edits: [
			{
				type: "show.set",
				show: {
					show_id: "evening",
					name: "Evening show",
					program: [
						{ clip_id: "intro", scene_id: "scn_a", length_ms: 30000 },
						{ clip_id: "main", scene_id: "scn_b", length_ms: 30000 },
					],
					bookmarks: [{ bookmark_id: "bm_main", name: "Main", at_ms: 30000 }],
				},
			},
		],
	});
	try {
		await seeded;
	} catch (err) {
		// A JStream from before shows doesn't know `show.set`.
		if (/malformed/.test(String(err))) {
			t.skip("this JStream has no shows");
			return;
		}
		throw err;
	}
	const { code } = await call({ kind: "action", action: { type: "pairing.start" } });
	const paired = await pair(lanHost(), lanPort, fingerprint, code, "Stream Deck shows");
	const client = new JStreamClient("Stream Deck (test)");
	t.after(() => client.close());
	const studio = new Studio(client);
	client.setEndpoint({ host: lanHost(), port: lanPort, fingerprint, token: paired.token });
	await until("the studio", () => studio.state !== undefined && studio.studio !== undefined);
	assert.deepEqual(
		studio.state!.shows!.map((s) => [s.show_id, s.bookmarks!.map((b) => b.bookmark_id)]),
		[["evening", ["bm_main"]]],
	);

	// Play lights the Play key; the show runs from the start.
	await client.action({ type: "show.play", show_id: "evening" });
	await until("the show playing", () => studio.studio?.show?.show_id === "evening" && studio.studio.show.state === "playing");
	await until("program on Intro", () => studio.studio?.program_scene === "scn_a");

	// Pause holds it: the Continue and Pause keys light.
	await client.action({ type: "show.pause" });
	await until("the show paused", () => studio.studio?.show?.state === "paused");
	await client.action({ type: "show.continue" });
	await until("the show playing again", () => studio.studio?.show?.state === "playing");

	// Go to a bookmark puts its clip on program; it pauses there (a pausing bookmark).
	await client.action({ type: "show.go_to", show_id: "evening", bookmark_id: "bm_main" });
	await until("program on Main", () => studio.studio?.program_scene === "scn_b");
	await client.action({ type: "show.go_to_number", number: 1 });

	// Stop ends it: no key stays lit.
	await client.action({ type: "show.stop" });
	await until("the show stopped", () => !studio.studio?.show);
});

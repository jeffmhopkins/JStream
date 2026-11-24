/**
 * The keys: show controls (outputs, channels, hotkey actions), scene controls (switch
 * scene, place a keyframe), source controls (show or hide an item, mute a source),
 * sequence controls and effect switches. Each reads its target from the live state,
 * lights up while it's on, and acts through JStream's protocol.
 */
import { action } from "@elgato/streamdeck";

import { type Action } from "../jstream/protocol.ts";
import { findEffect, parseEffectKey } from "../jstream/studio.ts";
import { COLORS, type Glyph } from "../render.ts";
import { type Live, type Mode, type View, JStreamAction, fitTitle, wantsOn } from "./base.ts";
import { client, studio } from "../hub.ts";
import { outputLabel } from "../lists.ts";

export const PLUGIN = "io.github.jeffmhopkins.jstream";

// ---- Show controls ----

/** What a show key does: the transport of ADR-0210's shows. */
export type ShowCommand = "restart" | "play" | "continue" | "pause" | "stop" | "back" | "skip" | "go_to" | "number";

type ShowSettings = { show_id?: string; command?: ShowCommand; bookmark_id?: string; number?: string; rehearse?: string };

const SHOW_GLYPHS: Record<ShowCommand, Glyph> = {
	restart: "back",
	play: "play",
	continue: "continue",
	pause: "pause",
	stop: "stop",
	back: "back",
	skip: "skip",
	go_to: "bookmark",
	number: "bookmark",
};

/**
 * Runs a show: restart it, play it (or rehearse it on preview), continue, pause, stop,
 * go to the previous or next bookmark (Prev and Next: `show.back` and `show.skip`), or
 * jump to a bookmark. Restart plays it from its start, running or not. With no show
 * picked, a key follows the selected show: the one running, else the one open in
 * JStream's Show panel (its `show.open` UI setting), else the only one. Play toggles its show: it plays it, pauses it while it
 * plays, and continues it while it waits (at a bookmark, paused, or off script). It
 * lights while the show plays, showing the pause glyph, and is ringed while it waits.
 * Continue lights while the show waits for it; Pause while it's paused.
 */
@action({ UUID: `${PLUGIN}.show` })
export class ShowAction extends JStreamAction<ShowSettings> {
	protected override watch(delta: 1 | -1): void {
		// No event says which show the Show panel opens, so it is read while a key shows.
		studio.watchOpenShow(delta);
	}

	protected view(s: ShowSettings, live: Live | undefined): View {
		const command = s.command ?? "play";
		const glyph = SHOW_GLYPHS[command];
		if (!live) return { glyph, look: "offline" };
		const shows = live.state.shows ?? [];
		const show = shows.find((x) => x.show_id === (s.show_id || selectedShow(live)));
		const needsShow = command === "play" || command === "restart" || command === "go_to";
		if (needsShow && !show) return { glyph, look: "unset", title: shows.length ? (s.show_id ? "Pick a\nshow" : "Open a\nshow") : "No shows" };
		const running = live.studio.show ?? undefined;
		const mine = running && (!show || running.show_id === show.show_id) ? running : undefined;
		const waiting = mine !== undefined && mine.state !== "playing";
		let title = fitTitle(show?.name || show?.show_id);
		if (command === "go_to") {
			const bookmark = (show?.bookmarks ?? []).find((b) => b.bookmark_id === s.bookmark_id);
			if (!bookmark) return { glyph, look: "unset", title: "Pick a\nbookmark" };
			title = fitTitle(bookmark.name || bookmark.bookmark_id);
		} else if (command === "number") {
			title = `Bookmark\n${s.number || "1"}`;
		} else if (!show) {
			title = { continue: "Continue", pause: "Pause", stop: "Stop", back: "Prev", skip: "Next" }[command as "continue"];
		}
		switch (command) {
			case "play":
				if (!mine) return { glyph, look: "off", title };
				return waiting ? { glyph: "continue", look: "busy", color: COLORS.show, title } : { glyph: "pause", look: "on", color: COLORS.show, title };
			case "continue":
				return { glyph, look: waiting ? "on" : "off", color: COLORS.playing, title };
			case "pause":
				return { glyph, look: mine?.state === "paused" ? "on" : "off", color: COLORS.playing, title };
			case "stop":
				return { glyph, look: mine ? "on" : "off", color: COLORS.program, title };
			default:
				return { glyph, look: "off", title };
		}
	}

	protected async press(s: ShowSettings, live: Live): Promise<void> {
		const command = s.command ?? "play";
		switch (command) {
			case "play": {
				const show_id = s.show_id || (await selectedShowNow(live));
				if (!show_id) throw new Error("no show picked or open");
				// A toggle while this show runs: pause it while it plays, continue it while
				// it waits.
				const running = live.studio.show ?? undefined;
				if (running && running.show_id === show_id) {
					await client.action({ type: running.state === "playing" ? "show.pause" : "show.continue" });
					return;
				}
				const a: Action = { type: "show.play", show_id };
				if (s.rehearse === "yes") a.rehearse = true;
				await client.action(a);
				return;
			}
			case "restart": {
				const show_id = s.show_id || (await selectedShowNow(live));
				if (!show_id) throw new Error("no show picked or open");
				// `show.play` of the show that runs, paused, plays on: stopping first makes
				// it start over.
				if (live.studio.show) await client.action({ type: "show.stop" });
				const a: Action = { type: "show.play", show_id };
				if (s.rehearse === "yes") a.rehearse = true;
				await client.action(a);
				return;
			}
			case "go_to":
				if (!s.show_id || !s.bookmark_id) throw new Error("no bookmark picked");
				await client.action({ type: "show.go_to", show_id: s.show_id, bookmark_id: s.bookmark_id });
				return;
			case "number":
				await client.action({ type: "show.go_to_number", number: Math.min(8, Math.max(1, Number(s.number) || 1)) });
				return;
			default:
				await client.action({ type: `show.${command}` });
		}
	}
}

/**
 * The selected show: the one running, else the one JStream's Show panel has open, else
 * the only one there is.
 */
function selectedShow(live: Live): string | undefined {
	const shows = live.state.shows ?? [];
	const known = (id: string | undefined) => (id && shows.some((x) => x.show_id === id) ? id : undefined);
	return known(live.studio.show?.show_id) ?? known(live.store.openShow) ?? (shows.length === 1 ? shows[0].show_id : undefined);
}

/** [`selectedShow`], reading the Show panel's open show first, for a press. */
async function selectedShowNow(live: Live): Promise<string | undefined> {
	if (!live.studio.show) await live.store.readOpenShow();
	return selectedShow(live);
}

// ---- Scene controls ----

type SceneSettings = { scene_id?: string; preset_id?: string; keyframe_id?: string };

/**
 * Switches program to a scene, optionally at one of its keyframes. Lit red while it's on
 * program (at that keyframe, when one is picked), green while on preview.
 */
@action({ UUID: `${PLUGIN}.scene` })
export class SceneAction extends JStreamAction<SceneSettings> {
	protected view(s: SceneSettings, live: Live | undefined): View {
		if (!live) return { glyph: "scene", look: "offline" };
		const scene = live.state.scenes.find((x) => x.scene_id === s.scene_id);
		if (!scene) return { glyph: "scene", look: "unset", title: "Pick a\nscene" };
		const keyframe = s.keyframe_id ? live.state.scenes.find((x) => x.scene_id === s.keyframe_id && x.variant_of === scene.scene_id) : undefined;
		if (s.keyframe_id && !keyframe) return { glyph: "scene", look: "unset", title: "Pick a\nkeyframe" };
		const title = fitTitle(keyframe ? keyframe.name : scene.name);
		const atKeyframe = !keyframe || (live.studio.keyframes ?? []).some((k) => k.keyframe_id === keyframe.scene_id);
		if (live.studio.program_scene === scene.scene_id) {
			return { glyph: "scene", look: atKeyframe ? "on" : "off", color: COLORS.program, title };
		}
		if (live.studio.preview_scene === scene.scene_id) return { glyph: "scene", look: "on", color: COLORS.preview, title };
		return { glyph: "scene", look: "off", title };
	}

	protected async press(s: SceneSettings, live: Live): Promise<void> {
		if (!s.scene_id) throw new Error("no scene picked");
		if (s.keyframe_id) {
			const keyframe = live.state.scenes.find((x) => x.scene_id === s.keyframe_id && x.variant_of === s.scene_id);
			if (!keyframe) throw new Error("no keyframe picked");
			const place: Action = { type: "scene.set_keyframe", scene_id: s.scene_id, keyframe_id: keyframe.scene_id };
			if (live.studio.program_scene === s.scene_id) {
				// Already on program: move it to the keyframe with the transition.
				if (s.preset_id) place.preset_id = s.preset_id;
				await client.action(place);
				return;
			}
			// Placed first, so the scene goes live already in the keyframe's pose.
			await client.action(place);
		}
		const a: Action = { type: "scene.switch", scene_id: s.scene_id };
		if (s.preset_id) a.preset_id = s.preset_id;
		await client.action(a);
	}
}

type KeyframeSettings = { keyframe_id?: string; preset_id?: string };

/** Moves a scene to one of its keyframes (Go to keyframe). Lit while it is at that keyframe. */
@action({ UUID: `${PLUGIN}.keyframe` })
export class KeyframeAction extends JStreamAction<KeyframeSettings> {
	protected view(s: KeyframeSettings, live: Live | undefined): View {
		if (!live) return { glyph: "keyframe", look: "offline" };
		const keyframe = live.state.scenes.find((x) => x.scene_id === s.keyframe_id && x.variant_of);
		if (!keyframe) return { glyph: "keyframe", look: "unset", title: "Pick a\nkeyframe" };
		const placed = (live.studio.keyframes ?? []).some((k) => k.keyframe_id === keyframe.scene_id);
		return { glyph: "keyframe", look: placed ? "on" : "off", color: COLORS.keyframe, title: fitTitle(keyframe.name) };
	}

	protected async press(s: KeyframeSettings, live: Live): Promise<void> {
		const keyframe = live.state.scenes.find((x) => x.scene_id === s.keyframe_id && x.variant_of);
		if (!keyframe) throw new Error("no keyframe picked");
		const a: Action = { type: "scene.set_keyframe", scene_id: keyframe.variant_of, keyframe_id: keyframe.scene_id };
		if (s.preset_id) a.preset_id = s.preset_id;
		await client.action(a);
	}
}

// ---- Source controls ----

type VisibilitySettings = { scene_id?: string; item_id?: string; mode?: Mode };

/** Shows or hides one item in a scene. Lit while it's shown. */
@action({ UUID: `${PLUGIN}.visibility` })
export class VisibilityAction extends JStreamAction<VisibilitySettings> {
	#find(s: VisibilitySettings, live: Live) {
		for (const scene of live.state.scenes) {
			const item = (scene.items ?? []).find((i) => i.item_id === s.item_id);
			if (item) return item;
		}
		return undefined;
	}

	protected view(s: VisibilitySettings, live: Live | undefined): View {
		if (!live) return { glyph: "eye", look: "offline" };
		const item = this.#find(s, live);
		if (!item) return { glyph: "eye", look: "unset", title: "Pick a\nsource" };
		const name = live.state.sources.find((x) => x.source_id === item.source_id)?.name ?? item.item_id;
		const shown = item.visible !== false;
		return { glyph: shown ? "eye" : "eye-off", look: shown ? "on" : "off", color: COLORS.shown, title: fitTitle(name) };
	}

	protected async press(s: VisibilitySettings, live: Live): Promise<void> {
		const item = this.#find(s, live);
		if (!item) throw new Error("no item picked");
		const type = s.mode === "on" ? "item.show" : s.mode === "off" ? "item.hide" : "item.toggle";
		await client.action({ type, item_id: item.item_id });
	}
}

type MuteSettings = { source_id?: string; mode?: Mode };

/** Mutes or unmutes a source's audio. Lit red while it's muted. */
@action({ UUID: `${PLUGIN}.mute` })
export class MuteAction extends JStreamAction<MuteSettings> {
	protected view(s: MuteSettings, live: Live | undefined): View {
		if (!live) return { glyph: "speaker", look: "offline" };
		const source = live.state.sources.find((x) => x.source_id === s.source_id);
		if (!source) return { glyph: "speaker", look: "unset", title: "Pick a\nsource" };
		const muted = source.audio?.muted === true;
		return { glyph: muted ? "speaker-off" : "speaker", look: muted ? "on" : "off", color: COLORS.muted, title: fitTitle(source.name) };
	}

	protected async press(s: MuteSettings, live: Live): Promise<void> {
		const source = live.state.sources.find((x) => x.source_id === s.source_id);
		if (!source) throw new Error("no source picked");
		// `mode` "on" mutes: the key is lit while muted.
		const muted = wantsOn(s.mode, source.audio?.muted === true);
		await client.batch([{ type: "source.set_audio", source_id: source.source_id, muted }]);
	}
}

// ---- Sequence controls ----

type SequenceSettings = { timeline_id?: string; mode?: Mode };

/** Plays or stops a sequence. Lit while it plays; ringed while it waits to start. */
@action({ UUID: `${PLUGIN}.sequence` })
export class SequenceAction extends JStreamAction<SequenceSettings> {
	protected view(s: SequenceSettings, live: Live | undefined): View {
		const glyph = s.mode === "off" ? "stop" : "play";
		if (!live) return { glyph, look: "offline" };
		const timeline = live.state.scenes.flatMap((x) => x.timelines ?? []).find((t) => t.timeline_id === s.timeline_id);
		if (!timeline) return { glyph, look: "unset", title: "Pick a\nsequence" };
		const title = fitTitle(timeline.name || timeline.timeline_id);
		const playing = (live.studio.timelines ?? []).some((t) => t.timeline_id === timeline.timeline_id);
		const waiting = (live.studio.waiting ?? []).some((t) => t.timeline_id === timeline.timeline_id);
		if (playing) return { glyph: s.mode === "on" ? "play" : "stop", look: "on", color: COLORS.playing, title };
		return { glyph, look: waiting ? "busy" : "off", title };
	}

	protected async press(s: SequenceSettings, live: Live): Promise<void> {
		if (!s.timeline_id) throw new Error("no sequence picked");
		const playing = (live.studio.timelines ?? []).some((t) => t.timeline_id === s.timeline_id);
		const play = wantsOn(s.mode, playing);
		await client.action({ type: play ? "timeline.play" : "timeline.stop", timeline_id: s.timeline_id });
	}
}

// ---- Effect controls ----

type EffectSettings = { effect?: string; mode?: Mode };

/** Turns an effect on a source, a scene or an item on or off. Lit while it's on. */
@action({ UUID: `${PLUGIN}.effect` })
export class EffectAction extends JStreamAction<EffectSettings> {
	#find(s: EffectSettings, live: Live) {
		const [owner, filterId] = (s.effect ?? "").split("|");
		return owner && filterId ? findEffect(live.state, owner, filterId) : undefined;
	}

	protected view(s: EffectSettings, live: Live | undefined): View {
		if (!live) return { glyph: "effect", look: "offline" };
		const effect = this.#find(s, live);
		if (!effect) return { glyph: "effect", look: "unset", title: "Pick an\neffect" };
		return { glyph: "effect", look: effect.enabled ? "on" : "off", color: COLORS.effect, title: fitTitle(effect.kind) };
	}

	protected async press(s: EffectSettings, live: Live): Promise<void> {
		const effect = this.#find(s, live);
		const owner = parseEffectKey((s.effect ?? "").split("|")[0]);
		if (!effect || !owner) throw new Error("no effect picked");
		const enabled = wantsOn(s.mode, effect.enabled);
		await client.batch([{ type: "filter.set_enabled", owner, filter_id: effect.filter_id, enabled }]);
	}
}

// ---- Show controls ----

type OutputSettings = { output_id?: string; mode?: Mode };

/** Starts or stops a stream or a recording. Lit while live; ringed while starting or stopping. */
@action({ UUID: `${PLUGIN}.output` })
export class OutputAction extends JStreamAction<OutputSettings> {
	protected view(s: OutputSettings, live: Live | undefined): View {
		if (!live) return { glyph: "stream", look: "offline" };
		const output = (live.state.outputs ?? []).find((o) => o.output_id === s.output_id);
		if (!output) return { glyph: "stream", look: "unset", title: "Pick an\noutput" };
		const glyph = output.kind.type === "record" ? "record" : "stream";
		const title = fitTitle(outputLabel(output).replace(/ \(.*\)$/, ""));
		switch (live.store.outputs.get(output.output_id)) {
			case "active":
				return { glyph, look: "on", color: COLORS.live, title };
			case "starting":
			case "stopping":
			case "reconnecting":
				return { glyph, look: "busy", title };
			default:
				return { glyph, look: "off", title };
		}
	}

	protected async press(s: OutputSettings, live: Live): Promise<void> {
		if (!s.output_id) throw new Error("no output picked");
		const state = live.store.outputs.get(s.output_id);
		const running = state === "active" || state === "starting" || state === "reconnecting";
		const start = wantsOn(s.mode, running);
		await client.action({ type: start ? "output.start" : "output.stop", output_id: s.output_id });
	}
}

type ChannelSettings = { channel_id?: string; mode?: Mode };

/** Switches a channel on or off. Lit while it's on. */
@action({ UUID: `${PLUGIN}.channel` })
export class ChannelAction extends JStreamAction<ChannelSettings> {
	protected override watch(delta: 1 | -1): void {
		// No event reports a channel's switch, so channels are read while a key shows one.
		studio.watchChannels(delta);
	}

	protected view(s: ChannelSettings, live: Live | undefined): View {
		if (!live) return { glyph: "power", look: "offline" };
		const channel = (live.state.channels ?? []).find((c) => c.channel_id === s.channel_id);
		if (!channel) return { glyph: "power", look: "unset", title: "Pick a\nchannel" };
		const on = live.store.channels.get(channel.channel_id)?.on === true;
		return { glyph: "power", look: on ? "on" : "off", color: COLORS.on, title: fitTitle(channel.name || channel.channel_id) };
	}

	protected async press(s: ChannelSettings): Promise<void> {
		if (!s.channel_id) throw new Error("no channel picked");
		const to = s.mode === "on" ? "on" : s.mode === "off" ? "off" : "toggle";
		await client.action({ type: "channel.switch", channel_id: s.channel_id, to });
	}
}

type HotkeySettings = { chord?: string; custom?: string };

/**
 * Runs the action a JStream hotkey is bound to, or an action written as JSON: anything
 * the protocol can do from a key.
 */
@action({ UUID: `${PLUGIN}.action` })
export class HotkeyAction extends JStreamAction<HotkeySettings> {
	#action(s: HotkeySettings, live: Live): Action | undefined {
		if (s.custom && s.custom.trim()) {
			try {
				const parsed = JSON.parse(s.custom) as Action;
				return parsed && typeof parsed.type === "string" ? parsed : undefined;
			} catch {
				return undefined;
			}
		}
		return (live.state.hotkeys ?? []).find((h) => h.chord === s.chord)?.action;
	}

	protected view(s: HotkeySettings, live: Live | undefined): View {
		if (!live) return { glyph: "bolt", look: "offline" };
		const a = this.#action(s, live);
		if (!a) return { glyph: "bolt", look: "unset", title: "Pick an\naction" };
		return { glyph: "bolt", look: "off", title: s.custom?.trim() ? undefined : fitTitle(s.chord) };
	}

	protected async press(s: HotkeySettings, live: Live): Promise<void> {
		const a = this.#action(s, live);
		if (!a) throw new Error("no action picked, or its JSON doesn't parse");
		await client.action(a);
	}
}

export const ACTIONS = [ShowAction, SceneAction, KeyframeAction, VisibilityAction, MuteAction, SequenceAction, EffectAction, OutputAction, ChannelAction, HotkeyAction];

/**
 * What the keys show, kept current from JStream: the collection (`state.get`), the
 * studio (`studio.get`: program, preview, playing sequences, placed keyframes), each
 * output's state and each channel's switch. Events say what changed; the store reads
 * it again and tells the keys.
 */
import { EventEmitter } from "node:events";

import type { JStreamClient } from "./client.ts";
import type { ChannelValue, FilterInfo, OutputState, ServerEvent, StateSnapshot, StudioSnapshot } from "./protocol.ts";

/** How long to gather events before reading the state again. */
const DEBOUNCE_MS = 80;
/** How often channel switches are read while a channel key is visible (no event reports them). */
const CHANNEL_POLL_MS = 1000;

export interface StudioEvents {
	changed: [];
}

export class Studio extends EventEmitter<StudioEvents> {
	readonly #client: JStreamClient;
	state: StateSnapshot | undefined;
	studio: StudioSnapshot | undefined;
	outputs = new Map<string, OutputState>();
	channels = new Map<string, ChannelValue>();
	#stateTimer: NodeJS.Timeout | undefined;
	#studioTimer: NodeJS.Timeout | undefined;
	#channelTimer: NodeJS.Timeout | undefined;
	#channelWatchers = 0;

	constructor(client: JStreamClient) {
		super();
		this.#client = client;
		client.on("welcome", () => void this.refresh());
		client.on("status", (status) => {
			if (status.state !== "connected") {
				this.state = undefined;
				this.studio = undefined;
				this.outputs.clear();
				this.channels.clear();
				this.emit("changed");
			}
		});
		client.on("event", (event) => this.#onEvent(event));
	}

	/** Reads everything again. */
	async refresh(): Promise<void> {
		await Promise.all([this.#readState(), this.#readStudio(), this.#readChannels()]);
	}

	/** A channel key appeared (`+1`) or went (`-1`): channels are polled while any is visible. */
	watchChannels(delta: 1 | -1): void {
		this.#channelWatchers = Math.max(0, this.#channelWatchers + delta);
		if (this.#channelWatchers > 0 && !this.#channelTimer) {
			this.#channelTimer = setInterval(() => void this.#readChannels(), CHANNEL_POLL_MS);
		} else if (this.#channelWatchers === 0 && this.#channelTimer) {
			clearInterval(this.#channelTimer);
			this.#channelTimer = undefined;
		}
	}

	#onEvent(event: ServerEvent): void {
		switch (event.type) {
			case "state.changed":
				this.#later("state");
				break;
			case "studio.changed":
			case "timeline.changed":
			case "scene.keyframe_changed":
			case "show.changed":
				this.#later("studio");
				break;
			case "output.state_changed":
				this.outputs.set(String(event.output_id), event.state as OutputState);
				this.emit("changed");
				break;
			case "events.dropped":
			case "collection.file_changed":
				this.#later("state");
				this.#later("studio");
				break;
		}
	}

	#later(what: "state" | "studio"): void {
		if (what === "state" && !this.#stateTimer) {
			this.#stateTimer = setTimeout(() => {
				this.#stateTimer = undefined;
				void this.#readState();
			}, DEBOUNCE_MS);
		} else if (what === "studio" && !this.#studioTimer) {
			this.#studioTimer = setTimeout(() => {
				this.#studioTimer = undefined;
				void this.#readStudio();
			}, DEBOUNCE_MS);
		}
	}

	async #readState(): Promise<void> {
		try {
			const state = await this.#client.query<StateSnapshot>({ type: "state.get" });
			const known = new Set((this.state?.outputs ?? []).map((o) => o.output_id));
			this.state = state;
			// Outputs new to the store get their state read once; events keep it after.
			await Promise.all(
				(state.outputs ?? [])
					.filter((o) => !known.has(o.output_id) || !this.outputs.has(o.output_id))
					.map(async (o) => {
						try {
							const status = await this.#client.query<{ state: OutputState }>({ type: "output.status", output_id: o.output_id });
							this.outputs.set(o.output_id, status.state);
						} catch {
							// An output that can't report stays unlit.
						}
					}),
			);
			this.emit("changed");
		} catch {
			// Not connected: the status event has cleared the store.
		}
	}

	async #readStudio(): Promise<void> {
		try {
			this.studio = await this.#client.query<StudioSnapshot>({ type: "studio.get" });
			this.emit("changed");
		} catch {
			// As above.
		}
	}

	async #readChannels(): Promise<void> {
		if (!this.#client.connected) {
			return;
		}
		try {
			const answer = await this.#client.query<{ channels: ChannelValue[] }>({ type: "channel.values" });
			let changed = false;
			for (const value of answer.channels ?? []) {
				const old = this.channels.get(value.channel_id);
				if (!old || old.on !== value.on) {
					changed = true;
				}
				this.channels.set(value.channel_id, value);
			}
			if (changed) {
				this.emit("changed");
			}
		} catch {
			// An older JStream without channels, or not connected.
		}
	}
}

/** Where an effect lives: the owner `filter.set_enabled` names. */
export type EffectOwner =
	| { type: "source"; source_id: string }
	| { type: "scene"; scene_id: string }
	| { type: "item"; item_id: string };

/** One effect a key can turn on and off, with a label for the list. */
export type EffectChoice = { owner: EffectOwner; filter_id: string; kind: string; enabled: boolean; label: string };

/** Every effect on a source, a scene or an item, as `state.get` reports them. */
export function listEffects(state: StateSnapshot): EffectChoice[] {
	const out: EffectChoice[] = [];
	const sourceName = new Map(state.sources.map((s) => [s.source_id, s.name]));
	const chains = (filters: { video?: FilterInfo[]; audio?: FilterInfo[] } | null | undefined): FilterInfo[] => [
		...(filters?.video ?? []),
		...(filters?.audio ?? []),
	];
	for (const source of state.sources) {
		for (const f of chains(source.filters)) {
			out.push({ owner: { type: "source", source_id: source.source_id }, filter_id: f.filter_id, kind: f.kind, enabled: f.enabled, label: `${source.name} › ${f.kind}` });
		}
	}
	for (const scene of state.scenes) {
		for (const f of chains(scene.filters)) {
			out.push({ owner: { type: "scene", scene_id: scene.scene_id }, filter_id: f.filter_id, kind: f.kind, enabled: f.enabled, label: `Scene ${scene.name} › ${f.kind}` });
		}
		for (const item of scene.items ?? []) {
			for (const f of item.effects ?? []) {
				const name = (item.source_id && sourceName.get(item.source_id)) || item.item_id;
				out.push({ owner: { type: "item", item_id: item.item_id }, filter_id: f.filter_id, kind: f.kind, enabled: f.enabled, label: `${scene.name} › ${name} › ${f.kind}` });
			}
		}
	}
	return out;
}

/** The effect a key names, as the state has it now. */
export function findEffect(state: StateSnapshot, ownerKey: string, filterId: string): EffectChoice | undefined {
	return listEffects(state).find((e) => effectKey(e.owner) === ownerKey && e.filter_id === filterId);
}

/** An owner as one string, for a settings value. */
export function effectKey(owner: EffectOwner): string {
	switch (owner.type) {
		case "source":
			return `source:${owner.source_id}`;
		case "scene":
			return `scene:${owner.scene_id}`;
		case "item":
			return `item:${owner.item_id}`;
	}
}

/** The owner a settings value names. */
export function parseEffectKey(key: string): EffectOwner | undefined {
	const at = key.indexOf(":");
	const kind = key.slice(0, at);
	const id = key.slice(at + 1);
	if (at < 0 || !id) {
		return undefined;
	}
	switch (kind) {
		case "source":
			return { type: "source", source_id: id };
		case "scene":
			return { type: "scene", scene_id: id };
		case "item":
			return { type: "item", item_id: id };
		default:
			return undefined;
	}
}

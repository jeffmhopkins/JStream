/**
 * What every JStream key shares: it draws itself from the live state whenever that
 * changes, presses only while connected (a press that fails shows Stream Deck's alert),
 * and passes its property inspector's connection messages to the hub.
 */
import {
	type DidReceiveSettingsEvent,
	type KeyAction,
	type KeyDownEvent,
	type SendToPluginEvent,
	SingletonAction,
	type WillAppearEvent,
	type WillDisappearEvent,
} from "@elgato/streamdeck";
import streamDeck from "@elgato/streamdeck";
import type { JsonObject, JsonValue } from "@elgato/utils";

import { client, onInspectorMessage, studio } from "../hub.ts";
import type { StateSnapshot, StudioSnapshot } from "../jstream/protocol.ts";
import type { Studio } from "../jstream/studio.ts";
import { type Glyph, type Look, keyImage } from "../render.ts";

export { type Mode, fitTitle, wantsOn } from "../lists.ts";

/** How a key looks now: its glyph, whether it's lit and in what colour, and its title. */
export type View = { glyph: Glyph; look: Look; color?: string; title?: string };

/** The live state a key reads, present only while connected. */
export type Live = { state: StateSnapshot; studio: StudioSnapshot; store: Studio };

type Instance<S extends JsonObject> = { action: KeyAction<S>; settings: S; drawn?: string; titled?: string };

export abstract class JStreamAction<S extends JsonObject> extends SingletonAction<S> {
	#visible = new Map<string, Instance<S>>();

	constructor() {
		super();
		studio.on("changed", () => this.#drawAll());
		client.on("status", () => this.#drawAll());
	}

	/** How the key with `settings` looks, given the live state (`undefined` while offline). */
	protected abstract view(settings: S, live: Live | undefined): View;

	/** What a press does. Throws to show the key's alert. */
	protected abstract press(settings: S, live: Live): Promise<void>;

	/** Called as a key appears (`+1`) and goes (`-1`). */
	protected watch(_delta: 1 | -1): void {}

	override async onWillAppear(ev: WillAppearEvent<S>): Promise<void> {
		if (!ev.action.isKey()) {
			return;
		}
		const instance: Instance<S> = { action: ev.action, settings: ev.payload.settings };
		this.#visible.set(ev.action.id, instance);
		this.watch(1);
		await this.#draw(instance);
	}

	override onWillDisappear(ev: WillDisappearEvent<S>): void {
		if (this.#visible.delete(ev.action.id)) {
			this.watch(-1);
		}
	}

	override async onDidReceiveSettings(ev: DidReceiveSettingsEvent<S>): Promise<void> {
		const instance = this.#visible.get(ev.action.id);
		if (instance) {
			instance.settings = ev.payload.settings;
			await this.#draw(instance);
		}
	}

	override async onKeyDown(ev: KeyDownEvent<S>): Promise<void> {
		const live = currentLive();
		if (!live) {
			await ev.action.showAlert();
			return;
		}
		try {
			await this.press(ev.payload.settings, live);
		} catch (err) {
			streamDeck.logger.warn(`${this.manifestId}: ${err instanceof Error ? err.message : String(err)}`);
			await ev.action.showAlert();
		}
	}

	override async onSendToPlugin(ev: SendToPluginEvent<JsonValue, S>): Promise<void> {
		await onInspectorMessage(ev.payload);
	}

	#drawAll(): void {
		for (const instance of this.#visible.values()) {
			void this.#draw(instance);
		}
	}

	async #draw(instance: Instance<S>): Promise<void> {
		const view = this.view(instance.settings, currentLive());
		const image = keyImage(view.glyph, view.look, view.color);
		if (image !== instance.drawn) {
			instance.drawn = image;
			await instance.action.setImage(image);
		}
		const title = view.title ?? "";
		if (title !== instance.titled) {
			instance.titled = title;
			await instance.action.setTitle(title);
		}
	}
}

/** The live state, while connected and read. */
export function currentLive(): Live | undefined {
	if (!client.connected || !studio.state || !studio.studio) {
		return undefined;
	}
	return { state: studio.state, studio: studio.studio, store: studio };
}

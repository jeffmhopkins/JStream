/**
 * The JStream Stream Deck plugin's entry point.
 * Docs: integrations/streamdeck/README.md
 */
import streamDeck from "@elgato/streamdeck";

import { ACTIONS } from "./actions/index.ts";
import { start } from "./hub.ts";

streamDeck.logger.setLevel("info");

for (const Action of ACTIONS) {
	streamDeck.actions.registerAction(new Action());
}

await streamDeck.connect();
await start();

// The Settings dialog's Audio plugins page (#1212): the folders the engine searches for
// CLAP plugins, what its last scan found and couldn't read, and Rescan; and Add filter's
// Plugins group on an audio chain with no CLAP plugin, which links to the page. The mock
// engine hosts no CLAP plugins, so the page's WebSocket is routed: `plugin.clap_scan`,
// `plugin.clap_rescan` and `kind.list` are answered here, and everything else goes on
// to the mock daemon.
import { test, expect, connect, showPanel } from '../fixtures.js';

const GAIN = {
  kind_id: 'gain', category: 'audio_filter', name: 'Gain', version: 1, video: false, audio: true,
  params: [],
};

/** Answers the CLAP calls from `state` and `kind.list` from `state.kinds`; a rescan
 * moves `state.next` into the scan and the kinds. Records each rescan sent. */
async function fakeClap(page, state) {
  const sent = [];
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      const call = parsed?.call;
      const query = call?.kind === 'query' ? call.query : null;
      const action = call?.kind === 'action' ? call.action : null;
      if (query?.type === 'plugin.clap_scan') {
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'plugin.clap_scan', scan: state.scan } }));
      } else if (query?.type === 'kind.list') {
        const kinds = state.kinds.map(({ kind_id, category, name, version }) => ({ kind_id, category, name, version }));
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'kind.list', kinds } }));
      } else if (action?.type === 'plugin.clap_rescan') {
        sent.push(action);
        if (state.next) {
          state.scan.found.push(state.next.found);
          state.kinds.push(state.next.kind);
          state.next = null;
        }
        ws.send(answer(parsed.id, { kind: 'action' }));
      } else {
        server.send(message);
      }
    });
    server.onMessage((message) => ws.send(message));
  });
  return sent;
}

function scanState() {
  return {
    scan: {
      available: true,
      scanning: false,
      scanned: true,
      folders: [
        { path: 'C:\\Program Files\\Common Files\\CLAP', exists: true },
        { path: 'C:\\Users\\me\\AppData\\Local\\Programs\\Common\\CLAP', exists: false },
      ],
      found: [],
      failed: [{ file: 'C:\\Program Files\\Common Files\\CLAP\\Broken.clap', reason: 'the plugin host crashed' }],
    },
    kinds: [GAIN],
    next: {
      found: { kind_id: 'clap/com.example.verb', name: 'Room Verb', file: 'C:\\Program Files\\Common Files\\CLAP\\Verb.clap' },
      kind: { kind_id: 'clap/com.example.verb', category: 'audio_filter', name: 'Room Verb', version: 1, video: false, audio: true, params: [] },
    },
  };
}

/** Opens Add filter on the program output's chain, an audio chain. */
async function addProgramFilter(page) {
  await showPanel(page, 'Outputs');
  await page.getByRole('button', { name: 'Program output filters' }).click();
  await page.locator('.filters-panel').getByRole('button', { name: '+ Add filter…' }).click();
  return page.getByRole('dialog', { name: 'Add filter' });
}

test('Add filter says where CLAP plugins go, and a rescan offers one installed since', async ({ page, daemon }) => {
  const state = scanState();
  const sent = await fakeClap(page, state);
  await connect(page, daemon);

  // No CLAP plugin yet: the audio chain still shows Plugins, with a link to the page.
  const add = await addProgramFilter(page);
  await expect(add.locator('.add-filter-group')).toHaveText(['Levels and dynamics', 'Plugins']);
  await expect(add.locator('.add-filter-clap')).toContainText('No CLAP audio plugins found.');
  // A search hides it.
  await add.getByRole('textbox', { name: 'Search filters' }).fill('gain');
  await expect(add.locator('.add-filter-clap')).toHaveCount(0);
  await add.getByRole('textbox', { name: 'Search filters' }).fill('');
  await add.getByRole('button', { name: 'Where to install them…' }).click();

  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByRole('tab', { name: 'Audio plugins' })).toHaveAttribute('aria-selected', 'true');
  const pane = settings.locator('section.clap-plugins');
  await expect(pane.locator('.clap-hint')).toContainText('C:\\Program Files\\Common Files\\CLAP');
  await expect(pane.locator('.clap-folder .clap-tag')).toHaveText(['searched', "doesn't exist yet"]);
  await expect(pane.locator('.clap-summary')).toHaveText("No CLAP plugins found; 1 file couldn't be read.");
  await expect(pane.locator('.clap-failed')).toContainText('the plugin host crashed');

  await pane.getByRole('button', { name: 'Rescan' }).click();
  await expect(pane.locator('.clap-summary')).toHaveText("1 CLAP plugin found; 1 file couldn't be read.");
  await expect(pane.locator('.clap-plugin .plugin-name')).toHaveText('Room Verb');
  expect(sent).toEqual([{ type: 'plugin.clap_rescan' }]);
  await page.keyboard.press('Escape');
  await expect(settings).toHaveCount(0);

  // The rescan re-read the kinds: Add filter offers the plugin under Plugins now.
  await expect(add.locator('.add-filter-group')).toHaveText(['Levels and dynamics', 'Plugins']);
  await expect(add.getByRole('option', { name: 'Room Verb' })).toBeVisible();
  await expect(add.locator('.add-filter-clap')).toHaveCount(0);
});

test('an engine without CLAP says why and offers no rescan', async ({ page, daemon }) => {
  // The mock engine itself answers: it hosts no CLAP plugins.
  await connect(page, daemon);
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Settings…', exact: true }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Audio plugins' }).click();
  await expect(settings.locator('section.clap-plugins')).toContainText(
    "CLAP audio plugins aren't available here: the mock engine hosts no CLAP plugins",
  );
  await expect(settings.getByRole('button', { name: 'Rescan' })).toHaveCount(0);
});

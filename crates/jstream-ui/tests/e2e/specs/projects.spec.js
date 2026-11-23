// The File menu's project dialogs (ADR-0121, #1650): Open lists the recent projects other
// than the open one, a click, Space, Up and Down fill Location, a double-click or Enter
// opens the row, and the list is one tab stop; New and Save as show no list. The mock
// engine keeps no collection file, so the page's WebSocket is routed: `collection.info`
// is answered here with an open project and its recent list, `collection.open` is
// recorded and answered, and every other message goes on to the mock daemon.
import { test, expect, connect } from '../fixtures.js';

const OPEN = '/shows/Talk.json';
const RECENT = [OPEN, '/shows/Weekly.json', '/archive/Gala.json', '/shows/Pilot.json'];

/** Answers `collection.info` with the open project and its recent list, and records each
 * `collection.open` the page sends. */
async function fakeProject(page) {
  const opened = [];
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
      if (call?.kind === 'query' && call.query?.type === 'collection.info') {
        const result = { type: 'collection.info', path: OPEN, recent: RECENT };
        ws.send(answer(parsed.id, { kind: 'query', result }));
        return;
      }
      if (call?.kind === 'action' && call.action?.type === 'collection.open') {
        opened.push(call.action.path);
        ws.send(answer(parsed.id, { kind: 'action' }));
        return;
      }
      server.send(message);
    });
    server.onMessage((message) => ws.send(message));
  });
  return { opened };
}

/** Opens the File menu and picks `item`, once the menu shows the open project. */
async function fileMenu(page, item) {
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await expect(page.locator('.project-head')).toContainText('Talk');
  await page.getByRole('menuitem', { name: item }).click();
}

/** The open project dialog titled `title`. */
function dialog(page, title) {
  return page.getByRole('dialog', { name: title });
}

/** The tabindex of each recent row, in order. */
async function tabStops(list) {
  return list.getByRole('option').evaluateAll((rows) => rows.map((row) => row.getAttribute('tabindex')));
}

test('Open lists the recent projects, and a click, Space, Up and Down fill Location', async ({ page, daemon }) => {
  await fakeProject(page);
  await connect(page, daemon);
  await fileMenu(page, 'Open project…');

  const open = dialog(page, 'Open project');
  const list = open.getByRole('listbox', { name: 'Recent projects' });
  const rows = list.getByRole('option');
  const location = open.getByLabel('Location');
  // The open project isn't offered; the others are, most recent first, with their folders.
  await expect(rows.locator('.project-recent-name')).toHaveText(['Weekly', 'Gala', 'Pilot']);
  await expect(rows.locator('.project-recent-folder')).toHaveText(['/shows', '/archive', '/shows']);
  await expect(rows.nth(1)).toHaveAttribute('title', '/archive/Gala.json');
  await expect(location).toHaveValue('');
  await expect(rows.nth(0)).toHaveAttribute('aria-selected', 'false');
  // One tab stop: the first row while none is picked.
  expect(await tabStops(list)).toEqual(['0', '-1', '-1']);

  await rows.nth(1).click();
  await expect(location).toHaveValue('/archive/Gala.json');
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true');
  expect(await tabStops(list)).toEqual(['-1', '0', '-1']);

  await rows.nth(1).focus();
  await page.keyboard.press('ArrowDown');
  await expect(location).toHaveValue('/shows/Pilot.json');
  await expect(rows.nth(2)).toBeFocused();
  // Down at the last row stays there.
  await page.keyboard.press('ArrowDown');
  await expect(location).toHaveValue('/shows/Pilot.json');
  await expect(rows.nth(2)).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('ArrowUp');
  await expect(location).toHaveValue('/shows/Weekly.json');
  await expect(rows.nth(0)).toBeFocused();
  await expect(rows.nth(0)).toHaveAttribute('aria-selected', 'true');
  expect(await tabStops(list)).toEqual(['0', '-1', '-1']);

  // A path typed in Location picks no row, and Space on a row fills it again.
  await location.fill('/elsewhere/Other.json');
  await expect(rows.nth(0)).toHaveAttribute('aria-selected', 'false');
  expect(await tabStops(list)).toEqual(['0', '-1', '-1']);
  await rows.nth(2).focus();
  await page.keyboard.press(' ');
  await expect(location).toHaveValue('/shows/Pilot.json');
  expect(await tabStops(list)).toEqual(['-1', '-1', '0']);

  // The list is one tab stop, between the close button and Location.
  await location.focus();
  await page.keyboard.press('Shift+Tab');
  await expect(rows.nth(2)).toBeFocused();
  await page.keyboard.press('Shift+Tab');
  await expect(open.getByRole('button', { name: 'Close' })).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(rows.nth(2)).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(location).toBeFocused();
});

test('a double-click or Enter on a recent project opens it', async ({ page, daemon }) => {
  const { opened } = await fakeProject(page);
  await connect(page, daemon);
  await fileMenu(page, 'Open project…');

  const open = dialog(page, 'Open project');
  const rows = open.getByRole('listbox', { name: 'Recent projects' }).getByRole('option');
  await rows.nth(1).dblclick();
  await expect(open).toHaveCount(0);
  await expect(page.locator('.toast')).toHaveText('Opened Gala');
  expect(opened).toEqual(['/archive/Gala.json']);

  await fileMenu(page, 'Open project…');
  await rows.nth(0).focus();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(open).toHaveCount(0);
  await expect(page.locator('.toast')).toHaveText('Opened Pilot');
  // Enter opens it once, though the dialog's own Enter presses Open too.
  expect(opened).toEqual(['/archive/Gala.json', '/shows/Pilot.json']);
});

test('New and Save as show no recent list, and start beside the open project', async ({ page, daemon }) => {
  await fakeProject(page);
  await connect(page, daemon);

  await fileMenu(page, 'New project…');
  const created = dialog(page, 'New project');
  await expect(created.getByLabel('Location')).toHaveValue('/shows/Untitled.json');
  await expect(created.getByRole('listbox')).toHaveCount(0);
  await created.getByRole('button', { name: 'Cancel' }).click();
  await expect(created).toHaveCount(0);

  await fileMenu(page, 'Save project as…');
  const saved = dialog(page, 'Save project as');
  await expect(saved.getByLabel('Location')).toHaveValue('/shows/Talk copy.json');
  await expect(saved.getByRole('listbox')).toHaveCount(0);
});

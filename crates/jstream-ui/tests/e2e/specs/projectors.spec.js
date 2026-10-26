// Projector windows: the View menu opens the program and the multiview, and a scene's
// Pop out submenu opens that scene, each as a page of its own that connects with the
// browser's credentials and fills its window; Escape closes one. The mock engine sends no
// preview streams, so a picture shows why instead; the frames themselves are covered by
// the engine's tests.
import { test, expect, connect } from '../fixtures.js';

/** The open right-click menu. */
function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** Clicks `click` and returns the projector window it opens, once its page shows. */
async function opened(page, click) {
  const [popup] = await Promise.all([page.waitForEvent('popup'), click()]);
  await expect(popup.locator('main.projector')).toBeVisible();
  return popup;
}

/** Picks `label` in the header's View menu. */
async function fromView(page, label) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  await page.getByRole('menuitem', { name: label }).click();
}

test('the View menu opens the program in a window, which Escape closes', async ({ page, daemon }) => {
  await connect(page, daemon);
  const popup = await opened(page, () => fromView(page, 'Program in a window'));
  expect(new URL(popup.url()).search).toBe('?projector=program');
  await expect(popup).toHaveTitle('Program · JStream projector');
  await expect(popup.locator('.projector-hint')).toContainText('Program');
  await expect(popup.locator('.projector-picture canvas')).toHaveCount(1);
  await expect(page.locator('.toast')).toHaveText('Projector opened. Esc on it closes it.');

  // The window closes while the key is still being pressed, which ends the press with an
  // error: the close is what counts.
  const closed = popup.waitForEvent('close');
  await popup.keyboard.press('Escape').catch(() => {});
  await closed;
  expect(popup.isClosed()).toBe(true);
});

test('a scene pops out from its row', async ({ page, daemon }) => {
  await connect(page, daemon);
  await page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' }).click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Pop out' }).hover();
  const sub = page.locator('.context-menu.submenu');
  await expect(sub.locator('.ctx-label')).toHaveText(['Scene in a window', 'Scene full screen']);
  const popup = await opened(page, () => sub.getByRole('menuitem', { name: 'Scene in a window' }).click());
  expect(new URL(popup.url()).search).toBe('?projector=scene:intro');
  await expect(popup).toHaveTitle('Scene: Intro · JStream projector');
});

test('a full-screen projector in a browser says how to fill the screen', async ({ page, daemon }) => {
  await connect(page, daemon);
  const popup = await opened(page, () => fromView(page, 'Program full screen'));
  expect(new URL(popup.url()).search).toBe('?projector=program&full=1');
  await expect(page.locator('.toast')).toHaveText(
    'Projector opened. Double-click it to fill the screen; Esc closes it.',
  );
});

test('the multiview shows program over the scenes marked for it', async ({ page, daemon }) => {
  await connect(page, daemon);
  const popup = await opened(page, () => fromView(page, 'Multiview in a window'));
  await expect(popup).toHaveTitle('Multiview · JStream projector');
  await expect(popup.locator('.tile.program figcaption')).toHaveText('Program · Main');
  // Not in studio mode: no preview tile.
  await expect(popup.locator('.tile.preview')).toHaveCount(0);
  const scenes = popup.locator('.multiview-scenes .tile');
  await expect(scenes.locator('figcaption')).toHaveText(['Intro', 'Main']);
  // The scene on program has the red frame.
  await expect(scenes.filter({ hasText: 'Main' })).toHaveClass(/live/);
  await expect(scenes.filter({ hasText: 'Intro' })).not.toHaveClass(/live/);
});

// The browser's own behaviour kept off the chrome (chrome.rs): a right-click that no
// element answers with a menu is swallowed, a text field keeps the browser's text menu
// even inside a panel that has one, a click or a drag across panels selects no text while
// a text field still selects, and the header's dropdowns hang flush under the bar.
import { test, expect, connect } from '../fixtures.js';

/** Right-clicks `locator` at its centre with a cancelable event; whether the browser's
 *  menu was stopped (the event's default prevented). */
async function swallowed(locator) {
  return locator.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const event = new MouseEvent('contextmenu', {
      bubbles: true,
      cancelable: true,
      button: 2,
      clientX: r.left + r.width / 2,
      clientY: r.top + r.height / 2,
    });
    return !el.dispatchEvent(event);
  });
}

test('a right-click on the chrome never reaches the browser, and a text field keeps its menu', async ({ page, daemon }) => {
  await connect(page, daemon);
  const chrome = [
    page.locator('header.app-header .brand'),
    page.locator('header.app-header .readout'),
    page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button'),
    page.getByRole('tab').first(),
    page.locator('.panel h2').first(),
    page.getByRole('toolbar', { name: 'Canvas tools' }),
  ];
  for (const surface of chrome) {
    expect(await swallowed(surface)).toBe(true);
    await page.keyboard.press('Escape');
  }

  // The Sources panel has a menu, but not over its search field: that keeps Cut, Copy
  // and Paste.
  const search = page.getByRole('textbox', { name: 'Search sources' });
  expect(await swallowed(search)).toBe(false);
  await expect(page.locator('.context-menu')).toHaveCount(0);

  // Inside a dialog too: its header swallows, its fields keep the text menu.
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Connection details…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Connection details' });
  expect(await swallowed(dialog.locator('.modal-header'))).toBe(true);
  expect(await swallowed(dialog.locator('dl.connection dd').first())).toBe(false);
  await page.keyboard.press('Escape');

  // About's version and links keep the browser's menu (Copy, Copy link).
  await page.locator('header.app-header .top-menu[data-menu="help"] > .menu-button').click();
  await page.getByRole('menuitem', { name: /About/ }).click();
  const about = page.getByRole('dialog', { name: 'About JStream' });
  expect(await swallowed(about.locator('dl.about-facts dd').first())).toBe(false);
  expect(await swallowed(about.locator('a[href]').first())).toBe(false);
});

test('a drag across panels or a double-click selects no text, but a text field still does', async ({ page, daemon }) => {
  await connect(page, daemon);
  // From the first tab to the far corner of the last panel: over the panels between.
  const tabs = page.locator('[role=tab]:visible');
  const from = await tabs.first().boundingBox();
  const to = await page.locator('.panel:visible').last().boundingBox();
  await page.mouse.move(from.x + 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(to.x + to.width - 8, to.y + to.height - 8, { steps: 8 });
  await page.mouse.up();
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');

  await tabs.first().dblclick();
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');
  await page.locator('.tree-caption').first().dblclick();
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');

  const search = page.getByRole('textbox', { name: 'Search sources' });
  await search.fill('backdrop');
  await search.dblclick();
  const selected = await search.evaluate((input) => input.selectionEnd - input.selectionStart);
  expect(selected).toBe('backdrop'.length);
  await search.fill('');
});

test('the header dropdowns hang flush under the bar', async ({ page, daemon }) => {
  await connect(page, daemon);
  const bar = await page.locator('header.app-header').boundingBox();
  for (const name of ['file', 'edit', 'view']) {
    await page.locator(`header.app-header .top-menu[data-menu="${name}"] > .menu-button`).click();
    const dropdown = await page.locator(`header.app-header .top-menu[data-menu="${name}"] .dropdown`).boundingBox();
    expect(Math.abs(dropdown.y - (bar.y + bar.height))).toBeLessThanOrEqual(0.5);
    await page.keyboard.press('Escape');
  }
});

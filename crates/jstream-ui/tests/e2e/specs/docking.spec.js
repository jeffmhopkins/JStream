// The dock: the Editing layout, the View and Layout menus, dragging a tab to group or
// split panels, collapsing, closing with two clicks, resizing, and the layout kept
// across a reload.
import { test, expect, connect } from '../fixtures.js';

/** The zone `label` (its accessible name). */
function zone(page, label) {
  return page.getByRole('region', { name: label, exact: true });
}

/** The tab of panel `name`: a dock tab, not the Inspector's own. */
function tab(page, name) {
  return page.locator('.dock-strip').getByRole('tab', { name, exact: true });
}

/** Opens the header menu whose button starts with `label`. */
async function openMenu(page, label) {
  await page.locator('header.app-header .menu-button', { hasText: label }).click();
}

/** Drags from the centre of `from` to the point `at` (page coordinates). */
async function drag(page, from, at) {
  const box = await from.boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 10, box.y + box.height / 2 + 10, { steps: 3 });
  await page.mouse.move(at.x, at.y, { steps: 8 });
  await page.mouse.up();
}

/** The centre of `locator`. */
async function centre(locator) {
  const box = await locator.boundingBox();
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

test('the Editing layout docks every panel, and the View menu hides and adds them', async ({ page, daemon }) => {
  await connect(page, daemon);
  await expect(zone(page, 'Left sidebar').getByRole('tab')).toHaveText(['Scenes', 'Sources']);
  await expect(zone(page, 'Right sidebar').getByRole('tab')).toHaveText(['Inspector', 'Filters', 'Arrange']);
  await expect(zone(page, 'Bottom dock').getByRole('tab')).toHaveText(['Audio mixer', 'Outputs', 'Transitions']);
  // Empty zones are hidden until a panel is dragged.
  await expect(zone(page, 'Top dock')).toBeHidden();
  await expect(page.locator('header.app-header')).toContainText('Layout: Editing');

  await openMenu(page, 'View');
  const view = page.getByRole('menu', { name: 'View' });
  const inspector = view.getByRole('menuitemcheckbox', { name: /Inspector/ });
  await expect(inspector).toHaveAttribute('aria-checked', 'true');
  await inspector.click();
  await expect(tab(page, 'Inspector')).toHaveCount(0);
  await expect(inspector).toHaveAttribute('aria-checked', 'false');
  // The menu stays open; adding puts the panel back at the end of its home zone.
  await inspector.click();
  await expect(zone(page, 'Right sidebar').getByRole('tab')).toHaveText(['Filters', 'Arrange', 'Inspector']);
  await page.keyboard.press('Escape');
  await expect(page.locator('header.app-header')).toContainText('Layout: Editing (modified)');
});

test('a tab dragged onto a strip joins its group, and onto a zone starts one', async ({ page, daemon }) => {
  await connect(page, daemon);
  await drag(page, tab(page, 'Outputs'), await centre(tab(page, 'Audio mixer')));
  const bottom = zone(page, 'Bottom dock');
  // Transitions stays behind as the lone panel of the group Outputs left.
  await expect(bottom.getByRole('tablist')).toHaveCount(2);
  await expect(bottom.getByRole('tab')).toHaveText(['Audio mixer', 'Outputs', 'Transitions']);
  await expect(tab(page, 'Outputs')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByRole('tabpanel', { name: 'Audio mixer' })).toBeHidden();
  await tab(page, 'Audio mixer').click();
  await expect(page.getByRole('tabpanel', { name: 'Audio mixer' })).toBeVisible();

  // Dragging shows the empty zones; dropping in one docks the panel there.
  const scenes = tab(page, 'Scenes');
  const box = await scenes.boundingBox();
  await page.mouse.move(box.x + 5, box.y + 5);
  await page.mouse.down();
  await page.mouse.move(box.x + 40, box.y + 40, { steps: 4 });
  const right = zone(page, 'Outer right column');
  await expect(right).toContainText('Drop here: outer right');
  const target = await centre(right);
  await page.mouse.move(target.x, target.y, { steps: 8 });
  await page.mouse.up();
  await expect(right.getByRole('tab')).toHaveText(['Scenes']);
  await expect(zone(page, 'Left sidebar').getByRole('tab')).toHaveText(['Sources']);
  await expect(zone(page, 'Top dock')).toBeHidden();
});

test('a lone panel collapses by its title, and closes on a second click of its ×', async ({ page, daemon }) => {
  await connect(page, daemon);
  const scenes = page.getByRole('tabpanel', { name: 'Scenes' });
  await tab(page, 'Scenes').click();
  await expect(scenes).toBeHidden();
  await tab(page, 'Scenes').click();
  await expect(scenes).toBeVisible();

  const close = page.getByRole('button', { name: 'Close Arrange' });
  await tab(page, 'Arrange').hover();
  await close.click();
  await expect(close).toHaveAttribute('title', 'Click again to close Arrange');
  await close.click();
  await expect(tab(page, 'Arrange')).toHaveCount(0);
  const closeFilters = page.getByRole('button', { name: 'Close Filters' });
  await tab(page, 'Filters').hover();
  await closeFilters.click();
  await closeFilters.click();
  await expect(tab(page, 'Filters')).toHaveCount(0);
  // The inspector is now alone in its zone, so it doesn't collapse.
  await tab(page, 'Inspector').click();
  await expect(page.getByRole('tabpanel', { name: 'Inspector' })).toBeVisible();
});

test('splitters resize zones, and the layout survives a reload', async ({ page, daemon }) => {
  await connect(page, daemon);
  const left = zone(page, 'Left sidebar');
  const before = (await left.boundingBox()).width;
  const splitter = page.getByRole('separator', { name: 'Resize left sidebar' });
  const at = await centre(splitter);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x + 60, at.y, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => (await left.boundingBox()).width).toBeCloseTo(before + 60, 0);

  await openMenu(page, 'Layout');
  await page.getByRole('menuitem', { name: 'Save current layout as new' }).click();
  await expect(page.locator('header.app-header')).toContainText('Layout: My layout 1');

  await connect(page, daemon);
  await expect.poll(async () => (await zone(page, 'Left sidebar').boundingBox()).width).toBeCloseTo(before + 60, 0);
  await expect(page.locator('header.app-header')).toContainText('Layout: My layout 1');
});

test('the Layout menu switches to Live and resets to Editing', async ({ page, daemon }) => {
  await connect(page, daemon);
  await openMenu(page, 'Layout');
  const menu = page.getByRole('menu', { name: 'Layout' });
  await expect(menu.getByRole('menuitemradio', { name: /Editing/ })).toHaveAttribute('aria-checked', 'true');
  await menu.getByRole('menuitemradio', { name: 'Live' }).click();
  await expect(zone(page, 'Dock below the canvas').getByRole('tab')).toHaveText(['Audio mixer', 'Transitions']);
  await expect(zone(page, 'Right sidebar').getByRole('tab')).toHaveText(['Stream stats', 'Outputs', 'Agent activity']);
  await expect(zone(page, 'Bottom dock')).toBeHidden();

  await openMenu(page, 'Layout');
  await page.getByRole('menuitem', { name: 'Reset to default' }).click();
  await expect(zone(page, 'Bottom dock').getByRole('tab')).toHaveText(['Audio mixer', 'Outputs', 'Transitions']);
  await expect(page.locator('header.app-header')).toContainText('Layout: Editing');
});

/** Drags tab `name` until zone `label` shows, drops it there, and returns the zone. */
async function dropInto(page, name, label, text) {
  const box = await tab(page, name).boundingBox();
  await page.mouse.move(box.x + 5, box.y + 5);
  await page.mouse.down();
  await page.mouse.move(box.x + 40, box.y + 40, { steps: 4 });
  const target = zone(page, label);
  if (text) await expect(target).toContainText(text);
  const at = await centre(target);
  await page.mouse.move(at.x, at.y, { steps: 8 });
  await page.mouse.up();
  return target;
}

test('a drop past an outer edge adds a full-width row or a full-height column, which goes once empty', async ({ page, daemon }) => {
  await connect(page, daemon);
  const firstRow = zone(page, 'Bottom dock');
  // Past the bottom's row: a second full-width row under it, with its own splitter.
  const secondRow = await dropInto(page, 'Transitions', 'Bottom dock 2', 'Drop here: another full-width row, bottom');
  await expect(secondRow.getByRole('tab')).toHaveText(['Transitions']);
  await expect(page.getByRole('separator', { name: 'Resize bottom dock 2' })).toBeVisible();
  const [a, b] = [await firstRow.boundingBox(), await secondRow.boundingBox()];
  expect(b.y).toBeGreaterThanOrEqual(a.y + a.height);
  expect(Math.abs(b.width - a.width)).toBeLessThanOrEqual(1);
  // And a third past that.
  const thirdRow = await dropInto(page, 'Outputs', 'Bottom dock 3');
  await expect(thirdRow.getByRole('tab')).toHaveText(['Outputs']);

  // The sides stack full-height columns the same way, outwards.
  const firstColumn = await dropInto(page, 'Arrange', 'Outer left column', 'Drop here: outer left');
  const secondColumn = await dropInto(page, 'Filters', 'Outer left column 2', 'Drop here: another column, outer left');
  const [c, d] = [await firstColumn.boundingBox(), await secondColumn.boundingBox()];
  expect(d.x + d.width).toBeLessThanOrEqual(c.x + 1);
  expect(Math.abs(d.height - c.height)).toBeLessThanOrEqual(1);

  // Kept across a reload.
  await connect(page, daemon);
  await expect(zone(page, 'Bottom dock 3').getByRole('tab')).toHaveText(['Outputs']);
  await expect(zone(page, 'Outer left column 2').getByRole('tab')).toHaveText(['Filters']);

  // Moving the second row's panel out removes that row, and the third moves in.
  await drag(page, tab(page, 'Transitions'), await centre(tab(page, 'Audio mixer')));
  await expect(zone(page, 'Bottom dock 2').getByRole('tab')).toHaveText(['Outputs']);
  await expect(zone(page, 'Bottom dock 3')).toHaveCount(0);
});

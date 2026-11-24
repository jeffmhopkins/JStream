// Mapping a setting from its right-click menu: the Map control dialog and the Learn
// popover (docs/UI-FEATURES.md, VC-03). The mock engine hears no DAW and no MIDI, so the
// `liveSources` fixture says what is arriving.
import { test, expect, connect, rev, liveSources, showPanel } from '../fixtures.js';

const mods = (moved = {}) => {
  const values = Array(32).fill(0);
  for (const [slot, value] of Object.entries(moved)) values[slot - 1] = value;
  return { values, names: Array(32).fill('') };
};

/** Shows the mixer with the master's rows open, and returns the first fader's control
 * button and a point on the fader. The row also holds its mute's control button, first,
 * so the fader names its own setting. */
async function fader(page) {
  await showPanel(page, 'Audio mixer');
  await page.locator('.mixer-panel').getByRole('button', { name: /^Master Program output/ }).click();
  const row = page.locator('.mix-row').first();
  const button = row.locator(':scope > button.learn').nth(1);
  await expect(button).toBeVisible();
  const box = await row.locator('input.fader').boundingBox();
  return { button, x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

test('a right-click on a setting maps it to a Mod from the Map control dialog', async ({ page, daemon }) => {
  const live = await liveSources(page);
  live({ senders: { Bitwig: { params: mods({ 20: 0.62 }) } } });
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

  // A right-click on the fader's number opens the setting's menu, not the browser's.
  await page.locator('.mix-row').getByRole('textbox').first().click({ button: 'right' });
  const menu = page.getByRole('menu', { name: 'Control' });
  await expect(menu).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(menu).toBeHidden();

  await page.mouse.click(x, y, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Map control…' }).click();
  const dialog = page.getByRole('dialog', { name: /^Map control/ });
  await expect(dialog).toBeVisible();
  const use = dialog.locator('button[data-primary]');
  await expect(use).toBeDisabled();

  // Picking a source changes nothing until Use.
  await dialog.getByRole('button', { name: /^Mod 20/ }).click();
  await expect(use).toBeEnabled();
  expect(await rev(page)).toBe(before);

  await use.click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(button).toHaveClass(/bound/);
  await expect(button).toContainText('Mod 20');
});

test('at a UI scale of 150% a setting\'s right-click menu opens at the pointer', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await connect(page, daemon);
  for (let i = 0; i < 4; i += 1) await page.keyboard.press('Control+Equal');
  await expect.poll(() => page.evaluate(() => document.documentElement.style.zoom)).toBe('1.5');
  await fader(page);
  const number = page.locator('.mix-row').getByRole('textbox').first();
  await number.scrollIntoViewIfNeeded();
  const box = await number.boundingBox();
  const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
  await page.mouse.click(x, y, { button: 'right' });
  const menu = page.getByRole('menu', { name: 'Control' });
  await expect(menu).toBeVisible();
  // 12 px under the pointer, or 330 px above it when it wouldn't fit below, in the
  // page's own pixels: 18 and 495 on the screen.
  const placed = await menu.boundingBox();
  const off = Math.min(Math.abs(placed.y - (y + 18)), Math.abs(placed.y - (y - 495)));
  expect(off).toBeLessThan(3);
  expect(placed.y).toBeGreaterThanOrEqual(0);
});

test('Cancel in the Map control dialog changes nothing', async ({ page, daemon }) => {
  const live = await liveSources(page);
  live({ senders: { Bitwig: { params: mods() } } });
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

  await page.mouse.click(x, y, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Map control…' }).click();
  const dialog = page.getByRole('dialog', { name: /^Map control/ });
  await dialog.getByRole('button', { name: /^Mod 3/ }).first().click();
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  expect(await rev(page)).toBe(before);
  await expect(button).not.toHaveClass(/bound/);
});

test('Learn from the menu takes the one MIDI control that moves', async ({ page, daemon }) => {
  const live = await liveSources(page);
  const control = (value, count) => ({ input: 'Pads', channel: 1, controller: 21, value, count });
  // A Mod that is already moving when Learn starts is not what the user reached for.
  live({ senders: { Bitwig: { params: mods({ 4: 0.1 }) } }, controls: [control(10, 1)] });
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

  await page.mouse.click(x, y, { button: 'right' });
  await page.getByRole('menuitem', { name: /^Learn/ }).click();
  const popover = page.getByRole('dialog', { name: 'Learn' });
  await expect(popover).toBeVisible();
  await expect(popover).toContainText('Move a knob');

  live({ senders: { Bitwig: { params: mods({ 4: 0.1 }) } }, controls: [control(90, 2)] });
  await expect(popover).toBeHidden();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(button).toContainText('CC 21');
});

test('mapping mode maps the armed source by the amount dragged on a setting', async ({ page, daemon }) => {
  const live = await liveSources(page);
  live({ senders: { Bitwig: { params: mods({ 20: 0.62 }) } } });
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

  await page.mouse.click(x, y, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Map control…' }).click();
  const dialog = page.getByRole('dialog', { name: /^Map control/ });
  await dialog.getByRole('button', { name: /^Mod 20/ }).click();
  await dialog.getByRole('button', { name: 'Map by dragging' }).click();
  await expect(dialog).toBeHidden();
  const bar = page.locator('.mapping-bar');
  await expect(bar).toContainText('Mapping Mod 20');
  // Arming a source maps nothing by itself, and neither does a click on a setting.
  await page.mouse.click(x, y);
  expect(await rev(page)).toBe(before);

  // 20 px up is a tenth of the range: one edit, when the pointer is let go.
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y - 10, { steps: 3 });
  await page.mouse.move(x, y - 20, { steps: 3 });
  await expect(button).toHaveText('+10 %');
  expect(await rev(page)).toBe(before);
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(button).toHaveText('+10 %');

  // The same source dragged again moves the far end, as one more edit.
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y + 10, { steps: 4 });
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(before + 2);
  await expect(button).toHaveText('+5 %');

  // Escape closes a menu in front before it leaves mapping mode.
  await page.mouse.click(x, y, { button: 'right' });
  await expect(page.getByRole('menu', { name: 'Control' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu', { name: 'Control' })).toBeHidden();
  await expect(bar).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(bar).toBeHidden();
  await expect(button).toContainText('Mod 20');

  const number = page.locator('.mix-row').getByRole('textbox').first();
  // A drag makes an offset: the Mod adds to the fader. The dialog lists it, and can
  // bypass it without removing it.
  await number.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Edit mapping…' }).click();
  const row = dialog.locator('.map-row');
  await expect(row).toHaveCount(1);
  await expect(row).toContainText('Offset');
  await row.getByRole('button', { name: 'Bypass' }).click();
  await expect.poll(() => rev(page)).toBe(before + 3);
  await expect(row).toHaveClass(/bypassed/);
  await expect(button).toHaveClass(/bypassed/);
  await row.getByRole('button', { name: 'Bypass' }).click();
  await expect.poll(() => rev(page)).toBe(before + 4);
  await expect(row).not.toHaveClass(/bypassed/);

  // A second source overrides the fader; the offset stays on top of it.
  await dialog.getByRole('button', { name: /^Mod 3/ }).first().click();
  await dialog.getByRole('radio', { name: 'Override' }).click();
  await dialog.locator('button[data-primary]').click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(before + 5);
  await expect(button).toContainText('Mod 3');
  await number.click({ button: 'right' });
  await expect(page.getByRole('menu', { name: 'Control' })).toContainText('Mapped to Mod 3 · Bitwig and 1 more');
  await page.getByRole('menuitem', { name: 'Edit mapping…' }).click();
  await expect(row).toHaveCount(2);
  await expect(row.filter({ hasText: 'Override' })).toContainText('Mod 3');
  await expect(row.filter({ hasText: 'Offset' })).toContainText('Mod 20');
});

test('the Audio tab maps a level of the sound onto a setting', async ({ page, daemon }) => {
  await liveSources(page);
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

  await page.mouse.click(x, y, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Map control…' }).click();
  const dialog = page.getByRole('dialog', { name: /^Map control/ });
  await dialog.getByRole('tab', { name: 'Audio' }).click();
  const use = dialog.locator('button[data-primary]');
  await expect(use).toBeDisabled();
  await dialog.getByRole('button', { name: 'Follow the program mix' }).click();
  await expect(dialog.getByRole('combobox', { name: 'What it hears' })).toBeVisible();
  await expect(dialog.locator('.map-chosen')).toContainText('Audio · Program mix · Level');
  await use.click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(button).toHaveText('Audio');
});

test('an item property is mapped by offsets, one per source, from the Inspector', async ({ page, daemon }) => {
  const live = await liveSources(page);
  live({ senders: { Bitwig: { params: mods({ 20: 0.62 }) } } });
  await connect(page, daemon);
  await page.locator('.sources-panel li.row').first().click();
  const inspector = page.locator('.panel-inspector');
  const tag = (property) => inspector.locator(`[data-map="item:box:${property}"] .stepper`);
  const rotation = inspector.getByRole('textbox', { name: 'Rotation' });
  await expect(rotation).toBeVisible();
  const before = await rev(page);
  // The crop edges are targets too.
  await expect(inspector.locator('[data-map="item:box:crop_left"]')).toBeVisible();

  // A right-click on the number opens the property's menu, not the browser's.
  await rotation.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Map control…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Map control' });
  await expect(dialog).toContainText('Map control: Rotation');
  await dialog.getByRole('button', { name: /^Mod 20/ }).click();
  await dialog.getByRole('button', { name: 'Map by dragging' }).click();
  expect(await rev(page)).toBe(before);

  // 50 px up is a quarter of a turn, added to the rotation the item keeps.
  const box = await rotation.boundingBox();
  const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y - 50, { steps: 5 });
  await expect(tag('rotation')).toHaveAttribute('data-map-amount', '+90°');
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(before + 1);
  await expect(rotation).toHaveValue('0.0');

  // The same source maps a second property while it stays armed.
  const across = await inspector.getByRole('textbox', { name: 'X' }).boundingBox();
  await page.mouse.move(across.x + 20, across.y + 10);
  await page.mouse.down();
  await page.mouse.move(across.x + 20, across.y + 40, { steps: 5 });
  await page.mouse.up();
  await expect.poll(() => rev(page)).toBe(before + 2);
  await expect(tag('x')).toHaveAttribute('data-map-amount', '-150 px');
  await page.keyboard.press('Escape');
  await expect(tag('rotation')).toHaveAttribute('data-map-amount', '+90°');

  // The property's menu bypasses a mapping, and removes it.
  await rotation.click({ button: 'right' });
  const menu = page.getByRole('menu', { name: 'Control' });
  await expect(menu).toContainText('Mod 20 · +90°');
  await menu.getByRole('menuitemcheckbox', { name: 'Bypass' }).click();
  await expect.poll(() => rev(page)).toBe(before + 3);
  await rotation.click({ button: 'right' });
  await expect(menu.getByRole('menuitemcheckbox', { name: 'Bypass' })).toHaveAttribute('aria-checked', 'true');
  await menu.getByRole('menuitem', { name: 'Remove' }).click();
  await expect.poll(() => rev(page)).toBe(before + 4);
  await expect(tag('rotation')).not.toHaveAttribute('data-map-amount');
});

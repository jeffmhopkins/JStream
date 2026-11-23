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
 * button and a point on its row beside it. */
async function fader(page) {
  await showPanel(page, 'Audio mixer');
  await page.locator('.mixer-panel').getByRole('button', { name: /^Master Program output/ }).click();
  const button = page.locator('.mix-row > button.learn').first();
  await expect(button).toBeVisible();
  const box = await button.boundingBox();
  return { button, x: box.x - 60, y: box.y + box.height / 2 };
}

test('a right-click on a setting maps it to a Mod from the Map control dialog', async ({ page, daemon }) => {
  const live = await liveSources(page);
  live({ senders: { Bitwig: { params: mods({ 20: 0.62 }) } } });
  await connect(page, daemon);
  const { button, x, y } = await fader(page);
  const before = await rev(page);

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

  await page.keyboard.press('Escape');
  await expect(bar).toBeHidden();
  await expect(button).toContainText('Mod 20');
});

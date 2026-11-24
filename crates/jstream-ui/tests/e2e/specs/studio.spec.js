// Studio mode and the Transitions panel. In studio mode the canvas is the preview side,
// with the studio controls and the program monitor beside it: clicking a scene puts it
// on preview, Transition takes it to program and swaps the old program scene back onto
// preview, which the canvas then shows. The Transitions panel edits the presets, with a
// custom curve's points. The seed puts `Main` on program. The mock engine has only the
// initial `cut` preset, can't change presets and sends no pictures, so the editor's
// changes are checked by what the page sends. With studio mode off the canvas stands for
// program and follows every program change, whatever made it.
import { test, expect, connect, showPanel, recordCalls } from '../fixtures.js';

/** Connects and turns studio mode on; returns the studio controls. */
async function openStudio(page, daemon) {
  await connect(page, daemon);
  await page.locator('header.app-header').getByRole('button', { name: 'Studio mode' }).click();
  return page.locator('section.studio-controls');
}

/** Clicks a scene's row in the Scenes panel. */
async function clickScene(page, name) {
  await page.locator('.scenes-panel li.row').filter({ hasText: name }).locator('.row-main').click();
}

test('studio mode shows preview and program side by side, with the controls between', async ({ page, daemon }) => {
  await connect(page, daemon);
  const row = page.locator('.stage-row');
  await expect(row).not.toHaveClass(/two-up/);
  await expect(page.getByRole('region', { name: 'Program' })).toHaveCount(0);
  await expect(page.locator('section.studio-controls')).toHaveCount(0);

  const button = page.locator('header.app-header').getByRole('button', { name: 'Studio mode' });
  await button.click();
  await expect(row).toHaveClass(/two-up/);
  // Turning it on puts the scene the canvas shows on preview.
  await expect(page.locator('.stage-label').first()).toHaveText('Preview · Main');
  await expect(page.locator('.stage-label.program')).toHaveText('Program · Main');
  await expect(page.getByRole('region', { name: 'Program' }).locator('canvas')).toHaveCount(1);
  const canvas = await page.getByRole('region', { name: 'Canvas' }).boundingBox();
  const program = await page.getByRole('region', { name: 'Program' }).boundingBox();
  const controls = await page.locator('section.studio-controls').boundingBox();
  expect(canvas.x + canvas.width).toBeLessThanOrEqual(controls.x + 1);
  expect(controls.x + controls.width).toBeLessThanOrEqual(program.x + 1);

  await button.click();
  await expect(row).not.toHaveClass(/two-up/);
  await expect(page.locator('section.studio-controls')).toHaveCount(0);
});

test('a portrait canvas stacks preview over the controls over program when that shows them larger', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await connect(page, daemon);
  await page.locator('header.app-header .top-menu[data-menu="file"] > .menu-button').click();
  await page.getByRole('menu', { name: 'File' }).getByRole('menuitem', { name: 'Video settings…' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('combobox', { name: 'Base resolution' }).selectOption({ label: '1080 × 1920 (9:16)' });
  await settings.getByRole('button', { name: 'Save' }).click();
  await expect(settings).toHaveCount(0);

  // In a widescreen window two 9:16 pictures fit larger side by side.
  await page.locator('header.app-header').getByRole('button', { name: 'Studio mode' }).click();
  const row = page.locator('.stage-row');
  await expect(row).toHaveClass(/two-up/);
  await expect(row).not.toHaveClass(/stacked/);
  // In a window on a portrait monitor they stack.
  await page.setViewportSize({ width: 1080, height: 1900 });
  await expect(row).toHaveClass(/stacked/);
  const canvas = await page.getByRole('region', { name: 'Canvas' }).boundingBox();
  const controls = await page.locator('section.studio-controls').boundingBox();
  const program = await page.getByRole('region', { name: 'Program' }).boundingBox();
  expect(canvas.y + canvas.height).toBeLessThanOrEqual(controls.y + 1);
  expect(controls.y + controls.height).toBeLessThanOrEqual(program.y + 1);
  // Each picture gets the stage's width.
  expect(Math.abs(canvas.width - program.width)).toBeLessThan(2);
  if (process.env.JSTREAM_E2E_SHOTS) await page.screenshot({ path: `${process.env.JSTREAM_E2E_SHOTS}/studio-portrait.png` });
});

test('a transition takes preview to program, and the old program comes back to preview', async ({ page, daemon }) => {
  const studio = await openStudio(page, daemon);
  await expect(studio.getByText('That scene is already on program.', { exact: false })).toBeVisible();
  await expect(studio.getByRole('button', { name: 'Transition' })).toBeDisabled();
  await expect(studio.getByRole('button', { name: 'Cut' })).toBeDisabled();

  await clickScene(page, 'Intro');
  await expect(page.locator('.stage-label.cued')).toHaveText('Preview · Intro');
  await expect(studio.getByText('Uses Cut (the default).')).toBeVisible();
  await studio.getByRole('button', { name: 'Transition' }).click();
  await expect(page.locator('.stage-label.program')).toHaveText('Program · Intro');
  // Swapped: Main is on preview, and the canvas shows it.
  await expect(page.locator('.stage-label.cued')).toHaveText('Preview · Main');
  await expect(page.locator('.canvas-caption')).toContainText('Main');
});

test('without swap the scene stays on preview, and Cut takes it too', async ({ page, daemon }) => {
  const studio = await openStudio(page, daemon);
  await studio.getByLabel('Swap').uncheck();
  await clickScene(page, 'Intro');
  await expect(page.locator('.stage-label.cued')).toHaveText('Preview · Intro');
  await studio.getByRole('button', { name: 'Cut' }).click();
  await expect(page.locator('.stage-label.program')).toHaveText('Program · Intro');
  await expect(page.locator('.stage-label.cued')).toHaveText('Preview · Intro');
  await expect(studio.getByRole('button', { name: 'Transition' })).toBeDisabled();
});

test('with studio mode off the canvas follows every program change, a hotkey\'s and another client\'s too (#1733)', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);
  const caption = page.locator('.canvas-caption');
  await expect(caption).toContainText('Main');

  // Another client puts Intro on program: this canvas shows it.
  await clickScene(other, 'Intro');
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Intro' })).toHaveAttribute('aria-current', 'true');
  await expect(caption).toContainText('Intro');

  // The other client binds F1 to Main, which goes on program there and here.
  await clickScene(other, 'Main');
  await expect(caption).toContainText('Main');
  const row = other.locator('.panel-inspector .hotkey-row');
  await row.getByRole('button', { name: 'Set key' }).click();
  await other.keyboard.press('F1');
  await expect(row.locator('kbd')).toHaveText(['F1']);

  // A hotkey on this page switches back to Main from Intro: the canvas follows.
  await clickScene(page, 'Intro');
  await expect(caption).toContainText('Intro');
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('F1');
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Main' })).toHaveAttribute('aria-current', 'true');
  await expect(caption).toContainText('Main');
  await expect(page.locator('.scenes-panel li.row').filter({ hasText: 'Main' })).toHaveClass(/selected/);
});

test('in studio mode a program change leaves the preview side, and turning it off shows program (#1733)', async ({ page, context, daemon }) => {
  // The other client connects first: studio mode is kept in the browser, which a page
  // reads when it loads.
  const other = await context.newPage();
  await connect(other, daemon);
  await openStudio(page, daemon);
  await expect(page.locator('.stage-label').first()).toHaveText('Preview · Main');

  // Another client switches program: the canvas stays on preview.
  await clickScene(other, 'Intro');
  await expect(page.locator('.stage-label.program')).toHaveText('Program · Intro');
  await expect(page.locator('.stage-label').first()).toHaveText('Preview · Main');
  await expect(page.locator('.canvas-caption')).toContainText('Main');

  // Off again, the canvas stands for program and shows it.
  await page.locator('header.app-header').getByRole('button', { name: 'Studio mode' }).click();
  await expect(page.locator('.stage-row')).not.toHaveClass(/two-up/);
  await expect(page.locator('.canvas-caption')).toContainText('Intro');
});

test('the Transitions panel points at the controls in studio mode', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  const panel = page.locator('section.studio');
  await expect(panel.locator('.studio-foot')).toHaveText('Studio mode off · a click on a scene puts it on program');
  await page.locator('header.app-header').getByRole('button', { name: 'Studio mode' }).click();
  await expect(panel.locator('.studio-foot')).toHaveText('Studio mode on · Transition and Cut take preview to program');
  await expect(panel.getByLabel('Transition preset')).toHaveCount(0);
});

test('the preset toolbar edits the default, adds a preset and holds the rest in its ⋯ menu', async ({ page, daemon, consoleErrors }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  const presets = page.locator('section.presets');
  const picker = presets.getByLabel('Preset to edit');
  await expect(picker.locator('option')).toHaveText(['Cut · Default']);
  await expect(presets.getByRole('combobox', { name: 'Type' })).toHaveValue('cut');
  // A cut has no duration or easing.
  await expect(presets.getByLabel('Duration')).toHaveCount(0);
  await expect(presets.getByText('The default: used unless a scene overrides it. Overrides: 0 in, 0 out.')).toBeVisible();

  // The ⋯ menu: the default is checked, and can't be set again or deleted.
  await presets.getByRole('button', { name: 'More for this preset' }).click();
  const menu = page.getByRole('menu');
  await expect(menu.getByRole('menuitemcheckbox', { name: 'Set as default' })).toHaveAttribute('aria-checked', 'true');
  await expect(menu.getByRole('menuitem', { name: 'Delete…' })).toBeDisabled();
  await page.keyboard.press('Escape');

  // The mock refuses preset edits, after the page has sent them.
  consoleErrors.allow(/transition/);
  await presets.getByRole('combobox', { name: 'Type' }).selectOption('fade');
  await expect.poll(() => calls.sent('transition.update').length).toBe(1);
  expect(calls.sent('transition.update')[0]).toMatchObject({ preset_id: 'cut', kind: 'fade' });

  // Rename swaps the picker for the name; Enter sends it.
  await presets.getByRole('button', { name: 'More for this preset' }).click();
  await page.getByRole('menu').getByRole('menuitem', { name: 'Rename' }).click();
  const name = presets.getByRole('textbox', { name: 'Preset name' });
  await expect(name).toBeFocused();
  await name.fill('Straight cut');
  await name.press('Enter');
  await expect(name).toHaveCount(0);
  await expect.poll(() => calls.sent('transition.update').length).toBe(2);
  expect(calls.sent('transition.update')[1]).toMatchObject({ preset_id: 'cut', name: 'Straight cut' });

  await presets.getByRole('button', { name: 'New preset' }).click();
  await expect.poll(() => calls.sent('transition.create').length).toBe(1);
  expect(calls.sent('transition.create')[0]).toMatchObject({
    preset_id: 'new-preset',
    name: 'New preset',
    kind: 'fade',
    duration_ms: 500,
    easing: 'ease_in_out',
  });
});

test('a preset\'s custom curve shows its points, which are sent as its easing', async ({ page, daemon, consoleErrors }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await showPanel(page, 'Transitions');
  const presets = page.locator('section.presets');
  // The mock refuses preset edits, after the page has sent them.
  consoleErrors.allow(/transition/);
  await presets.getByRole('combobox', { name: 'Type' }).selectOption('fade');
  await expect(presets.locator('.curve-editor')).toHaveCount(0);
  await presets.getByLabel('Easing', { exact: true }).selectOption({ label: 'Custom curve' });
  const editor = presets.locator('.curve-editor');
  await expect(editor.getByRole('img', { name: 'The custom curve' })).toBeVisible();

  // A point past its range is kept to it, and sent as the preset's easing.
  const x1 = editor.getByRole('textbox', { name: 'x1', exact: true });
  await x1.fill('1.7');
  await x1.press('Enter');
  await expect.poll(() => calls.sent('transition.update').filter((c) => c.easing?.cubic_bezier).length).toBeGreaterThan(1);
  const sent = calls.sent('transition.update').filter((c) => c.easing?.cubic_bezier);
  expect(sent.at(-1).easing.cubic_bezier).toMatchObject({ x1: 1 });
});

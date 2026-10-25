// Scene transitions outside the Transitions panel: a scene's Transition in and out
// submenus (on its row and on the canvas margin), its Properties dialog with the hotkey
// that switches to it, and an item's Scene switches in the Inspector. The mock engine has
// only the initial `cut` preset and doesn't implement `scene.set_transitions` or
// `item.set_transition`, so those are checked by what the page sends; it does keep
// hotkeys and names.
import { test, expect, connect, recordCalls } from '../fixtures.js';

/** The open right-click menu. */
function menu(page) {
  return page.getByRole('menu', { name: 'Context menu' });
}

/** The row of scene `name` in the scene list. */
function scene(page, name) {
  return page.locator('.scenes-panel li.row').filter({ hasText: name });
}

test('a scene row sets its transitions from submenus', async ({ page, daemon, consoleErrors }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await scene(page, 'Intro').click({ button: 'right' });
  const opener = menu(page).getByRole('menuitem', { name: 'Transition in' });
  await expect(opener.locator('.hint')).toHaveText('Default');
  await opener.hover();
  const sub = page.locator('.context-menu.submenu');
  await expect(sub.locator('.ctx-label')).toHaveText(['Default (Cut)', 'Cut', 'Manage presets…']);
  await expect(sub.getByRole('menuitemradio', { name: 'Default (Cut)' })).toHaveAttribute('aria-checked', 'true');
  await expect(sub.getByText('Override')).toBeVisible();

  consoleErrors.allow(/scene\.set_transitions/);
  // The preset's line reads "Cut", with its kind ("Cut") as the hint.
  const cut = sub.getByRole('menuitemradio').filter({ has: page.locator('.ctx-label', { hasText: /^Cut$/ }) });
  await expect(cut.locator('.hint')).toHaveText('Cut');
  await cut.click();
  await expect.poll(() => calls.sent('scene.set_transitions').length).toBe(1);
  expect(calls.sent('scene.set_transitions')[0]).toEqual({
    type: 'scene.set_transitions',
    scene_id: 'intro',
    transition_in: 'cut',
  });

  // Manage presets… shows the Transitions panel.
  await scene(page, 'Intro').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Transition out' }).hover();
  await page.locator('.context-menu.submenu').getByRole('menuitem', { name: 'Manage presets…' }).click();
  await expect(page.getByRole('tabpanel', { name: 'Transitions', exact: true })).toBeVisible();
});

test('a double click opens the scene properties, which set its name and hotkey', async ({ page, daemon }) => {
  await connect(page, daemon);
  await scene(page, 'Intro').locator('.row-main').dblclick();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel('Name')).toHaveValue('Intro');
  await expect(dialog.getByLabel('Transition in')).toHaveValue('');
  await expect(dialog.getByLabel('Transition in').locator('option')).toHaveText(['Default (Cut)', 'Cut']);

  const hotkey = dialog.getByLabel('Hotkey');
  await hotkey.click();
  await page.keyboard.press('Control+Digit3');
  await expect(hotkey).toHaveValue('Ctrl+3');
  await page.keyboard.press('Backspace');
  await expect(hotkey).toHaveValue('');
  await page.keyboard.press('F3');
  await expect(hotkey).toHaveValue('F3');
  await dialog.getByLabel('Name').fill('Opening');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator('.scenes-panel .tree-label')).toHaveText(['Opening', 'Main']);

  // The dialog reads the hotkey back.
  await scene(page, 'Opening').click({ button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Properties…' }).click();
  await expect(dialog.getByLabel('Hotkey')).toHaveValue('F3');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
});

test('the canvas margin opens the shown scene\'s properties', async ({ page, daemon }) => {
  await connect(page, daemon);
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  await page.mouse.click(Math.round(-10 * m.a + m.e), Math.round(540 * m.d + m.f), { button: 'right' });
  await menu(page).getByRole('menuitem', { name: 'Scene properties…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog.getByLabel('Name')).toHaveValue('Main');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

test('an item\'s scene switches are sent from the Inspector', async ({ page, daemon, consoleErrors }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await page.locator('.sources-panel li.row').filter({ hasText: 'Backdrop' }).locator('.row-main').click();
  const inspector = page.locator('.inspector');
  const fold = inspector.locator('section.fold').filter({ hasText: 'Scene switches' });
  await fold.getByRole('button', { name: 'Scene switches' }).click();
  await expect(fold.getByLabel('Glide to other scenes')).toHaveValue('auto');
  await expect(fold.getByLabel('Appear', { exact: true })).toHaveValue('inherit');

  consoleErrors.allow(/item\.set_transition/);
  await fold.getByLabel('Appear', { exact: true }).selectOption({ label: 'Slide' });
  await expect.poll(() => calls.sent('item.set_transition').length).toBe(1);
  expect(calls.sent('item.set_transition')[0]).toMatchObject({
    item_id: 'box',
    transition: { appear: 'slide', glide: 'auto', easing: 'inherit', disappear: 'inherit', delay_ms: 0 },
  });
});

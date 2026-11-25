// 3D scenes (ADR-0094): the scene Properties dialog's camera, and the canvas seen
// through it, where Alt-drag orbits the camera. The mock engine keeps scene cameras.
import { test, expect, connect, describeKinds, openAddSource, recordCalls, rev } from '../fixtures.js';

/** The seed item's outline in the overlay. */
function outline(page) {
  return page.locator('.stage-overlay g').filter({ hasText: 'Backdrop' }).locator('polygon').first();
}

/** The outline's corners as `[x, y]` pairs, in canvas pixels. */
async function corners(page) {
  const points = await outline(page).getAttribute('points');
  return points.trim().split(/\s+/).map((pair) => pair.split(',').map(Number));
}

/** Where canvas point (`x`, `y`) is on the screen, in whole pixels. */
async function onScreen(page, x, y) {
  const m = await page.locator('svg.stage-overlay').evaluate((svg) => {
    const { a, d, e, f } = svg.getScreenCTM();
    return { a, d, e, f };
  });
  return { x: Math.round(x * m.a + m.e), y: Math.round(y * m.d + m.f) };
}

/** Opens the shown scene's Properties from the canvas margin. */
async function sceneProperties(page) {
  const margin = await onScreen(page, -10, 540);
  await page.mouse.click(margin.x, margin.y, { button: 'right' });
  await page
    .getByRole('menu', { name: 'Context menu' })
    .getByRole('menuitem', { name: 'Scene properties…' })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Scene properties' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('scene properties give a scene a camera, and Alt-drag orbits it', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const flat = await corners(page);

  // Switched on, the camera starts from one that shows the scene as it is.
  let dialog = await sceneProperties(page);
  const toggle = dialog.getByRole('checkbox', { name: '3D camera' });
  await expect(toggle).not.toBeChecked();
  await toggle.check();
  await expect(dialog.getByLabel('Position X')).toHaveValue('960');
  await expect(dialog.getByLabel('Target Z')).toHaveValue('0');
  await expect(dialog.getByRole('checkbox', { name: 'Nearest on top' })).toBeChecked();
  const near = dialog.getByLabel('Position Z');
  await near.fill('1500');
  await near.press('Tab');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => calls.sent('scene.set_camera').length).toBe(1);
  const sent = calls.sent('scene.set_camera')[0];
  expect(sent.scene_id).toBe('main');
  expect(sent.camera.position.z).toBe(1500);

  // Seen from further back than the framing camera, the box draws smaller.
  await expect.poll(async () => {
    const [[x0], [x1]] = await corners(page);
    return x1 - x0;
  }).toBeLessThan(flat[1][0] - flat[0][0] - 1);

  // The dialog reads the camera back.
  dialog = await sceneProperties(page);
  await expect(dialog.getByRole('checkbox', { name: '3D camera' })).toBeChecked();
  await expect(dialog.getByLabel('Position Z')).toHaveValue('1500');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();

  // Alt-drag orbits the camera, live on output, and saves nothing (#1809): the Save bar
  // shows instead.
  const before = await corners(page);
  const startRev = await rev(page);
  const from = await onScreen(page, 960, 540);
  const orbit = async () => {
    await page.keyboard.down('Alt');
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 60, from.y, { steps: 6 });
    await expect(page.locator('.stage-overlay .readout text')).toHaveText(/^Camera -?\d+, -?\d+, -?\d+ · 1500 px from target · \d+°$/);
    await page.mouse.up();
    await page.keyboard.up('Alt');
  };
  const bar = page.getByRole('toolbar', { name: 'Unsaved camera' });
  await expect(bar).toBeHidden();
  await orbit();
  await expect.poll(() => calls.sent('live.set').length).toBeGreaterThan(0);
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('Camera moved, not saved');
  expect(await corners(page)).not.toEqual(before);
  await page.waitForTimeout(500);
  expect(await rev(page)).toBe(startRev);

  // Leaving the scene and coming back puts the saved camera back.
  const sceneRow = (name) => page.locator('.scenes-panel li.row').filter({ hasText: name }).locator('.row-main');
  await sceneRow('Intro').click();
  await expect(bar).toBeHidden();
  await sceneRow('Main').click();
  await expect.poll(async () => JSON.stringify(await corners(page))).toBe(JSON.stringify(before));
  await expect(bar).toBeHidden();
  expect(await rev(page)).toBe(startRev);

  // Revert puts it back too, without leaving.
  await orbit();
  await expect(bar).toBeVisible();
  await bar.getByRole('button', { name: 'Revert' }).click();
  await expect(bar).toBeHidden();
  await expect.poll(async () => JSON.stringify(await corners(page))).toBe(JSON.stringify(before));
  expect(await rev(page)).toBe(startRev);

  // Save commits the moved camera as one revision, ending the gesture.
  await orbit();
  const shown = await corners(page);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(bar).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const orbited = calls.sent('scene.set_camera').at(-1);
  expect(orbited.scene_id).toBe('main');
  expect(orbited.camera.position.x).not.toBe(960);
  expect(await corners(page)).toEqual(shown);

  // Saved, it stays when the scene is left and shown again.
  await sceneRow('Intro').click();
  await sceneRow('Main').click();
  await expect.poll(async () => JSON.stringify(await corners(page))).toBe(JSON.stringify(shown));

  // One undo puts the camera back.
  await page.locator('.preview.stage').focus();
  await page.keyboard.press('Control+z');
  await expect.poll(async () => JSON.stringify(await corners(page))).toBe(JSON.stringify(before));
});

test('the live aim drives an axis only while a button is held', async ({ page, daemon }) => {
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await expect(outline(page)).toBeVisible();
  const dialog = await sceneProperties(page);
  await dialog.getByRole('checkbox', { name: '3D camera' }).check();
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await page.getByRole('button', { name: 'Camera mode' }).click();
  const aim = (name) => page.getByRole('button', { name, exact: true });
  await expect(aim('Pan left')).toBeVisible();
  const drives = () => calls.sent('scene.drive_camera');
  const away = await onScreen(page, 960, 540);

  // Passing the pointer over the buttons sends nothing, so it stops no axis that a
  // controller or a binding drives.
  for (const name of ['Pan left', 'Tilt up', 'Tilt down', 'Pan right', 'Zoom out', 'Zoom in']) {
    await aim(name).hover();
  }
  await page.mouse.move(away.x, away.y);
  await page.waitForTimeout(400);
  expect(drives()).toHaveLength(0);

  // A press drives its axis alone; the pointer may wander off the button while held,
  // and the release stops it once.
  await aim('Pan right').hover();
  await page.mouse.down();
  await expect.poll(() => drives().length).toBe(1);
  expect(drives()[0]).toMatchObject({ scene_id: 'main', pan: 12 });
  expect(drives()[0].tilt ?? null).toBeNull();
  await page.mouse.move(away.x, away.y, { steps: 4 });
  await page.waitForTimeout(300);
  expect(drives()).toHaveLength(1);
  await page.mouse.up();
  await expect.poll(() => drives().length).toBe(2);
  expect(drives()[1]).toMatchObject({ scene_id: 'main', pan: 0 });
  await page.waitForTimeout(300);
  expect(drives()).toHaveLength(2);

  // Space holds a focused button, its repeats send nothing more, and its release stops.
  await aim('Tilt up').focus();
  await page.keyboard.down('Space');
  await page.keyboard.down('Space');
  await page.keyboard.down('Space');
  await expect.poll(() => drives().length).toBe(3);
  expect(drives()[2]).toMatchObject({ tilt: 12 });
  await page.waitForTimeout(300);
  expect(drives()).toHaveLength(3);
  await page.keyboard.up('Space');
  await expect.poll(() => drives().length).toBe(4);
  expect(drives()[3]).toMatchObject({ tilt: 0 });

  // Enter holds one too, Shift slowly, and moving the focus away stops it, once.
  await aim('Zoom in').focus();
  await page.keyboard.down('Shift');
  await page.keyboard.down('Enter');
  await page.keyboard.up('Shift');
  await expect.poll(() => drives().length).toBe(5);
  expect(drives()[4]).toMatchObject({ zoom: 2 });
  await aim('Zoom out').focus();
  await expect.poll(() => drives().length).toBe(6);
  expect(drives()[5]).toMatchObject({ zoom: 0 });
  await page.keyboard.up('Enter');
  await page.waitForTimeout(300);
  expect(drives()).toHaveLength(6);
});

// A window capture so Follow pointer can be added. The mock describes no kinds.
const WINDOW = [
  {
    kind_id: 'window', category: 'source', name: 'Window capture', version: 1, video: true, audio: false,
    params: [],
  },
];

/** Whether `.props-settings` has grown wider than the column. */
function settingsFit(dialog) {
  return dialog.locator('.props-settings').evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
}

test('the 3D camera section fits the settings column', async ({ page, daemon }) => {
  await describeKinds(page, WINDOW);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Window capture' }).click();
  await add.getByRole('button', { name: 'Create' }).click();
  await expect(add).toBeHidden();
  const source = page.getByRole('dialog', { name: 'Properties' });
  await source.getByRole('button', { name: 'Cancel' }).click();
  await expect(source).toBeHidden();

  const dialog = await sceneProperties(page);
  await dialog.getByRole('checkbox', { name: '3D camera' }).check();
  await dialog.getByRole('checkbox', { name: 'Depth of field' }).check();
  // Follow pointer is a filter on the capture (ADR-0120): the section offers to add it.
  await expect(dialog.getByRole('button', { name: 'Add Follow pointer…' })).toBeVisible();
  await dialog.getByRole('button', { name: '+ Add camera motion' }).click();

  // Default 860-wide dialog, then the dialog clamped to a shorter viewport.
  await expect.poll(() => settingsFit(dialog)).toBe(true);
  await page.setViewportSize({ width: 720, height: 640 });
  await expect.poll(() => settingsFit(dialog)).toBe(true);

  // The check sits in front of its text, on the same line.
  const aligned = await dialog.locator('.camera-check', { hasText: 'Nearest on top' }).evaluate((el) => {
    const box = el.querySelector('input').getBoundingClientRect();
    const text = el.querySelector('span').getBoundingClientRect();
    return box.right <= text.left + 1 && box.top < text.bottom && text.top < box.bottom;
  });
  expect(aligned).toBe(true);

  // The angle stepper keeps room for the number, not only the degree mark.
  const rollWidth = await dialog.locator('input[aria-label="Roll"]').evaluate((el) => el.clientWidth);
  expect(rollWidth).toBeGreaterThan(28);
});

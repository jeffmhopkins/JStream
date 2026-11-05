// The Properties dialog: opened from an item's menu, it edits a draft of the source's
// name and settings that OK sends as one edit, showing each setting the kind marks live
// as a live value meanwhile (ADR-0091), and lists the source's filters (seeded: Sharpen,
// then Blur turned off). Dragging its title moves the dialog, which reopens where it
// was left, and the header's × still closes it. The mock engine
// describes no kinds, so the settings form itself is covered natively; the webcam's
// live picture gets its descriptor from the page's socket (`describeKinds`).
import { test, expect, connect, describeKinds, openAddSource, rev } from '../fixtures.js';

/** The rows of the Sources panel. */
function rows(page) {
  return page.locator('.sources-panel li.row');
}

/** Opens the Properties dialog of the top item from its row's menu. */
async function openProperties(page) {
  await rows(page).first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Properties…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test('the dialog shows the source, and Cancel drops the draft', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  await expect(dialog.locator('.modal-title')).toHaveText('Color properties');
  await expect(dialog.locator('.modal-sub')).toHaveText('Backdrop');
  // The source's filters, as the Filters panel shows them (filters.spec.js).
  await expect(dialog.getByRole('list', { name: 'Video filters' }).getByRole('listitem')).toHaveCount(2);
  await expect(dialog.getByRole('button', { name: '+ Add filter…' })).toBeEnabled();

  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Backdrop');
  expect(await rev(page)).toBe(startRev);

  // Escape cancels too.
  await openProperties(page);
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

test('OK renames the source in one edit, and Enter presses OK', async ({ page, daemon }) => {
  await connect(page, daemon);
  let dialog = await openProperties(page);
  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Sky');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  await expect.poll(() => rev(page)).toBe(startRev + 1);

  // A blank name is ignored, and with nothing changed OK sends nothing.
  dialog = await openProperties(page);
  await dialog.getByRole('textbox', { name: 'Name' }).fill('  ');
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(rows(page).first()).toContainText('Sky');
  expect(await rev(page)).toBe(startRev + 1);
});

const WEBCAM = {
  kind_id: 'webcam', category: 'source', name: 'Webcam', version: 1, video: true, audio: true,
  params: [{ id: 'device', type: 'string', multiline: false, max_len: 1024, default: '', label: 'Device' }],
};

test('a webcam shows a live picture of the device being picked, before OK', async ({ page, daemon }) => {
  // Every request the page sends, recorded on its way to the mock engine.
  const requests = [];
  await describeKinds(page, [WEBCAM], { refuse: (request) => { requests.push(request); return null; } });
  const sent = (type) => requests.map((r) => r.call?.action).filter((a) => a?.type === type);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Video capture device' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog.locator('.modal-title')).toHaveText('Video capture device properties');
  await expect(dialog.getByLabel('Preview')).toBeVisible();

  // The draft's device, streamed on a channel of its own, apart from the canvas's.
  await expect.poll(() => sent('preview.subscribe').filter((a) => a.source).length).toBe(1);
  const [first] = sent('preview.subscribe').filter((a) => a.source);
  // The mock stores the settings as sent: none, so every parameter at its default.
  expect(first.source).toEqual({ kind: 'webcam' });
  expect(first.scene_id).toBeUndefined();
  expect(first.channel).toBeGreaterThanOrEqual(2 ** 31);

  // Picking another device opens a stream of it, closing the first, with nothing saved.
  const startRev = await rev(page);
  await dialog.getByRole('textbox', { name: 'Device' }).fill('usb-cam-2');
  await dialog.getByRole('textbox', { name: 'Device' }).press('Tab');
  await expect.poll(() => sent('preview.subscribe').filter((a) => a.source).length).toBe(2);
  const second = sent('preview.subscribe').filter((a) => a.source)[1];
  expect(second.source.settings).toEqual({ device: 'usb-cam-2' });
  expect(second.channel).not.toBe(first.channel);
  await expect.poll(() => sent('preview.unsubscribe').map((a) => a.channel)).toContain(first.channel);
  expect(await rev(page)).toBe(startRev);

  // Closing the dialog closes its stream.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => sent('preview.unsubscribe').map((a) => a.channel)).toContain(second.channel);
});

test('a source that captures nothing shows no picture', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  await expect(dialog.locator('.modal-title')).toHaveText('Color properties');
  await expect(dialog.getByLabel('Preview')).toHaveCount(0);
});

const COLOR = {
  kind_id: 'color', category: 'source', name: 'Color', version: 1, video: true, audio: false,
  params: [
    { id: 'color', type: 'color', alpha: true, default: '#000000ff', label: 'Color', live: true },
    { id: 'width', type: 'int', min: 1, max: 16384, step: 1, unit: 'px', default: 1920, label: 'Width', live: false },
  ],
};

test('a live setting shows as a live value, OK commits it in one edit, and Cancel leaves nothing', async ({ page, daemon }) => {
  // Every request the page sends, as text.
  const requests = [];
  await describeKinds(page, [COLOR], { refuse: (request) => { requests.push(JSON.stringify(request)); return null; } });
  const sent = (marker) => requests.filter((r) => r.includes(marker));
  await connect(page, daemon);
  const startRev = await rev(page);

  // A changed color goes out as a live value of the dialog's gesture, not as an edit.
  let dialog = await openProperties(page);
  await dialog.locator('input.color-text').fill('#00ff00ff');
  await dialog.locator('input.color-text').press('Tab');
  await expect.poll(() => sent('"live.set"').length).toBe(1);
  expect(sent('"live.set"')[0]).toContain('"source.set_settings"');
  expect(sent('"live.set"')[0]).toContain('#00ff00ff');
  expect(await rev(page)).toBe(startRev);

  // Cancel drops it: the gesture is cancelled and nothing is saved.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => sent('"live.cancel"').length).toBe(1);
  expect(sent('"ends_gesture"')).toHaveLength(0);
  expect(await rev(page)).toBe(startRev);

  // OK commits the final value in one batch that ends the gesture: one edit.
  dialog = await openProperties(page);
  await dialog.locator('input.color-text').fill('#0000ffff');
  await dialog.locator('input.color-text').press('Tab');
  await expect.poll(() => sent('"live.set"').length).toBe(2);
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  const [commit] = sent('"ends_gesture"');
  expect(commit).toContain('#0000ffff');
  expect(sent('"live.cancel"')).toHaveLength(1);

  // The dialog opens again on the committed value.
  dialog = await openProperties(page);
  await expect(dialog.locator('input.color-text')).toHaveValue('#0000ffff');
});

const TEXT = {
  kind_id: 'text', category: 'source', name: 'Text', version: 1, video: true, audio: false,
  params: [
    { id: 'text', type: 'string', multiline: true, max_len: 4096, default: '', label: 'Text', live: true },
    { id: 'font', type: 'asset', accept: ['ttf', 'otf', 'ttc'], default: null, label: 'Font', live: false },
  ],
};

test('a text source picks an installed font from the list, or another file', async ({ page, daemon }) => {
  await describeKinds(page, [TEXT]);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Text' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  let dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(dialog.locator('.modal-title')).toHaveText('Text properties');

  // The mock's two fonts, between the bundled font and any other file.
  const fonts = dialog.locator('select.font-pick');
  await expect(fonts.locator('option')).toHaveText(['Bundled font', 'Mock Sans Bold', 'Mock Sans Regular', 'Other file…']);
  await expect(fonts).toHaveValue('');
  await expect(dialog.locator('.font-file')).toBeHidden();

  // A picked font is the file's absolute path, saved on OK as one edit.
  const startRev = await rev(page);
  await fonts.selectOption({ label: 'Mock Sans Regular' });
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 1);
  await rows(page).first().click({ button: 'right' });
  await page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem', { name: 'Properties…' }).click();
  dialog = page.getByRole('dialog', { name: 'Properties' });
  await expect(fonts).toHaveValue('font:/mock/fonts/MockSans-Regular.ttf');

  // Other file shows the root and path (absolute, as the stored font's); a path the
  // list doesn't name keeps them shown.
  await fonts.selectOption({ label: 'Other file…' });
  const file = dialog.locator('.font-file');
  await expect(file).toBeVisible();
  await file.locator('input[type="text"]').fill('/home/me/fonts/Mine.otf');
  await file.locator('input[type="text"]').press('Tab');
  await expect(fonts).toHaveValue('other');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => rev(page)).toBe(startRev + 2);
});

/** Within a pixel: the drag follows the pointer, and layout can sit on a half pixel. */
function closeTo(actual, expected) {
  expect(Math.abs(actual - expected)).toBeLessThan(1.5);
}

test('dragging the header moves the dialog, and it reopens there', async ({ page, daemon }) => {
  await connect(page, daemon);
  const dialog = await openProperties(page);
  const before = await dialog.boundingBox();
  const title = dialog.locator('.modal-title');
  const titleBox = await title.boundingBox();
  const from = { x: titleBox.x + 12, y: titleBox.y + titleBox.height / 2 };
  const dx = 80;
  const dy = 30;
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + dx, from.y + dy, { steps: 8 });
  await page.mouse.up();

  const moved = await dialog.boundingBox();
  closeTo(moved.x, before.x + dx);
  closeTo(moved.y, before.y + dy);
  const viewport = page.viewportSize();
  expect(moved.x).toBeGreaterThanOrEqual(-0.5);
  expect(moved.y).toBeGreaterThanOrEqual(-0.5);
  expect(moved.x + moved.width).toBeLessThanOrEqual(viewport.width + 0.5);
  expect(moved.y + moved.height).toBeLessThanOrEqual(viewport.height + 0.5);

  // A drag past the corner stays inside the window, header included.
  const titleNow = await title.boundingBox();
  await page.mouse.move(titleNow.x + 12, titleNow.y + titleNow.height / 2);
  await page.mouse.down();
  await page.mouse.move(-400, -300, { steps: 6 });
  await page.mouse.up();
  const clamped = await dialog.boundingBox();
  expect(clamped.x).toBeGreaterThanOrEqual(-0.5);
  expect(clamped.y).toBeGreaterThanOrEqual(-0.5);
  expect(clamped.x + clamped.width).toBeLessThanOrEqual(viewport.width + 0.5);
  expect(clamped.y + clamped.height).toBeLessThanOrEqual(viewport.height + 0.5);
  expect(clamped.x).toBeLessThan(moved.x);
  expect(clamped.y).toBeLessThan(moved.y);

  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toBeHidden();
  const again = await openProperties(page);
  const reopened = await again.boundingBox();
  closeTo(reopened.x, clamped.x);
  closeTo(reopened.y, clamped.y);

  // A press on × does not drag, and a click there still closes the dialog.
  const at = await again.boundingBox();
  const close = again.locator('.modal-close');
  const closeBox = await close.boundingBox();
  await page.mouse.move(closeBox.x + closeBox.width / 2, closeBox.y + closeBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(closeBox.x + closeBox.width / 2 + 48, closeBox.y + closeBox.height / 2 + 36, { steps: 4 });
  await page.mouse.up();
  await expect(again).toBeVisible();
  const stayed = await again.boundingBox();
  closeTo(stayed.x, at.x);
  closeTo(stayed.y, at.y);
  await close.click();
  await expect(again).toBeHidden();
});

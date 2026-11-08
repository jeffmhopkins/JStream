// The Outputs panel and the header: adding outputs through the settings dialog, the
// write-only stream key, start and stop on each row, Go Live and Record in the header,
// the encoder profile, the audio bus and the video mix applied on OK, editing encoder
// profiles and video mixes in their dialog, and deleting after a confirmation, through a
// real browser and the WebSocket API.
import {
  test, expect, connect, showPanel, outputRow, addOutput, openOutputSettings, openEncoders,
} from '../fixtures.js';

/** Connects and opens the inspector's Outputs page. */
async function openOutputs(page, daemon) {
  await connect(page, daemon);
  await showPanel(page, 'Outputs');
}

/** Deletes output `id` through its ⋯ menu and the confirmation. */
async function deleteOutput(page, id) {
  await outputRow(page, id).getByRole('button', { name: `More for ${id}` }).click();
  await page.getByRole('menuitem', { name: 'Delete…' }).click();
  const confirm = page.getByRole('dialog', { name: 'Delete output' });
  await expect(confirm).toContainText(`Delete ${id}?`);
  await confirm.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(confirm).toHaveCount(0);
}

test('the panel is a compact list: health in the header, then the program and each output on one row', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  const panel = page.locator('section.outputs');
  await expect(panel.locator('.outputs-head .health')).toHaveText('No stats yet');
  const rows = page.getByRole('list', { name: 'Outputs' }).getByRole('listitem');
  await expect(rows).toHaveCount(1);
  // Nothing to fill in until asked: no form, and no library under the rows.
  await expect(panel.getByRole('textbox')).toHaveCount(0);
  await expect(panel.getByRole('region', { name: 'Encoders' })).toHaveCount(0);

  await addOutput(page, 'rtmp', 'rtmp://live.example.com/app');
  await expect(rows).toHaveCount(2);
  const stream = outputRow(page, 'output-1');
  await expect(stream.locator('.output-row-target')).toContainText('live.example.com · Engine default');
  await expect(stream.locator('.output-row-main')).toHaveAttribute('title', 'Stream to rtmp://live.example.com/app');
  // Plain rtmp:// to another machine: a small warning, the full text on hover.
  await expect(stream.getByRole('img', { name: 'Stream key sent unencrypted' })).toHaveAttribute('title', /plain rtmp:\/\//);
  await expect(stream.locator('.output-row-note')).toHaveText('Set a stream key first.');

  // A double-click opens its settings, as Settings… does.
  await stream.locator('.output-row-name').dblclick();
  const dialog = page.getByRole('dialog', { name: 'Output settings' });
  await expect(dialog.locator('.modal-title')).toHaveText('output-1 settings');
  await expect(dialog.getByRole('note')).toContainText('plain rtmp://');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
});

test('a stream goes live once its key is set, and the key is never shown', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'rtmp', 'rtmp://127.0.0.1/live');

  const stream = outputRow(page, 'output-1');
  await expect(stream.locator('.badge')).toHaveText('Stopped');
  await expect(stream.locator('.output-row-main')).toHaveAttribute('title', 'Stream to rtmp://127.0.0.1/live');
  await expect(stream.getByRole('button', { name: 'Start' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Go live' })).toBeDisabled();

  const key = 'live_e2e_9f3a7c21-secret';
  let dialog = await openOutputSettings(page, 'output-1');
  await dialog.getByLabel('Stream key').fill(`${key}\n`);
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(stream.locator('.output-row-note')).toHaveCount(0);
  expect(await page.content()).not.toContain(key);

  // A set key reads as masked, with Change… and Remove.
  dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.locator('.key-mask')).toHaveText('••••••••');
  await expect(dialog.locator('input[type="password"]')).toHaveCount(0);
  await dialog.getByRole('button', { name: 'Cancel' }).click();

  await page.getByRole('button', { name: 'Go live' }).click();
  await expect(stream.locator('.badge')).toHaveText('Live');
  await expect(stream).toHaveClass(/\blive\b/);
  dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.getByLabel('Server URL')).toBeDisabled();
  await expect(dialog.getByRole('button', { name: 'Delete…' })).toBeDisabled();
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await stream.getByRole('button', { name: 'More for output-1' }).click();
  await expect(page.getByRole('menuitem', { name: 'Delete…' })).toBeDisabled();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'End stream' }).click();
  await expect(stream.locator('.badge')).toHaveText('Stopped');

  // Remove applies on OK, like every other field.
  dialog = await openOutputSettings(page, 'output-1');
  await dialog.getByRole('button', { name: 'Remove' }).click();
  await expect(dialog.locator('.key-mask')).toHaveText('Removed on OK');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(stream.locator('.output-row-note')).toHaveText('Set a stream key first.');

  // Delete asks first; Cancel keeps it.
  await stream.getByRole('button', { name: 'More for output-1' }).click();
  await page.getByRole('menuitem', { name: 'Delete…' }).click();
  await page.getByRole('dialog', { name: 'Delete output' }).getByRole('button', { name: 'Cancel' }).click();
  await expect(stream).toHaveCount(1);
  await deleteOutput(page, 'output-1');
  await expect(stream).toHaveCount(0);
});

test('a stream added with its key can start at once', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await page.getByRole('button', { name: '+ Add output' }).click();
  const dialog = page.getByRole('dialog', { name: 'Output settings' });
  await expect(dialog.locator('.modal-sub')).toHaveText('It will be output-1');
  await expect(dialog.getByRole('button', { name: 'Add', exact: true })).toBeDisabled();
  await dialog.getByLabel('Server URL').fill('rtmps://127.0.0.1/app');
  await dialog.getByLabel('Stream key').fill('live_e2e_added-with-key');
  await dialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  const stream = outputRow(page, 'output-1');
  await expect(stream.getByRole('button', { name: 'Start' })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Go live' })).toBeEnabled();
  expect(await page.content()).not.toContain('live_e2e_added-with-key');
});

test('a recording starts and stops on its own row', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'record', '/tmp/show.mp4');

  const recording = outputRow(page, 'output-1');
  await expect(recording.locator('.output-row-target')).toContainText('show.mp4');
  await expect(recording.locator('.output-row-main')).toHaveAttribute('title', 'Record to /tmp/show.mp4');
  const dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.getByLabel('Stream key')).toHaveCount(0);
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  // "Go live" is for streams only.
  await expect(page.getByRole('button', { name: 'Go live' })).toBeDisabled();
  await recording.getByRole('button', { name: 'Start' }).click();
  await expect(recording.locator('.badge')).toHaveText('Live');
  await recording.getByRole('button', { name: 'Stop' }).click();
  await expect(recording.locator('.badge')).toHaveText('Stopped');

  // The header's Record starts every recording, and stops them while they run.
  const header = page.locator('header.app-header');
  await header.getByRole('button', { name: 'Record', exact: true }).click();
  await expect(recording.locator('.badge')).toHaveText('Live');
  await header.getByRole('button', { name: 'Stop recording' }).click();
  await expect(recording.locator('.badge')).toHaveText('Stopped');
});

test('a server URL edit and an encoder choice reach the engine on OK, and Cancel sends nothing', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'rtmp', 'rtmp://127.0.0.1/live');
  const stream = outputRow(page, 'output-1');

  let dialog = await openOutputSettings(page, 'output-1');
  await dialog.getByLabel('Server URL').fill('rtmps://127.0.0.1/discarded');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(stream.locator('.output-row-main')).toHaveAttribute('title', 'Stream to rtmp://127.0.0.1/live');

  dialog = await openOutputSettings(page, 'output-1');
  await dialog.getByLabel('Server URL').fill('rtmps://127.0.0.1/app');
  await dialog.getByLabel('Encoder').selectOption({ label: 'High quality' });
  // Apply keeps the dialog open.
  await dialog.getByRole('button', { name: 'Apply' }).click();
  await expect(stream.locator('.output-row-main')).toHaveAttribute('title', 'Stream to rtmps://127.0.0.1/app');
  await expect(stream.locator('.output-row-target')).toContainText('127.0.0.1 · High quality');
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);

  // A fresh page reads both back from the engine.
  await openOutputs(page, daemon);
  await expect(stream.locator('.output-row-main')).toHaveAttribute('title', 'Stream to rtmps://127.0.0.1/app');
  dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.getByLabel('Encoder')).toHaveValue('hq');
});

test('an output carries the bus its Audio select picks', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  const buses = page.locator('.mixer-panel .fold');
  await buses.locator('.fold-toggle').click();
  await buses.getByRole('textbox', { name: 'New bus name' }).fill('Stream');
  await buses.getByRole('button', { name: 'Add bus' }).click();
  await expect(buses.locator('.strip .strip-name')).toHaveText(['Master', 'Stream']);

  await showPanel(page, 'Outputs');
  await addOutput(page, 'record', '/tmp/a.mp4');
  let dialog = await openOutputSettings(page, 'output-1');
  const audio = dialog.getByLabel('Audio');
  await expect(audio.locator('option:checked')).toHaveText('Master');
  await audio.selectOption({ label: 'Stream' });
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);
  // A fresh page reads it back from the engine.
  await openOutputs(page, daemon);
  dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.getByLabel('Audio').locator('option:checked')).toHaveText('Stream');
});

test('an encoder profile and a vertical video mix are made, used and kept', async ({ page, daemon }) => {
  await openOutputs(page, daemon);
  await addOutput(page, 'record', '/tmp/a.mkv');

  let library = await openEncoders(page);
  const encoders = library.getByRole('region', { name: 'Encoders' });
  await encoders.getByRole('button', { name: 'New encoder…' }).click();
  const form = encoders.getByRole('form', { name: 'New encoder' });
  // A new profile is named for what it encodes, so a select reads it as a choice.
  await expect(form.getByLabel('Encoder name')).toHaveValue('Default H.264');
  await form.getByLabel('Encoder name').fill('AV1 archive');
  await form.getByLabel('Video codec').selectOption({ label: 'AV1' });
  await form.getByLabel('Rate control').selectOption('vbr');
  await form.getByLabel('Bit rate', { exact: true }).fill('6000');
  await form.getByLabel('Peak bit rate').fill('5000');
  await form.getByRole('button', { name: 'Save' }).click();
  // The form names the field the engine would refuse, and stays open.
  await expect(form.getByRole('alert')).toContainText('Peak bit rate');
  await form.getByLabel('Peak bit rate').fill('9000');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(form).toHaveCount(0);
  const row = encoders.locator('li.profile').filter({ hasText: 'AV1 archive' });
  await expect(row).toContainText('AV1 · VBR 6000 kbps, peak 9000');

  const mixes = library.getByRole('region', { name: 'Video mixes' });
  await mixes.getByRole('button', { name: 'New video mix…' }).click();
  const mixForm = mixes.getByRole('form', { name: 'New video mix' });
  await expect(mixForm.getByLabel('Size')).toHaveValue('3');
  await mixForm.getByRole('button', { name: 'Save' }).click();
  const vertical = mixes.locator('li.profile').filter({ hasText: 'Vertical' });
  await expect(vertical).toContainText('Program · 1080 × 1920 · every frame');
  await library.getByRole('button', { name: 'Close', exact: true }).last().click();
  await expect(library).toHaveCount(0);

  let dialog = await openOutputSettings(page, 'output-1');
  await dialog.getByLabel('Encoder').selectOption({ label: 'AV1 archive' });
  await dialog.getByLabel('Video').selectOption({ label: 'Vertical' });
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(outputRow(page, 'output-1').locator('.output-row-target')).toContainText('a.mkv · AV1 archive');
  // In use, neither can be deleted.
  library = await openEncoders(page);
  await expect(row.getByRole('button', { name: 'Delete AV1 archive' })).toBeDisabled();
  await expect(vertical.getByRole('button', { name: 'Delete Vertical' })).toBeDisabled();

  // A fresh page reads it all back from the engine, and an edit keeps the mix's ID.
  await openOutputs(page, daemon);
  dialog = await openOutputSettings(page, 'output-1');
  await expect(dialog.getByLabel('Encoder').locator('option:checked')).toHaveText('AV1 archive');
  await expect(dialog.getByLabel('Video').locator('option:checked')).toHaveText('Vertical');
  // The encoder's "Encoders & mixes…" opens the library over the settings.
  await dialog.getByRole('button', { name: 'Encoders & mixes…' }).click();
  library = page.getByRole('dialog', { name: 'Encoders & mixes' });
  await vertical.getByRole('button', { name: 'Edit Vertical' }).click();
  const edit = mixes.getByRole('form', { name: 'Edit video mix' });
  await edit.getByLabel('Size').selectOption({ label: '720 × 1280 (vertical)' });
  await edit.getByRole('button', { name: 'Save' }).click();
  await expect(vertical).toContainText('Program · 720 × 1280');
  await library.getByRole('button', { name: 'Close', exact: true }).last().click();
  await expect(dialog.getByLabel('Video').locator('option:checked')).toHaveText('Vertical');

  // Back on the program, the mix can go.
  await dialog.getByLabel('Video').selectOption({ label: 'Program (canvas size)' });
  await dialog.getByRole('button', { name: 'OK' }).click();
  await expect(dialog).toHaveCount(0);
  library = await openEncoders(page);
  await vertical.getByRole('button', { name: 'Delete Vertical' }).click();
  await expect(vertical).toHaveCount(0);
});

test('another client sees outputs come and go', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await openOutputs(page, daemon);
  await openOutputs(other, daemon);

  await addOutput(page, 'record', '/tmp/a.mp4');
  await expect(outputRow(other, 'output-1').locator('.output-row-main')).toHaveAttribute('title', 'Record to /tmp/a.mp4');
  await outputRow(other, 'output-1').getByRole('button', { name: 'Start' }).click();
  await expect(outputRow(page, 'output-1').locator('.badge')).toHaveText('Live');
  await outputRow(page, 'output-1').getByRole('button', { name: 'Stop' }).click();
  await expect(outputRow(page, 'output-1').locator('.badge')).toHaveText('Stopped');
  await deleteOutput(page, 'output-1');
  await expect(outputRow(other, 'output-1')).toHaveCount(0);
});

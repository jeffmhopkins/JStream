// The Mixer, after the prototype's: the master card with its loudness and true peak, the
// channels folded under it, each with its dB field, and the buses folded below. The seed
// has two audio inputs, Mic · ES-9 Line at −3 dB and Desktop audio at −8.5 dB. The mock
// engine sends no levels, so the levels test sends `meters.tick` itself (`meterTicks`).
import { test, expect, connect, meterTicks, rev, showPanel } from '../fixtures.js';

function mixer(page) {
  return page.locator('.mixer-panel');
}

/** The master card's fold button. */
function fold(page) {
  return mixer(page).getByRole('button', { name: /Master · program output/ });
}

test('the master card folds its channels, whose dB fields set the gain', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  const card = mixer(page).getByRole('group', { name: 'Master' });
  await expect(card.locator('.master-count')).toHaveText('2 channels');
  await expect(card.getByRole('button', { name: 'Hear: after FX' })).toBeDisabled();
  await expect(card.getByRole('button', { name: 'FX 0' })).toBeDisabled();

  // Folded at first.
  const channels = mixer(page).getByRole('list', { name: 'Channels' });
  await expect(fold(page)).toHaveAttribute('aria-expanded', 'false');
  await expect(channels).toBeHidden();
  await fold(page).click();
  await expect(fold(page)).toHaveAttribute('aria-expanded', 'true');
  await expect(channels.locator('.strip-name')).toHaveText(['Desktop audio', 'Mic · ES-9 Line']);
  const mic = channels.getByRole('textbox', { name: 'Mic · ES-9 Line gain' });
  await expect(mic).toHaveValue('−3.0');
  await expect(channels.getByRole('textbox', { name: 'Desktop audio gain' })).toHaveValue('−8.5');

  // A typed value is one edit, and so is a stepper.
  const start = await rev(page);
  await mic.fill('-6');
  await mic.press('Enter');
  await expect(mic).toHaveValue('−6.0');
  await expect.poll(() => rev(page)).toBe(start + 1);
  await channels.getByRole('button', { name: 'Increase Mic · ES-9 Line gain' }).click();
  await expect(mic).toHaveValue('−5.5');
  await expect.poll(() => rev(page)).toBe(start + 2);
  await expect(channels.getByRole('slider', { name: 'Mic · ES-9 Line fader' })).toHaveAttribute('aria-valuetext', '-5.5 dB');

  // Folding hides them again.
  await fold(page).click();
  await expect(channels).toBeHidden();
});

test('the master card shows the loudness against the target and warns on true peak', async ({ page, daemon }) => {
  const tick = await meterTicks(page);
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  const card = mixer(page).getByRole('group', { name: 'Master' });
  const rows = card.locator('.master-row');
  await expect(rows.nth(0).locator('.master-value')).toHaveText('– LUFS');
  await expect(rows.nth(1).locator('.master-value')).toHaveText('– dBTP');

  const master = (truePeak) => ({
    bus_id: 'master',
    peak: [0.5, 0.5],
    rms: [0.2, 0.2],
    true_peak: truePeak,
    loudness: { momentary: -12.5, short_term: -14.2, integrated: -16.0 },
  });
  tick({ sources: [], buses: [master([0.5, 0.25])] });
  await expect(rows.nth(0).locator('.master-value')).toHaveText('−14.2 LUFS');
  await expect(rows.nth(0).locator('.master-value')).toHaveAttribute(
    'title',
    'Momentary −12.5 · short-term −14.2 · integrated −16.0 LUFS',
  );
  // The bar runs from −60 to +6 LUFS, so the target, −14 LUFS, is at 70%.
  const fill = await rows.nth(0).locator('.level-fill').evaluate((el) => el.style.width);
  expect(parseFloat(fill)).toBeCloseTo(69.4, 1);
  await expect(rows.nth(1).locator('.master-note')).toHaveText('safe');
  await expect(rows.nth(1).locator('.master-value')).toHaveText('−6.0 dBTP');
  await expect(rows.nth(1)).not.toHaveClass(/\bover\b/);

  tick({ sources: [], buses: [master([1.2, 1.0])] });
  await expect(rows.nth(1).locator('.master-note')).toHaveText('over 0 dBTP: add a true-peak limiter');
  await expect(rows.nth(1)).toHaveClass(/\bover\b/);
});

test('the buses fold below the channels, the master first', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  const buses = mixer(page).locator('.fold');
  await expect(buses.locator('.fold-toggle')).toHaveText(/Buses\s*1 bus/);
  await expect(buses.locator('.strip')).toBeHidden();
  await buses.locator('.fold-toggle').click();
  await expect(buses.locator('.strip .strip-name')).toHaveText(['Master']);
  await buses.getByRole('textbox', { name: 'New bus name' }).fill('Stream');
  await buses.getByRole('button', { name: 'Add bus' }).click();
  await expect(buses.locator('.strip .strip-name')).toHaveText(['Master', 'Stream']);
});

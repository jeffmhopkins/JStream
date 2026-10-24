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

/** The mixer's title-bar button that flips its meters. */
function flip(page) {
  return page.getByRole('button', { name: /^Meters: / });
}

test('the title bar flips the meters to vertical strips, whose gain scrubs up and down', async ({ page, daemon }) => {
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  // Docked in the bottom dock, the meters start horizontal.
  await expect(flip(page)).toHaveAttribute('title', 'Meters: horizontal · click for vertical');
  await expect(mixer(page).locator('.master-card')).toBeVisible();
  await flip(page).click();
  await expect(flip(page)).toHaveAttribute('title', 'Meters: vertical · click for horizontal');
  await expect(mixer(page).locator('.master-card')).toHaveCount(0);

  const strip = mixer(page).locator('.master-strip');
  await expect(strip.locator('.master-count')).toHaveText('2 channels');
  await expect(strip.locator('.level-column')).toHaveAttribute('title', 'Loudness · white line = target');
  await expect(strip.getByRole('button', { name: 'FX 0' })).toBeDisabled();
  await expect(strip.getByRole('button', { name: 'Hear post' })).toBeDisabled();

  // The channels fold beside the master; each shows its short name, the full one as a tooltip.
  const masterFold = strip.getByRole('button', { name: 'Master' });
  const channels = mixer(page).getByRole('list', { name: 'Channels' });
  await expect(channels).toBeHidden();
  await masterFold.click();
  await expect(masterFold).toHaveAttribute('aria-expanded', 'true');
  const names = channels.locator('.column-name');
  await expect(names).toHaveText(['Desktop audio', 'Mic']);
  await expect(names.nth(1)).toHaveAttribute('title', 'Mic · ES-9 Line');
  await expect(channels.locator('.meter.vertical')).toHaveCount(2);

  // + sits over the field and − under it; dragging up is louder, one edit on release.
  const mic = channels.getByRole('textbox', { name: 'Mic · ES-9 Line gain' });
  const plus = channels.getByRole('button', { name: 'Increase Mic · ES-9 Line gain' });
  const minus = channels.getByRole('button', { name: 'Decrease Mic · ES-9 Line gain' });
  // The strips are taller than the bottom dock, which scrolls.
  await minus.scrollIntoViewIfNeeded();
  const [above, field, below] = await Promise.all([plus, mic, minus].map((l) => l.boundingBox()));
  expect(above.y).toBeLessThan(field.y);
  expect(below.y).toBeGreaterThan(field.y);
  await expect(mic).toHaveValue('−3.0');
  const start = await rev(page);
  const x = field.x + field.width / 2;
  const y = field.y + field.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y - 6, { steps: 2 });
  await page.mouse.move(x, y - 9, { steps: 1 });
  await page.mouse.up();
  await expect(mic).toHaveValue('−1.5');
  await expect.poll(() => rev(page)).toBe(start + 1);
  await minus.click();
  await expect(mic).toHaveValue('−2.0');
  await expect.poll(() => rev(page)).toBe(start + 2);

  // A mute here is the same mute as in the rows.
  await channels.locator('.mix-column').nth(1).getByRole('button', { name: 'Mute' }).click();
  await expect(channels.locator('.mix-column').nth(1).getByRole('button', { name: 'Mute' })).toHaveAttribute('aria-pressed', 'true');

  // Flipping back shows the rows, with the gain the strip set.
  await flip(page).click();
  await expect(mixer(page).locator('.master-card')).toBeVisible();
  await expect(mixer(page).getByRole('textbox', { name: 'Mic · ES-9 Line gain' })).toHaveValue('−2.0');
});

test('docked in a side column the meters turn vertical until the title bar flips them', async ({ page, daemon }) => {
  await connect(page, daemon);
  const tab = (name) => page.getByRole('tab', { name, exact: true });
  const from = await tab('Audio mixer').boundingBox();
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(from.x + from.width / 2 + 10, from.y + from.height / 2 + 10, { steps: 3 });
  // A drag shows the empty zones, which moves the strips: aim once they are shown.
  await expect(page.getByRole('region', { name: 'Top dock' })).toBeVisible();
  const to = await tab('Inspector').boundingBox();
  await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 8 });
  await page.mouse.up();
  await expect(page.getByRole('region', { name: 'Right sidebar' }).getByRole('tab', { name: 'Audio mixer' })).toBeVisible();
  await showPanel(page, 'Audio mixer');

  await expect(flip(page)).toHaveAttribute('title', 'Meters: vertical · click for horizontal');
  await expect(mixer(page).locator('.master-strip')).toBeVisible();
  // The strips scroll sideways inside the column rather than squeezing.
  await mixer(page).locator('.master-strip').getByRole('button', { name: 'Master' }).click();
  const widths = await mixer(page).locator('.mix-column').evaluateAll((els) => els.map((el) => el.getBoundingClientRect().width));
  expect(widths).toEqual([64, 64]);

  await flip(page).click();
  await expect(mixer(page).locator('.master-card')).toBeVisible();
  await expect(flip(page)).toHaveAttribute('title', 'Meters: horizontal · click for vertical');
});

test('vertical meters fill upwards from the engine levels', async ({ page, daemon }) => {
  const tick = await meterTicks(page);
  await connect(page, daemon);
  await showPanel(page, 'Audio mixer');
  await flip(page).click();
  const strip = mixer(page).locator('.master-strip');
  await strip.getByRole('button', { name: 'Master' }).click();
  tick({
    sources: [{ source_id: 'mic', peak: [1.0, 0.5], rms: [0.5, 0.25] }],
    buses: [{
      bus_id: 'master',
      peak: [0.5, 0.5],
      rms: [0.2, 0.2],
      true_peak: [1.2, 1.0],
      loudness: { momentary: -12.5, short_term: -14.2, integrated: -16.0 },
    }],
  });
  await expect(strip.locator('.master-value').first()).toHaveText('−14.2');
  await expect(strip.locator('.master-value').nth(1)).toHaveText('1.6');
  await expect(strip.locator('.master-value').nth(1)).toHaveClass(/\bover\b/);
  const fill = await strip.locator('.level-fill').evaluate((el) => el.style.height);
  expect(parseFloat(fill)).toBeCloseTo(69.4, 1);

  const mic = mixer(page).locator('.mix-column').nth(1).locator('.meter-peak');
  await expect.poll(() => mic.first().evaluate((el) => parseFloat(el.style.height))).toBe(100);
  expect(await mic.first().evaluate((el) => el.style.width)).toBe('');
});

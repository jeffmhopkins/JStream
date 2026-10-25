// The privacy screen (ADR-0027 §4): the header's Privacy button and the Pause key turn
// it on and off through `privacy.set`; while it is on, the canvas frame is blurred under
// a notice and the mixer says the microphone is muted. The UI shows what the engine
// reports, so another client's change shows too, and a client that connects while it is
// on shows it from the start.
import { test, expect, connect, showPanel } from '../fixtures.js';

function privacyButton(page) {
  return page.locator('header.app-header').getByRole('button', { name: /^Privacy/ });
}

function notice(page) {
  return page.locator('.canvas-view .privacy-notice');
}

/** Unfolds the mixer's channels and returns their names. */
async function channelNames(page) {
  await showPanel(page, 'Audio mixer');
  const fold = page.locator('.mixer-panel').getByRole('button', { name: /Master · program output/ });
  if ((await fold.getAttribute('aria-expanded')) !== 'true') await fold.click();
  return page.locator('.mixer-panel').getByRole('list', { name: 'Channels' }).locator('.strip-name');
}

test('the Privacy button blurs the canvas and mutes the mic until pressed again', async ({ page, daemon }) => {
  await connect(page, daemon);
  const button = privacyButton(page);
  const names = await channelNames(page);
  await expect(button).toHaveText('Privacy');
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await expect(button).toHaveAttribute('title', 'Privacy screen: blur the whole output and mute the mic (Pause)');
  await expect(notice(page)).toHaveCount(0);
  await expect(names).toHaveText(['Desktop audio', 'Mic · ES-9 Line']);

  await button.click();
  await expect(button).toHaveText('Privacy on');
  await expect(button).toHaveAttribute('aria-pressed', 'true');
  await expect(button).toHaveClass(/\bon\b/);
  await expect(notice(page)).toHaveText('Privacy screen on · output blurred · mic muted');
  await expect(notice(page)).toHaveAttribute('role', 'status');
  await expect(page.locator('.canvas-view .stage')).toHaveClass(/\bprivate\b/);
  await expect(page.locator('.canvas-view .stage')).toHaveCSS('filter', 'blur(14px) brightness(0.6)');
  // Only the microphone: desktop audio isn't an input device.
  await expect(names).toHaveText(['Desktop audio', 'Mic · ES-9 Line · muted (privacy)']);

  await button.click();
  await expect(button).toHaveText('Privacy');
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await expect(notice(page)).toHaveCount(0);
  await expect(page.locator('.canvas-view .stage')).not.toHaveClass(/\bprivate\b/);
  await expect(names).toHaveText(['Desktop audio', 'Mic · ES-9 Line']);
});

test('Pause toggles privacy from anywhere, and every client shows it', async ({ page, context, daemon }) => {
  const other = await context.newPage();
  await connect(page, daemon);
  await connect(other, daemon);

  // Pause works wherever focus is, here in a mixer gain field.
  const names = await channelNames(page);
  await page.locator('.mixer-panel').getByRole('textbox', { name: 'Desktop audio gain' }).focus();
  await page.keyboard.press('Pause');
  await expect(privacyButton(page)).toHaveText('Privacy on');
  await expect(names).toHaveText(['Desktop audio', 'Mic · ES-9 Line · muted (privacy)']);
  await expect(privacyButton(other)).toHaveText('Privacy on');
  await expect(notice(other)).toBeVisible();

  // The other client turns it off.
  await other.keyboard.press('Pause');
  await expect(privacyButton(page)).toHaveText('Privacy');
  await expect(notice(page)).toHaveCount(0);
  await expect(privacyButton(other)).toHaveText('Privacy');
});

test('a client that connects while privacy is on shows it at once', async ({ page, context, daemon }) => {
  await connect(page, daemon);
  await privacyButton(page).click();
  await expect(privacyButton(page)).toHaveText('Privacy on');

  const late = await context.newPage();
  await connect(late, daemon);
  await expect(privacyButton(late)).toHaveText('Privacy on');
  await expect(notice(late)).toHaveText('Privacy screen on · output blurred · mic muted');
});

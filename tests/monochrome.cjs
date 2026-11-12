const assert = require('node:assert/strict');
const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch({ executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
  try {
    const page = await browser.newPage();
    await page.goto('http://127.0.0.1:4317');
    await page.evaluate(() => document.fonts.ready);
    const result = await page.evaluate(() => {
      const invalid = [];
      const exceptions = [];
      function inspect(rules) {
        for (const rule of rules) {
          if (rule.cssRules) inspect(rule.cssRules);
          if (!rule.style) continue;
          if (/^\.(?:goal-top \.)?badge\.(complete|active|blocked|addition|unknown)/.test(rule.selectorText)) continue;
          if (rule.selectorText === '.flow-scope-region.additional') { exceptions.push(rule.style.cssText); continue; }
          for (const match of rule.style.cssText.matchAll(/rgba?\((\d+),\s*(\d+),\s*(\d+)/g)) {
            if (match[1] !== match[2] || match[2] !== match[3]) invalid.push(rule.selectorText + ': ' + match[0]);
          }
        }
      }
      for (const sheet of document.styleSheets) inspect(sheet.cssRules);
      return { invalid, exceptions };
    });
    assert.deepEqual(result.invalid, []);
    const colors = await page.evaluate(() => ['complete','in_progress','blocked','paused','unknown'].map(status => {
      const badge = document.createElement('span'); badge.className = 'badge ' + status; badge.textContent = status;
      document.body.append(badge); const color = getComputedStyle(badge).color; badge.remove(); return color;
    }));
    assert.deepEqual(colors, ['rgb(36, 136, 98)','rgb(91, 105, 191)','rgb(199, 95, 106)','rgb(176, 135, 62)','rgb(135, 141, 152)']);
    assert(result.exceptions.some(s => s.includes('rgb(242, 238, 245)')));
    assert(result.exceptions.some(s => s.includes('92, 108, 132')));
    for (const id of ['hide-dialog', 'hidden-dialog']) {
      await page.evaluate(id => document.getElementById(id).showModal(), id);
      assert(await page.locator('#' + id).isVisible());
      await page.keyboard.press('Escape');
      assert(!(await page.locator('#' + id).isVisible()));
    }
    await page.locator('#search').count();
    await page.keyboard.press('Tab');
    const focus = await page.evaluate(() => getComputedStyle(document.activeElement).outlineColor);
    assert.match(focus, /^rgb\((\d+), \1, \1\)$/);
    console.log('PASS neutral stylesheet, preserved Added background and dots, popup Escape paths, neutral keyboard focus.');
  } finally { await browser.close(); }
})().catch(e => { console.error(e); process.exitCode = 1; });

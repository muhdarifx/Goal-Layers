const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawn, spawnSync} = require('node:child_process');
const {chromium} = require('playwright');

// Run with port 4317 free; exercise the actual standalone executable from another directory.
(async () => {
  const root = path.resolve(__dirname, '..');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'goal-layers-launch-'));
  const exe = path.join(temp, 'goal-layers.exe');
  fs.copyFileSync(path.join(root, 'target/release/goal-layers.exe'), exe);
  const env = {...process.env};
  delete env.GOAL_ATLAS_DATA;
  const app = spawn(exe, ['--no-open'], {cwd: os.tmpdir(), env, windowsHide: true});
  let browser;
  try {
    let ready = false;
    for (let i = 0; i < 150; i++) {
      if (app.exitCode !== null) throw Error('Standalone app exited before serving');
      try { ready = (await fetch('http://127.0.0.1:4317/')).ok; } catch {}
      if (ready) break;
      await new Promise(resolve => setTimeout(resolve, 200));
    }
    assert(ready, 'Standalone app serves without a launcher or adjacent web files');
    assert(fs.existsSync(path.join(temp, 'data')), 'Data belongs beside the executable');
    for (const [url, type] of [['/logo.svg', 'image/svg+xml'], ['/favicon.ico', 'image/x-icon'], ['/apple-touch-icon.png', 'image/png'], ['/icon-192.png', 'image/png']]) {
      const response = await fetch('http://127.0.0.1:4317' + url);
      assert(response.ok, url);
      assert(response.headers.get('content-type').startsWith(type), url);
      assert((await response.arrayBuffer()).byteLength > 100, url);
    }
    const repeat = spawnSync(exe, ['--no-open'], {cwd: os.tmpdir(), env, timeout: 6000, windowsHide: true});
    assert.equal(repeat.status, 0, 'Second launch reuses the existing app');
    browser = await chromium.launch({executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true});
    const page = await browser.newPage();
    await page.goto('http://127.0.0.1:4317/');
    assert.equal(await page.title(), 'Goal Layers');
    for (const [width, height] of [[1440, 800], [360, 800], [320, 480]]) {
      await page.setViewportSize({width, height});
      if (width < 760) await page.locator('#open-navigation').click();
      await page.evaluate(() => document.fonts.ready);
      assert.equal(await page.locator('.brand strong').textContent(), 'GOAL LAYERS');
      assert.equal(await page.locator('.brand strong').evaluate(e => getComputedStyle(e).color), 'rgb(0, 0, 0)');
      assert((await (await page.request.get('http://127.0.0.1:4317/logo.svg')).text()).includes('fill="#000"'));
      assert(await page.locator('.brand-logo').evaluate(img => img.complete && img.naturalWidth > 0));
      assert.equal(await page.locator('.brand-logo').evaluate(img => img.getBoundingClientRect().width), 32);
      assert(await page.locator('.brand').evaluate(header => {
        const box = header.getBoundingClientRect(), logo = header.querySelector('img').getBoundingClientRect(), text = header.querySelector('strong');
        return logo.height === logo.width && logo.top >= box.top && logo.bottom <= box.bottom && text.getBoundingClientRect().right <= box.right && getComputedStyle(text).fontFamily.includes('SpaceGrotesk');
      }), 'Full logo and wordmark fit inside the header');
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      if (width < 760) await page.locator('#close-navigation').click();
    }
    await page.screenshot({path: path.join(os.tmpdir(), 'goal-layers-brand-preview.png')});
    console.log('PASS standalone launch, portable data, duplicate launch, embedded icons, desktop/mobile branding.');
  } finally {
    if (browser) await browser.close();
    if (app.exitCode === null) spawnSync('taskkill', ['/PID', String(app.pid), '/T', '/F'], {windowsHide: true});
    assert.equal(path.dirname(fs.realpathSync(temp)), fs.realpathSync(os.tmpdir()));
    assert(path.basename(temp).startsWith('goal-layers-launch-'));
    fs.rmSync(temp, {recursive: true, force: true});
  }
})().catch(error => {console.error(error); process.exitCode = 1;});

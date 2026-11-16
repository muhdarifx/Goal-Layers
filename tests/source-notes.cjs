const {chromium}=require('playwright'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
(async()=>{const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});try{
 const page=await browser.newPage({viewport:{width:1448,height:1086}}),errors=[];page.on('pageerror',error=>errors.push(error.message));
 await page.route('**/api/**',route=>route.request().method()==='GET'?route.continue():route.abort());
 if(!process.env.ATLAS_LIVE)for(const file of ['app.js','style.css'])await page.route('http://127.0.0.1:4317/'+file,route=>route.fulfill({body:fs.readFileSync(path.join(__dirname,'../web',file)),contentType:file.endsWith('.css')?'text/css':'text/javascript'}));
 await page.goto('http://127.0.0.1:4317');await page.locator('.flow-node').first().waitFor();const notes=await page.evaluate(()=>GoalFlow.sourceNotes());assert(notes.length>0);
 const open=async()=>{await page.locator('#run-activity summary').click();await page.locator('#source-notes').click();await page.locator('#inspector.source-notes-view').waitFor()};
 for(const width of [1448,780,390,320]){await page.setViewportSize({width,height:844});await open();
  assert.equal(await page.locator('#inspector .detail-title-row h2').innerText(),'Sources and limitations');assert.equal(await page.locator('#inspector .detail-tabs').count(),0);assert.equal(await page.locator('#inspector .chat-action-bar').count(),0);
  for(const note of notes.split(/\n\s*\n/).filter(Boolean))assert((await page.locator('#inspector').innerText()).includes(note),'Every source paragraph survives');
  const box=await page.locator('#inspector').boundingBox();assert(box.x>=0&&box.x+box.width<=width+1&&box.y>=0&&box.y+box.height<=845);assert(await page.locator('#inspector .detail-body').evaluate(e=>e.scrollWidth<=e.clientWidth+1));
  assert.equal(await page.locator('#close-inspector svg').evaluate(e=>getComputedStyle(e).color),'rgb(23, 23, 23)');
  await page.locator('#close-inspector').focus();await page.keyboard.press('Tab');assert(await page.evaluate(()=>document.activeElement===document.body||document.activeElement.closest('#inspector')));
  await page.locator('#inspector').screenshot({path:path.join(process.env.TEMP,'goal-layers-source-notes-'+width+'.png')});await page.keyboard.press('Escape');assert(!(await page.locator('#inspector').evaluate(e=>e.open)));
 }
 await page.evaluate(()=>{window.savedSourceNotes=GoalFlow.sourceNotes;GoalFlow.sourceNotes=()=>''});await open();assert.match(await page.locator('#inspector').innerText(),/No source notes recorded/);await page.locator('#close-inspector').click();
 await page.evaluate(()=>GoalFlow.sourceNotes=()=>'<img src=x onerror=alert(1)>\n\nUnrecognized limitation remains visible.');await open();assert.match(await page.locator('#inspector').innerText(),/Unrecognized limitation remains visible/);assert.equal(await page.locator('#inspector img').count(),0);await page.keyboard.press('Escape');
 await page.evaluate(()=>GoalFlow.sourceNotes=window.savedSourceNotes);await page.locator('.flow-node').first().click();assert.equal(await page.locator('#inspector').evaluate(e=>e.classList.contains('source-notes-view')),false);assert(await page.locator('#inspector .detail-tabs').isVisible());assert.deepEqual(errors,[]);
 console.log('PASS: Source notes preserves all real paragraphs; desktop/mobile bounds; keyboard and close; empty/unknown/escaped text; step-popup isolation; no live mutations.');
}finally{await browser.close()}})().catch(error=>{console.error(error);process.exitCode=1});

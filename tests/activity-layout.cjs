const {chromium}=require('playwright'),assert=require('node:assert/strict');
(async()=>{const b=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});try{
const p=await b.newPage();let status='ready';const started=Date.now()-8000,finished=started+8000;
await p.route('**/api/flow/*',async r=>{const a=await r.fetch(),j=await a.json();j.job={status,mode:'local',started_at:started,finished_at:status==='ready'?finished:null,message:'Local outline ready · 0 new or changed records · 6528 reused'};await r.fulfill({response:a,json:j})});
await p.goto('http://127.0.0.1:4317');await p.locator('[data-elapsed]').waitFor({state:'attached'});
for(const width of [1280,360]){await p.setViewportSize({width,height:700});await p.locator('#run-activity').evaluate(e=>e.open=true);
assert.equal(await p.locator('[data-elapsed]').innerText(),'00:08');assert.deepEqual(await p.locator('.activity-details dt').allTextContents(),['Started','Finished','Limit']);assert.equal(await p.locator('.activity-details dd').last().innerText(),'30 min');
const boxes=await p.locator('.activity-details>div').evaluateAll(es=>es.map(e=>{const b=e.getBoundingClientRect();return {y:b.y,right:b.right,x:b.x}}));assert(boxes.every(e=>Math.abs(e.y-boxes[0].y)<1));
const panel=await p.locator('.run-strip').boundingBox();assert(panel.x>=0&&panel.x+panel.width<=width+1);assert(panel.y+panel.height<=700);
await p.locator('.run-strip').screenshot({path:process.env.TEMP+'/goal-layers-activity-'+width+'.png'});
await p.locator('#close-activity').click();assert(!(await p.locator('#run-activity').evaluate(e=>e.open)));}
status='reconstructing';await p.evaluate(()=>GoalFlow.refresh());await p.waitForFunction(()=>document.querySelector('#run-activity').dataset.state==='reconstructing');assert.deepEqual(await p.locator('.activity-details dt').allTextContents(),['Started','Remaining','Limit']);
console.log('PASS: elapsed and timing values preserved; aligned desktop/mobile columns; panel in viewport; close action; running Remaining field.');
}finally{await b.close()}})().catch(e=>{console.error(e);process.exitCode=1});

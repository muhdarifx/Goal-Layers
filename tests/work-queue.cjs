// Synthetic queue controls: no real Codex chat is opened, cancelled, or sent a message.
const {chromium}=require('playwright'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),assert=require('node:assert/strict');
(async()=>{
 const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 try{
  const page=await browser.newPage(),original='11111111-2222-4333-8444-555555555555',actions=[],errors=[];
  let release,fail=false,detailFail=false,detailReads=0,holdDetail=false,releaseDetail;
  const base={requested_at:1,finished:false,node_ids:['root'],destination_thread_id:original};
  let runs=[{...base,request_id:'queued-request-1234',status:'queued',topic:'Implement permissions',can_cancel:true,message:'Confirmed in the original chat queue. The goal is paused.'},{...base,requested_at:2,request_id:'unknown-request-1234',status:'unconfirmed',topic:'Review deployment evidence',can_dismiss:true,message:'Delivery is unconfirmed. Open the original chat before retrying.'},{...base,requested_at:3,request_id:'running-request-1234',status:'running',topic:'Check accessibility',message:'Working in the original chat.'}];
  page.on('pageerror',e=>errors.push(e.message));
  const graph={summary:'Goal',limitations:[],inspected_files:[],nodes:[{id:'root',title:'Original goal',kind:'goal',lane:'aligned',status:'unknown',description:'',next:'',evidence:[]}],edges:[]};
  await page.route('http://queue.test/**',async route=>{
   const name=new URL(route.request().url()).pathname;
   if(name==='/events')return route.abort();
   if(route.request().method()==='POST'){
    assert.equal(name,`/api/thread/${original}/queue`);assert.equal(route.request().headers()['x-goal-atlas'],'1');
    const body=route.request().postDataJSON();actions.push(body);await new Promise(resolve=>release=resolve);
    if(fail)return route.fulfill({status:409,body:'The request already started. Refresh its progress.'});
    runs=runs.map(r=>r.request_id===body.request_id?{...r,status:body.action==='cancel'?'cancelled':'dismissed',finished:true,can_cancel:false,can_dismiss:false}:r);
    return route.fulfill({json:{status:body.action==='cancel'?'cancelled':'dismissed'}});
   }
   if(name==='/api/catalog')return route.fulfill({json:{projects:[{id:'p',name:'Example'}],threads:[{id:original,projectId:'p',name:'Goal'}],goals:{[original]:{status:'paused',objective:'Build an accessible app'}},nodes:[]}});
   if(name===`/api/flow/${original}`)return route.fulfill({json:{document:{thread_id:original,graph,source_count:1,source_threads:1}}});
   if(name===`/api/thread/${original}`){detailReads++;if(holdDetail)await new Promise(resolve=>releaseDetail=resolve);if(detailFail)return route.fulfill({status:503,body:'Observer unavailable'});return route.fulfill({json:{items:[],steps:[],statusUpdates:[],continuations:runs,updatedNodes:[]}});}
   if(name.startsWith('/api/'))return route.fulfill({json:{}});
   const file=name==='/'?'index.html':name.slice(1);
   return route.fulfill({body:fs.readFileSync(path.join(__dirname,'../web',file)),contentType:file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.svg')?'image/svg+xml':file.endsWith('.woff2')?'font/woff2':'text/html'});
  });
  await page.emulateMedia({reducedMotion:'reduce'});await page.goto('http://queue.test');await page.locator('#open-work-queue').click();
  await page.getByRole('heading',{name:'Work queue',exact:true}).waitFor();assert.equal(await page.locator('#queue-list>li').count(),3);assert.match(await page.locator('#queue-description').textContent(),/Goal paused/);
  const queued=page.locator('#queue-queued-request-1234'),unknown=page.locator('#queue-unknown-request-1234'),running=page.locator('#queue-running-request-1234');
  assert.equal(await queued.getByRole('button',{name:'Cancel queued request',exact:true}).count(),1);assert.equal(await unknown.getByRole('button',{name:'Dismiss record',exact:true}).count(),1);assert.equal(await unknown.getByRole('button',{name:'Cancel queued request',exact:true}).count(),0);assert.equal(await running.getByRole('button').count(),0);
  assert.equal(await queued.getByRole('link',{name:'Open original chat'}).getAttribute('href'),`codex://threads/${original}`);
  await page.waitForFunction(()=>document.querySelector('#queue-sync').dataset.state==='updated');
  assert.equal(await page.locator('#queue-sync').textContent(),'Updated');
  const before=await page.locator('#work-queue').boundingBox(),rowBefore=await queued.boundingBox();
  holdDetail=true;await page.evaluate(()=>{window.heldPoll=loadActivity(true)});
  await page.waitForFunction(()=>document.querySelector('#queue-sync').dataset.state==='checking');
  assert.equal(await page.locator('#queue-sync').textContent(),'Checking');assert(!((await page.locator('.queue-body').textContent()).includes('Checking Codex')));
  assert.equal((await page.locator('#work-queue').boundingBox()).height,before.height);assert.equal((await queued.boundingBox()).height,rowBefore.height);
  holdDetail=false;releaseDetail();await page.evaluate(()=>window.heldPoll);assert.equal(await page.locator('#queue-sync').textContent(),'Updated');
  await page.screenshot({path:path.join(os.tmpdir(),'goal-queue-desktop.png')});
  await queued.getByRole('button',{name:'Cancel queued request',exact:true}).click();await page.waitForFunction(()=>document.querySelector('[data-queue-request="queued-request-1234"]').disabled);await page.evaluate(()=>manageQueue('cancel','queued-request-1234'));assert.equal(actions.length,1);release();await queued.waitFor({state:'detached'});assert.deepEqual(actions[0],{action:'cancel',request_id:'queued-request-1234'});
  await unknown.getByRole('button',{name:'Dismiss record',exact:true}).focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>document.querySelector('[data-queue-request="unknown-request-1234"]').disabled);release();await unknown.waitFor({state:'detached'});assert.deepEqual(actions[1],{action:'dismiss',request_id:'unknown-request-1234'});assert(await running.isVisible());
  runs.push({...base,request_id:'race-request-123456',status:'queued',topic:'Newly started task',can_cancel:true,message:'Confirmed queued'});await page.evaluate(()=>loadActivity(true));fail=true;
  await page.locator('#queue-race-request-123456').getByRole('button',{name:'Cancel queued request'}).click();await page.waitForFunction(()=>document.querySelector('[data-queue-request="race-request-123456"]').disabled);release();await page.waitForFunction(()=>!!document.querySelector('#queue-error').textContent);assert.match(await page.locator('#queue-error').textContent(),/already started/);assert(await page.locator('#queue-race-request-123456').isVisible());
  fail=false;
  // The actual five-second poll must update multiple requests without action clicks.
  const checked=Date.now();runs=runs.map(r=>r.request_id==='running-request-1234'?{...r,checked_at:checked,updated_at:checked,progress:{completed:4,running:2,failed:1}}:r);
  await page.waitForFunction(()=>document.querySelector('#queue-running-request-1234 .queue-progress')?.textContent.includes('4 completed · 2 running · 1 failed'),null,{timeout:8000});
  assert.match(await running.locator('.queue-progress').textContent(),/Checked.*Last change/);
  assert.equal(await page.locator('#queue-error').textContent(),'');
  // Failed polling keeps known records and disables stale mutation controls, then recovers.
  detailFail=true;await page.evaluate(()=>loadActivity(true));assert.match(await page.locator('#queue-error').textContent(),/Could not verify/);assert.equal(await page.locator('#queue-sync').textContent(),'Unavailable');assert(await running.isVisible());
  assert.equal(await page.locator('#queue-race-request-123456 [data-queue-action]').isDisabled(),true);
  detailFail=false;runs=runs.map(r=>r.request_id==='running-request-1234'?{...r,status:'completed',finished:true,message:'Run finished. Node progress updated.',progress:{completed:6,running:0,failed:1}}:r);
  await page.evaluate(()=>loadActivity(true));assert.equal(await page.locator('#queue-error').textContent(),'');assert.match(await running.textContent(),/completed/i);assert.match(await running.locator('.queue-progress').textContent(),/6 completed · 0 running/);assert.equal(await running.getByRole('button').count(),0);assert.equal(await page.locator('#open-work-queue .queue-count').textContent(),'1');assert(detailReads>3);
  for(const width of [320,390]){await page.setViewportSize({width,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));const bounds=await page.locator('#work-queue').boundingBox();assert(bounds.width<=width&&bounds.height<=844);const title=await page.locator('#queue-title').boundingBox(),pill=await page.locator('#queue-sync').boundingBox(),close=await page.locator('#close-queue').boundingBox();assert(title.x+title.width<=pill.x);assert(pill.x+pill.width<=close.x);}
  await page.screenshot({path:path.join(os.tmpdir(),'goal-queue-mobile.png')});
  await page.keyboard.press('Escape');assert.equal(await page.locator('#work-queue').evaluate(d=>d.open),false);assert.equal(await page.locator('#open-work-queue').evaluate(b=>b===document.activeElement),true);assert.deepEqual(errors,[]);
  console.log('PASS: queue panel shows topics, paused context, original-chat links, confirmed cancel, local dismiss, running protection, pending deduplication, race errors, automatic multi-request progress polling, completed history, outage recovery, fixed-size header refresh pill without row jumps, keyboard/Escape and mobile.');
 }finally{await browser.close()}
})().catch(e=>{console.error(e);process.exitCode=1});

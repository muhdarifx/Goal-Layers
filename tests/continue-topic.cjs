// Isolated browser fixture: no real Codex chat is opened or sent a message.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 const original='11111111-2222-4333-8444-555555555555',fork='aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
 const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 try{
  const page=await browser.newPage({hasTouch:true}),requests=[],goalRequests=[];let release,releaseGoal,goalStatus='active',goalError=false,continuations=[],updatedNodes=[],trace={};
  const graph={summary:'Goal',limitations:[],inspected_files:[],nodes:[{id:'root',title:'Original goal',kind:'goal',lane:'aligned',status:'unknown',description:'',next:'',evidence:[]}],edges:[]};
  await page.route('http://continue.test/**',async route=>{
   const name=new URL(route.request().url()).pathname;
   if(name==='/events')return route.abort();
   if(name.endsWith('/goal')&&route.request().method()==='POST'){goalRequests.push(route.request().postDataJSON());await new Promise(resolve=>releaseGoal=resolve);if(goalError)return route.fulfill({status:409,body:typeof goalError==='string'?goalError:'Goal changed in Codex.'});goalStatus=goalRequests.at(-1).status;return route.fulfill({json:{thread_id:original,goal:{threadId:original,objective:'Build a tool',status:goalStatus}}});}
   if(route.request().method()==='POST'){
    requests.push({path:name,body:route.request().postDataJSON()});
    await new Promise(resolve=>release=resolve);
    return route.fulfill({json:{status:'queued',transport:'original',destination_thread_id:original,message:'Stored in the original chat queue.'}});
   }
   if(name==='/api/catalog')return route.fulfill({json:{projects:[{id:'p',name:'Test'}],threads:[{id:original,projectId:'p',name:'Goal'}],goals:{[original]:{objective:'Build a tool',status:goalStatus}},nodes:[]}});
   if(name===`/api/flow/${original}`)return route.fulfill({json:{document:{thread_id:original,source_count:1,source_threads:1,graph}}});
   if(name===`/api/thread/${original}`)return route.fulfill({json:{items:[],steps:[],statusUpdates:[],continuations,updatedNodes}});
   if(name===`/api/trace/${original}`)return route.fulfill({json:trace});
   if(name.startsWith('/api/'))return route.fulfill({json:{}});
   const file=name==='/'?'index.html':name.slice(1);
   return route.fulfill({body:fs.readFileSync(path.join(__dirname,'../web',file)),contentType:file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.svg')?'image/svg+xml':file.endsWith('.woff2')?'font/woff2':'text/html'});
  });
  await page.addInitScript(()=>{crypto.randomUUID=()=> '00000000-0000-4000-8000-000000000001';window.open=(url)=>{window.openedChats=(window.openedChats||[]).concat(url);return null}});
  await page.emulateMedia({reducedMotion:'reduce'});await page.goto('http://continue.test');await page.locator(`[data-thread="${original}"]`).click();await page.locator('[data-node="root"]').click();
  // Reference popup keeps its navigation and chat actions fixed while each tab scrolls.
  assert.equal(await page.locator('.node-overview h3 svg').count(),3);
  assert.equal(await page.locator('#close-inspector').evaluate(e=>getComputedStyle(e).backgroundColor),'rgb(255, 255, 255)');
  await page.locator('#close-inspector').focus();await page.keyboard.press('Shift+Tab');await page.keyboard.press('Tab');assert.equal(await page.locator('#close-inspector').evaluate(e=>getComputedStyle(e).outlineColor),'rgb(34, 34, 34)');
  assert.equal(await page.locator('[data-edit]').evaluate(e=>getComputedStyle(e).color),'rgb(23, 23, 23)');
  const verification=page.locator('#verification-help-trigger'),tooltip=page.locator('#verification-help-tooltip');
  assert.equal(await page.locator('details.verification-help').count(),0);
  assert.equal(await tooltip.isVisible(),false);
  const rowBefore=await page.locator('.node-overview .detail-section').first().boundingBox();
  await verification.hover();await tooltip.waitFor({state:'visible'});
  assert.match(await tooltip.innerText(),/Reported completion and verified checks are separate/);
  const rowAfter=await page.locator('.node-overview .detail-section').first().boundingBox();assert.equal(rowAfter.height,rowBefore.height,'Tooltip must not expand the row');
  const triggerBox=await verification.boundingBox();await page.mouse.move(triggerBox.x+5,triggerBox.y-4);await tooltip.hover();await page.waitForTimeout(250);assert(await tooltip.isVisible(),'Tooltip stays open when moving over its text');
  await page.keyboard.press('Escape');assert.equal(await tooltip.isVisible(),false);assert(await page.locator('#inspector').isVisible(),'Escape dismisses the tooltip first');
  await verification.focus();await tooltip.waitFor({state:'visible'});await page.evaluate(()=>renderInspector());assert(await tooltip.isVisible(),'Polling preserves and repositions the open tooltip');
  await page.getByRole('tab',{name:'Evidence',exact:true}).click();assert.equal(await tooltip.isVisible(),false);
  await page.getByRole('tab',{name:'Overview',exact:true}).click();
  await page.setViewportSize({width:390,height:900});await verification.tap();await tooltip.waitFor({state:'visible'});
  const tipBox=await tooltip.boundingBox();assert(tipBox.x>=0&&tipBox.x+tipBox.width<=390);assert(tipBox.y>=0&&tipBox.y+tipBox.height<=900);
  const alignment=await verification.evaluate(e=>{const text=e.querySelector('span').getBoundingClientRect(),icon=e.querySelector('svg').getBoundingClientRect();return {gap:icon.left-text.right,offset:Math.abs((text.top+text.height/2)-(icon.top+icon.height/2))}});assert(alignment.gap>=6&&alignment.gap<=8);assert(alignment.offset<=1);
  await page.locator('#inspector h2').click();assert.equal(await tooltip.isVisible(),false);
  for(const width of [390,1440]){
   await page.setViewportSize({width,height:900});
   assert.equal(await page.locator('#inspector').evaluate(e=>Math.round(e.getBoundingClientRect().width)),width>760?780:width-20,'Popup retains its previous compact width');
   for(const tab of ['overview','evidence','connections','response']){
    await page.locator('#detail-tab-'+tab).click();
    const pane=page.locator('#detail-panel-'+tab);assert(await pane.isVisible());
    await pane.evaluate(e=>{const content=document.createElement('div');content.dataset.scrollProbe='';content.style.height='1800px';e.append(content)});
    const before=await page.locator('.chat-action-bar').boundingBox();
    await pane.evaluate(e=>e.scrollTop=500);
    const after=await page.locator('.chat-action-bar').boundingBox();assert.equal(after.y,before.y);
    assert(await page.locator('.detail-tabs').isVisible());
    assert(await page.locator('#inspector').evaluate(e=>e.scrollWidth<=e.clientWidth));
    await pane.locator('[data-scroll-probe]').evaluate(e=>e.remove());
   }
  }
  await page.locator('#detail-tab-overview').focus();await page.keyboard.press('ArrowRight');assert.equal(await page.locator('#detail-tab-evidence').getAttribute('aria-selected'),'true');
  await page.evaluate(fork=>{chatRequests.set(chatRequestKey(),{action:'breakdown',transport:'fork',forkId:fork,queued:true,pending:false,message:'Reviewing'});inspectorTab='response';renderInspector()},fork);
  assert.equal(await page.getByRole('link',{name:'Open in Codex',exact:true}).getAttribute('href'),`codex://threads/${fork}`);
  await page.getByRole('button',{name:'Continue this topic',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('[data-chat-action="continue"]').disabled);
  await page.evaluate(()=>sendChatAction('continue'));
  assert.equal(requests.length,1);assert.equal(requests[0].path,`/api/thread/${original}/message`);assert.equal(requests[0].body.action,'continue');
  release();const link=page.getByRole('link',{name:'Open original chat',exact:true});await link.waitFor();assert(await page.locator('[data-node="root"] .badge.queued').isVisible(),'A successful queued receipt updates the node immediately');
  assert.equal(await link.getAttribute('href'),`codex://threads/${original}`);
  assert.equal(await page.getByRole('link',{name:'Open in Codex',exact:true}).count(),0);
  assert.equal(await page.evaluate(()=>inspectorTab),'overview');assert.deepEqual(await page.evaluate(()=>window.openedChats),[`codex://threads/${original}`]);
  await page.evaluate(()=>{activity=[{id:'new-reply',kind:'agentMessage',title:'PRIVATE ORIGINAL CHAT REPLY'}];renderInspector()});assert(!await page.locator('#inspector').textContent().then(t=>t.includes('PRIVATE ORIGINAL CHAT REPLY')));assert.equal(await page.locator('.response-spinner').count(),0);
  await page.evaluate(()=>loadActivity(true));assert.equal(await link.getAttribute('href'),`codex://threads/${original}`);
  const run={request_id:requests[0].body.request_id,requested_at:1,status:'running',finished:false,node_update_status:'queued',node_ids:['root'],destination_thread_id:original,message:'Working in the original chat. Progress will update when this run finishes.'};
  const recordedStatus=await page.evaluate(()=>allNodes().find(n=>n.id==='root').status);
  // Receipt queue state changes presentation, never the saved node or its edit value.
  continuations=[{...run,status:'queued',message:'Confirmed in the original chat queue.'}];await page.evaluate(()=>loadActivity(true));
  await page.locator('[data-node="root"] .badge.queued').waitFor();assert.equal(await page.locator('[data-node="root"] .badge.queued').innerText(),'Queued');assert.equal(await page.evaluate(()=>allNodes().find(n=>n.id==='root').status),recordedStatus);
  assert.equal(await page.evaluate(()=>nodeProgress({id:'unrelated',thread_id:selected.thread,status:'blocked'})),'blocked');
  await page.evaluate(()=>chatRequests.clear());await page.evaluate(()=>loadActivity(true));assert(await page.locator('[data-node="root"] .badge.queued').isVisible());assert.equal(requests.length,1);
  for(const status of ['cancelled','dismissed','failed','interrupted']){continuations=[{...run,status,finished:true}];await page.evaluate(()=>loadActivity(true));assert.equal(await page.locator('[data-node="root"] .badge.queued').count(),0);assert.equal(await page.evaluate(()=>allNodes().find(n=>n.id==='root').status),recordedStatus);}
  continuations=[{...run,status:'unconfirmed'}];await page.evaluate(()=>loadActivity(true));await page.locator('[data-node="root"] .badge.unconfirmed').waitFor();
  continuations=[run];await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));await page.locator('.continue-delivery:visible').filter({hasText:'Working in the original chat.'}).waitFor();assert.match(await page.locator('.continue-delivery:visible').textContent(),/Working in the original chat/);
  await page.locator('[data-node="root"] .badge.running').waitFor();continuations.push({...run,request_id:'another-queued-request',requested_at:2,status:'queued'});await page.evaluate(()=>loadActivity(true));assert(await page.locator('[data-node="root"] .badge.running').isVisible(),'A running request takes priority over another queued request for that node');
  // Replay the real failure: a detached observer says interrupted during the owner's live turn.
  continuations=[{...run,status:'interrupted',finished:true,turn_id:'owner-turn'}];
  trace={turn_lifecycle:{'owner-turn':{status:'running',updated_at:new Date().toISOString()}}};
  await page.evaluate(()=>loadActivity(true));await page.locator('[data-node="root"] .badge.running').waitFor();
  assert.equal(await page.locator('.node-overview .badge.running').innerText(),'Running');
  assert.equal(await page.evaluate(()=>queueRuns.get(selected.thread)[0].finished),false);
  trace.turn_lifecycle['owner-turn'].updated_at=new Date(Date.now()-120000).toISOString();
  await page.evaluate(()=>loadActivity(true));await page.locator('[data-node="root"] .badge.unconfirmed').waitFor();
  trace.turn_lifecycle['owner-turn']={status:'completed',updated_at:new Date().toISOString()};
  await page.evaluate(()=>loadActivity(true));assert.equal(await page.locator('[data-node="root"] .badge.running').count(),0);
  assert.equal(await page.evaluate(()=>queueRuns.get(selected.thread)[0].status),'completed');
  assert.equal(requests.length,1,'Lifecycle reconciliation must never send another request');
  trace={};continuations=[run,{...run,request_id:'another-queued-request',requested_at:2,status:'queued'}];
  await page.evaluate(()=>loadActivity(true));
  assert.equal(await page.locator('[data-node="root"] .badge.running').innerText(),'Running');
  assert.equal(await page.locator('.node-overview .badge.running').innerText(),'Running');
  for(const status of ['blocked','complete','pending','in_progress'])assert.equal(await page.evaluate(status=>nodeProgress({id:'root',thread_id:selected.thread,status}),status),'running','Live running takes precedence over the recorded work status');
  assert.equal(await page.evaluate(()=>allNodes().find(n=>n.id==='root').status),recordedStatus);
  for(const status of ['completed','cancelled','failed','interrupted']){
   assert.equal(await page.evaluate(({status,recordedStatus})=>{const old=queueRuns.get(selected.thread);queueRuns.set(selected.thread,[{node_ids:['root'],status,finished:true}]);try{return nodeProgress({id:'root',thread_id:selected.thread,status:recordedStatus})}finally{queueRuns.set(selected.thread,old)}},{status,recordedStatus}),recordedStatus,'Finished runs release the presentation overlay');
  }
  await page.evaluate(()=>sendChatAction('continue'));assert.equal(requests.length,1);
  updatedNodes=[{...graph.nodes[0],thread_id:original,status:'complete',status_report_id:'final-status',reported_evidence:'Recorded checks passed'}];
  continuations=[{...run,status:'completed',finished:true,node_update_status:'applied',message:'Run finished. Node progress updated.'}];
  await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));await page.locator('[data-node="root"] .badge.complete').waitFor();assert.match(await page.locator('.continue-delivery:visible').textContent(),/Run finished. Node progress updated/);assert.match(await page.locator('[data-node="root"] .node-footer').textContent(),/Complete/);assert.equal(requests.length,1,'Auto-update must not send another message');
  await page.evaluate(()=>{chatRequests.clear()});await page.evaluate(()=>loadActivity(true));assert.equal(await page.evaluate(()=>chatRequests.get(chatRequestKey()).finished),true,'Saved run restores after reload');
  continuations.push({...run,request_id:'newer-request',requested_at:2,message:'Working in the original chat.'});await page.evaluate(()=>{chatRequests.clear()});await page.evaluate(()=>loadActivity(true));assert.equal(await page.evaluate(()=>chatRequests.get(chatRequestKey()).requestId),'newer-request','The latest run replaces an older finished run after reload');assert.match(await page.locator('.continue-delivery:visible').textContent(),/Working in the original chat/);


  await link.evaluate(el=>el.addEventListener('click',e=>{e.preventDefault();window.openedOriginal=el.href},{once:true}));await link.focus();await page.keyboard.press('Enter');
  assert.equal(await page.evaluate(()=>window.openedOriginal),`codex://threads/${original}`);
  await page.locator('#close-inspector').click();
  assert.equal(await page.getByRole('group',{name:'Goal control',exact:true}).count(),1);assert.equal(await page.getByRole('group',{name:'Update and review',exact:true}).count(),1);assert.equal(await page.getByRole('group',{name:'View tools',exact:true}).count(),1);
  await page.screenshot({path:require('os').tmpdir()+'/goal-toolbar-desktop.png'});
  await page.getByRole('button',{name:'Pause goal',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#goal-control').disabled);await page.evaluate(()=>controlGoal());assert.equal(goalRequests.length,1);assert.deepEqual(goalRequests[0],{status:'paused'});
  releaseGoal();await page.getByRole('button',{name:'Resume goal',exact:true}).waitFor();
  await page.getByRole('button',{name:'Resume goal',exact:true}).focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>document.querySelector('#goal-control').disabled);assert.deepEqual(goalRequests[1],{status:'active'});releaseGoal();await page.getByRole('button',{name:'Pause goal',exact:true}).waitFor();
  await page.evaluate(()=>{data.goals[selected.thread].status='blocked';render()});
  await page.getByRole('button',{name:'Resume goal',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#goal-control').disabled);assert.deepEqual(goalRequests.at(-1),{status:'active'});releaseGoal();await page.getByRole('button',{name:'Pause goal',exact:true}).waitFor();
  goalError=true;await page.getByRole('button',{name:'Pause goal',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#goal-control').disabled);releaseGoal();await page.waitForFunction(()=>!document.querySelector('#goal-control').disabled);assert.equal(await page.getByRole('button',{name:'Pause goal',exact:true}).count(),1);assert.match(await page.locator('#notice').textContent(),/Could not change/);
  goalError=JSON.stringify({code:'goal_writer_busy',thread_id:original,message:'Codex internals must not be displayed'});
  const sendsBeforeConflict=requests.length;
  for(const state of ['active','paused']){
   await page.evaluate(state=>{data.goals[selected.thread].status=state;render()},state);
   await page.locator('#goal-control').click();await page.waitForFunction(()=>document.querySelector('#goal-control').disabled);releaseGoal();await page.waitForFunction(()=>!document.querySelector('#goal-control').disabled);
   assert.equal(await page.evaluate(()=>data.goals[selected.thread].status),state,'A conflict must not claim a goal changed');
   const recovery=page.getByRole('button',{name:'Open original chat',exact:true});await recovery.waitFor();
   assert.match(await page.locator('#notice').innerText(),/original Codex chat/);assert(!/internals|-32603|active writer/.test(await page.locator('#notice').innerText()));
   await recovery.focus();await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>openedChats.at(-1)),'codex://threads/'+original);
   const attempts=goalRequests.length;await page.getByRole('button',{name:state==='paused'?'Resume in Codex':'Pause in Codex',exact:true}).click();assert.equal(goalRequests.length,attempts,'Repeated clicks use backup instead of repeating a known writer conflict');assert.equal(await page.evaluate(()=>openedChats.at(-1)),'codex://threads/'+original);
  }
  assert.equal(requests.length,sendsBeforeConflict,'Conflict recovery must not send a message or interrupt a run');
  await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.screenshot({path:require('os').tmpdir()+'/goal-writer-conflict-mobile.png'});
  await page.setViewportSize({width:1440,height:1000});await page.screenshot({path:require('os').tmpdir()+'/goal-writer-conflict-desktop.png'});
  goalError=false;
  await page.evaluate(()=>{data.goals[selected.thread].status='complete';render()});assert(await page.locator('#goal-control').isHidden());
  await page.locator('[data-node="root"]').click();
  await page.setViewportSize({width:390,height:844});assert(await link.isVisible());assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await page.screenshot({path:require('os').tmpdir()+'/goal-controls-mobile.png'});
  await page.locator('#close-inspector').click();await page.evaluate(()=>{data.goals[selected.thread].status='paused';render()});assert(await page.getByRole('button',{name:'Resume in Codex',exact:true}).isVisible());assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.screenshot({path:require('os').tmpdir()+'/goal-toolbar-mobile.png'});
  // Header reproduction keeps primary actions separate and the full objective available.
  assert.equal(await page.locator('.notice-row #goal-run-status').count(),1);
  const longTitle='Build the complete product workspace with separate domains, team permissions, evidence-backed progress and clear next steps. '.repeat(5);
  await page.evaluate(title=>{data.goals[selected.thread].objective=title;render();$('#notice').textContent='Unconfirmed record dismissed locally. The original chat was not changed.';},longTitle);
  assert.equal(await page.locator('#flow-title').getAttribute('title'),longTitle.trim());
  assert.equal(await page.locator('#flow-title').innerText(),longTitle.trim());
  assert.equal(await page.locator('.goal-metadata #goal-state-label').innerText(),'Paused');assert.equal(await page.locator('.goal-metadata #goal-state-label svg').count(),1);assert.match(await page.locator('.flow-main-row').evaluate(e=>getComputedStyle(e).backgroundImage),/repeating-linear-gradient/);assert.deepEqual(await page.locator('#goal-state-label').evaluate(e=>({color:getComputedStyle(e).color,background:getComputedStyle(e).backgroundColor})),{color:'rgb(85, 85, 85)',background:'rgb(255, 255, 255)'});
  for(const width of [320,390,768,1024,1440,1920]){
   await page.setViewportSize({width,height:900});
   const layout=await page.evaluate(()=>{const box=s=>document.querySelector(s).getBoundingClientRect(),title=box('#flow-title'),main=box('.flow-main-row'),tools=box('.flow-toolbar'),notice=box('.notice-row');return {overflow:document.documentElement.scrollWidth>innerWidth,titleHeight:title.height,lineHeight:parseFloat(getComputedStyle(document.querySelector('#flow-title')).lineHeight),rows:main.bottom<=tools.top+1&&tools.bottom<=notice.top+1,bounds:[...document.querySelectorAll('.flow-header button:not([hidden]),.notice-row button')].filter(e=>e.getClientRects().length).every(e=>{const b=e.getBoundingClientRect();return b.left>=0&&b.right<=innerWidth})}});
   assert(!layout.overflow);assert(layout.rows);assert(layout.bounds);assert(Math.abs(layout.titleHeight-layout.lineHeight)<=1);
  }
  await page.getByRole('button',{name:'Dismiss notification',exact:true}).focus();await page.keyboard.press('Enter');assert(await page.locator('.notice-row').isHidden());assert.equal(await page.locator('#reconstruct').evaluate(e=>e===document.activeElement),true);
  await page.evaluate(()=>render());assert(await page.locator('.notice-row').isHidden(),'Dismissed continuation notice stays hidden across renders');await page.evaluate(()=>$('#notice').textContent='A new update is available.');assert(await page.locator('.notice-row').isVisible());
  await page.getByRole('button',{name:'Dismiss notification',exact:true}).tap();assert(await page.locator('.notice-row').isHidden());
  console.log('PASS: queued node badges update immediately and after restore; running takes precedence, terminal/unconfirmed states reconcile without changing recorded evidence; Continue opens original chat, shows only delivery confirmation, never renders the reply; duplicates suppressed. Continued runs restore after reload and auto-update nodes without new requests. Toolbar groups are distinct. Native goal controls handle pause/resume, pending clicks, keyboard, errors, completed goals and mobile.');
 }finally{await browser.close()}
})().catch(e=>{console.error(e);process.exitCode=1});

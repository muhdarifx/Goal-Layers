// Synthetic browser fixtures only; no real chat or model calls.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 try{
 const page=await browser.newPage(),requests=[],errors=[];
 let outcome=null,items=[],failFresh=false,requireConsent=false,job=null;page.on('dialog',async d=>{errors.push('Unexpected browser popup');await d.dismiss()});
 const graph={summary:'Goal',limitations:[],inspected_files:[],nodes:[{id:'root',title:'Original goal',kind:'goal',lane:'aligned',status:'unknown',description:'',next:'',evidence:[{source_id:'goal:t',quote:'Build a tool'}]}],edges:[]};
 graph.nodes.push({...graph.nodes[0],id:'r1',kind:'Requirement',title:'R1 Build the tool'});
 graph.edges.push({source:'root',target:'r1',label:'requires',feedback:false});
 page.on('pageerror',e=>errors.push(e.message));
 await page.route('http://scope.test/**',async route=>{
  const name=new URL(route.request().url()).pathname;let body,contentType='application/json';
  if(name==='/events')return route.abort();
  if(route.request().method()==='POST'){
   const request=route.request().postDataJSON();requests.push({path:name,body:request});
   if(name.endsWith('/review/stop')){outcome={...outcome,status:'cancelled',message:'Review stopped'};return route.fulfill({contentType,body:JSON.stringify(outcome)});}
   if(requireConsent&&!request.confirmed)return route.fulfill({contentType,body:JSON.stringify({status:'confirmation_required',options:{speed:'Ultrafast',model:'review-model'},message:'Run a read-only AI review? Ultrafast consumes usage or credits and may incur API charges. Continue?'})});
   if(request?.fresh&&failFresh)return route.fulfill({status:503,body:'Review service unavailable'});
   if(request?.fresh)return route.fulfill({contentType,body:JSON.stringify({action:'breakdown',status:'queued',request_id:request.request_id,message:'Preparing a read-only review fork'})});
   body=name.endsWith('/reconstruct')?'{}':JSON.stringify(outcome?.status==='applied'?{...outcome,reused:true}:{action:'breakdown',status:'queued',request_id:request.request_id,message:'Preparing a read-only review fork'});
  }else if(name==='/api/catalog')body=JSON.stringify({projects:[{id:'p',name:'Test'}],threads:[{id:'t',projectId:'p',name:'Goal'}],goals:{t:{objective:'Build a tool',status:'active'}},nodes:[]});
  else if(name==='/api/flow/t')body=JSON.stringify({job,document:{thread_id:'t',source_count:1,source_threads:1,graph}});
  else if(name==='/api/thread/t')body=JSON.stringify({items,steps:[],statusUpdates:outcome?[outcome]:[]});
  else if(name.startsWith('/api/'))body='{}';
  else{const file=name==='/'?'index.html':name.slice(1);body=fs.readFileSync(path.join(__dirname,'../web',file));contentType=file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html';}
  return route.fulfill({body,contentType});
 });
 await page.addInitScript(()=>{let id=0;crypto.randomUUID=()=> '00000000-0000-4000-8000-'+String(++id).padStart(12,'0')}); await page.goto('http://scope.test');await page.locator('[data-thread=t]').click();
 await page.getByRole('button',{name:'Deep review',exact:true}).waitFor({state:'visible'});
 assert.equal(await page.locator('#review-flow').count(),0);
 assert.equal(await page.locator('#deep-review-flow svg[aria-hidden="true"]').count(),1);
 await page.locator('#reconstruct').click();assert.equal(requests[0].path,'/api/flow/t/reconstruct');requests.length=0;
 for(const width of [1280,390]){
  await page.setViewportSize({width,height:844});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 }
 await page.locator('#deep-review-flow').click();
 await page.waitForFunction(()=>document.body.textContent.includes('Preparing a read-only review fork'));
 assert.equal(requests.length,1);
 assert.equal(requests[0].path,'/api/thread/t/message');
 assert.equal(requests[0].body.action,'breakdown');assert.deepEqual(requests[0].body.node_ids,[]);
 await page.keyboard.press('Escape');await page.locator('#deep-review-flow').click();await page.waitForFunction(()=>document.body.textContent.includes('Preparing a read-only review fork'));assert.equal(requests.length,1);
 outcome={action:'breakdown',transport:'fork',fork_id:'11111111-2222-4333-8444-555555555555',fork_deleted:false,request_id:requests[0].body.request_id,status:'delivered',progress:{status:'inProgress',completed:3,running:1},message:'Request received by the read-only fork. Waiting for its completed scope breakdown.'};
 items=[{id:'progress',kind:'agentMessage',title:'Unrelated original-chat update'}];
 await page.getByRole('link',{name:'Open in Codex',exact:true}).waitFor();
 await page.getByRole('heading',{name:'Reviewing scope',exact:true}).waitFor();
 assert(await page.locator('.response-spinner').isVisible());
 assert.equal(await page.getByText('Unrelated original-chat update',{exact:true}).count(),0);
 assert.match(await page.locator('.review-progress').innerText(),/3 operations finished.*1 running/);
 const reviewLink=page.getByRole('link',{name:'Open in Codex',exact:true});
 assert.equal(await reviewLink.getAttribute('href'),'codex://threads/11111111-2222-4333-8444-555555555555');
 const sendsBeforeRestore=requests.length;await page.evaluate(()=>{chatRequests.clear();return loadActivity(true)});await reviewLink.waitFor();assert.equal(requests.length,sendsBeforeRestore,'Restoring a pending review must not send another request');
 await reviewLink.evaluate(e=>e.addEventListener('click',event=>{event.preventDefault();window.openedReview=e.href},{once:true}));await reviewLink.focus();await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>window.openedReview),'codex://threads/11111111-2222-4333-8444-555555555555');
 outcome.message='# Review notes\n\n| Step | Status |\n| --- | --- |\n| **Build** | Complete |\n\n'+Array(80).fill('- Evidence item').join('\n')+'\n\n<script>window.unsafeReview=true</script>';
 await page.evaluate(()=>loadActivity());
 assert.equal(await page.locator('.response-scroll h1').innerText(),'Review notes');assert.equal(await page.locator('.response-scroll table').count(),1);assert.equal(await page.evaluate(()=>window.unsafeReview),undefined);
 for(const width of [1280,390]){await page.setViewportSize({width,height:844});const layout=await page.evaluate(()=>{const scroll=document.querySelector('.response-scroll'),link=document.querySelector('.review-chat-link'),modal=document.querySelector('#inspector');return {overflow:scroll.scrollHeight>scroll.clientHeight,link:link.getBoundingClientRect().bottom,modal:modal.getBoundingClientRect().bottom,height:modal.getBoundingClientRect().height}});assert(layout.overflow);assert(layout.link<=layout.modal);assert(layout.modal<=844);if(width===1280)assert(layout.height<=844*.8+2);}
 outcome.message='Request received by the read-only fork. Waiting for its completed scope breakdown.';await page.evaluate(()=>loadActivity());
 const stopButton=page.getByRole('button',{name:'Stop review',exact:true});assert.equal(await stopButton.innerText(),'Stop');assert.equal(await stopButton.locator('svg').count(),1);assert.deepEqual(await stopButton.evaluate(e=>({background:getComputedStyle(e).backgroundColor,color:getComputedStyle(e).color})),{background:'rgb(220, 38, 38)',color:'rgb(255, 255, 255)'});
 const beforeStop=requests.length;await page.getByRole('button',{name:'Stop review',exact:true}).click();await page.getByRole('heading',{name:'Review stopped',exact:true}).waitFor();assert.equal(requests.length,beforeStop+1);assert.equal(requests.at(-1).path,'/api/thread/t/review/stop');await reviewLink.waitFor();await page.getByRole('button',{name:'Run fresh review',exact:true}).waitFor();assert.equal(await reviewLink.evaluate(e=>getComputedStyle(e).backgroundColor),'rgb(32, 32, 32)');outcome.status='delivered';outcome.message='Request received by the read-only fork. Waiting for its completed scope breakdown.';await page.evaluate(()=>loadActivity());
 outcome.fork_id='javascript:alert(1)';await page.evaluate(()=>loadActivity());assert.equal(await reviewLink.count(),0);
 outcome.fork_id='11111111-2222-4333-8444-555555555555';await page.evaluate(()=>loadActivity());await reviewLink.waitFor();
 // Deletion can arrive without a new message; remove the stale link on that update.
 outcome.progress.completed=4;await page.waitForFunction(()=>document.querySelector('.review-progress')?.textContent.includes('4 operations finished'));assert.match(await page.locator('.review-progress').innerText(),/4 operations finished/);
 outcome.fork_deleted=true;await page.evaluate(()=>loadActivity());assert.equal(await reviewLink.count(),0);
 outcome.fork_deleted=false;await page.evaluate(()=>loadActivity());await reviewLink.waitFor();

 assert(await page.locator('.chat-delivery').textContent().then(t=>t.includes('Request received')));
 assert.equal(await page.locator('.chat-delivery.error').count(),0);
 graph.nodes.push({...graph.nodes[0],id:'detail',kind:'Scope detail',title:'Nested component',status:'blocked',next:'Resolve the missing input'});
 graph.edges.push({source:'r1',target:'detail',label:'includes',feedback:false});
 outcome={action:'breakdown',transport:'fork',fork_id:'11111111-2222-4333-8444-555555555555',fork_deleted:true,request_id:requests[0].body.request_id,status:'applied',message:'Deep review complete',requested_at:1,added_work_summary:{text:'Export support was added; authorization is uncertain.',evidence:[{source_id:'history:1',quote:'Export support'}]}};
 await page.evaluate(()=>loadActivity());
 await page.locator('#flow-edges [data-source="r1"][data-target="detail"]').waitFor({state:'attached'});assert.equal(await reviewLink.count(),0);
 assert.equal(await page.locator('[data-node="detail"]').count(),1);
 assert.match(await page.locator('.flow-node[data-node="detail"]').innerText(),/blocked/i);
 const completedFresh=page.getByRole('button',{name:'Run fresh review',exact:true});await completedFresh.waitFor();
 const beforeRestore=requests.length;await page.evaluate(()=>{chatRequests.clear();return loadActivity()});
 await page.getByRole('heading',{name:'Using saved review',exact:true}).waitFor();await completedFresh.waitFor();
 assert.match(await page.locator('.chat-delivery').innerText(),/Cached results\. No new AI request was sent/);assert.equal(requests.length,beforeRestore);
 await page.evaluate(()=>addedWorkSummaries.clear());await page.keyboard.press('Escape');await page.locator('#deep-review-flow').click();await page.waitForFunction(()=>addedWorkSummaries.get('t')?.text.includes('Export support'));
 const fresh=page.getByRole('button',{name:'Run fresh review',exact:true});
 await fresh.waitFor();assert.equal(await fresh.locator('svg').count(),1);
 await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 const oldRequest=requests.at(-1).body.request_id,count=requests.length;
 await fresh.focus();await page.keyboard.press('Enter');
 await page.waitForFunction(()=>document.body.textContent.includes('Preparing a read-only review fork'));
 assert.equal(requests.length,count+1);assert.equal(requests.at(-1).body.fresh,true);assert.notEqual(requests.at(-1).body.request_id,oldRequest);
 assert.equal(await fresh.count(),0);assert(await page.locator('.response-spinner').isVisible());
 await page.keyboard.press('Escape');await page.locator('#deep-review-flow').click();assert.equal(requests.length,count+1);
 outcome={...outcome,request_id:requests.at(-1).body.request_id};await page.evaluate(()=>loadActivity());
 await page.keyboard.press('Escape');await page.locator('#deep-review-flow').click();await fresh.waitFor();
 failFresh=true;await fresh.click();await page.getByRole('heading',{name:'Unable to send',exact:true}).waitFor();assert.equal(await page.locator('.response-spinner').count(),0);
 assert(await page.locator('.chat-delivery').textContent().then(t=>t.includes('Review service unavailable')));await fresh.waitFor();
 graph.nodes.push({...graph.nodes[0],id:'added',kind:'Changed files',lane:'addition',title:'Export support'});await page.evaluate(()=>GoalFlow.refresh());await page.keyboard.press('Escape');await page.setViewportSize({width:1280,height:844});await page.locator('#flow-minimap').click();await page.locator('[data-node="view-scope-addition"]').click();await page.getByRole('tab',{name:'Overview',exact:true}).click();await page.getByText('Export support was added; authorization is uncertain.',{exact:true}).waitFor();


 requireConsent=true;failFresh=false;outcome=null;let before=requests.length;
 const confirmation=page.locator('#review-confirmation');
 let pending=page.evaluate(()=>sendChatAction('breakdown',true));await confirmation.waitFor({state:'visible'});
 assert.equal(await page.locator('#review-confirm-speed').innerText(),'Ultrafast');assert.match(await confirmation.innerText(),/API charges/);assert.equal(await page.locator('#cancel-review-confirmation').evaluate(e=>e===document.activeElement),true);
 for(const width of [1280,390]){await page.setViewportSize({width,height:844});const box=await confirmation.boundingBox();assert(box.x>=0&&box.x+box.width<=width);assert(box.y>=0&&box.y+box.height<=844);await confirmation.screenshot({path:process.env.TEMP+'/goal-layers-review-confirm-'+width+'.png'});}
 await page.locator('#cancel-review-confirmation').click();await pending;assert.equal(requests.length,before+1);assert.equal(requests.at(-1).body.confirmed,false);assert.match(await page.locator('.chat-delivery').innerText(),/cancelled/);
 before=requests.length;pending=page.evaluate(()=>sendChatAction('breakdown',true));await confirmation.waitFor({state:'visible'});await page.locator('#start-review-confirmation').click();await pending;assert.equal(requests.length,before+2);assert.equal(requests.at(-1).body.confirmed,true);assert.equal(requests.at(-1).body.request_id,requests.at(-2).body.request_id);
 // First-build and status-refresh reviews share the same consent dialog, without repeated prompts.
 for(const action of ['breakdown','refresh_status']){
  await page.evaluate(()=>chatRequests.clear());job={status:'ready',started_at:1,finished_at:2,review_confirmation:{action,request_id:'auto-review-consent-'+action,node_ids:action==='breakdown'?[]:['r1']}};before=requests.length;await page.evaluate(()=>GoalFlow.refresh());await confirmation.waitFor({state:'visible'});await page.keyboard.press('Escape');await confirmation.waitFor({state:'hidden'});await page.waitForFunction(()=>document.querySelector('.chat-delivery')?.textContent.includes('cancelled'));assert.equal(requests.length,before+1);assert.equal(requests.at(-1).body.action,action);await page.evaluate(()=>GoalFlow.refresh());assert.equal(requests.length,before+1);
 }
 for(const [speed,service_tier,count,fill] of [['Ultrafast','ultrafast',1,'none'],['Fast','priority',1,'currentColor'],['Normal','default',1,'none']]){
 const prompt=page.evaluate(({speed,service_tier})=>confirmReview({options:{speed,service_tier,model:'test'}}),{speed,service_tier});await confirmation.waitFor({state:'visible'});assert.equal(await page.locator('#review-confirm-speed svg').count(),count);assert.equal(await page.locator('#review-confirm-speed svg').first().getAttribute('fill'),fill);assert.equal(await page.locator('#review-confirm-speed').innerText(),speed);if(service_tier==='ultrafast'){assert.equal(await page.locator('#review-confirm-speed path').getAttribute('d'),"M12.496 1.55042C13.3266 0.810781 14.6664 1.57948 14.3945 2.70277L13.3867 6.86097H18.0634C19.0535 6.86124 19.5922 8.01841 18.955 8.77601L11.248 17.9284C11.0114 18.2091 10.5914 18.2449 10.3105 18.0084C10.0297 17.7718 9.99389 17.3518 10.2304 17.0709L17.7089 8.19105H15.206C15.199 8.46403 15.1069 8.74127 14.9043 8.98206L7.24312 18.0797C6.45061 19.0208 4.92981 18.25 5.21968 17.0543L6.1689 13.1383H1.7607C0.663874 13.1383 0.0668853 11.8562 0.773392 11.0172L8.43453 1.91956C9.22701 0.978906 10.7474 1.74955 10.458 2.94495L9.50874 6.86097H12.0175L12.956 2.98987L12.7773 3.17933C12.5246 3.44588 12.1034 3.45737 11.8369 3.20472C11.5706 2.95204 11.5599 2.53172 11.8125 2.26527L12.4169 1.62659L12.496 1.55042Z");assert.equal(await page.locator('#review-confirm-speed pattern').count(),0);}await page.locator('#cancel-review-confirmation').click();assert.equal(await prompt,false);
 }
 assert.deepEqual(errors,[]);
 console.log('PASS: custom consent dialog, first-build and refresh consent, cancellation, keyboard and mobile; review chat link, keyboard navigation, unsafe ID rejection, deleted fork removal, cached-result fresh button, keyboard activation, new request ID, pending deduplication, error feedback, nested tree, desktop/mobile.');
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1});

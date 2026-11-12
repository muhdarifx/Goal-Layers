const assert=require('node:assert/strict');
const {chromium}=require('playwright');

(async()=>{
 const base='http://127.0.0.1:4317',id=require('./test-thread.cjs');
 const catalog=await(await fetch(base+'/api/catalog')).json();
 const {DatabaseSync}=require('node:sqlite');
 const db=new DatabaseSync(require('path').join(process.env.USERPROFILE,'.codex/goals_1.sqlite'),{readOnly:true});
 const saved=db.prepare('SELECT thread_id,status,tokens_used FROM thread_goals').all();db.close();
 assert.equal(Object.keys(catalog.goals).length,saved.length);
 for(const g of saved){assert(catalog.threads.some(t=>t.id===g.thread_id));assert.equal(catalog.goals[g.thread_id]?.status,g.status==='usage_limited'?'usageLimited':g.status==='budget_limited'?'budgetLimited':g.status);assert.equal(catalog.goals[g.thread_id]?.tokensUsed,g.tokens_used)}
 const project=catalog.projects.find(p=>p.id===catalog.threads.find(t=>t.id===id)?.projectId);
 assert(project,'Selected demo chat must belong to a project');
 assert.equal(catalog.threads.find(t=>t.id===id)?.projectId,project.id);
 assert(catalog.goals[id],'Selected demo chat must have a saved goal');
 const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 try{
  const page=await browser.newPage();await page.goto(base);
  for(const projectId of [...catalog.projects.map(p=>p.id),'unassigned']){
   await page.locator(`[data-project="${projectId}"]`).click();
   assert.equal(await page.locator('#chats-disclosure').count(),0);
   const expected=catalog.threads.filter(t=>(t.projectId||'unassigned')===projectId&&catalog.goals[t.id]).sort((a,b)=>(catalog.goals[b.id].updatedAt||catalog.goals[b.id].createdAt||0)-(catalog.goals[a.id].updatedAt||catalog.goals[a.id].createdAt||0)||a.id.localeCompare(b.id)).map(t=>t.id);
   assert.deepEqual(await page.locator('#goals [data-thread]').evaluateAll(es=>es.map(e=>e.dataset.thread)),expected);
  }
  await page.locator(`[data-project="${project.id}"]`).click();
  const goal=page.locator(`#goals [data-thread="${id}"]`);
  await goal.waitFor();assert((await goal.innerText()).trim());
  await goal.click();assert.equal(await goal.getAttribute('aria-pressed'),'true');
  await page.screenshot({path:require('path').join(__dirname,'../project-discovery.png')});
  console.log(`PASS: all ${saved.length} saved goals retain status and usage, appear exactly once under their assigned project, and sort newest goal activity first. Selected demo goal opens.`);
 }finally{await browser.close()}
})().catch(e=>{console.error(e);process.exitCode=1});

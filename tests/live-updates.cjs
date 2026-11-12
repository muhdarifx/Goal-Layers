const {chromium}=require('playwright'),fs=require('fs'),path=require('path'),assert=require('node:assert/strict');
(async()=>{
 const base='http://127.0.0.1:4317',root=path.resolve(__dirname,'..'),id=require('./test-thread.cjs');
 const catalog=await(await fetch(base+'/api/catalog')).json(),thread=catalog.threads.find(t=>t.id===id);
 const browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 try{
  const page=await browser.newPage();
  if(!process.env.ATLAS_LIVE)await page.route(base+'/**',async r=>{const name=new URL(r.request().url()).pathname;if(['/','/app.js','/flow.js','/style.css'].includes(name)){const f=name==='/'?'index.html':name.slice(1);await r.fulfill({body:fs.readFileSync(path.join(root,'web',f)),contentType:f.endsWith('.js')?'text/javascript':f.endsWith('.css')?'text/css':'text/html'})}else await r.continue()});
  await page.goto(base);await page.locator(`[data-project="${thread.projectId}"]`).click();await page.locator(`[data-thread="${id}"]`).click();await page.locator('[data-node="original-goal"]').click();
  await page.getByRole('tab',{name:'Evidence',exact:true}).click();
  const selectedText=await page.evaluate(()=>{window.keptNode=document.querySelector('[data-node="original-goal"]');window.keptClose=document.querySelector('#close-inspector');const text=document.querySelector('#inspector h2').firstChild,range=document.createRange();range.selectNodeContents(text);getSelection().removeAllRanges();getSelection().addRange(range);return getSelection().toString()});
  await page.evaluate(()=>{for(let i=0;i<5;i++){data.goals[selected.thread].tokensUsed++;render()}});
  assert.equal(await page.evaluate(()=>getSelection().toString()),selectedText);
  assert(await page.evaluate(()=>keptNode===document.querySelector('[data-node="original-goal"]')&&keptClose===document.querySelector('#close-inspector')));
  assert.equal(await page.getByRole('tab',{name:'Evidence',exact:true}).getAttribute('aria-selected'),'true');
  await page.evaluate(()=>getSelection().removeAllRanges());
  await page.getByRole('button',{name:'Edit step',exact:true}).click();await page.locator('[name=title]').fill('Unsaved title');await page.getByRole('combobox',{name:'Status',exact:true}).click();
  await page.evaluate(()=>{data.goals[selected.thread].tokensUsed++;render()});
  assert.equal(await page.locator('[name=title]').inputValue(),'Unsaved title');assert.equal(await page.getByRole('combobox',{name:'Status',exact:true}).getAttribute('aria-expanded'),'true');assert(await page.locator('#editor').isVisible());
  console.log('PASS: incoming changes preserve node identity, selected text, open inspector/tab, unsaved edit and open choice menu.');
 }finally{await browser.close()}
})().catch(e=>{console.error(e);process.exitCode=1});

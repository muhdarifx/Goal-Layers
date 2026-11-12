// Real local API with isolated notes; never writes the user's saved graph.
const {chromium}=require('playwright');
const {spawn,execFileSync}=require('node:child_process');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const root=path.resolve(__dirname,'..'),base='http://127.0.0.1:4317';
const work=path.join(root,'work');fs.mkdirSync(work,{recursive:true});
const dir=fs.mkdtempSync(path.join(work,'api-test-')),notes=path.join(dir,'nodes.json');
const exe=process.env.GOAL_ATLAS_TEST_EXE||path.join(root,'target/release/goal-atlas.exe');
let server,browser;const errors=[];
function start(){server=spawn(exe,[],{cwd:root,env:{...process.env,GOAL_ATLAS_DATA:notes},windowsHide:true,stdio:'ignore'});}
function stop(){if(server){server.kill();server=null;}}
async function until(fn){for(let i=0;i<300;i++){const value=await fn().catch(()=>null);if(value)return value;await new Promise(r=>setTimeout(r,100));}throw Error('Local API did not become ready');}
async function catalog(){return(await fetch(base+'/api/catalog')).json();}
async function post(route,node,headers={}){return fetch(base+route,{method:'POST',headers:{'content-type':'application/json','x-goal-atlas':'1',...headers},body:JSON.stringify(node)});}
(async()=>{try{
 start();const c=await until(async()=>{const c=await catalog();return !c.scanning&&c.scanned>0&&!c.error?c:null;});
 const t=c.threads.find(t=>c.goals[t.id]&&t.projectId);assert(t);
 const seed={id:'api-fixture',thread_id:t.id,parent_id:null,title:'API fixture',status:'pending',lane:'aligned',evidence:'Isolated integration fixture',next:'',revision:0};
 assert.equal((await post('/api/nodes',seed)).status,409,'Manual API must reject new steps');
 assert.equal((await post('/api/reports',seed,{'origin':'https://example.com'})).status,403);
 let r=await post('/api/reports',seed);assert.equal(r.status,200);let saved=await r.json();
 assert.equal((await post('/api/nodes',{...saved,parent_id:'missing'})).status,409);
 assert.equal((await post('/api/nodes',{...saved,status:'complete',evidence:''})).status,409);
 r=await post('/api/nodes',{...saved,title:'Edited existing step'});assert.equal(r.status,200);saved=await r.json();
 assert.equal((await post('/api/nodes',seed)).status,409,'Stale writes must fail');
 const cli=execFileSync(exe,['report'],{input:JSON.stringify({...saved,next:'CLI report retained'}),encoding:'utf8',windowsHide:true});assert(cli.includes('CLI report retained'));
 assert.equal(await new Promise((resolve,reject)=>require('node:http').get(base+'/api/catalog',{headers:{Host:'evil.example'}},r=>{r.resume();resolve(r.statusCode)}).on('error',reject)),403);
 browser=await chromium.launch({executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
 const p=await browser.newPage();p.on('pageerror',e=>errors.push(e.message));await p.goto(base);await p.locator('[data-project="'+t.projectId+'"]').click();await p.locator('[data-thread="'+t.id+'"]').click();
 assert.equal(await p.locator('#add-root,[data-add]').count(),0);
 const live=await p.evaluate(()=>new Promise(resolve=>{const e=new EventSource('/events');e.onopen=()=>{e.close();resolve(true)};e.onerror=()=>{e.close();resolve(false)}}));assert(live);
 await browser.close();browser=null;stop();await until(async()=>{try{await fetch(base);return false}catch{return true}});start();
 await until(async()=>(await catalog()).nodes?.some(n=>n.id===seed.id&&n.next==='CLI report retained'));
 assert.deepEqual(errors,[]);console.log('PASS: manual creation rejected; existing edits, validation, agent API/CLI, events, restart persistence and origin protection retained');
 }finally{if(browser)await browser.close();stop();await new Promise(r=>setTimeout(r,150));for(const name of ['nodes.json','nodes.tmp']){const f=path.join(dir,name);if(fs.existsSync(f))fs.unlinkSync(f)}fs.rmdirSync(dir);}
})().catch(e=>{console.error(e);process.exitCode=1});

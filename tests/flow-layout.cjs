const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const source=fs.readFileSync(require('node:path').join(__dirname,'../web/flow.js'),'utf8');
const layout=source.slice(source.indexOf(' function treeEdges('),source.indexOf(' function visibleEdges('));
const context=vm.createContext({card:{width:220,height:90},positions:{},manual:{},dimensions:{}});
vm.runInContext(layout,context);
const nodes=[{id:'goal',kind:'goal',lane:'aligned'}],edges=[];
for(let i=0;i<8;i++){
 const id='parent'+i;nodes.push({id,kind:'Requirement',lane:'aligned'});edges.push({source:'goal',target:id});
 for(let j=0;j<6;j++){const child=id+'-'+j;nodes.push({id:child,kind:'Scope detail',lane:'aligned'});edges.push({source:id,target:child})}
}
nodes.push({id:'added',kind:'Scope detail',lane:'addition'});
context.nodes=nodes;context.edges=edges;
vm.runInContext('arrange(nodes,edges)',context);
assert.equal(Object.keys(context.positions).length,nodes.length);
for(const [i,a]of nodes.entries())for(const b of nodes.slice(i+1)){
 const p=context.positions[a.id],q=context.positions[b.id];
 assert(!(p.x<q.x+220&&p.x+220>q.x&&p.y<q.y+90&&p.y+90>q.y),`${a.id} overlaps ${b.id}`);
}
assert(context.dimensions.h<2500,'Wide branches should not be forced into a long two-column stack');
for(const n of nodes.filter(n=>n.kind!=='goal'))assert(n.lane==='aligned'?context.positions[n.id].x+220<context.dimensions.dividerX:context.positions[n.id].x>context.dimensions.dividerX);
const first=JSON.stringify(context.positions);vm.runInContext('arrange(nodes,edges)',context);assert.equal(JSON.stringify(context.positions),first);
context.manual={parent0:{x:123,y:456}};vm.runInContext('arrange(nodes,edges)',context);assert.equal(context.positions.parent0.x,123);assert.equal(context.positions.parent0.y,456);
console.log('PASS: compact layout, no overlaps, separate lanes, deterministic positions, manual positions retained');

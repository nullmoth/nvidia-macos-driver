const fs = require('fs');
const vm = require('vm');
const assert = require('assert');
const elements = new Map();
const sent = [];
function element(id) {
  if (!elements.has(id)) elements.set(id, { hidden: false, disabled: false, value: '', textContent: '', innerHTML: '', options: [], classList: { toggle() {} }, insertAdjacentHTML() {}, querySelectorAll() { return [] } });
  return elements.get(id);
}
const sandbox = {document: {getElementById: element, querySelectorAll() {return []}}, window: {webkit: {messageHandlers: {nm: {postMessage(m) {sent.push(m)}}}}}, confirm: () => true, setTimeout};
vm.createContext(sandbox);
vm.runInContext(fs.readFileSync(__dirname + '/../app/Resources/app.js', 'utf8'), sandbox);
const event = (event, data) => sandbox.window.NM.on({event, data});
event('upd', {state:'checked', newer:true, latest:'1.0.9', installed:'1.0.8'});
assert.equal(element('upddrv').disabled,false);
event('dl', {state:'start'});
for (const name of ['dl','updchk','upddrv']) assert.equal(element(name).disabled,true);
event('dl', {state:'error', why:'network failed'});
for (const name of ['dl','updchk','upddrv']) assert.equal(element(name).disabled,false);
event('dl', {state:'start'});
event('dl', {state:'done',path:'/tmp/driver.tar.gz'});
assert.equal(element('updchk').disabled,false);
assert.equal(sent.at(-1).act,'scan');
event('upd', {state:'checked', newer:false, latest:'1.0.8', installed:'1.0.8'});
assert.equal(element('upddrv').hidden,true);
assert.equal(element('upddrv').disabled,true);
console.log('Update download failure, retry, completion and version controls passed.');

const machine = (major) => ({version:'1.1', macos:String(major), major, arch:'x86_64', translated:false,
  gpus:[{vendor:'10DE', name:'NVIDIA RTX 5060', supported:true, tested:true}],
  kexts:4, metal:['NVIDIA RTX 5060'], opencore:'1.0.6', files:true,
  packages:[{ok:'yes',path:'/tmp/driver.tar.gz'}]});
// The Tahoe button (back in 2.0.4, now that Tahoe is released): offered on macOS 15 with the checked package only
event('scan',machine(15));
assert(element('verdict').innerHTML.includes('Check for a newer driver'));
assert(element('verdict').innerHTML.includes('Prepare this Mac for Tahoe'));
assert.equal(element('upd').disabled,false);
event('scan',machine(26));
assert(!element('verdict').innerHTML.includes('Prepare'));
assert.equal(element('upd').disabled,true);
assert(element('updt').innerHTML.includes('already runs macOS 26'));
const nopkg = machine(15); nopkg.packages = [];
event('scan',nopkg);
assert.equal(element('upd').disabled,true);
assert(element('updt').innerHTML.includes('Download the driver package'));
event('scan',machine(15));
element('upd').onclick();
assert.deepEqual({act:sent.at(-1).act, cancel:sent.at(-1).cancel, pkg:sent.at(-1).pkg}, {act:'osupdate', cancel:false, pkg:'/tmp/driver.tar.gz'});
element('updc').onclick();
assert.deepEqual({act:sent.at(-1).act, cancel:sent.at(-1).cancel}, {act:'osupdate', cancel:true});
event('run',{state:'start', mode:'tahoe'});
assert.equal(element('upd').disabled,true);
event('run',{state:'end', mode:'tahoe', ok:true});
assert.equal(element('swu').hidden,false);
element('swu').onclick();
assert.equal(sent.at(-1).act,'swupdate');
event('run',{state:'end', mode:'tahoe', ok:false});
assert(element('updt').textContent.includes('stopped'));
// A Mac whose only GPU OpenCore switched off has an empty GPU list (Core Ultra laptop, 2.0.3): it is pointed at the
// test driver, not told to look for an NVIDIA card; -wegnoegpu (which hides the NVIDIA card) keeps its own advice.
const bare = (extra) => Object.assign(machine(15), {gpus:[], kexts:0, metal:[], files:false, hwModel:'MacBookPro16,2', gpuHiding:[], hiddenNvidia:[]}, extra);
event('scan',bare({}));
assert(element('verdict').innerHTML.includes('sees no GPU at all'));
assert(!element('verdict').innerHTML.includes('No NVIDIA card was found'));
event('scan',bare({gpuHiding:['-wegnoigpu']}));
assert(element('verdict').innerHTML.includes('-wegnoigpu in boot-args'));
event('scan',bare({gpuHiding:['-wegnoegpu']}));
assert(!element('verdict').innerHTML.includes('sees no GPU at all'));
assert(element('verdict').innerHTML.includes('-wegnoegpu'));
console.log('A Mac with no visible GPU is offered the test driver; NVIDIA-hiding boot-args keep their advice.');
const html=fs.readFileSync(__dirname+'/../app/Resources/index.html','utf8');
for (const name of ['upd','swu','updc']) assert(html.includes('id="'+name+'"'));
const native=fs.readFileSync(__dirname+'/../app/Sources/main.swift','utf8');
assert(native.includes('case "osupdate":'));
assert(native.includes('case "swupdate":'));
assert(native.includes('extra: ["--update", "prepare"]'));
assert(native.includes('extra: ["--update", "cancel"]'));
assert(native.includes('"tahoe", "tahoecancel"].contains(mode)'));
assert(native.includes('AppActions.directRunMode(b["mode"])'));
console.log('Tahoe preparation: offered on macOS 15 with the package, refused on 26 and without it, prepare/cancel/Software Update wired.');

event('run',{state:'done', mode:'dry', ok:true});
element('efi').value='disk2s1';
element('efi').onchange();
assert.equal(element('go').disabled,true);
assert.equal(vm.runInContext('S.previewed',sandbox),false);
console.log('Changing the selected EFI requires a fresh preview before install.');

// The install button says why it is greyed out (10-13: "Install Driver is greyed out, what do I do").
const fresh = () => Object.assign(machine(15), {kexts:0, metal:[], files:false});
event('scan', fresh());
assert.equal(element('go').disabled, true);
assert(element('go').textContent.includes('Show me the changes first'), element('go').textContent);
event('run', {state:'start', mode:'dry'});
event('run', {state:'done', mode:'dry', ok:true});
event('scan', fresh());
assert.equal(element('go').disabled, false);
assert.equal(element('go').textContent, 'Install the driver');
console.log('Install button names the step that comes first, and reads plainly once it is ready.');

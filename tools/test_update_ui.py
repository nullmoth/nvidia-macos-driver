#!/usr/bin/env python3
"""Replay driver update outcomes through the production web view event handler."""
import pathlib
import shutil
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
node = shutil.which('node')
if not node:
    raise SystemExit('Node.js is required for web view event regression tests')
fixture = r'''
const fs = require('fs'), vm = require('vm'), assert = require('assert');
const elements = new Map(), messages = [];
function element(id) {
  if (!elements.has(id)) elements.set(id, {textContent:'', innerHTML:'', disabled:false,
    hidden:false, value:'auto', options:[], classList:{toggle(){}},
    insertAdjacentHTML(_where, text){this.innerHTML += text;}, querySelectorAll(){return [];}});
  return elements.get(id);
}
const context = {document:{getElementById:element,querySelectorAll(){return [];}},
  window:{webkit:{messageHandlers:{nm:{postMessage(m){messages.push(m);}}}}},setTimeout};
vm.createContext(context);
vm.runInContext(fs.readFileSync(process.argv[2], 'utf8'), context);
const emit = (event,data) => context.window.NM.on({event,data});
const click = () => element('upddrv').onclick();
click(); assert.equal(messages.at(-1).act,'updateDriver');
assert(element('upddrv').disabled);
emit('dl',{state:'start'});
emit('dl',{state:'done'});
assert.match(element('updr').textContent,/Installing/);
assert(element('upddrv').disabled);
emit('run',{state:'start',mode:'install'});
emit('run',{state:'end',mode:'install',ok:true});
assert.match(element('updr').textContent,/Driver installed.*Restart/);
assert(element('upddrv').disabled && element('upddrv').hidden);
click(); emit('dl',{state:'error',why:'Checksum mismatch <unsafe>'});
assert.match(element('updr').innerHTML,/Checksum mismatch &lt;unsafe&gt;/);
emit('run',{state:'end',mode:'install',ok:true});
assert.match(element('updr').innerHTML,/Checksum mismatch/);
click(); emit('dl',{state:'done'}); emit('run',{state:'end',mode:'install',ok:false});
assert.match(element('updr').textContent,/stopped.*setup log/);
assert(!element('upddrv').disabled);
click(); emit('dl',{state:'done'}); emit('run',{state:'cancelled',mode:'install'});
assert.match(element('updr').textContent,/cancelled/);
assert(!element('upddrv').disabled);
emit('upd',{state:'checked',installed:'1.14.7',latest:'1.14.7',newer:false});
assert(element('upddrv').disabled && element('upddrv').hidden);
emit('upd',{state:'checked',installed:'1.14.7',latest:'1.14.8',newer:true});
assert(!element('upddrv').disabled && !element('upddrv').hidden);
emit('scan',{version:'1.14.4',installedDriverVersion:'1.14.7',macos:'15.8.1',major:15,
  arch:'x86_64',translated:false,gpus:[{vendor:'10DE',supported:true,name:'RTX 5060'}],
  kexts:4,metal:['NVIDIA GeForce RTX 5060'],files:true,opencore:'REL-109',packages:[]});
assert.match(element('mac').innerHTML,/Installed driver package.*1\.14\.7/);
assert.equal(element('ver').textContent,'1.14.4');
console.log('Update event regressions passed: success, checksum refusal, setup failure, cancellation, recheck.');
'''
with tempfile.TemporaryDirectory() as directory:
    script = pathlib.Path(directory) / 'update.js'
    script.write_text(fixture)
    subprocess.run([node, str(script), str(root/'app/Resources/app.js')], check=True, timeout=30)

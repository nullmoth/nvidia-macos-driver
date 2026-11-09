#!/usr/bin/env python3
"""A sampler bind does not allocate a key string. The pixel-sampler tables are keyed by the slot number as text, and the
key was built with [@(i) stringValue] on every setFragmentSamplerState / setVertexSamplerState (and their lod-clamp
forms): an NSNumber and an NSString per bind. Arms: the source builds no key string inside the four bind methods; the
production nvmtl_sampler_slot_key returns the same string as before for every slot and the same object each time."""
import pathlib, re, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLObjects.m').read_text()
fails = 0
def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else '')); fails += not ok

for sel in ('setFragmentSamplerState:(id<MTLSamplerState>)s atIndex', 'setVertexSamplerState:(id<MTLSamplerState>)s atIndex',
            'setFragmentSamplerState:(id<MTLSamplerState>)ss lodMinClamp', 'setVertexSamplerState:(id<MTLSamplerState>)ss lodMinClamp'):
    at = src.index('- (void)' + sel)
    body = src[at:src.index('\n}\n', at)]
    check(f'{sel.split(":")[0]} {"lod-clamp form" if "lod" in sel else ""} builds no key string per bind'.replace('  ', ' '),
          'stringValue' not in body and 'stringWithFormat' not in body)

m = re.search(r'static NSString \*nvmtl_sampler_slot_key\(NSUInteger i\) \{.*?\n\}\n', src, re.S)
if not m:
    check('the slot key comes from a table made once', False, 'nvmtl_sampler_slot_key missing')
else:
    h = '#import <Foundation/Foundation.h>\n' + m.group(0) + r'''
int main(void) { int bad = 0, same = 1;
  for (NSUInteger i = 0; i < 40; i++) { NSString *a = nvmtl_sampler_slot_key(i);
    if (![a isEqualToString:[@(i) stringValue]]) bad++;
    if (i < 16 && nvmtl_sampler_slot_key(i) != a) same = 0; }
  printf("%d %d\n", bad, same); return 0; }
'''
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d); (d / 'k.m').write_text(h)
        r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-o', str(d / 'k'), str(d / 'k.m'), '-framework', 'Foundation'], capture_output=True, text=True)
        if r.returncode: print(r.stderr[-800:]); sys.exit('could not compile - UNKNOWN')
        bad, same = map(int, subprocess.run([str(d / 'k')], capture_output=True, text=True).stdout.split())
        check('every slot key equals the old [@(i) stringValue] text', bad == 0, f'{bad} differ')
        check('slots 0-15 return the same string object each time', same == 1)
sys.exit(1 if fails else 0)

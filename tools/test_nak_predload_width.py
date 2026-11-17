#!/usr/bin/env python3
"""Check hardware legality at the predicated-load optimization boundary.

Executes the predicate embedded in the production patch. Includes the
12-byte render reproducer, under-aligned loads, and all supported LDG widths.
Actual pipeline compilation is tested separately against a built NVK library.
"""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
patch = (ROOT / 'nvk/nvk-predload-width.patch').read_text()
source = '\n'.join(line[1:] for line in patch.splitlines()
                   if line.startswith('+') and not line.startswith('+++'))
start = source.index('static bool\nnak_cg_predload_width_supported(')
end = source.index('\n}', start) + 2
helper = source[start:end]
main = r'''
#include <assert.h>
int main(void) {
   /* An xyz-only read shrunk from vec4 must retain normal legalization. */
   assert(!nak_cg_predload_width_supported(3, 32, 16));
   assert(!nak_cg_predload_width_supported(3, 16, 8));
   assert(!nak_cg_predload_width_supported(3, 8, 4));
   assert(!nak_cg_predload_width_supported(4, 64, 32));
   /* Hardware supports 1, 2, 4, 8, 16 bytes, each naturally aligned. */
   for (unsigned bytes = 1; bytes <= 16; bytes *= 2) {
      unsigned bits = bytes < 4 ? bytes * 8 : 32;
      unsigned lanes = bytes < 4 ? 1 : bytes / 4;
      assert(nak_cg_predload_width_supported(lanes, bits, bytes));
      assert(nak_cg_predload_width_supported(lanes, bits, 64));
      if (bytes > 1)
         assert(!nak_cg_predload_width_supported(lanes, bits, bytes / 2));
   }
   return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='nvmtl-predload-') as tmp:
    tmp = Path(tmp)
    c = tmp / 'test.c'
    c.write_text('#include <stdbool.h>\n' + helper + main)
    subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror', str(c), '-o', str(tmp / 'test')], check=True)
    subprocess.run([str(tmp / 'test')], check=True)
assert 'if (!nak_cg_predload_width_supported(intr->def.num_components,' in source
assert 'nir_intrinsic_align(intr)))\n             return -1;' in patch
chain = next(line for line in (ROOT / 'build/build_nvk.sh').read_text().splitlines()
             if 'CHAIN=' in line and 'nvk-stream-uncached' in line)
assert 'nvk-predload-width' in chain
print('predicated loads retain hardware-supported widths and alignment')

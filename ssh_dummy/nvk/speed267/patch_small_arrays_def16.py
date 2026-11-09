#!/usr/bin/env python3
"""patch_small_arrays_def16.py <nak compiler dir> - per-thread arrays of up to 16 elements are lowered to if-else trees
(registers) by default instead of local memory. NAK_B83_ARRAYS=8|16|32 changes the limit, NVMTL_NO_SMALL_ARRAYS=1 turns
it off. Exact anchors, asserted counts, atomic writes."""
import os
import sys

d = sys.argv[1]


def patch(name, old, new):
    p = os.path.join(d, name)
    s = open(p).read()
    assert s.count(old) == 1, (name, s.count(old))
    open(p + '.new', 'w').write(s.replace(old, new))
    os.replace(p + '.new', p)


patch('nvmtl_raw_policy.h',
      '    if (!value || getenv("NVMTL_NO_SMALL_ARRAYS")) return 0;\n',
      '    if (getenv("NVMTL_NO_SMALL_ARRAYS")) return 0;\n'
      '    /* Default 16: per-thread arrays of up to 16 elements are kept in registers (if-else trees) instead of local\n'
      '     * memory. Measured over 249 GB7/MPS kernels: 11 change, all lose local loads/stores, none loses occupancy.\n'
      "     * 32 also takes a ray tracer's BVH stack and drops it from 36 to 16 warps per SM, so 32 stays opt-in. */\n"
      '    if (!value) return 16;\n')
patch('nak_nir.c',
      "   /* R3: restore b83's pre-scratch lowering as an isolated experiment.\n"
      '    * Same existing NIR pass, bounded arrays; OFF without explicit permission. */\n',
      '   /* Small per-thread arrays (up to 16 elements by default, NAK_B83_ARRAYS=8|16|32 to change, NVMTL_NO_SMALL_ARRAYS\n'
      '    * to turn off) are lowered to if-else trees so they live in registers rather than local memory. */\n')
print('patched')

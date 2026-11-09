#!/usr/bin/env python3
"""AGDC's maxFramebuffers can cover slots that are not started yet (nvagdcheads=1), so a hot-plugged display's framebuffer is accepted.

The helper is lifted from nvrm-agdc.cpp. Control: rel-29 reports exactly the started count (1 with one display at boot), which is
the 'maxfb=1' of issue 146."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMAGDC/nvrm-agdc.cpp').read_text()
assert 'PE_parse_boot_argn("nvagdcheads"' in src and 'nvrmAgdcMaxFramebuffers(c->maxFramebuffers, heads, kAgdcMaxFB)' in src
a = src.index('static inline unsigned nvrmAgdcMaxFramebuffers'); fn = src[a:src.index('\n}\n', a) + 3]
fixture = '#include <cassert>\n#include <cstdio>\n' + fn + r'''
int main() {
    assert(nvrmAgdcMaxFramebuffers(1, 4, 4) == 4);     // one display at boot, four head slots: 4
    assert(nvrmAgdcMaxFramebuffers(3, 4, 4) == 4);
    assert(nvrmAgdcMaxFramebuffers(2, 1, 4) == 2);     // never below what is started
    assert(nvrmAgdcMaxFramebuffers(1, 12, 4) == 4);    // never above the table
    assert(nvrmAgdcMaxFramebuffers(1, 0, 4) == 1 && nvrmAgdcMaxFramebuffers(1, -3, 4) == 1);
    puts("PASS: maxFramebuffers covers the published head slots, bounded by the started count and the table");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)

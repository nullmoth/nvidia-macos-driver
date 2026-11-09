#!/usr/bin/env python3
"""Driver off on a Resizable BAR card: the firmware screen is shown from the repaired BAR1 (switch nvoffbar=1, default off).

Why the branch that tried this hung the test box at boot: its probe() ran, on the matching thread, a recursive registry walk,
an AML _CRS evaluation and PCI bridge window rewrites. Nothing here blocks matching: NVRM does the placement in start() (the
same call, the same context, as every driver start), stays attached so systemWillShutdown() puts the layout back, and asks for
matching again; NVRMBootFB's probe only reads two properties. The detection helper is compiled from nvrm_off_bar.h."""
import filecmp, pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
assert filecmp.cmp(root / 'kexts/NVRM/nvrm_off_bar.h', root / 'kexts/NVRMFB/nvrm_off_bar.h', shallow=False)
rm = (root / 'kexts/NVRM/NVRM.cpp').read_text(); fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['PE_parse_boot_argn("nvoffbar"', 'nvrmOffBarWanted(provider) && super::start(provider)', 'if (placed) return true;', 'registerService(kIOServiceAsynchronous)']:
    assert need in rm, need
for need in ['PE_parse_boot_argn("nvoffbar"', 'nvrm-bar1-placed', 'kPEBaseAddressChange']:
    assert need in fb, need
# the probe path must not do what hung: no registry iteration, no AML, no config writes inside NVRMBootFB::probe
a = fb.index('class NVRMBootFB'); probe = fb[a:fb.index('void free() override', a)]
for bad in ['IORegistryIterator', 'evaluateObject', 'configWrite', 'extendedConfigWrite', 'IOSleep']:
    assert bad not in probe, 'blocking or writing call in NVRMBootFB::probe: ' + bad
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm_off_bar.h"
int main() {
    // healthy card: BAR1 (0x14/0x18) 64-bit prefetchable at 0xa000000000, backed by a 256 MB aperture
    NvrmApertureSpan ok[2] = { { 0x80000000ull, 0x1000000 }, { 0xa000000000ull, 0x10000000 } };
    assert(!nvrmBar64Lost(0x0000000c, 0xa0, ok, 2));
    // lost: the nub knows only a 16-byte stray where BAR1's range was
    NvrmApertureSpan lost[2] = { { 0x80000000ull, 0x1000000 }, { 0xa000000000ull, 16 } };
    assert(nvrmBar64Lost(0x0000000c, 0xa0, lost, 2));
    NvrmApertureSpan none[1] = { { 0x80000000ull, 0x1000000 } };
    assert(nvrmBar64Lost(0x0000000c, 0xa0, none, 1));                      // no aperture at that address at all
    assert(nvrmBar64Lost(0x0000000c, 0x0, none, 1));                       // a 64-bit BAR with no address
    assert(!nvrmBar64Lost(0x00000008, 0xa0, none, 1) && !nvrmBar64Lost(0x1, 0, none, 1));   // 32-bit and I/O BARs are not this failure
    puts("PASS: lost 64-bit BAR1 told from a backed one, 32-bit/I/O BARs ignored");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRM',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)

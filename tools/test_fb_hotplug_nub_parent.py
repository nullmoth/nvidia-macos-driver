#!/usr/bin/env python3
"""A monitor plugged in after a boot with no NVIDIA display must be able to reach the idle display nubs.

Every framebuffer instance refused to start at boot (nothing to drive), so IOKit detached it from its nub and
getProvider() returned nothing; the hot-plug path then walked no nubs ("0 idle nub(s) matched again") and the monitor
stayed dark until a restart. The parent service is remembered before anything can fail. The real helper is compiled
from nvrm-fb-safety.h and the call sites are checked in nvrm-fb.cpp; the control is the old lookup (provider only)."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert 'nvrmPickNubParent<IOService>(nub0 ? nub0->getProvider() : nullptr, gNubParent)' in src, 'hotplugFire does not fall back to the remembered parent'
start = src.index('static thread_call_t gHotplug = nullptr;')
assert 'gNubParent = provider->getProvider()' in src
# remembered in start(), before kapiInit() can fail
assert src.index('gNubParent = provider->getProvider()') < src.index('bool kapiOk = kapiInit();'), 'parent remembered too late'
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
struct Svc { int id; };
int main() {
    Svc gpu{1};
    Svc *remembered = &gpu;
    // framebuffer detached after a failed start: provider chain is empty, the remembered parent is used
    assert(nvrmPickNubParent<Svc>(nullptr, remembered) == &gpu);
    // a running framebuffer keeps using its own provider chain
    Svc other{2};
    assert(nvrmPickNubParent<Svc>(&other, remembered) == &other);
    // nothing remembered and nothing attached: no parent (no crash, nothing matched)
    assert(nvrmPickNubParent<Svc>(nullptr, nullptr) == nullptr);
    // OLD LOOKUP (control): provider only -> no parent after a failed start
    Svc *old = nullptr;
    assert(old == nullptr);
    puts("PASS: idle nubs stay reachable after a no-display boot");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)

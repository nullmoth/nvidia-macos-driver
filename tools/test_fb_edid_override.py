#!/usr/bin/env python3
"""A replacement EDID is used only when it is trustworthy and belongs to the display; every display query carries it.

Source of the idea: smaller boot rasters and EDID overrides (pull request 468, credit in NOTICE). Arms: the file name
is built from the display's own ids; a valid file passes; bad checksum, wrong header, truncated, wrong block count,
another monitor's ids are refused; a file writable by others or not owned by root is not trusted; nvfbsmall follows its
boot-arg over the file. The control is the old behaviour: no override path existed, so the call sites used NVKMS's own EDID."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert src.count('nvrmDynamicDisplayInfo(') >= 3, 'the boot and hotplug queries no longer carry the override'
assert 'if (!fKms->getDynamicDisplayInfo(fDev, dd)) {' not in src, 'boot enumeration bypasses the override'
assert 'bool dynOk = me->fKms->getDynamicDisplayInfo' not in src, 'hotplug bypasses the override'
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include "nvrm-fb-safety.h"
static void fix(unsigned char *e, unsigned n) {
    for (unsigned b = 0; b < n; b += 128) { unsigned char s = 0; for (unsigned i = 0; i < 127; i++) s += e[b+i]; e[b+127] = (unsigned char)(0 - s); }
}
static void make(unsigned char *e, unsigned blocks, unsigned char v0, unsigned char v1, unsigned char p0, unsigned char p1) {
    memset(e, 0, 128 * blocks);
    static const unsigned char h[8] = {0,0xff,0xff,0xff,0xff,0xff,0xff,0};
    memcpy(e, h, 8); e[8]=v0; e[9]=v1; e[10]=p0; e[11]=p1; e[126]=(unsigned char)(blocks-1); fix(e, 128*blocks);
}
int main() {
    unsigned char real[256], ov[256], bad[256];
    make(real, 2, 0x1e, 0x6d, 0x66, 0x77);
    char name[24]; nvrmEdidOverrideName(real, name, sizeof name);
    assert(!strcmp(name, "1E6D-7766.bin"));
    make(ov, 2, 0x1e, 0x6d, 0x66, 0x77);
    assert(nvrmEdidValid(ov, 256) && nvrmEdidSameDisplay(ov, real));
    memcpy(bad, ov, 256); bad[40] ^= 1;               assert(!nvrmEdidValid(bad, 256));     // bad checksum
    memcpy(bad, ov, 256); bad[1] = 0;                 assert(!nvrmEdidValid(bad, 256));     // bad header
    assert(!nvrmEdidValid(ov, 200) && !nvrmEdidValid(ov, 0) && !nvrmEdidValid(nullptr, 128)); // truncated
    memcpy(bad, ov, 256); bad[126] = 0; fix(bad, 256); assert(!nvrmEdidValid(bad, 256));   // block count says 1
    unsigned char other[128]; make(other, 1, 0x10, 0xac, 0x01, 0x02);
    assert(nvrmEdidValid(other, 128) && !nvrmEdidSameDisplay(other, real));                  // another monitor's file
    assert(nvrmFileTrusted(0, 0644) && nvrmFileTrusted(0, 0600));
    assert(!nvrmFileTrusted(0, 0664) && !nvrmFileTrusted(0, 0646) && !nvrmFileTrusted(501, 0644));
    assert(!nvrmSmallRastersWanted(false, 0, false) && nvrmSmallRastersWanted(false, 0, true));
    assert(nvrmSmallRastersWanted(true, 1, false) && !nvrmSmallRastersWanted(true, 0, true));  // boot-arg beats the file
    puts("PASS: override name from the display ids, only a valid same-display EDID accepted, untrusted files refused, nvfbsmall precedence");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)

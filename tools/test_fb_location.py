#!/usr/bin/env python3
"""Two framebuffer instances must not share one registry path.

Both heads were NVRMFramebuffer with no location, so two identical monitors produced the same display0 path and
macOS merged their colour profiles and identity. Instance 0 keeps its unnumbered path (existing single-display
preferences stay valid); the others get @<index>. The real helper is compiled from nvrm-fb-safety.h; the control is
the old behaviour (no location for any instance)."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert 'nvrmFbLocation(fIndex, loc, sizeof loc)) setLocation(loc)' in src, 'start() no longer gives each head its own location'
# start() has two super::start calls: the proxy head for another GPU's panel returns from its own, earlier branch; every
# NVIDIA head goes through the one after setLocation.
name = src.index('setName("NVRMFramebuffer")'); loc = src.index('setLocation(loc)', name)
starts = []
i = name
while len(starts) < 2:
    i = src.find('if (!super::start(provider))', i + 1)
    if i < 0: break
    starts.append(i)
assert len(starts) == 2 and 'proxy: super::start failed' in src[starts[0]:starts[0] + 120], 'start() changed shape: read it and update this test'
assert starts[0] < loc < starts[1], 'location set after super::start'
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>
#include "nvrm-fb-safety.h"
static std::string path(unsigned idx) {
    char loc[12]; std::string p = "NVRMFramebuffer";
    if (nvrmFbLocation(idx, loc, sizeof loc)) p += std::string("@") + loc;
    return p + "/display0";
}
int main() {
    assert(path(0) == "NVRMFramebuffer/display0");      // single-display preferences keep their path
    assert(path(1) == "NVRMFramebuffer@1/display0");
    assert(path(1) != path(2) && path(0) != path(1));    // two identical monitors no longer collide
    assert(path(12) == "NVRMFramebuffer@12/display0");
    char tiny[4]; assert(!nvrmFbLocation(1, tiny, sizeof tiny) && !nvrmFbLocation(1, nullptr, 12));
    // OLD behaviour (negative control): no location at all makes every head the same path
    std::string old0 = "NVRMFramebuffer/display0", old1 = "NVRMFramebuffer/display0";
    assert(old0 == old1);
    puts("PASS: head 0 keeps its path, every other head gets its own, no collision between heads");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)

#!/usr/bin/env python3
"""Check the backend's CPU-map request against the Turing RM mapping contract."""
import re, subprocess, tempfile
from pathlib import Path

root=Path(__file__).resolve().parents[1]
patch=(root/'nvk/nvk-macos.patch').read_text()
needle='   const NvU32 map_flags_os33 = '
start=patch.index('+'+needle)
lines=patch[start:].splitlines()[:2]
production='\n'.join(line[1:] for line in lines)
assert 'nvRmApiMapMemory(' in production
source=r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
using NvU32=uint32_t; using NV_STATUS=uint32_t;
struct Memory { uint32_t hMemoryPhys; struct { uint64_t size_B; } base; bool isSystemMem; };
struct Device { uint32_t hSubdevice; };
struct Mapping {};
struct Api {};
static bool gpuCached, turing;
static uint32_t observed;
static NV_STATUS nvRmApiMapMemory(Api*,uint32_t,uint32_t,uint64_t,uint64_t,bool sys,uint32_t flags,Mapping*) {
    observed=flags;
    // NVOS33_FLAGS_MAPPING is bits 16:15; DIRECT is 1. GM107 rejects
    // GPU-cached system memory in AUTO mode when reflected access is disabled.
    const uint32_t mode=(flags>>15)&3u;
    if (turing && sys && gpuCached && mode!=1u) return 0x56;
    return 0;
}
static NV_STATUS map(Memory *mem,Device *pdev) {
    Api rm; Mapping object; Mapping *mapping=&object;
PRODUCTION
    return nvRes;
}
int main() {
    Memory system={4,{65536},true}, video={5,{65536},false}; Device dev={7};
    Api rm; Mapping mapping;
    turing=true;gpuCached=true;
    assert(nvRmApiMapMemory(&rm,7,4,0,65536,true,0,&mapping)==0x56);
    assert(map(&system,&dev)==0);assert(((observed>>15)&3u)==1);
    assert(map(&video,&dev)==0);assert(observed==0);
    gpuCached=false;assert(map(&system,&dev)==0);
    turing=false;gpuCached=true;assert(map(&system,&dev)==0);
    puts("Turing cached system-memory failure reproduced; direct-map request accepted; video-memory and later-generation controls passed.");
}
'''.replace('PRODUCTION',production)
with tempfile.TemporaryDirectory() as d:
    p=Path(d)/'mapping.cpp';p.write_text(source);binary=Path(d)/'mapping'
    subprocess.run(['xcrun','clang++','-std=c++17',str(p),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)

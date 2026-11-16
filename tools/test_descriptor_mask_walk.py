#!/usr/bin/env python3
"""Compare actual descriptor writes from sparse and full production walks."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'tools/rotation'))
from test_support import method

source = (root / 'plugin/nvmtl_vk.c').read_text()
def section(start, end):
    first = source.index(start)
    return source[first:source.index(end, first)]

snapshot = method(source, 'static VkDescriptorSet nvmtl_snapshot_set_md(nvk_cmdbuf *c, uint32_t which, uint32_t stex_hi, const uint64_t *mask, const uint64_t *tb,\n                                             VkDescriptorSet from, const uint64_t *copym, const VkDescriptorBufferInfo *uniform)\n{')
# Allocation is unchanged. Exercise the complete descriptor construction and
# update call with deterministic handles, capturing every pointed-to value.
body = snapshot[snapshot.index('    VkWriteDescriptorSet w['):]
macros = section('#define NVMTL_NBUF ', '#define NVMTL_DISPATCH_PC_BYTES')
slot = re.search(r'typedef struct \{ uint8_t kind;.*?\} nvmtl_slot;', source, re.S).group(0)
options = section('static int g_snap_walk;', 'static VkDescriptorSet nvmtl_snapshot_set_md(')
fixture = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
#define nvlog(...) ((void)0)
@@MACROS@@
enum { NVMTL_SLOT_NONE,NVMTL_SLOT_BUF,NVMTL_SLOT_TEX,NVMTL_SLOT_SAMP,NVMTL_SLOT_TEXBUF };
@@SLOT@@
typedef struct { nvmtl_slot s[2][NVMTL_NBIND]; } nvmtl_bindtable;
static unsigned g_mutable=1,dynamic=1;
static nvk_buffer g_dummyBuf={.buf=(void *)0x10000};
static nvk_image g_dummyImg={.view=(void *)0x11000},g_dummyStex={.view=(void *)0x12000};
static VkSampler g_dummySampler=(VkSampler)0x13000;
static VkBufferView g_dummyTexView=(VkBufferView)0x14000;
static VkDevice g_dev;
#define NVMTL_E78_HAS(m,i) (!(m) || (((m)[(i)>>6] >> ((i)&63))&1))
#define NVMTL_COPY_HAS(m,i) ((m) && (((m)[(i)>>6] >> ((i)&63))&1))
#define E78C_WRITES 0
static void nvmtl_enc78_count(unsigned key,unsigned n) {}
static int nvmtl_table_ubo(void) { return dynamic; }
static int nvmtl_address_snapshot(nvk_cmdbuf *c,unsigned which,VkDescriptorBufferInfo *out) {
    *out=(VkDescriptorBufferInfo){(VkBuffer)0x15000,256+which*256,1024};return 0;
}
static const nvmtl_slot *nvmtl_slot_checked(const nvmtl_slot *s,unsigned stage,unsigned i) { return s; }
@@BINDING_TYPE@@
@@OPTIONS@@
typedef struct { unsigned binding,type; uint64_t h0,h1,offset,range,layout; } write;
typedef struct { unsigned count,copies; write w[NVMTL_NBIND+1]; VkCopyDescriptorSet cp[NVMTL_NBIND]; } result;
static result current;
static void pvkUpdateDescriptorSets(VkDevice device,unsigned n,const VkWriteDescriptorSet *w,unsigned nc,const VkCopyDescriptorSet *cp) {
    memset(&current,0,sizeof current);current.count=n;current.copies=nc;
    assert(n<=NVMTL_NBIND+1 && nc<=NVMTL_NBIND);
    memcpy(current.cp,cp,nc*sizeof *cp);
    for(unsigned i=0;i<n;i++) {
        write *out=&current.w[i];out->binding=w[i].dstBinding;out->type=w[i].descriptorType;
        assert(w[i].descriptorCount==1 && w[i].dstSet==(VkDescriptorSet)0x16000);
        if(w[i].pBufferInfo) { out->h0=(uintptr_t)w[i].pBufferInfo->buffer;out->offset=w[i].pBufferInfo->offset;out->range=w[i].pBufferInfo->range; }
        if(w[i].pImageInfo) { out->h0=(uintptr_t)w[i].pImageInfo->imageView;out->h1=(uintptr_t)w[i].pImageInfo->sampler;out->layout=w[i].pImageInfo->imageLayout; }
        if(w[i].pTexelBufferView) out->h0=(uintptr_t)*w[i].pTexelBufferView;
    }
}
static VkDescriptorSet snapshot(nvk_cmdbuf *c,unsigned which,unsigned stex_hi,const uint64_t *mask,const uint64_t *tb,
                               VkDescriptorSet from,const uint64_t *copym,const VkDescriptorBufferInfo *uniform) {
    VkDescriptorSet set=(VkDescriptorSet)0x16000;
@@BODY@@
static uint32_t rng=0x31f27;
static uint32_t random32(void) { rng^=rng<<13;rng^=rng>>17;rng^=rng<<5;return rng; }
int main(void) {
    nvmtl_bindtable table={0};nvk_cmdbuf cb={.table=&table};
    pthread_once(&g_snap_walk_once,nvmtl_snap_walk_options);
    for(unsigned round=0;round<4000;round++) {
        unsigned stage=round&1;g_mutable=(round>>1)&1;dynamic=(round>>2)&1;
        uint64_t mask[4]={0},copies[NVMTL_BINDING_WORDS],texel[4];
        for(unsigned w=0;w<4;w++) texel[w]=((uint64_t)random32()<<32)|random32();
        for(unsigned w=0;w<NVMTL_BINDING_WORDS;w++) copies[w]=((uint64_t)random32()<<32)|random32();
        for(unsigned i=0;i<NVMTL_NBIND;i++) {
            table.s[stage][i]=(nvmtl_slot){.kind=random32()%5,.h=random32()%3?(void *)(uintptr_t)(0x20000+i*256):NULL,.off=random32()%32};
            if(i<NVMTL_STEX_BASE && random32()%7==0) mask[i>>6]|=1ull<<(i&63);
        }
        if(round%7==0) memset(mask,0xff,sizeof mask);
        if(round%11==0) memset(mask,0,sizeof mask);
        // Always exercise the final named bit and mask word boundaries.
        if(round%5==0) { mask[0]|=1ull<<63;mask[1]|=1;mask[NVMTL_ADDRESS_BINDING>>6]|=1ull<<(NVMTL_ADDRESS_BINDING&63); }
        const uint64_t *m=round%13?mask:NULL,*tb=round%3?texel:NULL;
        const unsigned hi=round%(NVMTL_NSTEX+7);
        VkDescriptorBufferInfo uniform={(VkBuffer)0x17000,0,64};
        const VkDescriptorBufferInfo *u=round%3?&uniform:NULL;
        VkDescriptorSet from=round%2?(VkDescriptorSet)0x18000:VK_NULL_HANDLE;
        cb.table=round%17?&table:NULL;
        g_snap_walk=0;snapshot(&cb,stage,hi,m,tb,from,copies,u);result expected=current;
        g_snap_walk=1;snapshot(&cb,stage,hi,m,tb,from,copies,u);
        assert(!memcmp(&expected,&current,sizeof current));
    }
    puts("PASS: 4000 full/sparse descriptor comparisons, stage/word boundaries, null/empty/dense masks, address tables, copies, dummies, texel/storage views and GL uniforms");
}
'''
for key, value in [('MACROS', macros), ('SLOT', slot), ('OPTIONS', options),
                   ('BINDING_TYPE', method(source, 'static VkDescriptorType binding_type(')), ('BODY', body)]:
    fixture = fixture.replace('@@' + key + '@@', value)
with tempfile.TemporaryDirectory(prefix='nvmtl-descriptor-walk-') as directory:
    p = Path(directory)
    (p / 'test.c').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-O2', '-Werror', '-fsanitize=address,undefined',
                    '-fno-sanitize-recover=all', '-I' + str(root / 'plugin'),
                    '-I' + str(Path.home() / 'Vulkan-Headers/include'), str(p / 'test.c'), '-o', str(p / 'test')], check=True)
    subprocess.run([str(p / 'test')], check=True, timeout=30)

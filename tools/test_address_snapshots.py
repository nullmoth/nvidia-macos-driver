#!/usr/bin/env python3
"""Exercise production address snapshots and texture invalidation under ASan/UBSan."""
import pathlib
import os
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'plugin/nvmtl_vk.c').read_text()

def section(start, end):
    at = src.index(start)
    return src[at:src.index(end, at)]

macros = section('#define NVMTL_NBUF ', '#define NVMTL_DISPATCH_PC_BYTES')
slot = re.search(r'typedef struct \{ uint8_t kind;.*?\} nvmtl_slot;', src, re.S).group(0)
table = re.search(r'typedef struct \{ nvmtl_slot s\[.*?\} nvmtl_bindtable;', src, re.S).group(0)
page = section('#define NVMTL_TABLES_PER_PAGE', '\n#include "nvmtl_pool_cost.inc"')
bindings = section('static int g_addr_tex_inc', '\nint nvmtl_vk_emb_same(')
view = section('int nvmtl_vk_bind_texture_view(', '\nint nvmtl_vk_bind_storage_view(')
snapshot = section('static int nvmtl_address_snapshot(', '\nstatic void nvmtl_spirv_retarget_set(')
fixture = r'''
#include <assert.h>
#include <stdint.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
''' + macros + r'''
#define NVMTL_EMB_KEYMAX 480
enum { NVMTL_SLOT_NONE, NVMTL_SLOT_BUF, NVMTL_SLOT_TEX, NVMTL_SLOT_SAMP, NVMTL_SLOT_TEXBUF };
''' + re.search(r'typedef struct \{ void \*mem;.*?\} hz_ent;', src).group(0) + '\n#include "nvmtl_page_life.h"\n' + '\n'.join((slot, table, page)) + r'''
static nvmtl_pl_state g_pl; static pthread_mutex_t g_pl_lock=PTHREAD_MUTEX_INITIALIZER;
static nvk_buffer g_dummyBuf={.buf=(void *)0x10000};
static unsigned dummy_queries, page_queries, allocations;
static int fail_alloc;
static nvmtl_address_page *parked;
static void nvlog(const char *fmt, ...) { (void)fmt; }
static int nvmtl_handle_plausible(const void *h) { return (uintptr_t)h>=0x10000 && !((uintptr_t)h&7); }
uint32_t nvmtl_bl_slot(void *v) { return (uint32_t)((uintptr_t)v/8); }
uint64_t nvmtl_vk_buffer_address(nvk_buffer *b) {
    if(b==&g_dummyBuf) dummy_queries++; else page_queries++;
    return b->buf ? (uint64_t)(uintptr_t)b->buf+0x100000000ull : 0;
}
int nvmtl_vk_buffer_create(size_t size,int host,nvk_buffer *out) {
    (void)host; if(fail_alloc) return -1;
    out->map=calloc(1,size); out->buf=out->map; out->size=size; allocations++;
    return out->map?0:-1;
}
void nvmtl_vk_buffer_destroy(nvk_buffer *b) { free(b->map); memset(b,0,sizeof *b); }
static nvmtl_address_page *nvmtl_keep_page_pop(void) { nvmtl_address_page *p=parked; parked=NULL; return p; }
''' + r'''
static void nvmtl_hz_slot_bound(nvk_cmdbuf *c,nvmtl_bindtable *t,uint32_t set,uint32_t binding,uint32_t kind,const void *h) {}
static void nvmtl_live_relabel(nvk_buffer *b,uint32_t kind) {}
''' + bindings + view + snapshot + r'''
static uint64_t *words(nvk_cmdbuf *c,const VkDescriptorBufferInfo *info) {
    return (uint64_t *)((uint8_t *)((nvmtl_address_page *)c->address_pages)->buffer.map+info->offset);
}
static void check(nvk_cmdbuf *c,unsigned set,const VkDescriptorBufferInfo *info) {
    uint64_t *a=words(c,info); nvmtl_bindtable *t=c->table;
    assert(info->range==NVMTL_TABLE_BYTES);
    for(unsigned i=0;i<NVMTL_NBUF;i++) {
        const nvmtl_slot *s=t?&t->s[set][i]:NULL;
        assert(a[i]==(s && s->kind==NVMTL_SLOT_BUF?s->address+s->off:0x100010000ull));
        assert(a[NVMTL_NBUF+i]==(uint64_t)(uintptr_t)info->buffer+0x100000000ull+info->offset+i*8);
    }
    for(unsigned i=0;i<NVMTL_NTEX;i++) {
        const nvmtl_slot *s=t?&t->s[set][NVMTL_NBUF+i]:NULL;
        assert(a[2*NVMTL_NBUF+i]==(s && s->kind==NVMTL_SLOT_TEX && s->h?s->size:0));
    }
}
static void release(nvk_cmdbuf *c) {
    nvmtl_address_page *p=c->address_pages;
    while(p) { nvmtl_address_page *next=p->next; nvmtl_vk_buffer_destroy(&p->buffer); free(p); p=next; }
    free(c->table); memset(c,0,sizeof *c);
}
int main(void) {
    nvk_cmdbuf c={0}; VkDescriptorBufferInfo info;
    assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
    assert(dummy_queries==1 && page_queries==1); // not 32 dummy queries
    assert(!nvmtl_vk_bind_texture_view(&c,0,0,(void *)0x18000));
    assert(!nvmtl_vk_bind_texture_view(&c,0,127,(void *)0x18800));
    assert(!nvmtl_vk_bind_texture_view(&c,1,0,(void *)0x19000));
    assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
    uint64_t *old=words(&c,&info), saved[NVMTL_TABLE_BYTES/8]; memcpy(saved,old,sizeof saved);
    nvmtl_bindtable *t=c.table; assert(t->address_tex_valid[0]);
    // Uniform offset churn must preserve texture indices, the other stage and all earlier snapshots.
    for(unsigned i=0;i<260;i++) {
        assert(!nvmtl_bind_slot(&c,0,0,NVMTL_SLOT_BUF,(void *)0x20000,i*256));
        t->s[0][0].address=0x300000000ull;
        assert(t->address_tex_valid[0]);
        assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
        assert(!memcmp(old,saved,sizeof saved));
    }
    assert(allocations==2); // crossed the 256-snapshot page boundary
    assert(!nvmtl_address_snapshot(&c,1,&info)); check(&c,1,&info);
    assert(!nvmtl_vk_bind_texture_view(&c,0,0,(void *)0x19800));
    assert(t->address_tex_valid[1]);
    if (nvmtl_addr_tex_inc_on()) assert(t->address_tex_valid[0] && (t->address_tex_dirty[0][0]&1));
    else assert(!t->address_tex_valid[0]);
    assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
    assert(!nvmtl_vk_bind_texture_view(&c,0,0,(void *)0x19800)); assert(t->address_tex_valid[0]);
    assert(!nvmtl_bind_slot(&c,0,NVMTL_NBUF,NVMTL_SLOT_TEXBUF,(void *)0x19800,0));
    assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
    assert(!words(&c,&info)[2*NVMTL_NBUF]); // texel views cannot retain sampled-image tokens
    assert(!nvmtl_vk_bind_texture_view(&c,0,127,NULL));
    assert(!nvmtl_address_snapshot(&c,0,&info)); check(&c,0,&info);
    assert(!words(&c,&info)[2*NVMTL_NBUF+127]);
    assert(nvmtl_vk_bind_texture_view(&c,0,NVMTL_NTEX,(void *)0x18000)==-1);
    // Churn every word boundary in both stages, including multiple changes
    // before a snapshot. Compare each immutable result with the slot table.
    for(unsigned round=0;round<1000;round++) {
        const unsigned stage=round&1;
        for(unsigned j=0;j<5;j++) {
            const unsigned i=(round*31+j*63)%NVMTL_NTEX;
            void *handle=(round+j)%3?(void *)(uintptr_t)(0x20000+8*(round*5+j)):NULL;
            if(j&1) assert(!nvmtl_bind_slot(&c,stage,NVMTL_NBUF+i,NVMTL_SLOT_TEXBUF,handle,0));
            else assert(!nvmtl_vk_bind_texture_view(&c,stage,i,handle));
        }
        assert(!nvmtl_address_snapshot(&c,stage,&info)); check(&c,stage,&info);
        assert(!t->address_tex_dirty[stage][0] && !t->address_tex_dirty[stage][1]);
        assert(!memcmp(old,saved,sizeof saved));
    }
    // All 321 binding slots need distinct dirty bits. High storage slots must
    // not overwrite adjacent generation/cache fields.
    for(unsigned stage=0;stage<2;stage++) {
        for(unsigned binding=255;binding<NVMTL_NBIND;binding++) {
            uint64_t generation=t->texgen[stage];
            assert(!nvmtl_bind_slot(&c,stage,binding,NVMTL_SLOT_TEX,(void *)0x18000,0));
            assert(t->texgen[stage]==generation+1);
        }
    }
    release(&c);
    // Recycled address pages must receive the new recording's table, including empty texture slots.
    assert(!nvmtl_address_snapshot(&c,0,&info)); parked=c.address_pages; c.address_pages=NULL;
    release(&c); unsigned before=allocations;
    assert(!nvmtl_address_snapshot(&c,1,&info)); check(&c,1,&info); assert(allocations==before);
    release(&c);
    fail_alloc=1; assert(nvmtl_address_snapshot(&c,0,&info)==-1);
    assert(!c.address_pages && !c.address_count); release(&c);
    puts("PASS: full address layout, 64-bit offsets, stage separation, texture/texel/null changes, immutable snapshots, page rollover/recycling and allocation failure");
}
'''
with tempfile.TemporaryDirectory(prefix='nvmtl-address-snapshot-') as directory:
    work = pathlib.Path(directory)
    (work / 'test.c').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-O2', '-Werror', '-fsanitize=address,undefined',
                    '-I' + str(root / 'plugin'), '-I' + str(pathlib.Path.home() / 'Vulkan-Headers/include'),
                    str(work / 'test.c'), '-o', str(work / 'test')], check=True)
    for incremental in ('1', '0'):
        subprocess.run([str(work / 'test')], check=True,
                       env={**os.environ, 'NVMTL_ADDR_TEX_INC': incremental, 'NVMTL_ADDR_TEX_VERIFY': '1'})

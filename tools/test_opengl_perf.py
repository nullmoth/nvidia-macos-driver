#!/usr/bin/env python3
"""Exercise production OpenGL descriptor reuse, encoder defaults and resource-cache lifetime without a GPU."""
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
vk = (ROOT / 'plugin/nvmtl_vk.c').read_text()
objects = (ROOT / 'plugin/NVMTLObjects.m').read_text()
header = (ROOT / 'plugin/NVMTLObjects.h').read_text()


def section(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]


cache = section(vk, 'static VkDescriptorSet nvmtl_snapshot_set_c(', '\nint nvmtl_vk_bind_buffer(')
macros = section(vk, '#define NVMTL_NBUF ', '#define NVMTL_DISPATCH_PC_BYTES')
fixture = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
''' + macros + r'''
#define NVMTL_E78_HAS(m,i) (!(m) || (((m)[(i)>>6] >> ((i)&63)) & 1))
#define NVMTL_TABLES_PER_PAGE 256
enum { NVMTL_E78_H3, E78C_SETREUSE, E78C_SETNEW };
enum { NVMTL_SLOT_BUF=1 };
typedef struct { uint8_t kind; void *h; size_t off,size; uint64_t address; } nvmtl_slot;
static nvk_buffer g_dummyBuf={.buf=(void *)1,.size=256};
typedef struct {
    nvmtl_slot s[2][NVMTL_NBIND];
    uint8_t dirty[2]; VkDescriptorSet last[2]; unsigned last_idx[2]; uint32_t last_hi[2];
    uint64_t lastw[2][4], chg[2][4], lasttb[2][4]; uint32_t lastdyn[2]; void *lasttab[2];
    uint32_t lastgl[2]; VkBuffer lastubuf[2]; VkDeviceSize lasturange[2];
} nvmtl_bindtable;
typedef struct { nvk_buffer buffer; } nvmtl_address_page;
static int enabled=1, dynamic=1, snapshots, allocations, copies;
static int nvmtl_enc78_on(int flag) { return enabled; }
static int nvmtl_table_ubo(void) { return dynamic; }
static int nvmtl_dirty_only_on(void) { return 1; }
static void nvmtl_enc78_count(int key, int count) {}
static int nvmtl_address_snapshot(nvk_cmdbuf *c, uint32_t which, VkDescriptorBufferInfo *out) {
    snapshots++;
    nvmtl_address_page *page=c->address_pages;
    if (!(c->address_count % NVMTL_TABLES_PER_PAGE)) page->buffer.buf=(void *)((uintptr_t)page->buffer.buf+1);
    *out=(VkDescriptorBufferInfo){(VkBuffer)page->buffer.buf,(c->address_count++ % NVMTL_TABLES_PER_PAGE)*256,256};
    return 0;
}
static VkDescriptorSet nvmtl_snapshot_set_md(nvk_cmdbuf *c, uint32_t which, uint32_t hi,
        const uint64_t *mask, const uint64_t *tb, VkDescriptorSet from, const uint64_t *cm,
        const VkDescriptorBufferInfo *uniform) {
    allocations++; copies += from != VK_NULL_HANDLE;
    c->dyn_out[which]=0; c->tab_out[which]=NULL;
    if (NVMTL_E78_HAS(mask,NVMTL_ADDRESS_BINDING)) {
        VkDescriptorBufferInfo ai; assert(!nvmtl_address_snapshot(c,which,&ai));
        c->dyn_out[which]=dynamic ? ai.offset : 0; c->tab_out[which]=dynamic ? (void *)ai.buffer : NULL;
    }
    VkDescriptorSet set=(VkDescriptorSet)(uintptr_t)(100+allocations);
    c->sets[c->nsets++].set=(void *)set;
    return set;
}
static VkDescriptorSet nvmtl_snapshot_set_t(nvk_cmdbuf *c, uint32_t which, uint32_t hi, const uint64_t *tb) {
    return nvmtl_snapshot_set_md(c,which,hi,NULL,tb,VK_NULL_HANDLE,NULL,NULL);
}
'''
# The mask helpers start at nvmtl_spirv_bmask_x's forward declaration: nvmtl_spirv_bmask calls it, and it logs whether
# the framebuffer-fetch set is skipped, so a no-op nvlog stands in for the plugin's logger here.
mask = '#define nvlog(...) ((void)0)\n' + section(vk, 'static int nvmtl_spirv_bmask_x(const void *p, size_t n, uint64_t out[4], int skip_fetch);', '\nenum { NVMTL_SLOT_NONE')
checks = r'''
int main(void) {
    // Bindless heap binding 0 belongs to set 2, not to the stage's buffer 0. Decoration order is arbitrary.
    uint32_t spv[]={0x07230203,0x00010500,0,10,0,
        (4u<<16)|71,7,33,0, (4u<<16)|71,8,33,640,
        (4u<<16)|71,7,34,2, (4u<<16)|71,8,34,0};
    uint64_t used[4]; assert(nvmtl_spirv_bmask(spv,sizeof spv,used));
    assert(!used[0] && used[NVMTL_ADDRESS_BINDING>>6]==(1ull<<(NVMTL_ADDRESS_BINDING&63)));
    spv[16]=0; assert(nvmtl_spirv_bmask(spv,sizeof spv,used) && (used[0]&1));
    spv[5]=0; assert(!nvmtl_spirv_bmask(spv,sizeof spv,used));
    nvmtl_bindtable table={0}; nvmtl_address_page page={.buffer.buf=(void *)20}; nvk_setref sets[64]={0};
    nvk_cmdbuf c={.table=&table,.sets=sets,.address_pages=&page,.address_count=1};
    nvk_pipeline p={.bmask_ok=1};
    p.bmask[NVMTL_ADDRESS_BINDING>>6] |= 1ull<<(NVMTL_ADDRESS_BINDING&63);
    VkDescriptorSet original=nvmtl_snapshot_set_c(&c,0,&p);
    assert(allocations==1 && snapshots==1);
    assert(nvmtl_snapshot_set_c(&c,0,&p)==original && allocations==1 && snapshots==1);
    for (unsigned i=0; i<100; i++) {
        table.dirty[0]=1; table.chg[0][0]=1;
        assert(nvmtl_snapshot_set_c(&c,0,&p)==original && allocations==1);
    }
    assert(snapshots==101 && c.dyn_out[0]==101*256);
    // Rolling to another address-table page must write a new descriptor, not reuse the old page's set.
    c.address_count=256; table.dirty[0]=1; table.chg[0][0]=1;
    VkDescriptorSet rolled=nvmtl_snapshot_set_c(&c,0,&p); assert(rolled!=original && allocations==2);
    // The next pipeline reads a texel buffer where the prior one read an image.
    p.tbany=1; p.tbm[0][0]=1; p.bmask[0] |= 1ull<<NVMTL_NBUF;
    VkDescriptorSet texel=nvmtl_snapshot_set_c(&c,0,&p); assert(texel!=rolled && allocations==3);
    assert(nvmtl_snapshot_set_c(&c,0,&p)==texel && allocations==3);
    table.dirty[0]=1; table.chg[0][0]=1;
    assert(nvmtl_snapshot_set_c(&c,0,&p)==texel && allocations==3);
    p.tbany=0;
    VkDescriptorSet image=nvmtl_snapshot_set_c(&c,0,&p); assert(image!=texel && allocations==4);
    // Image/sampler changes and direct buffer descriptors cannot take the offset-only path.
    table.dirty[0]=1; table.chg[0][0]=1ull<<NVMTL_NBUF;
    VkDescriptorSet changed=nvmtl_snapshot_set_c(&c,0,&p); assert(changed!=image && allocations==5);
    p.bmask[0] |= 1;
    VkDescriptorSet direct=nvmtl_snapshot_set_c(&c,0,&p); assert(direct!=changed && allocations==6);
    table.dirty[0]=1; table.chg[0][0]=1;
    assert(nvmtl_snapshot_set_c(&c,0,&p)!=direct && allocations==7);
    // A descriptor from a retired/recycled slot must not be returned as live.
    sets[table.last_idx[0]].set=(void *)999;
    assert(nvmtl_snapshot_set_c(&c,0,&p)!=(VkDescriptorSet)(uintptr_t)999 && allocations==8);
    // A growing pipeline mask can copy compatible descriptors but must allocate its new bindings.
    p.bmask[1] |= 1;
    nvmtl_snapshot_set_c(&c,0,&p); assert(allocations==9 && copies>=1);
    enabled=0; nvmtl_snapshot_set_c(&c,0,&p); assert(allocations==10);
    enabled=1; p.bmask[0]&=~1ull; p.gl_uniform_layout=1;
    p.gl_uniform_slot[0]=8; p.gl_uniform_range[0]=80;
    table.s[0][7]=(nvmtl_slot){.kind=NVMTL_SLOT_BUF,.h=(void *)77,.size=65536};
    c.address_count=1;
    VkDescriptorSet uniform=nvmtl_snapshot_set_c(&c,0,&p); assert(allocations==11);
    for(unsigned i=1;i<16;i++) {
        table.s[0][7].off=i*256; table.dirty[0]=1; table.chg[0][0]=1ull<<7;
        assert(nvmtl_snapshot_set_c(&c,0,&p)==uniform && allocations==11);
        assert(c.gl_uniform_dyn[0]==i*256);
    }
    p.gl_uniform_range[0]=96;
    assert(nvmtl_snapshot_set_c(&c,0,&p)!=uniform && allocations==12);
    table.s[0][7].h=(void *)78;
    nvmtl_snapshot_set_c(&c,0,&p); assert(allocations==13);
    p.gl_uniform_layout=0;
    nvmtl_snapshot_set_c(&c,0,&p); assert(allocations==14);
    puts("PASS: unchanged/offset-only reuse, table-page rollover, texel/image masks, direct buffers, recycled sets, growing masks, opt-out");
}
'''

iface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', header, re.S).group(0)
impl = section(objects, '@implementation NVMTLResSet', '\n@end') + '\n@end\n'
resources = r'''
#import <Foundation/Foundation.h>
#include <assert.h>
#include <os/lock.h>
#include <stdatomic.h>
static _Atomic unsigned long gRes2Tick;
static long refs;
static id lastReleased;
static void nvmtl_resref(id o,int d) { refs+=d; if(d<0) lastReleased=o; }
''' + iface + '\n' + impl + r'''
@interface EqualResource : NSObject { @public unsigned key; } @end
@implementation EqualResource
- (NSUInteger)hash { return key; }
- (BOOL)isEqual:(id)o { return [o isKindOfClass:[EqualResource class]] && ((EqualResource *)o)->key==key; }
@end
int main(void) { @autoreleasepool {
    NVMTLResSet *set=[NVMTLResSet new]; EqualResource *a=[EqualResource new], *alias=[EqualResource new];
    a->key=alias->key=1; [set addObject:a]; [set addObject:alias];
    assert(set.count==1 && refs==1);
    for(unsigned i=0;i<8;i++) assert(!set->_mru[i] || set->_mru[i]==(__bridge const void *)a);
    [set removeObject:alias]; assert(!set.count && !refs && lastReleased==a);
    for(unsigned i=0;i<8;i++) assert(!set->_mru[i]);
    lastReleased=nil;
    for(unsigned i=0;i<32;i++) [set addObject:[NSObject new]];
    assert(set.count==32 && refs==32);
    NVMTLResSet *taken=[set nvmtlTakeAll]; assert(!set.count && taken.count==32 && refs==32);
    for(unsigned i=0;i<8;i++) assert(!set->_mru[i]);
    [set addObject:a]; assert(set.count==1 && refs==33);
    [set removeAllObjects]; assert(!set.count && refs==32);
    [taken removeAllObjects]; assert(!refs);
    puts("PASS: MRU retains canonical members, equal-object removal, sealing, clearing, residency balance");
} }
'''

defaults = section(objects, '    const float defaultBlend[4]', '    e->_visBuf')
state = r'''
#include <assert.h>
#include <stdio.h>
typedef struct { unsigned restart_off; } Commands;
typedef struct { Commands _c; } Owner;
static int provoking=1; static float bias=7,slope=8,clamp=9;
static void nvmtl_vk_cmd_set_blend_color(Commands *c,const float *v) {}
static void nvmtl_vk_cmd_set_line_width(Commands *c,float v) {}
static int nvmtl_vk_cmd_set_polygon_mode(Commands *c,int v) { return 0; }
static int nvmtl_vk_cmd_set_depth_clip(Commands *c,int v) { return 0; }
static void nvmtl_vk_cmd_set_provoking_last(Commands *c,int v) { provoking=v; }
static void nvmtl_vk_cmd_set_depth_bias(Commands *c,float b,float s,float cl) { bias=b;slope=s;clamp=cl; }
static void next_encoder(Owner *owner) {
#define _c owner->_c
''' + defaults + r'''
#undef _c
}
int main(void) {
    Owner owner={._c.restart_off=1}; next_encoder(&owner);
    assert(!owner._c.restart_off && !provoking && bias==0 && slope==0 && clamp==0);
    puts("PASS: depth bias, provoking vertex and strip restart defaults reset between render encoders");
}
'''

bias = section(vk, 'void nvmtl_vk_cmd_set_depth_bias(', '\nvoid nvmtl_vk_cmd_set_line_width(')
pipeline_states = section(vk, '    VkDynamicState dyn[24]', '    // Metal has no depth-bias enable switch.')
bias_check = r'''
#include <assert.h>
#include <stdio.h>
#include <mach/mach.h>
#include <vulkan/vulkan.h>
#include "nvmtl_vk.h"
static int enabled; static float constant_value,slope_value,clamp_value;
static void pvkCmdSetDepthBiasEnable(void *c,int value) { enabled=value; }
static void pvkCmdSetDepthBias(void *c,float b,float cl,float s) { constant_value=b;clamp_value=cl;slope_value=s; }
''' + bias + r'''
static int g_dyn3,g_provoking,g_clip,g_wide,g_sample_locations;
static unsigned states(int hasDepth,nvk_pipeline *out,unsigned samples) {
''' + pipeline_states + r'''
    assert(dsci.dynamicStateCount<=sizeof dyn/sizeof dyn[0]);
    for(unsigned i=0;i<dsci.dynamicStateCount;i++) for(unsigned j=0;j<i;j++) assert(dyn[i]!=dyn[j]);
    return dsci.dynamicStateCount;
}
int main(void) {
    nvk_cmdbuf c={0};
    nvmtl_vk_cmd_set_depth_bias(&c,0,0,0); assert(!enabled);
    nvmtl_vk_cmd_set_depth_bias(&c,1,2,3); assert(enabled && constant_value==1 && slope_value==2 && clamp_value==3);
    nvmtl_vk_cmd_set_depth_bias(&c,0,2,0); assert(enabled);
    nvmtl_vk_cmd_set_depth_bias(&c,0,0,0); assert(!enabled);
    for(unsigned flags=0;flags<128;flags++) {
        g_dyn3=flags&1;g_provoking=flags&2;g_clip=flags&4;g_wide=flags&8;g_sample_locations=flags&16;
        nvk_pipeline p={.has_stencil=!!(flags&32),.vin_dyn=!!(flags&64)};
        states(0,&p,1);states(1,&p,4);
    }
    g_dyn3=g_provoking=g_clip=g_wide=g_sample_locations=1;
    nvk_pipeline p={.has_stencil=1,.vin_dyn=1}; assert(states(1,&p,4)==23);
    puts("PASS: depth bias is disabled at zero; all 128 dynamic-state combinations fit without duplicates");
}
'''

with tempfile.TemporaryDirectory() as d:
    path = pathlib.Path(d)
    includes = ['-I' + str(ROOT / 'plugin'), '-I' + str(pathlib.Path.home() / 'Vulkan-Headers/include')]
    for name, code, objc in [('descriptors', fixture + mask + cache + checks, False), ('resources', resources, True),
                             ('defaults', state, False), ('bias', bias_check, False)]:
        src = path / (name + ('.m' if objc else '.c'))
        src.write_text(code)
        executable = path / name
        cmd = ['xcrun', 'clang', '-O2', '-Werror', *includes, str(src), '-o', str(executable)]
        if objc:
            cmd += ['-fobjc-arc', '-framework', 'Foundation']
        subprocess.run(cmd, check=True)
        subprocess.run([str(executable)], check=True)

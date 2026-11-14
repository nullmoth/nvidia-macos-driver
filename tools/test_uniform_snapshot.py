#!/usr/bin/env python3
"""Exercise the production descriptor snapshot cache across uniform and pipeline changes."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
text = (root / 'plugin/nvmtl_vk.c').read_text()
start = text.index('static VkDescriptorSet nvmtl_snapshot_set_c(')
end = text.index('{', start) + 1
depth = 1
while depth:
    depth += (text[end] == '{') - (text[end] == '}')
    end += 1
source = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <vulkan/vulkan.h>
#define NVMTL_TABLES_PER_PAGE 256
#define NVMTL_ADDRESS_BINDING 192
#define NVMTL_NSTEX 128
#define NVMTL_STEX_BASE 193
#define NVMTL_E78_HAS(m,i) (!(m)||(((m)[(i)>>6]>>((i)&63))&1))
#define NVMTL_SLOT_BUF 1
#define NVMTL_E78_H3 0
#define E78C_SETREUSE 0
#define E78C_SETNEW 0
#define nvlog(...) ((void)0)
typedef struct {unsigned kind;void *h;size_t off;} nvmtl_slot;
typedef struct {
 nvmtl_slot s[2][321];uint64_t lasttb[2][4],lastw[2][4],chg[2][4];
 unsigned lastgl[2],dirty[2],last_idx[2],last_hi[2],lastdyn[2];
 VkBuffer lastubuf[2];VkDeviceSize lasturange[2];VkDescriptorSet last[2];void *lasttab[2];
} nvmtl_bindtable;
typedef struct {struct {VkBuffer buf;} buffer;} nvmtl_address_page;
typedef struct {
 nvmtl_bindtable *table;unsigned gl_uniform_dyn[2],dyn_out[2],nsets,address_count,icb_slot;
 struct {void *set;} sets[64];void *tab_out[2],*address_pages;
} nvk_cmdbuf;
typedef struct {
 unsigned gl_uniform_layout,gl_uniform_slot[2],gl_uniform_range[2],stex_hi[2];
 int gmask_ok,gmask2_ok,bmask_ok,bmask2_ok,tbany;
 uint64_t gmask[2][4],gmask2[2][4],bmask[4],bmask2[4],tbm[2][4];
} nvk_pipeline;
static struct {VkBuffer buf;} g_dummyBuf;
static unsigned snapshots,newsets;
static int nvmtl_enc78_on(int n){return 1;}
static int nvmtl_desc_mask_on(void){return 1;}
static int nvmtl_table_ubo(void){return 1;}
static int nvmtl_dirty_only_on(void){return 1;}
static void nvmtl_enc78_count(int n,int v){}
static const nvmtl_slot *nvmtl_slot_checked(const nvmtl_slot *s,unsigned w,unsigned b){return s;}
static int nvmtl_address_snapshot(nvk_cmdbuf *c,unsigned w,VkDescriptorBufferInfo *out){
 snapshots++;*out=(VkDescriptorBufferInfo){(VkBuffer)0x6000,c->address_count++*1536,1536};return 0;
}
static VkDescriptorSet nvmtl_snapshot_set_md(nvk_cmdbuf *c,unsigned w,unsigned hi,const uint64_t *m,const uint64_t *tb,VkDescriptorSet from,const uint64_t *cm,const VkDescriptorBufferInfo *u){
 VkDescriptorBufferInfo ai;nvmtl_address_snapshot(c,w,&ai);c->dyn_out[w]=ai.offset;c->tab_out[w]=(void*)ai.buffer;
 VkDescriptorSet s=(VkDescriptorSet)(uintptr_t)(++newsets*16);c->sets[c->nsets++].set=(void*)s;return s;
}
/*PRODUCTION*/
int main(void){
 nvmtl_bindtable t={0};nvmtl_address_page page={.buffer.buf=(VkBuffer)0x6000};
 nvk_cmdbuf c={.table=&t,.address_pages=&page,.address_count=1};
 nvk_pipeline p={.gl_uniform_layout=1,.gl_uniform_slot={20,20},.gl_uniform_range={64,64},.gmask_ok=1};
 for(unsigned w=0;w<2;w++){
  p.gmask[w][3]=1;for(unsigned i=0;i<32;i++)t.s[w][i]=(nvmtl_slot){1,(void*)0x8000,i*256};
  VkDescriptorSet first=nvmtl_snapshot_set_c(&c,w,&p);unsigned count=c.address_count,dyn=c.dyn_out[w];
  for(unsigned i=0;i<1000;i++){
   t.s[w][19].off=i*256;t.dirty[w]=1;t.chg[w][0]=1ull<<19;
   assert(nvmtl_snapshot_set_c(&c,w,&p)==first);assert(c.address_count==count);
   assert(c.dyn_out[w]==dyn&&c.gl_uniform_dyn[w]==i*256);
  }
  // An ordinary buffer, or an unknown dirty reason, must still get a new table.
  for(unsigned i=0;i<3;i++){
   t.dirty[w]=1;t.chg[w][0]=i==0?1ull<<4:i==1?((1ull<<4)|(1ull<<19)):0;
   assert(nvmtl_snapshot_set_c(&c,w,&p)==first);assert(c.address_count==++count);
  }
  // The previous page stays retained even when another stage filled a page.
  c.address_count=256;t.dirty[w]=1;t.chg[w][0]=1ull<<19;
  assert(nvmtl_snapshot_set_c(&c,w,&p)==first);assert(c.address_count==256);
  // Same buffer/range, different promoted slot: cannot reuse the old table.
  p.gl_uniform_slot[w]=19;t.dirty[w]=1;t.chg[w][0]=1ull<<18;
  unsigned sets=newsets;assert(nvmtl_snapshot_set_c(&c,w,&p)!=first);assert(newsets==sets+1);
  p.gl_uniform_slot[w]=20;sets=newsets;nvmtl_snapshot_set_c(&c,w,&p);assert(newsets==sets+1);
  // An ICB replay and a different uniform buffer retain their conservative path.
  c.icb_slot=1;t.dirty[w]=1;t.chg[w][0]=1ull<<19;sets=newsets;
  nvmtl_snapshot_set_c(&c,w,&p);assert(newsets==sets+1);c.icb_slot=0;
  t.s[w][19].h=(void*)0x9000;t.dirty[w]=1;t.chg[w][0]=1ull<<19;sets=newsets;
  nvmtl_snapshot_set_c(&c,w,&p);assert(newsets==sets+1);
  p.gl_uniform_layout=0;sets=newsets;nvmtl_snapshot_set_c(&c,w,&p);assert(newsets==sets+1);p.gl_uniform_layout=1;
 }
 puts("PASS both stages: uniform-only reuse, mixed/unknown changes, page boundary, promoted-slot switch, ICB, buffer change and fallback");
}
'''.replace('/*PRODUCTION*/', text[start:end])
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    (path / 'probe.c').write_text(source)
    for flags in ([], ['-fsanitize=address,undefined', '-fno-omit-frame-pointer']):
        subprocess.run(['xcrun', '--sdk', 'macosx', 'clang', '-std=c11', '-O1', *flags, str(path / 'probe.c'), '-o', str(path / 'probe')], check=True)
        subprocess.run([str(path / 'probe')], check=True, env={**os.environ, 'ASAN_OPTIONS': 'detect_leaks=0'})

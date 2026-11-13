#!/usr/bin/env python3
"""Check production per-draw address tables across page boundaries and ICB replay."""
import pathlib,re,subprocess,tempfile,os
root=pathlib.Path(__file__).resolve().parents[1]
source=(root/'plugin/nvmtl_vk.c').read_text()
start=source.index('static int nvmtl_address_snapshot(');end=source.index('{',start)+1;depth=1
while depth:
 depth+=(source[end]=='{')-(source[end]=='}');end+=1
production=source[start:end]
fixture=r"""
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <pthread.h>
#include <vulkan/vulkan.h>
#define NVMTL_NBUF 32
#define NVMTL_NTEX 128
#define NVMTL_TABLES_PER_PAGE 256
#define NVMTL_TABLE_BYTES ((2*NVMTL_NBUF+NVMTL_NTEX)*sizeof(uint64_t))
enum {NVMTL_SLOT_NONE,NVMTL_SLOT_BUF,NVMTL_SLOT_TEX,NVMTL_SLOT_SAMP};
typedef struct {unsigned kind;void *h;size_t off,size;uint64_t address;} nvmtl_slot;
typedef struct {nvmtl_slot s[2][NVMTL_NBUF+NVMTL_NTEX];} nvmtl_bindtable;
typedef struct {void *map;VkBuffer buf;uint64_t address;} nvk_buffer;
typedef struct nvmtl_address_page {nvk_buffer buffer;struct nvmtl_address_page *next;int life;} nvmtl_address_page;
typedef struct {unsigned icb_slot,icb_ntabs;uint64_t *icb_tabs[2];unsigned address_count;nvmtl_address_page *address_pages;nvmtl_bindtable *table;} nvk_cmdbuf;
static nvk_buffer g_dummyBuf={.address=0x123000};
static pthread_mutex_t g_pl_lock=PTHREAD_MUTEX_INITIALIZER;
static int g_pl,queries,fail_alloc;
static unsigned pages;
#define NVMTL_LV_PAGE 0
#define nvlog(...) ((void)0)
static void nvmtl_live_relabel(nvk_buffer *b,int n){}
static void nvmtl_pl_made(int *p){(*p)++;}
static nvmtl_address_page *nvmtl_keep_page_pop(void){return NULL;}
static int nvmtl_vk_buffer_create(size_t n,int host,nvk_buffer *b){if(fail_alloc)return -1;b->map=malloc(n);memset(b->map,0xa5,n);b->buf=(VkBuffer)(uintptr_t)(++pages*8);b->address=0x100000000ull+pages*0x100000;return 0;}
static void nvmtl_vk_buffer_destroy(nvk_buffer *b){free(b->map);}
static uint64_t nvmtl_vk_buffer_address(nvk_buffer *b){queries++;return b->address;}
/*PRODUCTION*/
int main(void){
 nvmtl_bindtable table={0};nvk_cmdbuf c={.table=&table};
 for(unsigned round=0;round<1100;round++){
  unsigned which=round&1;
  for(unsigned i=0;i<NVMTL_NBUF;i++)table.s[which][i]=(nvmtl_slot){.kind=(round+i)%3?NVMTL_SLOT_NONE:NVMTL_SLOT_BUF,.address=0x200000+i*8192,.off=round*16};
  for(unsigned i=0;i<NVMTL_NTEX;i++)table.s[which][NVMTL_NBUF+i]=(nvmtl_slot){.kind=i%2?NVMTL_SLOT_TEX:NVMTL_SLOT_NONE,.h=i%5?(void*)8:NULL,.size=0xabc000+i+round};
  VkDescriptorBufferInfo out;queries=0;assert(nvmtl_address_snapshot(&c,which,&out)==0);assert(queries<=2);assert(out.buffer==c.address_pages->buffer.buf);assert(out.offset==(round%256)*NVMTL_TABLE_BYTES);assert(out.range==NVMTL_TABLE_BYTES);
  const uint64_t *a=(const uint64_t*)((char*)c.address_pages->buffer.map+out.offset);const uint64_t self=c.address_pages->buffer.address+out.offset;
  for(unsigned i=0;i<32;i++){assert(a[i]==((round+i)%3?0x123000:0x200000+i*8192+round*16));assert(a[32+i]==self+i*8);}
  for(unsigned i=0;i<128;i++)assert(a[64+i]==(i%2&&i%5?0xabc000+i+round:0));
 }
 assert(pages==5&&g_pl==5);
 c.table=NULL;VkDescriptorBufferInfo out;assert(!nvmtl_address_snapshot(&c,1,&out));uint64_t *a=(uint64_t*)((char*)c.address_pages->buffer.map+out.offset);for(unsigned i=0;i<32;i++)assert(a[i]==0x123000);for(unsigned i=64;i<192;i++)assert(a[i]==0);
 uint64_t tabs[]={0x10,128,0x20,256};c.icb_slot=2;c.icb_ntabs=2;c.icb_tabs[1]=tabs;unsigned count=c.address_count;queries=0;assert(!nvmtl_address_snapshot(&c,1,&out));assert(out.buffer==(VkBuffer)(uintptr_t)0x20&&out.offset==256&&out.range==NVMTL_TABLE_BYTES);assert(c.address_count==count&&queries==0);
 c.icb_slot=0;c.address_pages->buffer.address=0;assert(!nvmtl_address_snapshot(&c,1,&out));a=(uint64_t*)((char*)c.address_pages->buffer.map+out.offset);for(unsigned i=32;i<64;i++)assert(a[i]==0);
 while(c.address_pages){nvmtl_address_page *p=c.address_pages;c.address_pages=p->next;free(p->buffer.map);free(p);}
 c.address_count=0;fail_alloc=1;assert(nvmtl_address_snapshot(&c,0,&out)==-1);assert(!c.address_pages);
 puts("PASS 1100 snapshots, both stages, five pages, null table, zero page address, ICB replay, allocation failure; at most two address queries per snapshot");
}
"""
# Keep fixture dimensions aligned with the production ABI.
for name in ('NVMTL_NBUF','NVMTL_NTEX','NVMTL_TABLES_PER_PAGE'):
 assert re.search(r'#define\s+'+name+r'\s+(\d+)',source).group(1)==re.search(r'#define '+name+r' (\d+)',fixture).group(1)
with tempfile.TemporaryDirectory(prefix='nvmtl-address-test-',dir='/private/tmp') as tmp:
 path=pathlib.Path(tmp);(path/'test.c').write_text(fixture.replace('/*PRODUCTION*/',production))
 env=os.environ.copy();env['TMPDIR']='/private/tmp'
 subprocess.run(['xcrun','clang','-O1','-g','-fsanitize=address,undefined','-I'+str(root.parent/'Vulkan-Headers-1.4.335/include'),str(path/'test.c'),'-o',str(path/'test')],check=True,env=env)
 subprocess.run([str(path/'test')],check=True,env=env)

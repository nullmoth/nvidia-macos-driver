#!/usr/bin/env python3
"""Exercise production submit accounting with prologues and failure injection."""
import argparse, pathlib, subprocess, tempfile
root=pathlib.Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline',metavar='REF',help='exercise this Git revision instead of the working tree')
args=parser.parse_args()
source=(subprocess.check_output(['git','show',f'{args.baseline}:plugin/nvmtl_vk.c'],cwd=root,text=True)
        if args.baseline else (root/'plugin/nvmtl_vk.c').read_text())
def function(signature):
 start=source.index(signature); end=source.index('{',start)+1; depth=1
 while depth:
  depth+=(source[end]=='{')-(source[end]=='}');end+=1
 return source[start:end]
production='\n'.join(function(x) for x in ['static uint64_t nvmtl_life_begin(void)','static void nvmtl_life_abandon(void)','static uint64_t nvmtl_life_submit(','static uint64_t nvmtl_grace_seq_locked(void)','int nvmtl_vk_cmd_abandon(nvk_cmdbuf *c)','int nvmtl_vk_submit_begin_at(nvk_cmdbuf *c,'])
fixture=r"""
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <pthread.h>
#include <time.h>
#include <vulkan/vulkan.h>
typedef struct page {struct page *next; struct {uint64_t last_seq;} life;} nvmtl_address_page;
typedef struct {int valid;} Clock;
typedef struct {Clock clock;uint64_t host_before_ns;} Timing;
typedef struct {void *cb,*pool,*timing_pool;int dead,counter_error,in_rp;uint64_t fx_seq;uint32_t wseq;nvmtl_address_page *address_pages;Timing timing;int fx;} nvk_cmdbuf;
typedef struct {nvk_cmdbuf cb;unsigned begun,n,fills,copies,nhold;unsigned long long bytes;void *hold[4];} nvmtl_prologue;
typedef struct {nvk_cmdbuf *c;VkFence fence;nvmtl_prologue pre;uint64_t pt[8];} nvmtl_inflight;
static pthread_mutex_t g_grace_lock=PTHREAD_MUTEX_INITIALIZER,nvmtl_submit_mutex=PTHREAD_MUTEX_INITIALIZER,g_fx_lock=PTHREAD_MUTEX_INITIALIZER;
static uint64_t g_cmd_begun,g_cmd_submitted,g_cmd_abandoned;
static int g_state=1,g_perf_kind,g_fx,fail,ends,mode,submittedBuffers,reaps,drops;
static unsigned long long g_pre_fail;
static const char *g_last_submit_who;
static void *g_dev,*g_queue;
static __thread nvmtl_prologue *g_pre_cur;
static void (*nvmtl_pre_submit_hook)(void);
#define nvlog(...) ((void)0)
static void nvmtl_hz_forget(VkCommandBuffer c){}
static void nvmtl_cmd_retire(nvk_cmdbuf *c){c->cb=NULL;}
static void nvmtl_buffer_grace_reap(void){reaps++;}
static void nvmtl_subwho_note(const char *s,int n){}
static int nvmtl_counter_flush(nvk_cmdbuf *c){return fail==1;}
static int nvmtl_counter_reset_finish(nvk_cmdbuf *c,VkCommandBuffer *r){return fail==2;}
static Clock nvmtl_clock_snapshot(void){return (Clock){0};}
static void pvkCmdWriteTimestamp(void *c,int s,VkQueryPool p,int i){}
static uint64_t nvmtl_perf_now(void){return 0;}
static int nvmtl_prologue_on(void){return 1;}
static int nvmtl_keep_on(void){return 0;}
static uint32_t nvmtl_wit_record(nvk_cmdbuf *c){return 0;}
static void nvmtl_wit_control(uint32_t s){}
static VkResult pvkEndCommandBuffer(void *c){ends++;return (fail==3&&ends==1)||(fail==5&&ends==2)?VK_ERROR_OUT_OF_HOST_MEMORY:VK_SUCCESS;}
static VkResult pvkCreateFence(void *d,const VkFenceCreateInfo *i,void *a,VkFence *f){*f=(VkFence)(uintptr_t)8;return fail==4?VK_ERROR_OUT_OF_HOST_MEMORY:VK_SUCCESS;}
static void pvkCmdPipelineBarrier(void *c,int s,int d,int z,int n,const VkMemoryBarrier *m,int nb,void *b,int ni,void *i){}
static VkResult pvkQueueSubmit(void *q,int n,const VkSubmitInfo *i,VkFence f){submittedBuffers=i->commandBufferCount;return fail==6?VK_ERROR_DEVICE_LOST:VK_SUCCESS;}
static void nvmtl_pl_stamp(void *life,uint64_t seq){*(uint64_t*)life=seq;}
static void nvmtl_fx_submit(void *s,uint64_t seq,void *cb,void *fx){}
static void nvmtl_vk_note_loss(const char *s,int e,int t){}
static void nvmtl_fx_dump(const char *s){}
static void nvmtl_pre_drop(nvmtl_prologue *p){drops++;p->nhold=0;}
/*PRODUCTION*/
static void hook(void){
 if(!mode)return;
 nvmtl_prologue *p=g_pre_cur;assert(p);
 p->begun=1;p->cb.cb=(void*)(uintptr_t)2;p->cb.fx_seq=nvmtl_life_begin();p->n=mode==1?1:0;
}
int main(void){
 nvmtl_pre_submit_hook=hook;
 const int modes[]={1,0,2};
 for(unsigned case_index=0;case_index<3;case_index++)for(fail=0;fail<=6;fail++){
  mode=modes[case_index];
  if(fail==5&&mode!=1)continue;
  g_cmd_begun=g_cmd_submitted=g_cmd_abandoned=0;g_state=1;ends=reaps=drops=submittedBuffers=0;
  nvk_cmdbuf c={.cb=(void*)(uintptr_t)1};nvmtl_life_begin();nvmtl_inflight f;
  int rc=nvmtl_vk_submit_begin_at(&c,&f,"fixture");
  assert(g_cmd_begun==g_cmd_submitted+g_cmd_abandoned);
  assert(nvmtl_grace_seq_locked()==g_cmd_submitted);
  if(fail>=1&&fail<=4){assert(rc==-1&&c.cb==NULL&&g_cmd_abandoned==1&&reaps==1);}
  else if(fail==6){assert(rc==-2&&g_state==-1&&c.cb!=NULL);}
  else {assert(rc==0);assert(submittedBuffers==(mode==1&&fail!=5?2:1));
   assert(c.fx_seq==(mode==1&&fail!=5?2:1));
   if(mode==2||fail==5)assert(g_cmd_abandoned==1&&reaps==1&&drops==1);
  }
 }
 puts("PASS production submit: main + prologue, empty/discarded prologue, four pre-submit failures, device-loss retention");
}
"""
with tempfile.TemporaryDirectory() as td:
 td=pathlib.Path(td);c=td/'test.c';binary=td/'test';c.write_text(fixture.replace('/*PRODUCTION*/',production))
 subprocess.run(['xcrun','clang','-std=c11','-O1','-g','-fsanitize=address,undefined','-Wno-incompatible-pointer-types','-Wno-int-conversion','-I'+str(root.parent/'Vulkan-Headers-1.4.335/include'),str(c),'-o',str(binary)],check=True)
 subprocess.run([str(binary)],check=True)

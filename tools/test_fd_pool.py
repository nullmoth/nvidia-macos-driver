#!/usr/bin/env python3
"""Exercise the published connection-close path with an fd-binding model."""
import pathlib, subprocess, tempfile, sys
patch=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else pathlib.Path(__file__).resolve().parents[1]/"nvk/nvk-macos.patch"
s=patch.read_text();start=s.index('+int nvrm_xnu_close(int fd)');end=s.index('\ndiff ',start)
close='\n'.join(line[1:] for line in s[start:end].splitlines() if line.startswith('+'))
start=s.index('+static io_connect_t pool_take(');end=s.index('+int nvrm_xnu_open(',start)
pool='\n'.join(line[1:] for line in s[start:end].splitlines() if line.startswith('+'))
pre=r'''
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <assert.h>
typedef unsigned io_connect_t;
typedef int kern_return_t;
#define KERN_SUCCESS 0
#define SEL_BINDFD 3
#define NVRM_XNU_MAX_MAP 16
#define NVRM_XNU_POOL 8
static io_connect_t gConn[64],gPendingClose[16],gPoolConn[8];
static uint32_t gConnType[64],gPoolType[8];
static int gPoolN,bound[64],liveMaps[64],closed[64],failUnbind,fdClosed;
static unsigned long gPoolPut,gPoolDrop;
static pthread_mutex_t gLock=PTHREAD_MUTEX_INITIALIZER;
static io_connect_t conn_of(int fd){return gConn[fd];}
static int maps_on(io_connect_t c){return liveMaps[c];}
static int close(int fd){fdClosed=fd;return 0;}
static kern_return_t IOServiceClose(io_connect_t c){closed[c]++;bound[c]=-2;return 0;}
static kern_return_t IOConnectCallScalarMethod(io_connect_t c,int sel,const uint64_t *in,int n,void *o,void *on){
 assert(sel==SEL_BINDFD && n==1 && (int64_t)*in==-2);
 /* A reuse thread must not see this connection until detachment completes. */
 pthread_mutex_lock(&gLock);
 for(int i=0;i<gPoolN;i++)assert(gPoolConn[i]!=c);
 pthread_mutex_unlock(&gLock);
 if(failUnbind)return 5;
 bound[c]=-2;return 0;
}
'''
post=r'''
static void reset(void){gPoolN=0;failUnbind=0;fdClosed=-1;for(int i=0;i<64;i++){gConn[i]=0;gConnType[i]=0;bound[i]=-2;closed[i]=liveMaps[i]=0;}for(int i=0;i<16;i++)gPendingClose[i]=0;}
int main(void){
 reset();gConn[7]=1;gConnType[7]=0;bound[1]=7;
 assert(nvrm_xnu_close(7)==0 && fdClosed==7 && !gConn[7]);
 assert(gPoolN==1 && bound[1]==-2 && !closed[1]);
 /* A different connection type misses the pool, reuses the fd and maps
    through the new client. Slot order must not pick the pooled old client. */
 assert(pool_take(1)==0);bound[2]=7;
 int match=0;for(int i=1;i<64;i++)if(bound[i]==7){match=i;break;}assert(match==2);
 assert(pool_take(0)==1);
 for(int i=0;i<1000;i++){
   unsigned c=1+(i%4);gConn[7]=c;gConnType[7]=i%2;bound[c]=7;
   assert(nvrm_xnu_close(7)==0);assert(bound[c]==-2);assert(pool_take(i%2)==c);
 }
 reset();gConn[7]=1;bound[1]=7;failUnbind=1;
 assert(nvrm_xnu_close(7)==0 && !gPoolN && closed[1]==1);
 reset();gConn[7]=1;bound[1]=7;liveMaps[1]=1;
 assert(nvrm_xnu_close(7)==0 && !gPoolN && !closed[1] && gPendingClose[0]==1 && bound[1]==-2);
 reset();gConn[7]=1;bound[1]=7;gPoolN=NVRM_XNU_POOL;
 for(int i=0;i<NVRM_XNU_POOL;i++)gPoolConn[i]=10+i;
 assert(nvrm_xnu_close(7)==0 && closed[1]==1 && gPoolN==NVRM_XNU_POOL);
 reset();assert(nvrm_xnu_close(7)==0 && fdClosed==7 && !gPoolN);
 puts("PASS: fd reuse, 1000 pool cycles, detach ordering, detach failure, live mapping, full pool, unknown fd");return 0;
}
'''
with tempfile.TemporaryDirectory() as d:
 p=pathlib.Path(d);(p/'test.c').write_text(pre+pool+close+post)
 subprocess.run(['clang','-std=c11','-Wall','-Wextra','-Wno-unused-parameter','-pthread',str(p/'test.c'),'-o',str(p/'test')],check=True)
 subprocess.run([str(p/'test')],check=True)

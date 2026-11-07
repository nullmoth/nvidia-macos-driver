/* Offline check of the cubin linker in nvk_cubin.c (edit 8 of patch_dlss_cache.py) against real cubins.
 * link_selftest.sh extracts nvk_cubin_module_link() from a patched Mesa tree, builds it here with stubs for the
 * Vulkan and memory-manager parts (under ASan/UBSan), links every cubin given on the command line and checks that
 * constant bank 4 (pointer tables) ends up holding addresses inside the module. DUMP=1 prints the layout. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <inttypes.h>
typedef int VkResult;
#define VK_SUCCESS 0
#define VK_ERROR_INITIALIZATION_FAILED (-3)
#define VK_ERROR_OUT_OF_HOST_MEMORY (-1)
#define NVKMD_MEM_LOCAL 1
#define NVKMD_MEM_MAP_WR 2
#define MAX2(a,b) ((a)>(b)?(a):(b))
static inline uint64_t align64(uint64_t v, uint64_t a){return (v+a-1)&~(a-1);}
struct va { uint64_t addr; };
struct nvkmd_mem { void *map; struct va *va; };
struct nv_cubin_module { uint32_t sm; };
struct nvk_device { void *nvkmd; struct { int base; } vk; };
struct nvk_cubin_module {
   struct nv_cubin_module *info; void *owned_data;
   struct nvkmd_mem *link_mem; uint32_t link_count; char **link_name; uint64_t *link_addr; uint64_t *link_size;
   uint64_t bank_addr[8]; uint32_t bank_size[8];
};
static char errbuf[512];
#define vk_errorf(dev, code, ...) (snprintf(errbuf, sizeof errbuf, __VA_ARGS__), code)
static uint64_t next_va = 0x7000000000ull;
static VkResult nvkmd_dev_alloc_mapped_mem(void *d, void *b, uint64_t size, uint64_t al, int a, int m, struct nvkmd_mem **out){
  struct nvkmd_mem *mm = calloc(1,sizeof *mm); mm->map = calloc(1,size); mm->va = malloc(sizeof *mm->va);
  mm->va->addr = next_va; next_va += align64(size,0x10000); *out = mm; return VK_SUCCESS; }
#include "link_fn.inc"
static uint8_t *rd(const char *p, size_t *n){FILE*f=fopen(p,"rb");if(!f)return 0;fseek(f,0,SEEK_END);*n=ftell(f);rewind(f);uint8_t*b=malloc(*n);fread(b,1,*n,f);fclose(f);return b;}
int main(int argc, char **argv){
  int ok=0, fail=0, linked=0, stubbed=0;
  for (int i=1;i<argc;i++){
    size_t n; uint8_t *b = rd(argv[i], &n); if(!b){puts("read fail");return 2;}
    uint32_t fl; memcpy(&fl,b+0x30,4);
    struct nv_cubin_module info; info.sm = b[7]>=0x41 ? (fl>>8)&0xff : (fl>>16)&0xff;
    struct nvk_cubin_module m; memset(&m,0,sizeof m); m.info=&info; struct nvk_device dev;
    errbuf[0]=0;
    VkResult r = nvk_cubin_module_link(&dev,&m,b,n);
    if (r!=VK_SUCCESS){ fail++; printf("FAIL %s sm%u: %s\n",argv[i],info.sm,errbuf); continue; }
    ok++;
    if (!m.link_mem) continue;
    linked++;
    if (getenv("DUMP")) { uint8_t *map=m.link_mem->map; uint64_t base=m.link_mem->va->addr;
      printf("%s sm%u base=0x%llx\n",argv[i],info.sm,(unsigned long long)base);
      for(uint32_t k=0;k<m.link_count;k++) printf("  sec %-34s +0x%llx size %llu\n",m.link_name[k],(unsigned long long)(m.link_addr[k]-base),(unsigned long long)m.link_size[k]);
      if(m.bank_size[4]) for(uint32_t o=0;o+8<=m.bank_size[4];o+=8){uint64_t v;memcpy(&v,map+(m.bank_addr[4]-base)+o,8);printf("  bank4[%u]=+0x%llx\n",o,(unsigned long long)(v-base));} }
    /* every 8-byte slot of bank 4 must now point inside the linked module */
    if (m.bank_size[4]) {
      uint8_t *map = m.link_mem->map; uint64_t base = m.link_mem->va->addr, end = base + 0x100000;
      uint64_t b4 = m.bank_addr[4] - base;
      for (uint32_t o=0;o+8<=m.bank_size[4];o+=8){ uint64_t v; memcpy(&v,map+b4+o,8);
        if (v==0 || v<base || v>=base+0x1000000) { printf("BAD %s bank4[%u]=0x%llx\n",argv[i],o,(unsigned long long)v); fail++; }
        else { /* classify */ if (v >= base + (m.link_addr[m.link_count-1]-base)) {} }
      }
    }
  }
  printf("ok %d fail %d linked %d\n",ok,fail,linked);
  return fail?1:0;
}

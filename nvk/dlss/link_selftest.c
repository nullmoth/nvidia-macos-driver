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
/* Independent decode of a bit range of a 16-byte instruction (the definitions in LLVM's CUDA ELF relocation list). */
static uint64_t getbits(const uint8_t *ins, unsigned lo, unsigned n){
  uint64_t v = 0; for (unsigned i = 0; i < n; i++) v |= (uint64_t)((ins[(lo+i)/8] >> ((lo+i)%8)) & 1) << i; return v; }
/* Every code relocation of kind 56/57/58/75 in cubin b, after linking, must hold the address of its target encoded as
 * the kind defines. Returns the number of mismatches; *checked counts the relocations examined. */
static int check_code_relocs(const uint8_t *b, size_t n, struct nvk_cubin_module *m, int *checked){
  uint64_t shoff; memcpy(&shoff, b+0x28, 8); uint16_t shn, shs; memcpy(&shn, b+0x3c, 2); memcpy(&shs, b+0x3e, 2);
  struct nvk_link_shdr *sh = calloc(shn, sizeof *sh); const char **nm = calloc(shn, sizeof *nm); int bad = 0;
  for (unsigned i = 0; i < shn; i++) memcpy(&sh[i], b + shoff + 64u*i, sizeof sh[i]);
  for (unsigned i = 0; i < shn; i++) nm[i] = (const char *)b + sh[shs].offset + sh[i].name;
  const uint8_t *map = m->link_mem->map; uint64_t base = m->link_mem->va->addr;
  for (unsigned r = 0; r < shn; r++){
    if (sh[r].type != 4 && sh[r].type != 9) continue;
    unsigned t = sh[r].info, sy = sh[r].link; if (t >= shn || strncmp(nm[t], ".text.", 6)) continue;
    uint64_t tgt = 0; for (uint32_t k = 0; k < m->link_count; k++) if (!strcmp(m->link_name[k], nm[t])) tgt = m->link_addr[k];
    if (!tgt) continue;
    uint64_t es = sh[r].type == 4 ? 24 : 16;
    for (uint64_t e = 0; e + es <= sh[r].size; e += es){
      uint64_t off, info; int64_t add = 0; memcpy(&off, b+sh[r].offset+e, 8); memcpy(&info, b+sh[r].offset+e+8, 8);
      if (sh[r].type == 4) memcpy(&add, b+sh[r].offset+e+16, 8);
      uint32_t ty = (uint32_t)info; if (ty != 56 && ty != 57 && ty != 58 && ty != 75) continue;
      uint16_t shndx; uint64_t val; memcpy(&shndx, b+sh[sy].offset+(info>>32)*24+6, 2); memcpy(&val, b+sh[sy].offset+(info>>32)*24+8, 8);
      uint64_t dst = 0; for (uint32_t k = 0; k < m->link_count; k++) if (!strcmp(m->link_name[k], nm[shndx])) dst = m->link_addr[k];
      if (!dst) { printf("BAD no placed section %s\n", nm[shndx]); bad++; continue; }
      uint64_t v = dst + (val - sh[shndx].addr) + (uint64_t)add;
      const uint8_t *ins = map + (tgt - base) + off; uint64_t got, want;
      if (ty == 56) { got = getbits(ins, 32, 32); want = (uint32_t)v; }
      else if (ty == 57) { got = getbits(ins, 32, 32); want = v >> 32; }
      else if (ty == 58) { got = getbits(ins, 34, 47); want = v >> 2; }
      else { got = getbits(ins, 16, 8) | (getbits(ins, 34, 47) << 8); want = v >> 2; }
      (*checked)++;
      if (got != want) { printf("BAD reloc kind %u in %s+0x%llx: holds 0x%llx want 0x%llx\n", ty, nm[t], (unsigned long long)off, (unsigned long long)got, (unsigned long long)want); bad++; }
    }
  }
  free(sh); free(nm); return bad;
}
int main(int argc, char **argv){
  int ok=0, fail=0, linked=0, stubbed=0, relocs=0;
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
    { int checked = 0; int bad = check_code_relocs(b, n, &m, &checked); fail += bad; relocs += checked; }
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
  printf("ok %d fail %d linked %d code relocations decoded %d\n",ok,fail,linked,relocs);
  return fail?1:0;
}

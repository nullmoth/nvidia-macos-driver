// Host selftest of nvmtl_hazard.h's model (no GPU, no Vulkan device): stubs for what the header calls, then named arms.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdatomic.h>
#include <pthread.h>
#include <unistd.h>
#include <vulkan/vulkan_core.h>
#include "nvmtl_vk.h"
static void nvlog(const char *f, ...) { (void)f; }
const char *nvmtl_log_path(void) { return "/dev/null"; }
static VkDevice g_dev; static PFN_vkGetDeviceProcAddr pvkGetDeviceProcAddr;
static int g_barriers;
static int g_hz_dynr;
static void stub_barrier(VkCommandBuffer cb, VkPipelineStageFlags a, VkPipelineStageFlags b, VkDependencyFlags d, uint32_t n, const VkMemoryBarrier *m,
                         uint32_t nb, const VkBufferMemoryBarrier *bb, uint32_t ni, const VkImageMemoryBarrier *ib) { g_barriers++; }
static PFN_vkCmdPipelineBarrier pvkCmdPipelineBarrier = stub_barrier;
// the fault context's barrier record is a plugin function; the hazard header calls it on every barrier
static void nvmtl_fx_note_barrier(VkCommandBuffer cb, VkPipelineStageFlags ss, VkPipelineStageFlags ds, uint32_t nm, const VkMemoryBarrier *m,
                                  uint32_t nb, const VkBufferMemoryBarrier *b, uint32_t ni, const VkImageMemoryBarrier *im) { (void)cb; (void)ss; (void)ds; (void)nm; (void)m; (void)nb; (void)b; (void)ni; (void)im; }
static VkAccessFlags nvmtl_dst_reads(void) { return VK_ACCESS_SHADER_READ_BIT; }
static int nvmtl_fbfetch_on(void) { return 0; }
static VkImageLayout nvmtl_image_access_layout(VkImageLayout l) { return l; }
static void nvmtl_clear_colour(VkClearColorValue *cv, uint32_t fmt, const float c[4]) { (void)cv; (void)fmt; (void)c; }
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c);
int nvmtl_vk_bind_texture_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view) { return 0; }
int nvmtl_vk_bind_storage_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view) { return 0; }
int nvmtl_vk_bind_texel_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view, int storage) { return 0; }
int nvmtl_vk_bind_buffer(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_buffer *b, size_t offset) { return 0; }
#define NVMTL_NBUF 32
#define NVMTL_NTEX 128
#define NVMTL_NSAMP 32
#define NVMTL_ADDRESS_BINDING (NVMTL_NBUF + NVMTL_NTEX + NVMTL_NSAMP)
#define NVMTL_NSTEX 128
#define NVMTL_STEX_BASE (NVMTL_ADDRESS_BINDING + 1)
#define NVMTL_NBIND (NVMTL_STEX_BASE + NVMTL_NSTEX)
enum { NVMTL_SLOT_NONE = 0, NVMTL_SLOT_BUF, NVMTL_SLOT_TEX, NVMTL_SLOT_SAMP, NVMTL_SLOT_TEXBUF };
typedef struct { uint8_t kind; void *h; size_t off; size_t size; uint64_t address; } nvmtl_slot;
typedef struct { void *mem; uint64_t lo, hi; uint8_t w, src, skip; } hz_ent;
typedef struct { nvmtl_slot s[2][NVMTL_NBIND]; hz_ent hk[2][NVMTL_NBIND]; const void *hkh[2][NVMTL_NBIND];
                 uint64_t hzdirty[2][(NVMTL_NBIND + 63) / 64]; uint64_t hzbound[2][(NVMTL_NBIND + 63) / 64];
                 uint32_t hzseen[2][NVMTL_NBIND]; uint32_t hzany; } nvmtl_bindtable;
#include "nvmtl_hazard.h"
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c) { g_barriers++; nvmtl_hz_full_barrier(c); }
static int g_c2c; void nvmtl_vk_cmd_barrier_c2c(nvk_cmdbuf *c) { (void)c; g_c2c++; }

static int fails, arms;
#define ARM(name, cond) do { arms++; if (!(cond)) { fails++; printf("FAIL  %s\n", name); } else printf("ok    %s\n", name); } while (0)
static hz_ent K(void *m, uint64_t lo, uint64_t hi, int w) { hz_ent e = { m, lo, hi, (uint8_t)w, HZS_BUF, 0 }; return e; }
static void enc(hz_state *s, hz_ent *list, int n) { s->npend = 0; s->pend_all = 0; for (int i = 0; i < n; i++) hz_pend(s, list[i]); }

int main(void)
{
    setenv("NVMTL_HAZARD", "census", 1);
    hz_state *s = malloc(sizeof *s); hz_init(s, (VkCommandBuffer)(uintptr_t)0x1000); s->all = 0;
    void *A = (void *)(uintptr_t)0x10000, *B = (void *)(uintptr_t)0x20000;

    // encoder 1 writes A[0,4K) and reads B whole
    hz_ent e1[] = { K(A, 0, 4096, 1), K(B, 0, UINT64_MAX, 0) };
    enc(s, e1, 2); ARM("the first encoder after a reset conflicts with nothing", !hz_conflict(s, 1)); hz_commit(s); hz_close_region(s);
    hz_ent r1[] = { K(A, 1024, 2048, 0) };
    enc(s, r1, 1); ARM("a read inside a range an earlier encoder wrote conflicts (narrow bins)", hz_conflict(s, 1));
    hz_ent r2[] = { K(A, 8192, 9000, 0) };
    enc(s, r2, 1); ARM("a read beside that range does not", !hz_conflict(s, 1));
    hz_ent r3[] = { K(B, 0, UINT64_MAX, 0) };
    enc(s, r3, 1); ARM("two reads of one whole memory do not conflict", !hz_conflict(s, 1));
    hz_ent w3[] = { K(B, 64, 128, 1) };
    enc(s, w3, 1); ARM("a narrow write into a memory an earlier encoder read whole conflicts (wide chain)", hz_conflict(s, 1));
    hz_ent w4[] = { K(A, 0, UINT64_MAX, 0) };
    enc(s, w4, 1); ARM("a whole-memory read over an earlier narrow write conflicts (memory chain)", hz_conflict(s, 1));
    hz_ent wb[] = { K(A, 4096 - 1 + (2u << 20) - (2u << 20), 4096 + 16, 0) };
    enc(s, wb, 1); ARM("a read starting in the last byte of the written range conflicts", hz_conflict(s, 1));
    // a write ending exactly where a range in the previous 2 MiB bin crosses into this one
    hz_ent cross[] = { K(A, (2u << 20) - 512, (2u << 20) + 512, 1) };
    enc(s, cross, 1); hz_commit(s); hz_close_region(s);
    hz_ent cr[] = { K(A, (2u << 20) + 256, (2u << 20) + 300, 0) };
    enc(s, cr, 1); ARM("an entry that crosses a bin boundary is found from the next bin", hz_conflict(s, 1));
    hz_ent wild[] = { hz_wild(0) };
    enc(s, wild, 1); ARM("a keyless read conflicts once anything earlier wrote", hz_conflict(s, 1));
    hz_reset(s);
    hz_ent rr[] = { K(B, 0, 64, 0) }; enc(s, rr, 1); hz_commit(s); hz_close_region(s);
    enc(s, wild, 1); ARM("a keyless read after earlier reads only does not conflict", !hz_conflict(s, 1));
    hz_ent ww[] = { hz_wild(1) }; enc(s, ww, 1);
    ARM("a keyless write conflicts with any earlier entry", hz_conflict(s, 1));
    // region: entries of the current encoder never conflict with the encoder itself
    hz_reset(s);
    hz_ent own[] = { K(A, 0, 64, 1) }; enc(s, own, 1); hz_commit(s);
    hz_ent own2[] = { K(A, 0, 64, 0) }; enc(s, own2, 1);
    ARM("the current encoder's own write is not a conflict for its later draws", !hz_conflict(s, 1));
    ARM("the same key again in the same encoder is deduplicated, not re-added", s->npend == 0);
    hz_close_region(s); enc(s, own2, 1);
    ARM("after the encoder ends, the next encoder's read of it conflicts", hz_conflict(s, 1));
    // "all" forces a barrier; a reset clears it
    s->all = 1; enc(s, r2, 1); ARM("unknown work since the last barrier forces a conflict", hz_conflict(s, 1));
    hz_reset(s); ARM("a full barrier clears the unknown state", !s->all && !s->n);
    // capacity: overflow turns into 'all', never into a silent miss
    hz_reset(s);
    for (uint32_t i = 0; i < HZ_CAP + 5; i++) { hz_ent x = K(A, (uint64_t)i * 64, (uint64_t)i * 64 + 64, 1); hz_append(s, &x); }
    ARM("overflowing the entry table marks the state unknown", s->all);
    // wrapper: untracked work pays a skipped barrier, then marks unknown; tracked arming is one-shot
    setenv("NVMTL_HAZARD", "census", 1);
    hz_state *t = hz_find((VkCommandBuffer)(uintptr_t)0x2000, 1); t->all = 0;
    hz_ent tw = K(A, 0, 64, 1); hz_append(t, &tw);
    nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x2000, 0);
    ARM("untracked work with entries outstanding pays (model reset) and marks unknown", t->n == 0 && t->all);
    t->all = 0; hz_append(t, &tw); t->arm_disp = 1;
    nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x2000, 0);
    ARM("an armed dispatch does not cover a copy recorded before it", t->n == 0 && t->all && !t->arm_disp);
    t->all = 0; hz_append(t, &tw); t->arm_disp = 1;
    nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x2000, 1);
    ARM("the armed dispatch itself is tracked (no pay)", t->n == 1 && !t->all && !t->arm_disp);
    nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x2000, 1);
    ARM("a second, unchecked dispatch is untracked", t->n == 0 && t->all);
    t->all = 0; t->debt = 1;
    nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x2000, 0);
    ARM("a skipped barrier with only untracked resources touched is still paid by untracked work", t->debt == 0 && t->all);
    nvmtl_hz_end((VkCommandBuffer)(uintptr_t)0x2000);
    ARM("the end of the command buffer drops its state", hz_find((VkCommandBuffer)(uintptr_t)0x2000, 0) == NULL);
    hz_state *u = hz_find((VkCommandBuffer)(uintptr_t)0x3000, 1);
    ARM("a new command buffer starts unknown (earlier buffers are not tracked)", u && u->all);
    { hz_state *v = hz_find((VkCommandBuffer)(uintptr_t)0x4000, 1); v->all = 0;
      hz_ent a = K(A, 0, 64, 1); a.src = HZS_ATT; hz_append(v, &a); hz_close_region(v);
      hz_ent b = K(A, 0, 64, 0); b.src = HZS_TEX; v->npend = 0; hz_pend(v, b);
      char why[512]; nvmtl_vk_hz_reasons(1.0, why, sizeof why);
      ARM("a conflict is counted under its reason (texture slot read after attachment write)",
          hz_conflict(v, 0) && (nvmtl_vk_hz_reasons(1.0, why, sizeof why), strstr(why, "restart: texture slot read after attachment write") != NULL));
      printf("      reasons: %s\n", why); }
    { hz_state *w = hz_find((VkCommandBuffer)(uintptr_t)0x5000, 1); w->all = 0;
      hz_ent a = K(A, 0, 64, 1); a.src = HZS_ATT; hz_append(w, &a); hz_close_region(w);
      w->npend = 0; hz_pend(w, a);
      ARM("a pass writing the attachment the previous pass wrote counts as the same-target reason",
          hz_conflict(w, 1) && atomic_load(&g_hzr[1][HZR_SAMEATT]) == 1);
      nvk_cmdbuf cc; memset(&cc, 0, sizeof cc); cc.cb = (void *)(uintptr_t)0x5000;
      ARM("a blit opens one scope", nvmtl_hz_blit_begin(&cc) == 1);
      ARM("a nested blit does not open a second", nvmtl_hz_blit_begin(&cc) == 0);
      nvk_buffer bb; memset(&bb, 0, sizeof bb); bb.mem = B; bb.size = 256; bb.placed = 1;
      nvmtl_hz_blit_key(&cc, &bb, NULL, 1); nvmtl_hz_blit_go(&cc);
      ARM("a blit to memory nothing touched takes no barrier", atomic_load(&g_hzc[HZC_BLIT_FREE]) == 1);
      nvmtl_hz_work((VkCommandBuffer)(uintptr_t)0x5000, 0);
      ARM("the blit's own copy is tracked work (no pay, no unknown)", !w->all);
      nvmtl_hz_blit_end(&cc);
      w->npend = 0; hz_ent rd = K(B, 0, 64, 0); rd.src = HZS_TEX; hz_pend(w, rd);
      ARM("the next encoder reading what the blit wrote conflicts", hz_conflict(w, 2)); }
    // NVMTL_HAZARD_DISPATCH: later dispatches of one compute encoder against the encoder's own earlier dispatches
    { hz_state *d = hz_find((VkCommandBuffer)(uintptr_t)0x6000, 1); d->all = 0;
      hz_ent w1 = K(A, 0, 4096, 1); d->npend = 0; hz_pend(d, w1); hz_commit(d);   // dispatch 1 writes A[0,4K)
      hz_ent r2 = K(A, 1024, 2048, 0); d->npend = 0; hz_pend(d, r2);
      ARM("NEGATIVE CONTROL: a later dispatch reading what an earlier dispatch of the same encoder wrote MUST get its compute-to-compute barrier",
          hz_conflict_intra(d) == 1);
      hz_c2c_done(d); hz_commit(d);
      hz_ent r3 = K(A, 0, 64, 0); d->npend = 0; hz_pend(d, r3);
      ARM("after that barrier the same read is ordered: no second barrier", hz_conflict_intra(d) == 0);
      hz_commit(d);
      hz_ent rb = K(B, 0, 4096, 0); d->npend = 0; hz_pend(d, rb);
      ARM("a later dispatch reading memory no dispatch of this encoder wrote takes no barrier", hz_conflict_intra(d) == 0);
      hz_commit(d);
      hz_ent wB = K(B, 0, 64, 1); d->npend = 0; hz_pend(d, wB);
      ARM("a later dispatch writing what an earlier one read (write-after-read) MUST get its barrier", hz_conflict_intra(d) == 1);
      hz_c2c_done(d); hz_commit(d);
      hz_ent kw = hz_wild(1); d->npend = 0; hz_pend(d, kw);
      ARM("a keyless write conflicts with the barrier-taking dispatch's own write, which was recorded after the barrier", hz_conflict_intra(d) == 1);
      hz_commit(d);
      hz_ent kr = hz_wild(0); d->npend = 0; hz_pend(d, kr);
      ARM("a keyless read after a keyless write since the barrier conflicts", hz_conflict_intra(d) == 1);
      ARM("the intra check leaves the lower bound cleared for the next inter-encoder check", d->hits_from == 0);
      // the inter-encoder check is untouched by the watermark: the next encoder's read of A still conflicts
      hz_close_region(d); hz_ent nr = K(A, 0, 64, 0); d->npend = 0; hz_pend(d, nr);
      ARM("the next encoder's read of A conflicts across the encoder boundary as before", hz_conflict(d, 2) == 1);
      ARM("an encoder end moves the intra watermark to the end (nothing pending from the old encoder)", d->c2c_k == d->n); }
    // exposure_texture_to_buffer: one thread reads one texel of an image a transfer wrote, in a new encoder
    { hz_state *x = hz_find((VkCommandBuffer)(uintptr_t)0x7000, 1); x->all = 0;
      nvk_cmdbuf cc; memset(&cc, 0, sizeof cc); cc.cb = (void *)(uintptr_t)0x7000;
      nvk_image im; memset(&im, 0, sizeof im); im.mem = B; im.placed = 1; im.hz_off = 0; im.hz_len = 1 << 20;
      nvmtl_hz_blit_begin(&cc); nvmtl_hz_blit_key(&cc, NULL, &im, 1); nvmtl_hz_blit_go(&cc); nvmtl_hz_blit_end(&cc);
      nvmtl_hz_encoder_end(&cc);   // the blit encoder ends; the exposure kernel's encoder begins
      hz_ent rd = hz_key_image(&im, 0); rd.src = HZS_TEX; x->npend = 0; hz_pend(x, rd);
      ARM("exposure case: the first dispatch reading the image the transfer wrote is a conflict - the model keeps its barrier (correct: it must wait for the copy)",
          hz_conflict(x, 2) == 1);
      hz_ent other = K(A, 0, 64, 0); x->npend = 0; hz_pend(x, other);
      ARM("exposure case: a dispatch reading something the transfer did not touch takes no barrier", hz_conflict(x, 2) == 0); }
    printf("%d arms, %d failed\n", arms, fails);
    return fails != 0;
}

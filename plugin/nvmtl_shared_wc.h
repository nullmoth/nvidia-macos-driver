/* nvmtl_shared_wc.h - a VRAM working copy for a Shared Metal buffer that the GPU writes.

   A Shared buffer lives in mapped system memory, so every GPU access to it crosses PCIe (measured 186x slower than VRAM
   on a convolution kernel). Metal only promises coherency between CPU and GPU at command buffer boundaries: the GPU
   sees the CPU's writes made before commit, and the CPU sees the GPU's writes once the command buffer has completed.
   That is enough to give the GPU its own copy in VRAM:

     - the system-memory side stays what contents returns (the "host side");
     - before a command buffer that binds the buffer is submitted, the pages of the host side that changed since the
       last synchronisation are copied to VRAM (the CPU gives no range, unlike a Managed buffer's didModifyRange, so
       the changed pages are found by comparing a 64-bit hash per page with the one taken at the last synchronisation);
     - after that command buffer has completed, the VRAM side is copied back to the host side, because the GPU may have
       written any of it (the plugin does not learn which bindings a kernel writes), and the hashes are retaken so the
       copy-back is not mistaken for a CPU write at the next submit.

   Opt-in (NVMTL_SHARED_VRAM=1), bounded by a size window. Pure over its own state (no Vulkan), so
   tools/test_shared_wc.c proves the coherency rules on the host. */
#ifndef NVMTL_SHARED_WC_H
#define NVMTL_SHARED_WC_H
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

#define NVMTL_SWC_PAGE 4096u

typedef struct nvmtl_swc {
    uint64_t *hash;        /* one hash per page of the host side at the last synchronisation */
    size_t size, pages;
    int vram_valid;        /* 0 until the first upload: the VRAM side then holds nothing the host side has */
    uint64_t uploads, upload_bytes, downloads, download_bytes, clean_submits;
} nvmtl_swc;

/* a byte range of the buffer */
typedef struct nvmtl_swc_range { size_t lo, hi; } nvmtl_swc_range;

static inline uint64_t nvmtl_swc_hash(const void *p, size_t n)
{
    /* 64-bit multiply-xorshift over 8-byte lanes (the tail zero-padded): not cryptographic, but a changed page
       leaves the hash unchanged only with probability 2^-64 for data that is not adversarial to this function */
    const uint8_t *b = (const uint8_t *)p; uint64_t h = 0x9E3779B97F4A7C15ull ^ (uint64_t)n;
    while (n >= 8) { uint64_t v; memcpy(&v, b, 8); h = (h ^ v) * 0xFF51AFD7ED558CCDull; h ^= h >> 32; b += 8; n -= 8; }
    if (n) { uint64_t v = 0; memcpy(&v, b, n); h = (h ^ v) * 0xFF51AFD7ED558CCDull; h ^= h >> 32; }
    h *= 0xC4CEB9FE1A85EC53ull; h ^= h >> 29;
    return h;
}

static inline int nvmtl_swc_init(nvmtl_swc *s, size_t size)
{
    memset(s, 0, sizeof *s);
    if (!size) return -1;
    s->size = size; s->pages = (size + NVMTL_SWC_PAGE - 1) / NVMTL_SWC_PAGE;
    s->hash = (uint64_t *)calloc(s->pages, sizeof(uint64_t));
    return s->hash ? 0 : -1;
}
static inline void nvmtl_swc_free(nvmtl_swc *s) { free(s->hash); memset(s, 0, sizeof *s); }

static inline size_t nvmtl_swc_page_end(const nvmtl_swc *s, size_t page)
{
    size_t e = (page + 1) * (size_t)NVMTL_SWC_PAGE; return e > s->size ? s->size : e;
}

/* retake every page hash from the host side */
static inline void nvmtl_swc_retake(nvmtl_swc *s, const void *host)
{
    for (size_t i = 0; i < s->pages; i++)
        s->hash[i] = nvmtl_swc_hash((const uint8_t *)host + i * NVMTL_SWC_PAGE, nvmtl_swc_page_end(s, i) - i * NVMTL_SWC_PAGE);
}

/* Before a submit: the ranges of the host side that must be copied to VRAM, adjacent changed pages merged, at most
   maxr of them (when there are more, the tail is folded into the last range, which then covers some clean pages: that
   is only extra bytes, never a missing one). Before the first upload the whole buffer is one range. The hashes are
   updated to what is about to be uploaded. Returns the count (0 = the VRAM side is already current). */
static inline int nvmtl_swc_before_submit(nvmtl_swc *s, const void *host, nvmtl_swc_range *out, int maxr)
{
    int n = 0;
    if (!s->vram_valid) {
        nvmtl_swc_retake(s, host); s->vram_valid = 1;
        out[0].lo = 0; out[0].hi = s->size; s->uploads++; s->upload_bytes += s->size; return 1;
    }
    for (size_t i = 0; i < s->pages; i++) {
        const size_t lo = i * NVMTL_SWC_PAGE, hi = nvmtl_swc_page_end(s, i);
        const uint64_t h = nvmtl_swc_hash((const uint8_t *)host + lo, hi - lo);
        if (h == s->hash[i]) continue;
        s->hash[i] = h;
        if (n && out[n - 1].hi == lo) out[n - 1].hi = hi;
        else if (n < maxr) { out[n].lo = lo; out[n].hi = hi; n++; }
        else out[n - 1].hi = hi;
    }
    if (!n) { s->clean_submits++; return 0; }
    s->uploads++;
    for (int i = 0; i < n; i++) s->upload_bytes += out[i].hi - out[i].lo;
    return n;
}

/* After the command buffer completed and the VRAM side was copied back over the whole host side. */
static inline void nvmtl_swc_after_complete(nvmtl_swc *s, const void *host)
{
    nvmtl_swc_retake(s, host); s->downloads++; s->download_bytes += s->size;
}

/* Which buffers qualify. 0 = no. A size window, because a tiny buffer's copy costs more than its PCIe reads and a
   huge one's working copy is not worth the VRAM; the shared kernel-argument and event words the plugin itself makes
   as Shared buffers are never promoted (they are written by the CPU between the same two submits all the time). */
static inline int nvmtl_swc_wants(int enabled, int storage_is_shared, size_t len, size_t lo, size_t hi)
{
    return enabled && storage_is_shared && len >= lo && len <= hi;
}
#endif

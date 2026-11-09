/* nvmtl_mem.h - every VkDeviceMemory this process allocates, with its base GPU address, for a fault report.

   NVK maps a device memory once: a buffer bound at offset 0 answers the memory's base address, and an image bound at
   offset O lives at base + O. So a fault address resolves to a memory (which kind of allocation, how large, created
   and died when) and, through the image binds recorded, to the image at that offset - the part the buffer registry
   cannot see. Pure over its own state; tools/test_slots.c proves it. */
#ifndef NVMTL_MEM_H
#define NVMTL_MEM_H
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

typedef struct { const void *mem; uint64_t base, size, seq, died; uint32_t type; char kind[40]; } nvmtl_mm_entry;
typedef struct { nvmtl_mm_entry *e; uint32_t n, cap; uint64_t live, unbased; } nvmtl_mm_state;
typedef struct { const void *img, *mem; uint64_t off; } nvmtl_mb_entry;
enum { NVMTL_MB_N = 4096 };
typedef struct { nvmtl_mb_entry e[NVMTL_MB_N]; uint32_t n; } nvmtl_mb_state;   /* the last image binds: image -> memory + offset */

/* "ALIVE" or "DIED at seq N" */
static inline const char *nvmtl_mm_liveness(uint64_t died, char *buf, size_t cap) { if (died) snprintf(buf, cap, "DIED at seq %llu", (unsigned long long)died); else snprintf(buf, cap, "ALIVE"); return buf; }
static inline uint32_t nvmtl_mm_add(nvmtl_mm_state *s, const void *mem, uint64_t base, uint64_t size, uint32_t type, const char *kind, uint64_t seq)
{
    if (!mem) return 0;
    if (s->n == s->cap) { const uint32_t cap = s->cap ? s->cap * 2 : 1024; nvmtl_mm_entry *e = (nvmtl_mm_entry *)realloc(s->e, (size_t)cap * sizeof *e); if (!e) return 0; s->e = e; s->cap = cap; }
    nvmtl_mm_entry *e = &s->e[s->n++];
    e->mem = mem; e->base = base; e->size = size; e->type = type; e->seq = seq; e->died = 0; snprintf(e->kind, sizeof e->kind, "%s", kind ? kind : "?");
    s->live++; if (!base) s->unbased++;
    return s->n;
}
/* the newest record of a memory handle, live or dead (handles are reused by the driver) */
static inline nvmtl_mm_entry *nvmtl_mm_by_mem(const nvmtl_mm_state *s, const void *mem)
{
    if (!mem) return NULL;
    for (uint32_t i = s->n; i-- > 0;) if (s->e[i].mem == mem) return &s->e[i];
    return NULL;
}
static inline void nvmtl_mm_died(nvmtl_mm_state *s, const void *mem, uint64_t seq)
{
    nvmtl_mm_entry *e = nvmtl_mm_by_mem(s, mem);
    if (e && !e->died) { e->died = seq ? seq : 1; s->live--; }
}
/* the newest memory, live or dead, whose base range held addr - or NULL: no memory this process mapped */
static inline const nvmtl_mm_entry *nvmtl_mm_at(const nvmtl_mm_state *s, uint64_t addr)
{
    for (uint32_t i = s->n; i-- > 0;) { const nvmtl_mm_entry *e = &s->e[i]; if (e->base && addr >= e->base && addr < e->base + e->size) return e; }
    return NULL;
}
static inline void nvmtl_mm_describe(const nvmtl_mm_state *s, uint64_t addr, char *line, size_t cap)
{
    const nvmtl_mm_entry *e = nvmtl_mm_at(s, addr);
    if (!e) { snprintf(line, cap, "0x%llx is in no memory this process ever mapped (%u memories, %llu without a base address)", (unsigned long long)addr, s->n, (unsigned long long)s->unbased); return; }
    char lv[40]; nvmtl_mm_liveness(e->died, lv, sizeof lv);
    snprintf(line, cap, "0x%llx = %s memory %p (base 0x%llx, size %llu, created seq %llu, %s) + offset %llu", (unsigned long long)addr, e->kind, e->mem,
             (unsigned long long)e->base, (unsigned long long)e->size, (unsigned long long)e->seq, lv, (unsigned long long)(addr - e->base));
}
static inline uint32_t nvmtl_mm_dump(const nvmtl_mm_state *s, FILE *f)
{
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_mm_entry *e = &s->e[i];
        fprintf(f, "memory %s mem %p base 0x%llx size %llu type %u seq %llu died %llu\n", e->kind, e->mem, (unsigned long long)e->base, (unsigned long long)e->size, e->type,
                (unsigned long long)e->seq, (unsigned long long)e->died); }
    return s->n;
}

static inline void nvmtl_mb_note(nvmtl_mb_state *s, const void *img, const void *mem, uint64_t off)
{
    nvmtl_mb_entry *e = &s->e[s->n % NVMTL_MB_N]; e->img = img; e->mem = mem; e->off = off; s->n++;
}
/* the newest bind of an image handle: its memory and offset; 0 when the ring no longer holds it */
static inline int nvmtl_mb_find(const nvmtl_mb_state *s, const void *img, const void **mem, uint64_t *off)
{
    const uint32_t n = s->n < NVMTL_MB_N ? s->n : NVMTL_MB_N;
    for (uint32_t k = 0; k < n; k++) { const nvmtl_mb_entry *e = &s->e[(s->n - 1 - k) % NVMTL_MB_N]; if (e->img == img) { *mem = e->mem; *off = e->off; return 1; } }
    return 0;
}
#endif

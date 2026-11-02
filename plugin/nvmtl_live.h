/* nvmtl_live.h - every live GPU address range the plugin owns, so a fault address can be resolved to its allocation.

   A page-table fault names an address. The fault context already keeps the last deaths; this keeps the living: every
   buffer with a device address (own memory, placed in a heap, pooled, imported, parked, or a binding-table page), from
   creation to destruction, in a slab indexed by a handle the buffer carries. O(1) add and remove, a linear scan to
   resolve (only at device loss), and a dump of the whole table to a file the resolver tool reads against the fault
   address and the dead ranges. Images have no device address in Vulkan, so they are not here: an address that lies in
   no live buffer, no dead buffer and no page is either an image or NVK's own memory, and the report says so. Pure over
   its own state; tools/test_live.c proves it on the host. */
#ifndef NVMTL_LIVE_H
#define NVMTL_LIVE_H
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

enum { NVMTL_LV_BUFFER = 1, NVMTL_LV_PLACED, NVMTL_LV_POOLED, NVMTL_LV_IMPORTED, NVMTL_LV_PAGE, NVMTL_LV_PARKED, NVMTL_LV_KINDS };
static inline const char *nvmtl_lv_kind_name(uint32_t k)
{
    static const char *const n[NVMTL_LV_KINDS] = { "?", "buffer", "placed buffer", "pooled buffer", "imported buffer", "address page", "parked buffer" };
    return k < NVMTL_LV_KINDS ? n[k] : "?";
}
typedef struct { uint64_t lo, hi, seq; uint32_t kind, next_free; } nvmtl_lv_entry;   /* next_free: index + 1 of the next free slot, when free */
typedef struct { nvmtl_lv_entry *e; uint32_t cap, n, free_head, live; uint64_t added, removed, refused; } nvmtl_lv_state;

/* Registers [lo, lo + size) and returns its handle (index + 1), or 0 when lo is 0 or memory ran out. */
static inline uint32_t nvmtl_lv_add(nvmtl_lv_state *s, uint64_t lo, uint64_t size, uint32_t kind, uint64_t seq)
{
    if (!lo) { s->refused++; return 0; }
    uint32_t i;
    if (s->free_head) { i = s->free_head - 1; s->free_head = s->e[i].next_free; }
    else {
        if (s->n == s->cap) {
            const uint32_t cap = s->cap ? s->cap * 2 : 1024;
            nvmtl_lv_entry *e = (nvmtl_lv_entry *)realloc(s->e, (size_t)cap * sizeof *e);
            if (!e) { s->refused++; return 0; }
            s->e = e; s->cap = cap;
        }
        i = s->n++;
    }
    s->e[i].lo = lo; s->e[i].hi = lo + size; s->e[i].kind = kind; s->e[i].seq = seq; s->e[i].next_free = 0;
    s->live++; s->added++;
    return i + 1;
}
static inline void nvmtl_lv_remove(nvmtl_lv_state *s, uint32_t id)
{
    if (!id || id > s->n || !s->e[id - 1].lo) return;
    s->e[id - 1].lo = s->e[id - 1].hi = 0; s->e[id - 1].kind = 0; s->e[id - 1].next_free = s->free_head; s->free_head = id;
    s->live--; s->removed++;
}
static inline void nvmtl_lv_relabel(nvmtl_lv_state *s, uint32_t id, uint32_t kind) { if (id && id <= s->n && s->e[id - 1].lo) s->e[id - 1].kind = kind; }
/* the live range holding addr, or NULL */
static inline const nvmtl_lv_entry *nvmtl_lv_find(const nvmtl_lv_state *s, uint64_t addr)
{
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_lv_entry *e = &s->e[i]; if (e->lo && addr >= e->lo && addr < e->hi) return e; }
    return NULL;
}
/* one line per live range: "live <kind> 0x<lo> 0x<hi> <bytes> seq <made>"; returns the count */
static inline uint32_t nvmtl_lv_dump(const nvmtl_lv_state *s, FILE *f)
{
    uint32_t n = 0;
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_lv_entry *e = &s->e[i]; if (!e->lo) continue;
        fprintf(f, "live %s 0x%llx 0x%llx %llu seq %llu\n", nvmtl_lv_kind_name(e->kind), (unsigned long long)e->lo, (unsigned long long)e->hi,
                (unsigned long long)(e->hi - e->lo), (unsigned long long)e->seq); n++; }
    return n;
}
/* "live buffer 0x.. .. 0x.. (65536 bytes, made at seq 7000)" into line, or "no live buffer or page holds 0x.." */
static inline void nvmtl_lv_describe(const nvmtl_lv_state *s, uint64_t addr, char *line, size_t cap)
{
    const nvmtl_lv_entry *e = nvmtl_lv_find(s, addr);
    if (e) snprintf(line, cap, "live %s 0x%llx..0x%llx (%llu bytes, made at seq %llu)", nvmtl_lv_kind_name(e->kind), (unsigned long long)e->lo,
                    (unsigned long long)e->hi, (unsigned long long)(e->hi - e->lo), (unsigned long long)e->seq);
    else snprintf(line, cap, "no live buffer or page holds 0x%llx", (unsigned long long)addr);
}
#endif

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

enum { NVMTL_LV_BUFFER = 1, NVMTL_LV_PLACED, NVMTL_LV_POOLED, NVMTL_LV_IMPORTED, NVMTL_LV_PAGE, NVMTL_LV_PARKED, NVMTL_LV_DYING, NVMTL_LV_KINDS };
static inline const char *nvmtl_lv_kind_name(uint32_t k)
{
    static const char *const n[NVMTL_LV_KINDS] = { "?", "buffer", "placed buffer", "pooled buffer", "imported buffer", "address page", "parked buffer", "dying buffer" };
    return k < NVMTL_LV_KINDS ? n[k] : "?";
}
typedef struct { uint64_t lo, hi, seq, died; uint32_t kind, next_free; const void *map; } nvmtl_lv_entry;   /* died: the submit sequence at removal, 0 while live; map: host view when host-visible */
typedef struct { nvmtl_lv_entry *e; uint32_t cap, n, free_head, live; uint64_t added, removed, refused; } nvmtl_lv_state;
/* Provenance: a removed range is kept, marked with the sequence it died at, and its slot is never reused, so the table is
   the history of every GPU range this process ever mapped - what says of a fault address 'this WAS buffer X, died at
   seq S' long after the dead ring has forgotten it. It grows by doubling; 100k entries are a few MB. */

/* Registers [lo, lo + size) and returns its handle (index + 1), or 0 when lo is 0 or memory ran out. */
static inline uint32_t nvmtl_lv_add(nvmtl_lv_state *s, uint64_t lo, uint64_t size, uint32_t kind, uint64_t seq, const void *map)
{
    if (!lo) { s->refused++; return 0; }
    uint32_t i;
    {
        if (s->n == s->cap) {
            const uint32_t cap = s->cap ? s->cap * 2 : 1024;
            nvmtl_lv_entry *e = (nvmtl_lv_entry *)realloc(s->e, (size_t)cap * sizeof *e);
            if (!e) { s->refused++; return 0; }
            s->e = e; s->cap = cap;
        }
        i = s->n++;
    }
    s->e[i].lo = lo; s->e[i].hi = lo + size; s->e[i].kind = kind; s->e[i].seq = seq; s->e[i].died = 0; s->e[i].next_free = 0; s->e[i].map = map;
    s->live++; s->added++;
    return i + 1;
}
/* the range dies at submit sequence seq: it stays in the table as history */
static inline void nvmtl_lv_remove(nvmtl_lv_state *s, uint32_t id, uint64_t seq)
{
    if (!id || id > s->n || !s->e[id - 1].lo || s->e[id - 1].died) return;
    s->e[id - 1].died = seq ? seq : 1; s->e[id - 1].map = NULL;
    s->live--; s->removed++;
}
static inline void nvmtl_lv_relabel(nvmtl_lv_state *s, uint32_t id, uint32_t kind) { if (id && id <= s->n && s->e[id - 1].lo && !s->e[id - 1].died) s->e[id - 1].kind = kind; }
/* the live range holding addr, or NULL */
static inline const nvmtl_lv_entry *nvmtl_lv_find(const nvmtl_lv_state *s, uint64_t addr)
{
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_lv_entry *e = &s->e[i]; if (e->lo && !e->died && addr >= e->lo && addr < e->hi) return e; }
    return NULL;
}
/* the newest range, live or dead, that ever held addr - or NULL: the address was never mapped by this process's buffers */
static inline const nvmtl_lv_entry *nvmtl_lv_find_ever(const nvmtl_lv_state *s, uint64_t addr)
{
    for (uint32_t i = s->n; i-- > 0;) { const nvmtl_lv_entry *e = &s->e[i]; if (e->lo && addr >= e->lo && addr < e->hi) return e; }
    return NULL;
}
/* one line per range ever registered: "live <kind> 0x<lo> 0x<hi> <bytes> seq <made>" or "was <kind> ... seq <made> died <seq>"; returns the live count */
static inline uint32_t nvmtl_lv_dump(const nvmtl_lv_state *s, FILE *f)
{
    uint32_t n = 0;
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_lv_entry *e = &s->e[i]; if (!e->lo) continue;
        if (e->died) fprintf(f, "was %s 0x%llx 0x%llx %llu seq %llu died %llu\n", nvmtl_lv_kind_name(e->kind), (unsigned long long)e->lo, (unsigned long long)e->hi,
                             (unsigned long long)(e->hi - e->lo), (unsigned long long)e->seq, (unsigned long long)e->died);
        else { fprintf(f, "live %s 0x%llx 0x%llx %llu seq %llu\n", nvmtl_lv_kind_name(e->kind), (unsigned long long)e->lo, (unsigned long long)e->hi,
                       (unsigned long long)(e->hi - e->lo), (unsigned long long)e->seq); n++; } }
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

/* nvmtl_slots.h - the bindless heap's slots and the images behind them, for a fault report.

   A kernel reaches a texture it writes through a heap slot: the slot index comes out of app memory, and the slot's
   storage-image descriptor (binding 2) names the image. A fault at an address no buffer ever held is then one of: the
   slot's descriptor still naming an image that has died, a slot whose bindings disagree (sampled and storage views of
   different images after a re-back or a reuse), or a slot nobody ever wrote. This keeps, per slot, the last view
   written to each binding and when, the slot's allocation count and drop time; per image, its handle, memory, size,
   shape and lifetime; and which image each heap-bound view belongs to. The report runs at device loss for the slot
   a dying dispatch indexed, and a census over every slot says how many disagree. Pure over its own state; tools/
   test_slots.c proves the verdicts, including the stale-binding negative control. */
#ifndef NVMTL_SLOTS_H
#define NVMTL_SLOTS_H
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include "nvmtl_mem.h"

enum { NVMTL_SL_NB = 5 };
typedef void (*nvmtl_sl_emit)(void *ctx, const char *line);
typedef struct { const void *view; uint64_t seq; } nvmtl_sl_bind;                       /* the last descriptor written to a binding, and when */
typedef struct { nvmtl_sl_bind b[NVMTL_SL_NB]; uint32_t allocs, state; uint64_t alloc_seq, drop_seq; } nvmtl_sl_slot;   /* state: 0 never allocated, 1 live, 2 dropped */
typedef struct { nvmtl_sl_slot *s; uint32_t n; uint64_t writes, allocs, drops; } nvmtl_sl_state;

typedef struct { const void *img, *mem, *view; uint64_t off, size, seq, died, parked, avail; uint32_t w, h, fmt, storage, placed, pooled, layers, mips, flags, usage, samples, chain; } nvmtl_im_entry;
/* parked: in a heap/buffer park since this seq, 0 when in use; size: the memory the image needs (its requirement); avail: the memory it was given from its bind offset (0 = unknown) */
/* heads/chain: a hash index over the append-only history - heads[bucket] is 1 + the newest entry of that bucket, each
   entry's chain is 1 + the next older one. Without it every image death, park and reuse scanned the WHOLE history under
   g_sl_lock: Cyberpunk creates and frees textures every frame, the history only grows, and on 11-07 ~28% of the game's
   main-thread samples sat in that scan inside -[NVMTLTexture dealloc] (the 100-135 ms hitches). */
enum { NVMTL_IM_BUCKETS = 16384 };
typedef struct { nvmtl_im_entry *e; uint32_t n, cap; uint64_t live; uint32_t heads[NVMTL_IM_BUCKETS]; } nvmtl_im_state;
static inline uint32_t nvmtl_im_bucket(const void *img)
{
    uint64_t key = (uint64_t)(uintptr_t)img;
    key ^= key >> 33; key *= UINT64_C(0xff51afd7ed558ccd); key ^= key >> 33;
    return (uint32_t)key & (NVMTL_IM_BUCKETS - 1);
}
typedef struct { const void *view, *img; uint32_t type, base_layer, layers, base_mip, mips; uint32_t s_idx, t_idx; uint64_t s_addr, t_addr, seq, died; uint32_t live_next; } nvmtl_vw_entry;
/* type: VkImageViewType + 1 (0 = not recorded); s_/t_: the sampled and storage descriptor indices and the header addresses the view carried (0 = not read); seq/died: created / destroyed */
enum { NVMTL_VW_LIVE_BUCKETS = 4096 };
/* The history remains append-only. Only live records participate in the hot-path index.
   Indices, rather than pointers, survive growth of the history allocation. Callers retain
   the existing state lock for both the records and the index. */
typedef struct { nvmtl_vw_entry *e; uint32_t n, cap, indexed_n, live_heads[NVMTL_VW_LIVE_BUCKETS]; } nvmtl_vw_state;
static inline uint32_t nvmtl_vw_live_bucket(const void *view)
{
    uint64_t key = (uint64_t)(uintptr_t)view;
    key ^= key >> 33; key *= UINT64_C(0xff51afd7ed558ccd); key ^= key >> 33;
    return (uint32_t)key & (NVMTL_VW_LIVE_BUCKETS - 1);
}
/* Also accepts a state populated before its first indexed operation. No extra allocation
   can fail here; a failed history growth leaves the previous records and index intact. */
static inline void nvmtl_vw_live_sync(nvmtl_vw_state *s)
{
    if (s->indexed_n == s->n) return;
    memset(s->live_heads, 0, sizeof s->live_heads);
    for (uint32_t i = 0; i < s->n; i++) {
        nvmtl_vw_entry *e = &s->e[i]; e->live_next = 0;
        if (!e->died && e->view) {
            uint32_t b = nvmtl_vw_live_bucket(e->view);
            e->live_next = s->live_heads[b]; s->live_heads[b] = i + 1;
        }
    }
    s->indexed_n = s->n;
}

static inline int nvmtl_sl_init(nvmtl_sl_state *s, uint32_t n)
{
    if (s->s) return 0;
    s->s = (nvmtl_sl_slot *)calloc(n, sizeof *s->s); s->n = s->s ? n : 0;
    return s->s ? 0 : -1;
}
static inline void nvmtl_sl_alloc(nvmtl_sl_state *s, uint32_t slot, uint64_t seq)
{
    if (!s->s || slot >= s->n) return;
    nvmtl_sl_slot *x = &s->s[slot]; x->allocs++; x->state = 1; x->alloc_seq = seq; s->allocs++;
}
static inline void nvmtl_sl_write(nvmtl_sl_state *s, uint32_t slot, uint32_t binding, const void *view, uint64_t seq)
{
    if (!s->s || slot >= s->n || binding >= NVMTL_SL_NB) return;
    s->s[slot].b[binding].view = view; s->s[slot].b[binding].seq = seq; s->writes++;
}
static inline void nvmtl_sl_drop(nvmtl_sl_state *s, uint32_t slot, uint64_t seq)
{
    if (!s->s || slot >= s->n) return;
    s->s[slot].state = 2; s->s[slot].drop_seq = seq; s->drops++;
}

static inline uint32_t nvmtl_im_add(nvmtl_im_state *s, const void *img, const void *mem, uint64_t off, uint64_t size, const void *view,
                                    uint32_t w, uint32_t h, uint32_t fmt, uint32_t storage, uint32_t placed, uint32_t pooled, uint64_t seq)
{
    if (!img) return 0;
    if (s->n == s->cap) { const uint32_t cap = s->cap ? s->cap * 2 : 4096; nvmtl_im_entry *e = (nvmtl_im_entry *)realloc(s->e, (size_t)cap * sizeof *e); if (!e) return 0; s->e = e; s->cap = cap; }
    nvmtl_im_entry *e = &s->e[s->n++];
    e->img = img; e->mem = mem; e->off = off; e->size = size; e->view = view; e->w = w; e->h = h; e->fmt = fmt; e->storage = storage; e->placed = placed; e->pooled = pooled; e->seq = seq; e->died = 0; e->parked = 0;
    { const uint32_t b = nvmtl_im_bucket(img); e->chain = s->heads[b]; s->heads[b] = s->n; }   /* s->n is already 1 + this entry */
    s->live++;
    return s->n;
}
/* the newest entry for an image handle, live or dead, or NULL */
static inline nvmtl_im_entry *nvmtl_im_by_img(const nvmtl_im_state *s, const void *img)
{
    if (!img) return NULL;
    for (uint32_t i = s->heads[nvmtl_im_bucket(img)]; i && i <= s->n; i = s->e[i - 1].chain) if (s->e[i - 1].img == img) return &s->e[i - 1];
    return NULL;
}
static inline void nvmtl_im_died(nvmtl_im_state *s, const void *img, uint64_t seq)
{
    nvmtl_im_entry *e = nvmtl_im_by_img(s, img);
    if (e && !e->died) { e->died = seq ? seq : 1; s->live--; }
}
/* a texture object died but its image went to a park for reuse (alive, mapped, recyclable); and the reuse */
static inline void nvmtl_im_parked(nvmtl_im_state *s, const void *img, uint64_t seq) { nvmtl_im_entry *e = nvmtl_im_by_img(s, img); if (e && !e->died) e->parked = seq ? seq : 1; }
static inline void nvmtl_im_recycled(nvmtl_im_state *s, const void *img) { nvmtl_im_entry *e = nvmtl_im_by_img(s, img); if (e) e->parked = 0; }
static inline void nvmtl_vw_add_ex(nvmtl_vw_state *s, const void *view, const void *img, uint32_t type1, uint32_t base_layer, uint32_t layers, uint32_t base_mip, uint32_t mips)
{
    if (!view || !img) return;
    if (s->n == s->cap) { const uint32_t cap = s->cap ? s->cap * 2 : 4096; nvmtl_vw_entry *e = (nvmtl_vw_entry *)realloc(s->e, (size_t)cap * sizeof *e); if (!e) return; s->e = e; s->cap = cap; }
    nvmtl_vw_live_sync(s);
    nvmtl_vw_entry *e = &s->e[s->n++]; e->view = view; e->img = img; e->type = type1; e->base_layer = base_layer; e->layers = layers; e->base_mip = base_mip; e->mips = mips;
    e->s_idx = e->t_idx = 0; e->s_addr = e->t_addr = 0; e->seq = 0; e->died = 0;
    uint32_t b = nvmtl_vw_live_bucket(view);
    e->live_next = s->live_heads[b]; s->live_heads[b] = s->n; s->indexed_n = s->n;
}
/* the newest live record of a view handle, for the recorder to fill */
static inline nvmtl_vw_entry *nvmtl_vw_find_live(nvmtl_vw_state *s, const void *view)
{
    if (!view) return NULL;
    nvmtl_vw_live_sync(s);
    for (uint32_t id = s->live_heads[nvmtl_vw_live_bucket(view)]; id; id = s->e[id - 1].live_next)
        if (s->e[id - 1].view == view && !s->e[id - 1].died) return &s->e[id - 1];
    return NULL;
}
static inline void nvmtl_vw_set_tic(nvmtl_vw_state *s, const void *view, uint32_t s_idx, uint64_t s_addr, uint32_t t_idx, uint64_t t_addr, uint64_t seq)
{ nvmtl_vw_entry *e = nvmtl_vw_find_live(s, view); if (e) { e->s_idx = s_idx; e->s_addr = s_addr; e->t_idx = t_idx; e->t_addr = t_addr; e->seq = seq; } }
/* a destroyed view handle: every live record of it dies (the driver reissues handles; older records of the same handle are earlier lives) */
static inline void nvmtl_vw_died(nvmtl_vw_state *s, const void *view, uint64_t seq)
{
    if (!view) return;
    nvmtl_vw_live_sync(s);
    uint32_t *next = &s->live_heads[nvmtl_vw_live_bucket(view)];
    while (*next) {
        nvmtl_vw_entry *e = &s->e[*next - 1];
        if (e->view == view) {
            if (!e->died) e->died = seq ? seq : 1;
            *next = e->live_next; e->live_next = 0;
        } else next = &e->live_next;
    }
}
/* one line per view record: "tic view V image I sidx N saddr 0x.. tidx M taddr 0x.. type T layers b+n mips b+n seq A died B" */
static inline uint32_t nvmtl_vw_dump(const nvmtl_vw_state *s, FILE *f)
{
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_vw_entry *e = &s->e[i];
        fprintf(f, "tic view %p image %p sidx %u saddr 0x%llx tidx %u taddr 0x%llx type %u layers %u+%u mips %u+%u seq %llu died %llu\n", e->view, e->img, e->s_idx, (unsigned long long)e->s_addr,
                e->t_idx, (unsigned long long)e->t_addr, e->type ? e->type - 1 : 0, e->base_layer, e->layers, e->base_mip, e->mips, (unsigned long long)e->seq, (unsigned long long)e->died); }
    return s->n;
}
/* Every view that ever held descriptor index idx (either role), oldest first: the index's history. Emits one line per
   holder and a verdict; returns how many holders. */
static inline unsigned nvmtl_vw_index_history(const nvmtl_vw_state *vw, const nvmtl_im_state *im, uint32_t idx, const void *now_view, nvmtl_sl_emit emit, void *ctx)
{
    char line[400]; unsigned n = 0; uint64_t prev_addr = 0; const void *prev_view = NULL;
    if (!idx) return 0;
    for (uint32_t i = 0; i < vw->n; i++) {
        const nvmtl_vw_entry *e = &vw->e[i]; const int st = e->t_idx == idx, sa = e->s_idx == idx;
        if (!st && !sa) continue;
        const nvmtl_im_entry *ie = nvmtl_im_by_img(im, e->img);
        char end[24]; if (e->died) snprintf(end, sizeof end, "%llu", (unsigned long long)e->died); else snprintf(end, sizeof end, "alive");
        snprintf(line, sizeof line, "fault context:     table[%u] held by view %p (%s) of image %p%s: header address 0x%llx, seq %llu..%s%s", idx, e->view, st ? "storage" : "sampled", e->img,
                 ie ? "" : " (image unknown)", (unsigned long long)(st ? e->t_addr : e->s_addr), (unsigned long long)e->seq, end, e->view == now_view ? " <- the current holder" : "");
        if (ie) { size_t l = strlen(line); snprintf(line + l, sizeof line - l, " [%ux%u fmt %u, image %s]", ie->w, ie->h, ie->fmt, ie->died ? "DIED" : "alive"); }
        emit(ctx, line); n++;
        if (e->view != now_view) { prev_addr = st ? e->t_addr : e->s_addr; prev_view = e->view; }
    }
    if (n > 1 && prev_view) snprintf(line, sizeof line, "fault context:     verdict: index %u was REISSUED %u time%s; the previous holder's bytes began at 0x%llx - a fault there is a STALE HEADER (the GPU still used the previous holder's header)", idx, n - 1, n == 2 ? "" : "s", (unsigned long long)prev_addr);
    else if (n == 1) snprintf(line, sizeof line, "fault context:     verdict: index %u is on its first use", idx);
    else snprintf(line, sizeof line, "fault context:     verdict: index %u has no recorded holder", idx);
    emit(ctx, line);
    return n;
}
static inline void nvmtl_vw_add(nvmtl_vw_state *s, const void *view, const void *img) { nvmtl_vw_add_ex(s, view, img, 0, 0, 0, 0, 0); }
/* the newest view record for a view handle, or NULL */
static inline const nvmtl_vw_entry *nvmtl_vw_find(const nvmtl_vw_state *s, const void *view)
{
    if (!view) return NULL;
    for (uint32_t i = s->n; i-- > 0;) if (s->e[i].view == view) return &s->e[i];
    return NULL;
}
/* the image's creation parameters and the memory it was given: an image whose requirement exceeds what its bind offers is UNDERSIZED */
static inline void nvmtl_im_set_params(nvmtl_im_state *s, uint32_t id, uint32_t layers, uint32_t mips, uint32_t flags, uint32_t usage, uint32_t samples, uint64_t avail)
{
    if (!id || id > s->n) return;
    nvmtl_im_entry *e = &s->e[id - 1]; e->layers = layers; e->mips = mips; e->flags = flags; e->usage = usage; e->samples = samples; e->avail = avail;
}
static inline int nvmtl_im_undersized(const nvmtl_im_entry *e) { return e->avail && e->size > e->avail; }
/* the image a heap-bound view belongs to (newest pairing, so a reused handle resolves to its latest image), or NULL */
static inline const nvmtl_im_entry *nvmtl_im_by_view(const nvmtl_im_state *im, const nvmtl_vw_state *vw, const void *view)
{
    if (!view) return NULL;
    for (uint32_t i = vw->n; i-- > 0;) if (vw->e[i].view == view) return nvmtl_im_by_img(im, vw->e[i].img);
    return NULL;
}
static inline uint32_t nvmtl_im_dump(const nvmtl_im_state *s, FILE *f)
{
    for (uint32_t i = 0; i < s->n; i++) { const nvmtl_im_entry *e = &s->e[i];
        fprintf(f, "image %p view %p mem %p off %llu size %llu %ux%u fmt %u%s%s%s seq %llu died %llu layers %u mips %u samples %u flags 0x%x usage 0x%x avail %llu%s\n", e->img, e->view, e->mem, (unsigned long long)e->off,
                (unsigned long long)e->size, e->w, e->h, e->fmt, e->storage ? " storage" : "", e->placed ? " placed" : "", e->pooled ? " pooled" : "",
                (unsigned long long)e->seq, (unsigned long long)e->died, e->layers, e->mips, e->samples, e->flags, e->usage, (unsigned long long)e->avail, nvmtl_im_undersized(e) ? " UNDERSIZED" : "");
        if (e->parked) fprintf(f, "image %p parked since seq %llu\n", e->img, (unsigned long long)e->parked); }
    return s->n;
}

/* The verdict flags of nvmtl_sl_report */
enum { NVMTL_SL_MISMATCH = 1, NVMTL_SL_DEAD = 2, NVMTL_SL_NEVER = 4, NVMTL_SL_DROPPED = 8, NVMTL_SL_UNWRITTEN = 16 };
typedef struct { const void *v[NVMTL_SL_NB]; } nvmtl_sl_dummies;   /* the dummy descriptor of each binding (NULL = none) */
static inline int nvmtl_sl_is_dummy(const nvmtl_sl_dummies *d, uint32_t b, const void *view) { return view && d->v[b] == view; }

/* Per-binding verdict flags for one slot, without emitting anything. */
static inline unsigned nvmtl_sl_flags(const nvmtl_sl_state *sl, const nvmtl_im_state *im, const nvmtl_vw_state *vw, uint32_t slot, const nvmtl_sl_dummies *d)
{
    if (!sl->s || slot >= sl->n) return NVMTL_SL_NEVER;
    const nvmtl_sl_slot *x = &sl->s[slot]; unsigned f = 0;
    if (!x->state) f |= NVMTL_SL_NEVER;
    if (x->state == 2) f |= NVMTL_SL_DROPPED;
    const nvmtl_im_entry *i0 = NULL, *i2 = NULL;
    for (uint32_t b = 0; b < NVMTL_SL_NB; b++) {
        const void *v = x->b[b].view;
        if (!v) { if (b == 2) f |= NVMTL_SL_UNWRITTEN; continue; }
        if (nvmtl_sl_is_dummy(d, b, v)) continue;
        const nvmtl_im_entry *e = nvmtl_im_by_view(im, vw, v);
        if (e && e->died) f |= NVMTL_SL_DEAD;
        if (b == 0) i0 = e; if (b == 2) i2 = e;
    }
    if (i0 && i2 && i0->img != i2->img) f |= NVMTL_SL_MISMATCH;
    return f;
}

/* The slot's full state, one line per binding plus a verdict line; `owner` is the caller's description of the live
   texture object holding the slot (may be ""). Returns the verdict flags. */
static inline unsigned nvmtl_sl_report(const nvmtl_sl_state *sl, const nvmtl_im_state *im, const nvmtl_vw_state *vw, uint32_t slot, const nvmtl_sl_dummies *d,
                                       const char *owner, const nvmtl_mm_state *mm, nvmtl_sl_emit emit, void *ctx)
{
    char line[512];
    if (!sl->s || slot >= sl->n) { snprintf(line, sizeof line, "fault context: heap slot %u: outside the heap (%u slots)", slot, sl->n); emit(ctx, line); return NVMTL_SL_NEVER; }
    const nvmtl_sl_slot *x = &sl->s[slot];
    snprintf(line, sizeof line, "fault context: heap slot %u: %s, allocated %u time%s (last at seq %llu)%s%s%s%s", slot,
             x->state == 1 ? "LIVE" : x->state == 2 ? "DROPPED" : "NEVER ALLOCATED", x->allocs, x->allocs == 1 ? "" : "s", (unsigned long long)x->alloc_seq,
             x->state == 2 ? ", dropped at seq " : "", x->state == 2 ? "" : "", owner && *owner ? "; owner: " : "", owner && *owner ? owner : "");
    if (x->state == 2) { size_t l = strlen(line); snprintf(line + l, sizeof line - l, "%llu", (unsigned long long)x->drop_seq); }
    emit(ctx, line);
    for (uint32_t b = 0; b < NVMTL_SL_NB; b++) {
        const void *v = x->b[b].view;
        if (!v) { snprintf(line, sizeof line, "fault context:   binding %u: never written%s", b, b == 2 ? " (a storage write through it reads an undefined descriptor)" : ""); emit(ctx, line); continue; }
        if (nvmtl_sl_is_dummy(d, b, v)) { snprintf(line, sizeof line, "fault context:   binding %u: the dummy (%s seq %llu)", b, x->b[b].seq ? "written at" : "prefilled at creation,", (unsigned long long)x->b[b].seq); emit(ctx, line); continue; }
        const nvmtl_im_entry *e = nvmtl_im_by_view(im, vw, v);
        if (!e) snprintf(line, sizeof line, "fault context:   binding %u: view %p written at seq %llu - of no image this table knows", b, v, (unsigned long long)x->b[b].seq);
        else { char lv[64]; if (e->died) snprintf(lv, sizeof lv, "DIED at seq %llu", (unsigned long long)e->died); else if (e->parked) snprintf(lv, sizeof lv, "ALIVE but PARKED for reuse since seq %llu", (unsigned long long)e->parked); else snprintf(lv, sizeof lv, "ALIVE");
               snprintf(line, sizeof line, "fault context:   binding %u: view %p written at seq %llu -> image %p %ux%u fmt %u%s%s%s mem %p+%llu %llu bytes, created seq %llu, %s",
                      b, v, (unsigned long long)x->b[b].seq, e->img, e->w, e->h, e->fmt, e->storage ? " storage" : "", e->placed ? " placed" : "", e->pooled ? " pooled" : "",
                      e->mem, (unsigned long long)e->off, (unsigned long long)e->size, (unsigned long long)e->seq, lv); }
        emit(ctx, line);
        if (e) {   /* what the image is, what it needs, what it got; what the view covers */
            const nvmtl_vw_entry *ve = nvmtl_vw_find(vw, v);
            snprintf(line, sizeof line, "fault context:     image: %u layer%s, %u mip%s, %u sample%s, flags 0x%x, usage 0x%x; needs %llu bytes, given %llu from its bind offset%s; view: %s%u, layers %u+%u, mips %u+%u",
                     e->layers, e->layers == 1 ? "" : "s", e->mips, e->mips == 1 ? "" : "s", e->samples ? e->samples : 1, e->samples == 1 || !e->samples ? "" : "s", e->flags, e->usage,
                     (unsigned long long)e->size, (unsigned long long)e->avail, nvmtl_im_undersized(e) ? " - UNDERSIZED BIND: the requirement exceeds the memory from the bind offset" : e->avail ? "" : " (memory size unknown)",
                     ve && ve->type ? "VkImageViewType " : "type ", ve && ve->type ? ve->type - 1 : 0, ve ? ve->base_layer : 0, ve ? ve->layers : 0, ve ? ve->base_mip : 0, ve ? ve->mips : 0);
            emit(ctx, line);
        }
        if (e && mm) {   /* where that image's bytes are: the memory it is bound into, its base address, and whether that memory still lives */
            const nvmtl_mm_entry *m = nvmtl_mm_by_mem(mm, e->mem);
            if (!m) snprintf(line, sizeof line, "fault context:     its memory %p is not in the memory table", e->mem);
            else { char lv[40]; nvmtl_mm_liveness(m->died, lv, sizeof lv);
                   snprintf(line, sizeof line, "fault context:     its memory %p: %s, base 0x%llx, size %llu, created seq %llu, %s -> the image's bytes start at 0x%llx",
                            e->mem, m->kind, (unsigned long long)m->base, (unsigned long long)m->size, (unsigned long long)m->seq, lv, (unsigned long long)(m->base + e->off)); }
            emit(ctx, line);
        }
    }
    const unsigned f = nvmtl_sl_flags(sl, im, vw, slot, d);
    snprintf(line, sizeof line, "fault context:   verdict: %s%s%s%s%s%s", f & NVMTL_SL_MISMATCH ? "MISMATCH (bindings 0 and 2 name different images) " : "",
             f & NVMTL_SL_DEAD ? "DEAD (a binding names an image that died) " : "", f & NVMTL_SL_NEVER ? "NEVER ALLOCATED " : "",
             f & NVMTL_SL_DROPPED ? "DROPPED (in quarantine or free) " : "", f & NVMTL_SL_UNWRITTEN ? "BINDING 2 UNWRITTEN " : "", f ? "" : "OK (consistent, alive)");
    emit(ctx, line);
    return f;
}
/* Over every slot: how many are live, and how many carry a stale or disagreeing descriptor. One line. */
static inline void nvmtl_sl_census(const nvmtl_sl_state *sl, const nvmtl_im_state *im, const nvmtl_vw_state *vw, const nvmtl_sl_dummies *d, nvmtl_sl_emit emit, void *ctx)
{
    char line[400]; uint32_t live = 0, dropped = 0, mismatch = 0, dead = 0, unwritten2 = 0, dropped_nondummy = 0;
    for (uint32_t k = 0; sl->s && k < sl->n; k++) {
        const nvmtl_sl_slot *x = &sl->s[k]; if (!x->state) continue;
        const unsigned f = nvmtl_sl_flags(sl, im, vw, k, d);
        if (x->state == 1) live++; else dropped++;
        if (f & NVMTL_SL_MISMATCH) mismatch++; if (f & NVMTL_SL_DEAD) dead++;
        if (x->state == 1 && (f & NVMTL_SL_UNWRITTEN)) unwritten2++;
        if (x->state == 2) for (uint32_t b = 0; b < NVMTL_SL_NB; b++) if (x->b[b].view && !nvmtl_sl_is_dummy(d, b, x->b[b].view)) { dropped_nondummy++; break; }
    }
    snprintf(line, sizeof line, "fault context: heap slot census: %u live, %u dropped; %u with bindings 0/2 naming different images, %u naming a dead image, %u live without a storage descriptor, %u dropped slots still holding a non-dummy descriptor; %llu images recorded (%llu alive)",
             live, dropped, mismatch, dead, unwritten2, dropped_nondummy, (unsigned long long)im->n, (unsigned long long)im->live);
    emit(ctx, line);
}
#endif

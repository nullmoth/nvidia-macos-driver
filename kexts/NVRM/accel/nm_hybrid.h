/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Hybrid laptops: frames composited on the NVIDIA GPU reach a panel that is wired to another GPU (Intel or AMD).
 *
 * This header is the logic of that path, in plain C with no kernel headers, so tools/test_hybrid_present.c runs the same
 * code on the host. The kernel pieces (NVAccel's display pipe, NMIntelFB, the NMXGPU framebuffer) only call it.
 *
 * One switch, default off: the boot-arg  nmhybrid=<level>
 *   0 or absent  nothing here runs; every code path behaves exactly as it did before this header existed
 *   1            copy: each composited frame is written into the other GPU's scan-out buffer; only the pixels that
 *                changed since the previous frame are written
 *   2            import: the composited surface's own pages are mapped into the display GPU and the plane is flipped to
 *                them (no pixel copy). Any surface that cannot be imported falls back to level 1 for that frame, and a
 *                flip that never lands switches level 2 off for the rest of the boot.
 *   3            as 2, and a surface that cannot be imported is written into one of three buffers the panel driver owns
 *                (ordinary cached memory mapped into its graphics table) which is then flipped to; no write-combined
 *                stores and no tearing, for frames import refuses (10-bit login window, odd pitch).
 *   4            as 3, and a frame whose newest pixels are still in the GPU's memory is written into an owned buffer by the
 *                copy engine (a DMA into the buffer's pages), with no CPU copy and no mapping of the application's surface.
 * Any other value is treated as 0.
 */
#ifndef NM_HYBRID_H
#define NM_HYBRID_H

#include <stdint.h>
#include "nv_iop_pixel.h"

#define NMH_FMT_BGRA NV_IOP_FMT_BGRA
#define NMH_FMT_L10R NV_IOP_FMT_L10R

/* ---------------------------------------------------------------------------------------------------------------------
 * the switch
 */
enum { NMH_OFF = 0, NMH_COPY = 1, NMH_IMPORT = 2, NMH_OWNED = 3, NMH_DMA = 4 };

/* 'present' is the boot-arg parse result, 'value' what it parsed to. */
static inline int nmh_level_parse(int present, long long value)
{
    if (!present) return NMH_OFF;
    return (value >= NMH_COPY && value <= NMH_DMA) ? (int)value : NMH_OFF;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * why a frame or a surface was refused: these strings are what the kernel logs after "hybrid: fallback to copy: ", and
 * what tools/hybrid_verify.sh and docs/HYBRID-LAPTOPS.md name.
 */
enum nmh_why {
    NMH_OK = 0,
    NMH_E_LEVEL,       /* import is not enabled (nmhybrid is below 2) */
    NMH_E_FMT,         /* not 8-bit BGRA (an l10r login window frame needs the narrowing copy) */
    NMH_E_SIZE,        /* the surface is not the size of the scan-out */
    NMH_E_PITCH,       /* row pitch not a multiple of 64, or past the plane's stride field */
    NMH_E_PAGES,       /* the surface's memory is not whole pages, or has no page list */
    NMH_E_RANGE,       /* a page address the display GPU cannot reach */
    NMH_E_GEN,         /* a display generation whose GGTT layout this code does not know */
    NMH_E_NOSPACE,     /* no unused GGTT range to map into */
    NMH_E_BUSY,        /* every import slot is on screen or about to be */
    NMH_E_STUCK,       /* a flip never reached the screen */
    NMH_E_PROVIDER,    /* the display GPU's driver does not offer import */
    NMH_E_NOTVRAM,     /* the surface's newest pixels are in system memory already (level 3 copies them) */
    NMH_E_DMALEN       /* the surface is longer than the GPU memory behind it or the owned buffer */
};

static inline const char *nmh_why_name(int why)
{
    switch (why) {
    case NMH_OK:         return "ok";
    case NMH_E_LEVEL:    return "import not enabled";
    case NMH_E_FMT:      return "surface is not 8-bit BGRA";
    case NMH_E_SIZE:     return "surface is not the scan-out size";
    case NMH_E_PITCH:    return "row pitch unusable for a display plane";
    case NMH_E_PAGES:    return "surface memory is not a whole page list";
    case NMH_E_RANGE:    return "page address out of the display GPU's reach";
    case NMH_E_GEN:      return "display generation not supported for import";
    case NMH_E_NOSPACE:  return "no unused GGTT range";
    case NMH_E_BUSY:     return "all import slots in use";
    case NMH_E_STUCK:    return "a flip never reached the screen";
    case NMH_E_PROVIDER: return "display driver has no import entry";
    case NMH_E_NOTVRAM:  return "the newest pixels are not in the GPU's memory";
    case NMH_E_DMALEN:   return "surface longer than its source or the owned buffer";
    default:             return "unknown";
    }
}

/* ---------------------------------------------------------------------------------------------------------------------
 * level 1: the copy. The source is the composited frame (BGRA, or l10r narrowed to 8 bits), the destination the other
 * GPU's scan-out, a write-combined mapping where every store is expensive and nothing may be read back. A shadow of the
 * previous source frame, in ordinary cached memory, tells which pixels changed; only those words are stored.
 */
struct nmh_shadow {
    uint32_t *buf;       /* w*h words of the previous source frame, raw (before narrowing); NULL = no shadow */
    uint32_t  w, h;
    uint8_t   valid;     /* the screen holds exactly what the shadow holds */
};

struct nmh_stats {
    uint32_t rows;       /* rows with at least one changed word */
    uint32_t words;      /* words stored to the scan-out */
    uint32_t first, last;/* first and last changed row, valid when rows > 0 */
    uint8_t  full;       /* every word was stored (no usable shadow) */
};

/* Something else wrote to the scan-out (a flip back to the boot surface, a mode change): the next frame stores it all. */
static inline void nmh_shadow_invalidate(struct nmh_shadow *sh) { if (sh) sh->valid = 0; }

/* Returns 0, or -1 when the geometry cannot be presented (nothing is stored). 'spitch' and 'dpitch' are in bytes. A
 * destination row shorter than the source row shows the left part of the frame, as the existing loop did. */
static inline int nmh_present(struct nmh_shadow *sh, const volatile uint32_t *src, uint32_t w, uint32_t h,
                              uint32_t spitch, int from10, volatile uint32_t *dst, uint32_t dpitch,
                              struct nmh_stats *st)
{
    uint32_t y, x, words, spr, dpr;
    int usable;
    st->rows = st->words = st->first = st->last = 0; st->full = 0;
    if (!src || !dst || !w || !h || spitch < 4 || dpitch < 4 || (spitch & 3) || (dpitch & 3)) return -1;
    spr = spitch / 4; dpr = dpitch / 4;
    words = w < spr ? w : spr;
    if (words > dpr) words = dpr;
    if (!words) return -1;
    usable = sh && sh->buf && sh->valid && sh->w == w && sh->h == h;
    st->full = usable ? 0 : 1;
    for (y = 0; y < h; y++) {
        const volatile uint32_t *sp = src + (uint64_t)y * spr;
        volatile uint32_t *dp = dst + (uint64_t)y * dpr;
        uint32_t *shp = (sh && sh->buf && sh->w == w && sh->h == h) ? sh->buf + (uint64_t)y * w : (uint32_t *)0;
        uint32_t x0 = 0, x1 = words;       /* [x0, x1) to store */
        if (usable) {
            for (x0 = 0; x0 < words && sp[x0] == shp[x0]; x0++) {}
            if (x0 == words) continue;
            for (x1 = words; x1 > x0 && sp[x1 - 1] == shp[x1 - 1]; x1--) {}
        }
        if (from10) for (x = x0; x < x1; x++) dp[x] = nv_iop_l10r_to_x8r8g8b8(sp[x]);
        else        for (x = x0; x < x1; x++) dp[x] = sp[x];
        if (shp) for (x = x0; x < x1; x++) shp[x] = sp[x];
        if (!st->rows) st->first = y;
        st->last = y; st->rows++; st->words += x1 - x0;
    }
    /* the shadow now equals what was stored; a shadow of another size cannot be kept, so the next frame is full */
    if (sh && sh->buf) sh->valid = (sh->w == w && sh->h == h) ? 1 : 0;
    return 0;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * level 2: import. The surface's pages are written into unused entries of the display GPU's graphics translation table
 * (GGTT), and the plane is pointed at that range. Layout from the Linux i915 driver (MIT): display versions 12 and 13 keep
 * the table in the upper half of BAR0, one 64-bit entry per 4 KiB page: bit 0 present, bits 45:12 the page address.
 */
#define NMH_PAGE 4096ull
#define NMH_SLOTS 4
#define NMH_PLANE_STRIDE_MAX 0x7FFu    /* PLANE_STRIDE bits 10:0, linear planes count 64-byte units */

static inline int nmh_gen_ok(unsigned display_version) { return display_version == 12 || display_version == 13; }

/* the entry for one page, or 0 when the display GPU cannot reach that address (bits 45:12 only) */
static inline uint64_t nmh_ggtt_pte(uint64_t bus)
{
    if ((bus & (NMH_PAGE - 1)) || (bus >> 46)) return 0;
    return bus | 1ull;
}

static inline uint64_t nmh_pages_for(uint32_t pitch, uint32_t h)
{
    return ((uint64_t)pitch * h + NMH_PAGE - 1) / NMH_PAGE;
}

/* Can this surface be scanned out by the plane as it is? Returns NMH_OK or the reason. */
static inline int nmh_import_check(int level, unsigned display_version, uint32_t fmt, uint32_t w, uint32_t h,
                                   uint32_t pitch, uint32_t scan_w, uint32_t scan_h)
{
    if (level < NMH_IMPORT) return NMH_E_LEVEL;
    if (!nmh_gen_ok(display_version)) return NMH_E_GEN;
    if (fmt != NMH_FMT_BGRA) return NMH_E_FMT;
    if (w != scan_w || h != scan_h) return NMH_E_SIZE;
    if (pitch < 4u * w || (pitch & 63u) || (pitch / 64u) > NMH_PLANE_STRIDE_MAX) return NMH_E_PITCH;
    return NMH_OK;
}

/* A run of 'need' consecutive entries that all hold the same value, searched downward from 'top' (exclusive) for at most
 * 'scan' entries, never touching [avoid_lo, avoid_hi]. Entries a firmware left unused are all zero or all the scratch
 * page; a run of equal entries is the only evidence of a free range that reading the table gives. Returns the first
 * entry index, or -1. */
static inline int64_t nmh_find_free_run(uint64_t (*rd)(void *ctx, uint64_t idx), void *ctx, uint64_t top, uint64_t scan,
                                        uint64_t need, uint64_t avoid_lo, uint64_t avoid_hi)
{
    uint64_t i, run = 0, val = 0, floor = top > scan ? top - scan : 0;
    if (!need || need > top) return -1;
    for (i = top; i > floor; i--) {
        uint64_t idx = i - 1, v;
        if (idx >= avoid_lo && idx <= avoid_hi) { run = 0; continue; }
        v = rd(ctx, idx);
        if (run && v == val) run++;
        else { val = v; run = 1; }
        if (run >= need) return (int64_t)idx;
    }
    return -1;
}

/* the import slots: which surface sits in which GGTT range, and what the plane is showing */
struct nmh_slot { uint32_t id; uint32_t used_at; uint8_t used, mapped; };
#define NMH_NONE (-2)
struct nmh_ring {
    struct nmh_slot s[NMH_SLOTS];
    uint64_t base;        /* GGTT byte offset of slot 0 */
    uint64_t slot_bytes;  /* page-aligned size of one slot */
    uint32_t tick;
    int      live;        /* slot on screen, -1 = the boot surface */
    int      pending;     /* slot a flip was submitted for, -1 = the boot surface, NMH_NONE = no flip in flight */
    uint32_t polls;       /* polls since the pending flip was submitted */
};

static inline void nmh_ring_init(struct nmh_ring *r, uint64_t base, uint64_t slot_bytes)
{
    unsigned i;
    for (i = 0; i < NMH_SLOTS; i++) { r->s[i].id = 0; r->s[i].used_at = 0; r->s[i].used = r->s[i].mapped = 0; }
    r->base = base; r->slot_bytes = slot_bytes; r->tick = 0; r->live = -1; r->pending = NMH_NONE; r->polls = 0;
}

static inline uint64_t nmh_slot_surf(const struct nmh_ring *r, int slot) { return r->base + (uint64_t)slot * r->slot_bytes; }

/* Find or claim the slot for surface 'id'. *need_map is 1 when its pages must be written (a new surface, or one that took
 * a slot over); *evicted is the id that lost its slot, or 0. Returns the slot, or -1 with every slot on screen or flipping. */
static inline int nmh_ring_acquire(struct nmh_ring *r, uint32_t id, int *need_map, uint32_t *evicted)
{
    int i, pick = -1;
    *need_map = 0; *evicted = 0;
    r->tick++;
    for (i = 0; i < NMH_SLOTS; i++)
        if (r->s[i].used && r->s[i].id == id) { r->s[i].used_at = r->tick; if (!r->s[i].mapped) *need_map = 1; return i; }
    for (i = 0; i < NMH_SLOTS && pick < 0; i++) if (!r->s[i].used) pick = i;
    if (pick < 0) {
        for (i = 0; i < NMH_SLOTS; i++) {
            if (i == r->live || i == r->pending) continue;
            if (pick < 0 || (int32_t)(r->s[i].used_at - r->s[pick].used_at) < 0) pick = i;
        }
        if (pick < 0) return -1;
        *evicted = r->s[pick].id;
    }
    r->s[pick].used = 1; r->s[pick].mapped = 0; r->s[pick].id = id; r->s[pick].used_at = r->tick;
    *need_map = 1;
    return pick;
}

static inline void nmh_ring_mapped(struct nmh_ring *r, int slot) { r->s[slot].mapped = 1; }
static inline void nmh_ring_forget(struct nmh_ring *r, int slot) { r->s[slot].used = r->s[slot].mapped = 0; r->s[slot].id = 0; }

/* The plane's surface register is about to be written with slot's range (slot -1 = the boot surface). The plane takes one
 * new surface per vertical blank, so a second flip while one is in flight is refused. A flip to what is already on screen
 * is a no-op. */
static inline int nmh_flip_begin(struct nmh_ring *r, int slot)
{
    if (r->pending != NMH_NONE) return NMH_E_BUSY;
    if (slot == r->live) return NMH_OK;
    r->pending = slot; r->polls = 0;
    return NMH_OK;
}

/* 'surflive' is the plane's live surface register (what is being scanned out now), 'home' the boot surface's offset.
 * Returns 1 when no flip is in flight or the pending one has landed, 0 while it waits, -1 once it has waited 'limit'
 * polls (the caller then gives import up and goes home). */
static inline int nmh_flip_poll(struct nmh_ring *r, uint32_t surflive, uint32_t home, uint32_t limit)
{
    uint64_t want;
    if (r->pending == NMH_NONE) return 1;
    want = r->pending < 0 ? home : nmh_slot_surf(r, r->pending);
    if ((uint64_t)(surflive & ~0xFFFu) == want) { r->live = r->pending; r->pending = NMH_NONE; return 1; }
    if (++r->polls >= limit) return -1;
    return 0;
}

/* A stuck flip is dropped: the plane may be anywhere, so nothing is assumed live until the next poll sees it. */
static inline void nmh_flip_abandon(struct nmh_ring *r) { r->pending = NMH_NONE; r->live = -3; }

/* The copy path writes into the boot surface, so the plane must be showing it first. */
static inline int nmh_need_home(const struct nmh_ring *r) { return r->live != -1 || r->pending != NMH_NONE; }

/* ---------------------------------------------------------------------------------------------------------------------
 * level 3: buffers the panel driver owns. The panel driver allocates NMH_OWN_BUFS buffers, maps them into the graphics
 * table once, and lends them out: a writer takes a free buffer, fills it, and presents it, which flips the plane to it;
 * the buffer that was on screen is free again when the new one is live. Nobody writes a buffer that is on screen or about
 * to be.
 */
#define NMH_OWN_BUFS 3
enum { NMH_OB_FREE = 0, NMH_OB_WRITER, NMH_OB_QUEUED, NMH_OB_LIVE, NMH_OB_QUARANTINE };
struct nmh_own {
    uint8_t st[NMH_OWN_BUFS];
    int     live;       /* buffer on screen, -1 = the boot surface */
    int     queued;     /* buffer a flip was submitted for, -1 = none */
};
static inline void nmh_own_init(struct nmh_own *o)
{
    unsigned i;
    for (i = 0; i < NMH_OWN_BUFS; i++) o->st[i] = NMH_OB_FREE;
    o->live = -1; o->queued = -1;
}
/* A free buffer for one writer, or -1 when every buffer is taken (the caller copies the old way for that frame). */
static inline int nmh_own_acquire(struct nmh_own *o)
{
    int i;
    for (i = 0; i < NMH_OWN_BUFS; i++) if (o->st[i] == NMH_OB_WRITER) return -1;     /* one writer at a time */
    for (i = 0; i < NMH_OWN_BUFS; i++) if (o->st[i] == NMH_OB_FREE) { o->st[i] = NMH_OB_WRITER; return i; }
    return -1;
}
/* A copy into this buffer timed out: the copy engine may still be writing it, so it is never handed out again (until the
 * panel driver stops) and it cannot be freed. */
static inline void nmh_own_quarantine(struct nmh_own *o, int idx)
{
    if (idx >= 0 && idx < NMH_OWN_BUFS && o->st[idx] == NMH_OB_WRITER) o->st[idx] = NMH_OB_QUARANTINE;
}
/* The writer is done: the buffer waits for its flip. Only the writer's own buffer, and only with no flip already queued. */
static inline int nmh_own_present(struct nmh_own *o, int idx)
{
    if (idx < 0 || idx >= NMH_OWN_BUFS || o->st[idx] != NMH_OB_WRITER || o->queued != -1) return NMH_E_BUSY;
    o->st[idx] = NMH_OB_QUEUED; o->queued = idx;
    return NMH_OK;
}
/* The writer gave up (the copy failed): the buffer goes back, nothing was shown. */
static inline void nmh_own_abort(struct nmh_own *o, int idx)
{
    if (idx >= 0 && idx < NMH_OWN_BUFS && o->st[idx] == NMH_OB_WRITER) o->st[idx] = NMH_OB_FREE;
}
/* The queued flip reached the screen. */
static inline void nmh_own_landed(struct nmh_own *o)
{
    if (o->queued < 0) return;
    if (o->live >= 0) o->st[o->live] = NMH_OB_FREE;
    o->st[o->queued] = NMH_OB_LIVE; o->live = o->queued; o->queued = -1;
}
/* The flip never landed: the buffer is not on screen and is not queued; whatever the plane shows is unknown. */
static inline void nmh_own_flip_failed(struct nmh_own *o)
{
    if (o->queued >= 0) o->st[o->queued] = NMH_OB_FREE;
    o->queued = -1;
    if (o->live >= 0) { o->st[o->live] = NMH_OB_FREE; }
    o->live = -1;
}
/* The plane is back on the boot surface (HOME): every buffer is free. */
static inline void nmh_own_home(struct nmh_own *o)
{
    unsigned i;
    for (i = 0; i < NMH_OWN_BUFS; i++) if (o->st[i] != NMH_OB_WRITER && o->st[i] != NMH_OB_QUARANTINE) o->st[i] = NMH_OB_FREE;
    o->live = -1; o->queued = -1;
}
/* Buffers may be released (module unload, mode change) only when nobody writes and nothing is on screen from them. */
static inline int nmh_own_may_free(const struct nmh_own *o)
{
    unsigned i;
    for (i = 0; i < NMH_OWN_BUFS; i++) if (o->st[i] == NMH_OB_WRITER || o->st[i] == NMH_OB_QUEUED || o->st[i] == NMH_OB_QUARANTINE) return 0;
    return o->live < 0;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * level 4: the copy engine writes the composited frame into an owned buffer. The order is the whole point, and this is the
 * only place it is written down:
 *   1. the GPU has finished drawing the surface (wait_render)   -- NO lock held: this can wait on the GPU
 *   2. the accelerator lock is taken, the surface's GPU memory is looked up and the frame is checked (lock_lookup)
 *   3. an owned buffer is taken and flushed from the CPU caches (acquire) -- so no dirty line can land over the DMA
 *   4. the copy engine writes it and the call returns only when the engine is done (ce_copy)   -- lock held
 *   5. the lock is released (unlock) -- before anything that waits for the display
 *   6. only then the plane is flipped to the buffer (present), which can wait up to 40 ms for the next vertical blank
 * The lock covers the lookup and the copy and nothing else: a flip wait with the accelerator lock held stalls every other
 * thread of WindowServer that needs the accelerator. lock_lookup returns 0 holding the lock, or non-zero holding nothing;
 * unlock is called exactly once for every successful lock_lookup, on every path.
 * A copy that fails cleanly returns the buffer (abort) and the caller copies the old way. A copy that TIMED OUT may still be
 * writing: the buffer is quarantined, never presented, never reused. Nothing is presented unless the copy completed.
 */
struct nmh_dma_ops {
    void *ctx;
    int (*wait_render)(void *ctx);                 /* 0 = the surface is final */
    int (*lock_lookup)(void *ctx);                 /* 0 = lock held and the frame is eligible; non-zero = no lock held */
    void (*unlock)(void *ctx);
    int (*acquire)(void *ctx, int *idx);           /* 0 and a buffer, flushed, or non-zero */
    int (*ce_copy)(void *ctx, int idx);            /* 0 done, 1 failed with nothing written, 2 timed out */
    int (*present)(void *ctx, int idx);            /* 0 = flip landed */
    void (*abort)(void *ctx, int idx);
    void (*quarantine)(void *ctx, int idx);
};
enum { NMH_DMA_DONE = 0, NMH_DMA_NOT_ELIGIBLE, NMH_DMA_RENDER, NMH_DMA_NO_BUFFER, NMH_DMA_COPY_FAILED, NMH_DMA_COPY_TIMEOUT, NMH_DMA_PRESENT_FAILED };

/* May this frame use level 4 at all? 'pending_in_vram': the surface's newest pixels are in the GPU's memory (not yet paged to
 * its system memory). The copy is a linear byte copy, so the surface must have the scan-out's geometry exactly. */
static inline int nmh_dma_eligible(int level, int pending_in_vram, uint32_t fmt, uint32_t w, uint32_t h, uint32_t pitch,
                                   uint32_t scan_w, uint32_t scan_h, uint32_t scan_pitch, uint64_t vram_bytes, uint64_t buf_bytes)
{
    const uint64_t len = (uint64_t)pitch * h;
    if (level < NMH_DMA) return NMH_E_LEVEL;
    if (!pending_in_vram) return NMH_E_NOTVRAM;
    if (fmt != NMH_FMT_BGRA) return NMH_E_FMT;
    if (w != scan_w || h != scan_h || pitch != scan_pitch) return NMH_E_SIZE;
    if (len > vram_bytes || len > buf_bytes) return NMH_E_DMALEN;
    return NMH_OK;
}

static inline int nmh_dma_frame(const struct nmh_dma_ops *d)
{
    int idx = -1, rc;
    if (d->wait_render(d->ctx)) return NMH_DMA_RENDER;
    if (d->lock_lookup(d->ctx)) return NMH_DMA_NOT_ELIGIBLE;
    if (d->acquire(d->ctx, &idx) || idx < 0) { d->unlock(d->ctx); return NMH_DMA_NO_BUFFER; }
    rc = d->ce_copy(d->ctx, idx);
    d->unlock(d->ctx);
    if (rc == 2) { d->quarantine(d->ctx, idx); return NMH_DMA_COPY_TIMEOUT; }
    if (rc) { d->abort(d->ctx, idx); return NMH_DMA_COPY_FAILED; }
    if (d->present(d->ctx, idx)) { d->abort(d->ctx, idx); return NMH_DMA_PRESENT_FAILED; }
    return NMH_DMA_DONE;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * the path as a whole, one state per thing a tester can see in the log
 */
enum nmh_state { NMH_S_OFF, NMH_S_ARMED, NMH_S_COPY, NMH_S_IMPORT, NMH_S_FALLBACK };
enum nmh_event { NMH_EV_PROVIDER, NMH_EV_COPIED, NMH_EV_IMPORT_OK, NMH_EV_IMPORT_FAIL, NMH_EV_IMPORT_STUCK, NMH_EV_PROVIDER_GONE };

struct nmh_sm { int level; int state; int why; };

static inline void nmh_sm_init(struct nmh_sm *m, int level)
{
    m->level = level; m->why = NMH_OK; m->state = level ? NMH_S_ARMED : NMH_S_OFF;
}

static inline const char *nmh_state_name(int s)
{
    switch (s) {
    case NMH_S_OFF: return "off";
    case NMH_S_ARMED: return "armed";
    case NMH_S_COPY: return "copy";
    case NMH_S_IMPORT: return "import";
    case NMH_S_FALLBACK: return "copy-only";
    default: return "?";
    }
}

/* Returns the new state. Off stays off. An import that is stuck is final for the boot; one refused surface is not: the
 * frame is copied and the next surface is tried again, so a single odd surface does not end zero-copy. */
static inline int nmh_sm_step(struct nmh_sm *m, int ev, int why)
{
    if (m->state == NMH_S_OFF) return m->state;
    switch (ev) {
    case NMH_EV_PROVIDER: if (m->state == NMH_S_ARMED) m->state = NMH_S_COPY; break;
    case NMH_EV_COPIED:   if (m->state == NMH_S_ARMED) m->state = NMH_S_COPY; break;
    case NMH_EV_IMPORT_OK:
        if (m->state == NMH_S_COPY || m->state == NMH_S_IMPORT) m->state = NMH_S_IMPORT;
        break;
    case NMH_EV_IMPORT_FAIL:
        m->why = why;
        if (m->state == NMH_S_IMPORT) m->state = NMH_S_COPY;
        break;
    case NMH_EV_IMPORT_STUCK: m->why = why; m->level = NMH_COPY; m->state = NMH_S_FALLBACK; break;
    case NMH_EV_PROVIDER_GONE: m->state = NMH_S_ARMED; break;
    }
    return m->state;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * start-up controls of the panel driver
 */

/* nmhybriddelay=<seconds>: wait before taking the panel from the firmware. Some machines froze without a wait after the
 * NVIDIA driver started. 0 to 120; anything else is 0. */
static inline unsigned nmh_delay_parse(int present, long long value)
{
    return (present && value > 0 && value <= 120) ? (unsigned)value : 0;
}

/* Why the panel driver should leave the panel to the firmware because the NVIDIA boot guard has the driver off:
 * the "-nvoff" boot-arg, the nullmoth-remove NVRAM flag, or 'limit' starts in a row that never lit a display. */
enum { NMH_STAY_NONE = 0, NMH_STAY_NVOFF = 1, NMH_STAY_REMOVE = 2, NMH_STAY_GUARD = 3 };
static inline int nmh_stand_down_why(int nvoff_bootarg, int remove_flag, unsigned starts, unsigned limit)
{
    if (nvoff_bootarg) return NMH_STAY_NVOFF;
    if (remove_flag) return NMH_STAY_REMOVE;
    if (limit && starts >= limit) return NMH_STAY_GUARD;
    return NMH_STAY_NONE;
}
static inline const char *nmh_stay_name(int why)
{
    switch (why) {
    case NMH_STAY_NVOFF:  return "boot-arg -nvoff";
    case NMH_STAY_REMOVE: return "the NVIDIA driver was removed from the boot picker (nullmoth-remove)";
    case NMH_STAY_GUARD:  return "the NVIDIA boot guard has the driver off";
    default:              return "";
    }
}

/* The last step the panel driver reached, kept as a sysctl and a registry property, and (where the platform allows it at
 * that stage) written to the NVRAM variable nullmoth-ifb-step, so a boot that froze says where. */
enum {
    NMH_STEP_NONE = 0, NMH_STEP_PROBE, NMH_STEP_STOOD_DOWN, NMH_STEP_DELAY, NMH_STEP_MMIO, NMH_STEP_ADOPT, NMH_STEP_SURF,
    NMH_STEP_FB_START, NMH_STEP_PUBLISHED, NMH_STEP_RING, NMH_STEP_FLIP, NMH_STEP_SUSPEND, NMH_STEP_RESUME_BEGIN,
    NMH_STEP_RESUME_DONE, NMH_STEP_RESUME_FAILED, NMH_STEP__COUNT
};
static inline const char *nmh_step_name(int step)
{
    static const char *const n[NMH_STEP__COUNT] = {
        "none", "probe", "stood-down", "delay", "mmio-mapped", "mode-adopted", "scanout-mapped", "framebuffer-started",
        "published", "import-ring", "first-flip", "suspend", "resume-begin", "resume-done", "resume-failed" };
    return (step >= 0 && step < NMH_STEP__COUNT) ? n[step] : "?";
}

/* nmflatskip=<n>: a composited frame that is one flat colour is not flipped to for up to n frames in a row, so a
 * momentary black frame never reaches the screen; after n the flat frame is shown. 0 (off) to 8. */
static inline unsigned nmh_flat_parse(int present, long long value)
{
    return (present && value > 0 && value <= 8) ? (unsigned)value : 0;
}
struct nmh_flat { unsigned held; };
/* 1 = keep the previous picture for this frame */
static inline int nmh_flat_hold(struct nmh_flat *st, int frame_is_flat, unsigned max_hold)
{
    if (!max_hold || !frame_is_flat) { st->held = 0; return 0; }
    if (st->held < max_hold) { st->held++; return 1; }
    st->held = 0;
    return 0;
}
/* 64 samples taken from a frame; 1 when they are all the same colour (alpha ignored) */
static inline int nmh_samples_flat(const uint32_t *sample, unsigned n)
{
    unsigned i;
    if (!n) return 0;
    for (i = 1; i < n; i++) if ((sample[i] & 0xffffffu) != (sample[0] & 0xffffffu)) return 0;
    return 1;
}

#endif

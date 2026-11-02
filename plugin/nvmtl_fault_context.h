/* nvmtl_fault_context.h - what was in flight when the device was lost, so a GPU fault carries names.

   A channel fault reports an address and a semaphore number and nothing else. This keeps three small rings, always, at
   the cost of a few stores per dispatch, submit and resource death: the last dispatches of each command buffer (kernel
   name and the storage views bound), the last submits (sequence number, so the fault's semaphore number maps to a
   submit), and the last image and heap-slot deaths (sequence number at death, so a kernel that was in flight while
   its image died is named). The report runs once, at device loss. Pure over its own state: no Vulkan calls, so
   tools/test_fault_context.c proves the matching on the host. The plugin's sequence number is its count of queue
   submits, which the NVK channel semaphore follows closely. */
#ifndef NVMTL_FAULT_CONTEXT_H
#define NVMTL_FAULT_CONTEXT_H
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

enum { NVMTL_FX_DISP = 8, NVMTL_FX_STOR = 6, NVMTL_FX_SUB = 48, NVMTL_FX_GONE = 128, NVMTL_FX_NAME = 40 };

/* one dispatch: its kernel and the storage views bound to it (explicit binds; a bindless read is not visible here) */
typedef struct { char name[NVMTL_FX_NAME]; const void *pipe; uint32_t key; uint64_t air; uint32_t nstor; uint32_t stor_idx[NVMTL_FX_STOR]; const void *stor_view[NVMTL_FX_STOR];
                 const void *tab[2]; uint32_t dyn[2]; /* the binding-table buffer and offset in force per stage slot: the dispatch's root addresses */ } nvmtl_fx_disp;
/* per command buffer: the last dispatches recorded into it, the storage binds and the binding tables in force */
typedef struct { nvmtl_fx_disp d[NVMTL_FX_DISP]; uint32_t n, total; uint32_t nstor; uint32_t stor_idx[NVMTL_FX_STOR]; const void *stor_view[NVMTL_FX_STOR];
                 const void *tab[2]; uint32_t dyn[2]; } nvmtl_fx_cb;
/* one submit: the sequence number it got and the command buffer's dispatch ring as it was */
typedef struct { uint64_t seq; const void *cb; uint32_t ndisp; nvmtl_fx_disp d[NVMTL_FX_DISP]; uint32_t n; } nvmtl_fx_sub;
/* one death: an image (view, size, format) or a heap slot, at a sequence number */
enum { NVMTL_FX_PLACED = 1, NVMTL_FX_POOLED = 2 };   /* where a dead image's memory lived */
typedef struct { uint64_t seq; const void *view; uint32_t slot, w, h, fmt, storage, where; char label[NVMTL_FX_NAME]; } nvmtl_fx_gone;
/* one buffer death: the GPU address range it held, at a sequence number. A page-table fault names an address and
   nothing else, so the ring of the last buffer deaths is what says whether that address was a buffer freed while a
   submit still read it. */
enum { NVMTL_FX_BUFS = 4096, NVMTL_FX_PARKED = 4 };   /* the game frees ~18 buffers per submit; 256 covered 14 submits */
typedef struct { uint64_t seq, lo, hi; uint32_t where; } nvmtl_fx_buf;
typedef struct { nvmtl_fx_sub sub[NVMTL_FX_SUB]; uint32_t nsub; nvmtl_fx_gone gone[NVMTL_FX_GONE]; uint32_t ngone;
                 nvmtl_fx_buf buf[NVMTL_FX_BUFS]; uint32_t nbuf; } nvmtl_fx_state;

static inline void nvmtl_fx_copy_name(char *dst, const char *src) { snprintf(dst, NVMTL_FX_NAME, "%s", src ? src : "-"); }

/* a storage view bound at index (NULL unbinds) */
static inline void nvmtl_fx_bind_storage(nvmtl_fx_cb *c, uint32_t index, const void *view)
{
    for (uint32_t k = 0; k < c->nstor; k++) if (c->stor_idx[k] == index) {
        if (view) { c->stor_view[k] = view; return; }
        c->stor_idx[k] = c->stor_idx[c->nstor - 1]; c->stor_view[k] = c->stor_view[c->nstor - 1]; c->nstor--; return;
    }
    if (!view) return;
    if (c->nstor < NVMTL_FX_STOR) { c->stor_idx[c->nstor] = index; c->stor_view[c->nstor] = view; c->nstor++; }
    else { c->stor_idx[NVMTL_FX_STOR - 1] = index; c->stor_view[NVMTL_FX_STOR - 1] = view; }   /* the newest keeps the last place */
}
/* a dispatch: the kernel name and the storage binds in force go into the command buffer's ring */
static inline void nvmtl_fx_dispatch(nvmtl_fx_cb *c, const char *name, const void *pipe, uint32_t key, uint64_t air)
{
    nvmtl_fx_disp *d = &c->d[c->n % NVMTL_FX_DISP];
    nvmtl_fx_copy_name(d->name, name); d->pipe = pipe; d->key = key; d->air = air;
    d->nstor = c->nstor; memcpy(d->stor_idx, c->stor_idx, sizeof d->stor_idx); memcpy(d->stor_view, c->stor_view, sizeof d->stor_view);
    d->tab[0] = c->tab[0]; d->tab[1] = c->tab[1]; d->dyn[0] = c->dyn[0]; d->dyn[1] = c->dyn[1];
    c->n++; c->total++;
}
/* a submit of command buffer cb at sequence number seq */
static inline void nvmtl_fx_submit(nvmtl_fx_state *s, uint64_t seq, const void *cb, const nvmtl_fx_cb *c)
{
    nvmtl_fx_sub *u = &s->sub[s->nsub % NVMTL_FX_SUB];
    u->seq = seq; u->cb = cb; u->ndisp = c ? c->total : 0; u->n = c ? c->n : 0;
    if (c) memcpy(u->d, c->d, sizeof u->d); else memset(u->d, 0, sizeof u->d);
    s->nsub++;
}
static inline void nvmtl_fx_image_gone(nvmtl_fx_state *s, uint64_t seq, const void *view, uint32_t w, uint32_t h, uint32_t fmt, uint32_t storage, uint32_t where, const char *label)
{
    nvmtl_fx_gone *g = &s->gone[s->ngone % NVMTL_FX_GONE];
    g->seq = seq; g->view = view; g->slot = 0; g->w = w; g->h = h; g->fmt = fmt; g->storage = storage; g->where = where; nvmtl_fx_copy_name(g->label, label);
    s->ngone++;
}
static inline void nvmtl_fx_slot_gone(nvmtl_fx_state *s, uint64_t seq, uint32_t slot)
{
    nvmtl_fx_gone *g = &s->gone[s->ngone % NVMTL_FX_GONE];
    memset(g, 0, sizeof *g); g->seq = seq; g->slot = slot; nvmtl_fx_copy_name(g->label, "heap slot");
    s->ngone++;
}
/* a buffer whose GPU address range [lo, lo + size) was released at sequence number seq */
static inline void nvmtl_fx_buffer_gone(nvmtl_fx_state *s, uint64_t seq, uint64_t lo, uint64_t size, uint32_t where)
{
    if (!lo) return;   /* a buffer that never had a device address cannot be the one a fault names */
    nvmtl_fx_buf *b = &s->buf[s->nbuf % NVMTL_FX_BUFS];
    b->seq = seq; b->lo = lo; b->hi = lo + size; b->where = where;
    s->nbuf++;
}
/* the newest recorded buffer death whose range held addr, or NULL: the question a FAULT_PTE address asks */
static inline const nvmtl_fx_buf *nvmtl_fx_buffer_at(const nvmtl_fx_state *s, uint64_t addr)
{
    const uint32_t nb = s->nbuf < NVMTL_FX_BUFS ? s->nbuf : NVMTL_FX_BUFS;
    for (uint32_t k = 0; k < nb; k++) { const nvmtl_fx_buf *b = &s->buf[(s->nbuf - 1 - k) % NVMTL_FX_BUFS]; if (addr >= b->lo && addr < b->hi) return b; }
    return NULL;
}

/* The report: calls emit(line) once per line. The last `subs` submits, newest first, with their dispatches; every
   dispatch whose storage view belongs to an image that died at or before that submit is marked DEAD; then the deaths
   of the last `window` sequence numbers. Returns how many DEAD marks it made. */
typedef void (*nvmtl_fx_emit)(void *ctx, const char *line);
static inline unsigned nvmtl_fx_report(const nvmtl_fx_state *s, uint64_t now_seq, uint32_t subs, uint64_t window, nvmtl_fx_emit emit, void *ctx)
{
    char line[512]; unsigned dead = 0;
    const uint32_t ns = s->nsub < NVMTL_FX_SUB ? s->nsub : NVMTL_FX_SUB;
    const uint32_t ng = s->ngone < NVMTL_FX_GONE ? s->ngone : NVMTL_FX_GONE;
    snprintf(line, sizeof line, "fault context: %u submits recorded, last sequence %llu; %u deaths recorded (sequence = this process's queue submits; the channel semaphore follows it)",
             s->nsub, (unsigned long long)now_seq, s->ngone);
    emit(ctx, line);
    for (uint32_t k = 0; k < ns && k < subs; k++) {
        const nvmtl_fx_sub *u = &s->sub[(s->nsub - 1 - k) % NVMTL_FX_SUB];
        snprintf(line, sizeof line, "fault context: submit seq %llu cb %p: %u dispatches in all", (unsigned long long)u->seq, u->cb, u->ndisp);
        emit(ctx, line);
        const uint32_t nd = u->n < NVMTL_FX_DISP ? u->n : NVMTL_FX_DISP;
        for (uint32_t j = 0; j < nd; j++) {
            const nvmtl_fx_disp *d = &u->d[(u->n - nd + j) % NVMTL_FX_DISP];
            int o = snprintf(line, sizeof line, "fault context:   dispatch %u %s spv-fnv %08x air %016llx pipe %p", u->ndisp - nd + j + 1, d->name, d->key, (unsigned long long)d->air, d->pipe);
            for (uint32_t m = 0; m < d->nstor && m < NVMTL_FX_STOR; m++) {
                const nvmtl_fx_gone *g = NULL;
                for (uint32_t q = 0; q < ng; q++) { const nvmtl_fx_gone *c = &s->gone[q]; if (c->view && c->view == d->stor_view[m] && c->seq <= u->seq) { g = c; break; } }
                if (g) { dead++; o += snprintf(line + o, sizeof line - (size_t)o, " | storage %u view %p DEAD: image %ux%u fmt %u%s \"%s\" died at seq %llu",
                                               d->stor_idx[m], d->stor_view[m], g->w, g->h, g->fmt, g->storage ? " storage" : "", g->label, (unsigned long long)g->seq); }
                else o += snprintf(line + o, sizeof line - (size_t)o, " | storage %u view %p", d->stor_idx[m], d->stor_view[m]);
                if (o >= (int)sizeof line) break;
            }
            emit(ctx, line);
        }
    }
    unsigned shown = 0;
    for (uint32_t k = 0; k < ng; k++) {
        const nvmtl_fx_gone *g = &s->gone[(s->ngone - 1 - k) % NVMTL_FX_GONE];
        if (now_seq > window && g->seq + window < now_seq) continue;
        if (g->slot) snprintf(line, sizeof line, "fault context: died at seq %llu: heap slot %u", (unsigned long long)g->seq, g->slot);
        else snprintf(line, sizeof line, "fault context: died at seq %llu: image view %p %ux%u fmt %u%s%s \"%s\"", (unsigned long long)g->seq, g->view, g->w, g->h, g->fmt, g->storage ? " storage" : "",
                      g->where & NVMTL_FX_POOLED ? " pooled" : g->where & NVMTL_FX_PLACED ? " placed" : " own memory", g->label);
        emit(ctx, line); shown++;
    }
    snprintf(line, sizeof line, "fault context: %u deaths within the last %llu submits, %u dispatches bound a dead image", shown, (unsigned long long)window, dead);
    emit(ctx, line);
    /* the buffer deaths of the window, newest first, four to a line: match the fault's address against these ranges */
    {   const uint32_t nb = s->nbuf < NVMTL_FX_BUFS ? s->nbuf : NVMTL_FX_BUFS;
        unsigned nshown = 0, per = 0; int o = 0;
        for (uint32_t k = 0; k < nb; k++) {
            const nvmtl_fx_buf *b = &s->buf[(s->nbuf - 1 - k) % NVMTL_FX_BUFS];
            if (now_seq > window && b->seq + window < now_seq) continue;
            if (!per) o = snprintf(line, sizeof line, "fault context: dead buffers (seq GPU lo..hi KB):");
            o += snprintf(line + o, sizeof line - (size_t)o, " %llu 0x%llx..0x%llx %llu%s%s%s |", (unsigned long long)b->seq, (unsigned long long)b->lo,
                          (unsigned long long)b->hi, (unsigned long long)((b->hi - b->lo) >> 10),
                          b->where & NVMTL_FX_POOLED ? " pooled" : "", b->where & NVMTL_FX_PLACED ? " placed" : "", b->where & NVMTL_FX_PARKED ? " parked" : "");
            nshown++;
            if (++per == 4 || o >= (int)sizeof line - 80) { emit(ctx, line); per = 0; }
            if (nshown == 64) break;   /* the log shows the newest 64; the fault file at device loss holds the whole ring */
        }
        if (per) emit(ctx, line);
        unsigned inwin = 0;
        for (uint32_t k = 0; k < nb; k++) { const nvmtl_fx_buf *b = &s->buf[(s->nbuf - 1 - k) % NVMTL_FX_BUFS]; if (!(now_seq > window && b->seq + window < now_seq)) inwin++; }
        snprintf(line, sizeof line, "fault context: %u of %u buffer deaths within the last %llu submits shown (%u recorded in all)", nshown, inwin, (unsigned long long)window, s->nbuf);
        emit(ctx, line);
    }
    return dead;
}
#endif

/* nvmtl_page_life.h - when a binding-table address page (or any per-submit GPU resource) may be reused.

   A page is read by the GPU for every draw and dispatch whose descriptor set points into it. It used to be released
   by the bookkeeping of the command buffer that allocated it: once that command buffer's fence had signalled and its
   witness read back, the page went to the reuse cache and the next command buffer overwrote it. That is only safe
   while exactly one submit ever references a page. This decides reuse by the submits instead: every submit that
   references a page stamps it with its sequence number, and the page is released only once the queue has completed
   that highest sequence. A page whose last referencing submit is still in flight when its owner lets go of it is
   parked on a cooling list and released when the completed sequence reaches its stamp - the same shape as the image
   grace list. Pure over its own state (no Vulkan), so tools/test_page_life.c proves it on the host, including the
   case that must not happen: a page referenced by two submits must not be released when the first one completes. */
#ifndef NVMTL_PAGE_LIFE_H
#define NVMTL_PAGE_LIFE_H
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

/* embedded in the resource; last_seq is the highest submit sequence that referenced it */
typedef struct nvmtl_pl_node { uint64_t last_seq; uint32_t reuse; struct nvmtl_pl_node *cool; } nvmtl_pl_node;

typedef struct nvmtl_pl_state {
    nvmtl_pl_node *cooling;          /* released by their owner while a referencing submit was still in flight */
    uint64_t live;                   /* pages allocated and not yet released or destroyed */
    uint64_t retired;                /* owner releases that could reuse or destroy the page at once */
    uint64_t held;                   /* owner releases that had to wait for a later submit: the count the census reports */
    uint64_t ncool;                  /* on the cooling list now */
    uint64_t reaped;                 /* taken off the cooling list once their submit completed */
} nvmtl_pl_state;

static inline void nvmtl_pl_made(nvmtl_pl_state *s) { s->live++; }

/* a submit with sequence seq references the page */
static inline void nvmtl_pl_stamp(nvmtl_pl_node *n, uint64_t seq) { if (seq > n->last_seq) n->last_seq = seq; }

/* The owner lets go of the page. Returns 1 when every submit that referenced it has completed (finished_seq is the
   highest completed sequence on the one in-order queue) and the caller may reuse or destroy it now; 0 when a later
   submit still references it, in which case it is parked on the cooling list with `reuse` recorded for the reap. */
static inline int nvmtl_pl_release(nvmtl_pl_state *s, nvmtl_pl_node *n, uint64_t finished_seq, uint32_t reuse)
{
    if (n->last_seq <= finished_seq) { s->retired++; s->live--; return 1; }
    n->reuse = reuse; n->cool = s->cooling; s->cooling = n; s->held++; s->ncool++;
    return 0;
}

/* Moves every cooling page whose last referencing submit has completed onto *due (a list through ->cool) and returns
   how many. The cooling list is unordered - owners release out of submit order - and short, so it is walked whole. */
static inline unsigned nvmtl_pl_reap(nvmtl_pl_state *s, uint64_t finished_seq, nvmtl_pl_node **due)
{
    nvmtl_pl_node **pp = &s->cooling; unsigned n = 0; *due = NULL;
    while (*pp) {
        nvmtl_pl_node *e = *pp;
        if (e->last_seq <= finished_seq) { *pp = e->cool; e->cool = *due; *due = e; s->ncool--; s->reaped++; s->retired++; s->live--; n++; }
        else pp = &e->cool;
    }
    return n;
}

/* the census line the plugin prints every few thousand releases and at device loss */
static inline void nvmtl_pl_census(const nvmtl_pl_state *s, char *line, size_t cap)
{
    snprintf(line, cap, "address pages: %llu live, %llu retired, %llu held past their own cb (%llu cooling now, %llu reaped)",
             (unsigned long long)s->live, (unsigned long long)s->retired, (unsigned long long)s->held,
             (unsigned long long)s->ncool, (unsigned long long)s->reaped);
}
#endif

/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * The life of the framebuffers one integrated GPU publishes: the internal panel's, which exists as long as the driver does, and one per
 * connected external port. All of them share the device (its register mapping, the graphics table, the firmware-load lock), so the device
 * is opened by the first and closed by the last. This is the part that must be right whatever the cable does:
 *   - a plug creates a head, brings the display up, and only then publishes it; a failure at any point releases what was created;
 *   - an unplug unpublishes the head first, then takes the display down, then releases the object, then (if it was the last user) the device;
 *   - a plug that arrives while the same port is being taken down is remembered and replayed afterwards, never run on top of it;
 *   - stopping the driver takes every external head down before the panel's, and ignores plugs from then on.
 * The operations are injected (create, bring up, publish, unpublish, take down, release, open and close the device), so the kernel glue and
 * the host test use the same logic. Plain C: tools/test_hybrid_heads.c.
 */
#ifndef NM_HYBRID_HEADS_H
#define NM_HYBRID_HEADS_H

#include <stdint.h>
#include <string.h>

#define NMHD_MAX 4              /* external heads at once (the accelerator's head indices after the panel's) */
enum { NMHD_FREE = 0, NMHD_STARTING, NMHD_UP, NMHD_STOPPING };

struct nmhd_ops {
    void *ctx;
    int  (*dev_open)(void *ctx);                       /* first user: map the registers and the graphics table; 0 or -1 */
    void (*dev_close)(void *ctx);                      /* last user gone */
    void *(*create)(void *ctx, unsigned slot, unsigned port);    /* the framebuffer object, not yet published; NULL on failure */
    int  (*bring_up)(void *ctx, void *obj, unsigned port);       /* the display engine side (nmo_enable); 0 or -1 */
    int  (*publish)(void *ctx, void *obj, unsigned slot);        /* make it visible to the window system; 0 or -1 */
    void (*unpublish)(void *ctx, void *obj);
    void (*take_down)(void *ctx, void *obj, unsigned port);      /* nmo_teardown */
    void (*release)(void *ctx, void *obj);
};
struct nmhd_head { uint8_t state, port; void *obj; };
struct nmhd {
    struct nmhd_head h[NMHD_MAX];
    unsigned dev_users;          /* the panel counts as one */
    uint8_t  stopping, busy;
    uint8_t  replug[8];          /* ports whose plug arrived while busy */
    uint8_t  replug_n;
};

static inline void nmhd_init(struct nmhd *d) { memset(d, 0, sizeof *d); }
static inline int nmhd_find(const struct nmhd *d, unsigned port) { unsigned i; for (i = 0; i < NMHD_MAX; i++) if (d->h[i].state != NMHD_FREE && d->h[i].port == port) return (int)i; return -1; }
static inline unsigned nmhd_count(const struct nmhd *d) { unsigned i, n = 0; for (i = 0; i < NMHD_MAX; i++) if (d->h[i].state == NMHD_UP) n++; return n; }

/* The panel's own head: opens the device. Call once from the driver's start. */
static inline int nmhd_panel_up(struct nmhd *d, const struct nmhd_ops *o)
{
    if (d->dev_users == 0 && o->dev_open(o->ctx)) return -1;
    d->dev_users++;
    return 0;
}
static inline void nmhd_dev_release(struct nmhd *d, const struct nmhd_ops *o) { if (d->dev_users && --d->dev_users == 0) o->dev_close(o->ctx); }

static inline int nmhd_down(struct nmhd *d, const struct nmhd_ops *o, unsigned slot)
{
    struct nmhd_head *h = &d->h[slot];
    if (h->state != NMHD_UP && h->state != NMHD_STARTING) return -1;
    h->state = NMHD_STOPPING;
    o->unpublish(o->ctx, h->obj);                  /* the window system stops using it before the display goes away */
    o->take_down(o->ctx, h->obj, h->port);
    o->release(o->ctx, h->obj);
    h->obj = 0; h->state = NMHD_FREE;
    nmhd_dev_release(d, o);
    return 0;
}
static inline int nmhd_up(struct nmhd *d, const struct nmhd_ops *o, unsigned port)
{
    unsigned slot; void *obj;
    for (slot = 0; slot < NMHD_MAX && d->h[slot].state != NMHD_FREE; slot++) ;
    if (slot == NMHD_MAX) return -1;
    if (d->dev_users == 0) return -1;                      // the panel has not opened the device
    d->dev_users++;                                // this head holds the device
    obj = o->create(o->ctx, slot, port);
    if (!obj) { nmhd_dev_release(d, o); return -1; }
    d->h[slot].state = NMHD_STARTING; d->h[slot].port = (uint8_t)port; d->h[slot].obj = obj;
    if (o->bring_up(o->ctx, obj, port)) { o->take_down(o->ctx, obj, port); o->release(o->ctx, obj); d->h[slot].state = NMHD_FREE; d->h[slot].obj = 0; nmhd_dev_release(d, o); return -1; }
    if (o->publish(o->ctx, obj, slot)) { o->take_down(o->ctx, obj, port); o->release(o->ctx, obj); d->h[slot].state = NMHD_FREE; d->h[slot].obj = 0; nmhd_dev_release(d, o); return -1; }
    d->h[slot].state = NMHD_UP;
    return 0;
}
static inline void nmhd_replay(struct nmhd *d, const struct nmhd_ops *o);
/* A cable appeared on port. Returns 0 when a head is up (or was already), 1 when the plug was remembered for later, -1 when refused or failed. */
static inline int nmhd_plug(struct nmhd *d, const struct nmhd_ops *o, unsigned port)
{
    int r;
    if (d->stopping) return -1;
    if (d->busy) { if (d->replug_n < sizeof d->replug) d->replug[d->replug_n++] = (uint8_t)port; return 1; }
    if (nmhd_find(d, port) >= 0) return 0;
    d->busy = 1; r = nmhd_up(d, o, port); d->busy = 0;
    nmhd_replay(d, o);
    return r;
}
static inline int nmhd_unplug(struct nmhd *d, const struct nmhd_ops *o, unsigned port)
{
    int slot, r; unsigned i;
    /* a plug for this port that was waiting is cancelled by the unplug */
    for (i = 0; i < d->replug_n; i++) if (d->replug[i] == port) { memmove(d->replug + i, d->replug + i + 1, (unsigned)(d->replug_n - i - 1)); d->replug_n--; i--; }
    slot = nmhd_find(d, port);
    if (slot < 0) return 0;
    d->busy = 1; r = nmhd_down(d, o, (unsigned)slot); d->busy = 0;
    nmhd_replay(d, o);
    return r;
}
static inline void nmhd_replay(struct nmhd *d, const struct nmhd_ops *o)
{
    while (d->replug_n && !d->busy && !d->stopping) {
        unsigned port = d->replug[0];
        memmove(d->replug, d->replug + 1, (unsigned)(d->replug_n - 1)); d->replug_n--;
        nmhd_plug(d, o, port);
    }
}
/* The driver stops: no more plugs, every external head down (newest first), then the panel's hold on the device. */
static inline void nmhd_stop(struct nmhd *d, const struct nmhd_ops *o)
{
    int i;
    d->stopping = 1; d->replug_n = 0;
    for (i = NMHD_MAX - 1; i >= 0; i--) if (d->h[i].state == NMHD_UP || d->h[i].state == NMHD_STARTING) nmhd_down(d, o, (unsigned)i);
    nmhd_dev_release(d, o);
}

#endif

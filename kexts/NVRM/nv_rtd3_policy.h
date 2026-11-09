/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * When a discrete laptop GPU may have its power resource switched off (D3cold), and the order that keeps it safe.
 * A pure state machine so a host test runs the same code as the kext (tools/test_nv_rtd3_policy.py).
 *
 * The sequence mirrors the Linux drivers' runtime D3: the GPU is first idle with the firmware suspended (RM has taken
 * it to its low-power state), then the platform power resource named by the GPU's _PR3 is turned off with its _OFF
 * method; on the way back _ON runs first and the GPU is resumed only after it. Turning the resource off under a GPU
 * that is still driving a display or holds clients would take the screen away, so every condition below must hold.
 */
#pragma once

enum { NV_RTD3_ON = 0, NV_RTD3_OFF = 1 };

struct nv_rtd3 {
    int enabled;          /* the nvrtd3 boot argument was given: off unless asked for */
    int state;            /* NV_RTD3_ON or NV_RTD3_OFF */
    int have_resource;    /* a power resource path is known */
};

#define NV_RTD3_IDLE_SECONDS 60

/* 1 when the resource may be switched off now (rm_suspended: the driver is about to be, or is, suspended) */
static inline int nv_rtd3_may_power_off(const struct nv_rtd3 *s, int rm_suspended, int heads_lit, int clients, unsigned idle_s)
{
    return s->enabled && s->have_resource && s->state == NV_RTD3_ON && rm_suspended && heads_lit == 0 && clients == 0 &&
           idle_s >= NV_RTD3_IDLE_SECONDS;
}

/* What counts as "a head is lit" and "a client is attached" on a laptop.
 * nvidia_heads: framebuffers of this GPU's own outputs. foreign_heads: panels of another GPU that this GPU composites for
 * (hybrid switch nmhybrid on: the NVIDIA GPU draws the desktop, so it is never idle while such a panel is up). With the
 * switch off the other GPU draws its own desktop and a foreign panel says nothing about this GPU.
 * rm_clients: user clients of the resource manager service. accel_clients: user clients of the accelerator, which is where
 * every Metal application attaches; without them an app using the GPU would look like an idle GPU. */
static inline int nv_rtd3_heads_lit(int nvidia_heads, int foreign_heads, int hybrid_level)
{
    return nvidia_heads + (hybrid_level > 0 ? foreign_heads : 0);
}
static inline int nv_rtd3_clients_total(int rm_clients, int accel_clients) { return rm_clients + accel_clients; }

/* The GPU must not be touched while its resource is off: 1 when a register access or an RM call is allowed */
static inline int nv_rtd3_gpu_reachable(const struct nv_rtd3 *s) { return s->state == NV_RTD3_ON; }

/* Record the result of the _OFF / _ON evaluation. A failed _OFF leaves the GPU on; a failed _ON leaves it marked off so
 * nothing touches it, and the caller reports the error. */
static inline void nv_rtd3_off_done(struct nv_rtd3 *s, int ok) { if (ok) s->state = NV_RTD3_OFF; }
static inline void nv_rtd3_on_done(struct nv_rtd3 *s, int ok) { if (ok) s->state = NV_RTD3_ON; }

/* What the monitor needs from the driver, so a host test can drive the same sequence with fakes. */
struct nv_rtd3_ops {
    void *ctx;
    int  (*heads_lit)(void *ctx);   /* a framebuffer exists for this GPU */
    int  (*clients)(void *ctx);     /* user clients are attached */
    void (*suspend)(void *ctx);     /* the driver's system-sleep path: NVKMS, RM standby */
    void (*resume)(void *ctx);      /* the driver's wake path: RM resume, NVKMS */
    int  (*acpi_off)(void *ctx);    /* power resource _OFF, 1 on success */
    int  (*acpi_on)(void *ctx);     /* power resource _ON, 1 on success */
};

enum { NV_RTD3_TICK_NONE = 0, NV_RTD3_TICK_POWERED_OFF = 1, NV_RTD3_TICK_OFF_FAILED = -1, NV_RTD3_TICK_WOKE = 2, NV_RTD3_TICK_WAKE_FAILED = -2 };

/* One poll. `idle_s` is the seconds the GPU has been idle, kept by the caller; `period_s` is the seconds this tick stands for.
 * Idle long enough: suspend the driver, then switch the resource off; if that fails the driver is resumed at once and the
 * GPU stays on. Off and something wants the GPU: switch the resource on first, resume only after it. */
static inline int nv_rtd3_tick(struct nv_rtd3 *s, const struct nv_rtd3_ops *o, unsigned *idle_s, unsigned period_s)
{
    int busy = o->heads_lit(o->ctx) || o->clients(o->ctx);
    if (s->state == NV_RTD3_ON) {
        if (busy) { *idle_s = 0; return NV_RTD3_TICK_NONE; }
        *idle_s += period_s;
        if (!nv_rtd3_may_power_off(s, 1, 0, 0, *idle_s)) return NV_RTD3_TICK_NONE;
        o->suspend(o->ctx);
        int ok = o->acpi_off(o->ctx);
        nv_rtd3_off_done(s, ok);
        if (!ok) { o->resume(o->ctx); *idle_s = 0; return NV_RTD3_TICK_OFF_FAILED; }
        return NV_RTD3_TICK_POWERED_OFF;
    }
    if (!busy) return NV_RTD3_TICK_NONE;
    int ok = o->acpi_on(o->ctx);
    nv_rtd3_on_done(s, ok);
    if (!ok) return NV_RTD3_TICK_WAKE_FAILED;
    o->resume(o->ctx);
    *idle_s = 0;
    return NV_RTD3_TICK_WOKE;
}

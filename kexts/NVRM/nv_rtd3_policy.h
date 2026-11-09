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

/* 1 when the resource may be switched off now */
static inline int nv_rtd3_may_power_off(const struct nv_rtd3 *s, int rm_suspended, int heads_lit, int clients, unsigned idle_s)
{
    return s->enabled && s->have_resource && s->state == NV_RTD3_ON && rm_suspended && heads_lit == 0 && clients == 0 &&
           idle_s >= NV_RTD3_IDLE_SECONDS;
}

/* The GPU must not be touched while its resource is off: 1 when a register access or an RM call is allowed */
static inline int nv_rtd3_gpu_reachable(const struct nv_rtd3 *s) { return s->state == NV_RTD3_ON; }

/* Record the result of the _OFF / _ON evaluation. A failed _OFF leaves the GPU on; a failed _ON leaves it marked off so
 * nothing touches it, and the caller reports the error. */
static inline void nv_rtd3_off_done(struct nv_rtd3 *s, int ok) { if (ok) s->state = NV_RTD3_OFF; }
static inline void nv_rtd3_on_done(struct nv_rtd3 *s, int ok) { if (ok) s->state = NV_RTD3_ON; }

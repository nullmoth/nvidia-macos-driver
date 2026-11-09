/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Watermarks and display-buffer (DBUF) allocation for one plane on a second pipe: display versions 12 and 13, a linear 32-bit
 * plane covering the whole pipe, no scaler, no rotation, no cursor. Linux's i915 (skl_watermark.c skl_read_wm_latency, adjust_wm_latency,
 * skl_wm_method1, skl_wm_method2, skl_compute_wm_params, skl_compute_plane_wm, skl_check_wm_level, skl_linetime_wm; skl_universal_plane_regs.h;
 * MIT). The memory latencies come from the platform controller. A level whose buffer need exceeds the allocation is switched off, as
 * Linux does, and a mode whose level 0 cannot be met is refused: the pipe would underrun.
 * Plain C: tools/test_hybrid_ext.c.
 */
#ifndef NM_HYBRID_WM_H
#define NM_HYBRID_WM_H

#include <string.h>
#include "nm_hybrid_resume.h"

#define NMW_LEVELS 8
#define NMW_BLOCK  512u            /* bytes in a buffer block */
#define NMW_READ_MEM_LATENCY 0x6u  /* platform controller mailbox command (GEN9_PCODE_READ_MEM_LATENCY) */
#define NMW_FP_MAX 0xFFFFFFFFull

struct nmw_level { uint8_t enable; uint16_t blocks, lines, min_ddb; };
struct nmw_in { uint32_t pixel_khz; uint16_t hactive, htotal; uint8_t ver; uint16_t latency[NMW_LEVELS]; };

/* The eight memory latencies in microseconds (0 = level unusable), read two sets of four through the mailbox, then sanitised as Linux does. */
static inline int nmw_read_latency(const struct nmr_ops *o, unsigned ver, uint16_t wm[NMW_LEVELS])
{
    uint32_t v; unsigned set, i, n = NMW_LEVELS, level;
    const unsigned read_latency = ver >= 12 ? 3u : 2u;
    for (set = 0; set < 2; set++) {
        v = set;
        if (nmr_pcode(o, NMW_READ_MEM_LATENCY, &v, 1)) return -1;
        wm[set * 4 + 0] = (uint16_t)(v & 0xFFu); wm[set * 4 + 1] = (uint16_t)((v >> 8) & 0xFFu);
        wm[set * 4 + 2] = (uint16_t)((v >> 16) & 0xFFu); wm[set * 4 + 3] = (uint16_t)((v >> 24) & 0xFFu);
    }
    for (level = 1; level < n; level++) if (wm[level] == 0) { for (i = level + 1; i < n; i++) wm[i] = 0; n = level; break; }     /* a zero level disables every higher one */
    if (wm[0] == 0) for (level = 0; level < n; level++) wm[level] = (uint16_t)(wm[level] + read_latency);                        /* WaWmMemoryReadLatency */
    wm[0] = (uint16_t)(wm[0] + 1);                                                                                              /* the 16 GB DIMM adjustment, assumed when the DIMM is unknown */
    return 0;
}

/* 16.16 fixed point helpers (intel_pm / i915_fixed.h) */
static inline uint64_t nmw_div16(uint32_t val, uint32_t d) { return (((uint64_t)val << 16) + d - 1u) / d; }
static inline uint32_t nmw_up(uint64_t fp) { return (uint32_t)((fp + 0xFFFFu) >> 16); }

/* The watermark of every level for the plane. */
static inline void nmw_compute(const struct nmw_in *in, struct nmw_level out[NMW_LEVELS])
{
    const uint32_t cpp = 4, bytes_per_line = in->hactive * cpp;
    const uint32_t interm_pbpl = (bytes_per_line + NMW_BLOCK - 1u) / NMW_BLOCK + 1u;         /* linear: one block more (version 10 and later) */
    const uint64_t pbpl = (uint64_t)interm_pbpl << 16;
    const uint32_t linetime_us = nmw_up(nmw_div16(in->htotal * 1000u, in->pixel_khz));
    const unsigned max_lines = in->ver >= 13 ? 255u : 31u;
    unsigned level;
    for (level = 0; level < NMW_LEVELS; level++) {
        const uint32_t latency = in->latency[level];
        uint64_t m1, m2, sel; uint32_t blocks, lines, min_ddb, w;
        memset(&out[level], 0, sizeof out[level]);
        if (!latency) { out[level].min_ddb = 0xFFFFu; continue; }
        m1 = nmw_div16(latency * in->pixel_khz * cpp, 1000u * NMW_BLOCK) + 0x10000ull;                    /* method 1, plus one block */
        w = (uint32_t)(((uint64_t)latency * in->pixel_khz + (uint64_t)in->htotal * 1000u - 1u) / ((uint64_t)in->htotal * 1000u));
        m2 = (uint64_t)w * pbpl;                                                                             /* method 2 */
        if ((cpp * in->htotal / NMW_BLOCK < 1u) && (bytes_per_line / NMW_BLOCK < 1u)) sel = m2;
        else if (latency >= linetime_us) sel = m2;
        else sel = m1;
        blocks = nmw_up(sel) + 1u;                                                                           /* version below 30: one block more */
        if (blocks < nmw_up(pbpl)) blocks = nmw_up(pbpl);                                                   /* at least one line */
        lines = (uint32_t)((sel + pbpl - 1u) / pbpl);
        min_ddb = blocks + (blocks + 9u) / 10u;                                                              /* version 11 and later, linear */
        if (lines > max_lines) { out[level].min_ddb = 0xFFFFu; continue; }
        out[level].enable = 1; out[level].blocks = (uint16_t)blocks; out[level].lines = (uint16_t)lines;
        out[level].min_ddb = (uint16_t)((min_ddb > blocks ? min_ddb : blocks) + 1u);
        (void)m1;
    }
}
/* skl_check_wm_level: a level that needs more buffer than the plane has is switched off. Returns 0 when level 0 survives, -1 when the mode would underrun. */
static inline int nmw_fit(struct nmw_level lv[NMW_LEVELS], unsigned ddb_blocks)
{
    unsigned i;
    for (i = 0; i < NMW_LEVELS; i++) if (lv[i].min_ddb > ddb_blocks) { lv[i].enable = 0; lv[i].blocks = lv[i].lines = 0; }
    return lv[0].enable ? 0 : -1;
}
/* The words for the registers: PLANE_WM for each level (enable, lines, blocks), PLANE_BUF_CFG (end - 1 in 27:16, start in 11:0), the line time. */
static inline uint32_t nmw_word(const struct nmw_level *l) { return l->enable ? ((1u << 31) | ((uint32_t)l->lines << 14) | l->blocks) : 0u; }
static inline uint32_t nmw_buf_cfg(unsigned start, unsigned end) { return ((end - 1u) << 16) | start; }
static inline uint32_t nmw_linetime(const struct nmw_in *in) { uint32_t v = (in->htotal * 1000u * 8u + in->pixel_khz - 1u) / in->pixel_khz; return v < 0x1FFu ? v : 0x1FFu; }

/* The total display buffer in blocks: 2048 on version 12 (two slices), 4096 on version 13 (four) (Linux intel_display_device.c). */
static inline unsigned nmw_dbuf_total(unsigned ver) { return ver >= 13 ? 4096u : 2048u; }
/* The largest free run of the display buffer, given the total (blocks) and the range the other pipe's plane already holds. */
static inline void nmw_free_run(unsigned total, unsigned used_start, unsigned used_end, unsigned *start, unsigned *end)
{
    if (used_start >= used_end) { *start = 0; *end = total; return; }
    if (used_start >= total - used_end) { *start = 0; *end = used_start; } else { *start = used_end; *end = total; }
}

/* Program one plane (plane 1 of the pipe): its watermark words, its buffer range, the line time. */
static inline void nmw_program(const struct nmr_ops *o, unsigned pipe, const struct nmw_in *in, const struct nmw_level lv[NMW_LEVELS], unsigned start, unsigned end)
{
    unsigned i;
    for (i = 0; i < NMW_LEVELS; i++) o->wr(o->ctx, 0x70240u + 0x1000u * pipe + 4u * i, nmw_word(&lv[i]));
    o->wr(o->ctx, 0x7027Cu + 0x1000u * pipe, nmw_buf_cfg(start, end));
    o->wr(o->ctx, 0x70268u + 0x1000u * pipe, 0);                                   /* transition watermark off */
    o->wr(o->ctx, NMR_WM_LINETIME(pipe), nmw_linetime(in));
}

#endif

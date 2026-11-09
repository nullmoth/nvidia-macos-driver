/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * What an external DisplayPort display needs before a mode can be set on a second head of the integrated GPU: the sink's EDID and DPCD
 * over the port's AUX channel, the preferred mode, the cheapest link that carries it, the link and data M/N values, and the
 * transcoder register values that mode gives. The formulas are Linux's i915 (intel_dp.c intel_dp_compute_link_config and the rate
 * helpers, intel_display.c intel_link_compute_m_n and compute_m_n, intel_ddi.c TRANS_DDI_FUNC_CTL fields; MIT). 8 bits per colour, one
 * stream (no MST), no DSC, no spread spectrum. Plain C: tools/test_hybrid_ext.c.
 */
#ifndef NM_HYBRID_EXT_H
#define NM_HYBRID_EXT_H

#include "nm_hybrid_resume.h"

/* ---- EDID over I2C-over-AUX */
/* The first 128 bytes of the sink's EDID, 16 at a time (the AUX payload limit). I2C address 0x50: write the offset keeping the bus, read
 * with the bus kept, the last read releases it. 0 on success. */
static inline int nme_edid_read(const struct nmr_ops *o, unsigned port, uint8_t *buf128)
{
    unsigned off;
    uint8_t zero = 0;
    if (nmr_aux_raw(o, port, 4, 0x50, &zero, 1) != 1) return -1;
    for (off = 0; off < 128; off += 16)
        if (nmr_aux_raw(o, port, off + 16 < 128 ? 5 : 1, 0x50, buf128 + off, 16) != 16) return -1;
    return 0;
}

struct nme_timing {
    uint32_t clock_khz;
    uint16_t hactive, hblank, hso, hsw, vactive, vblank, vso, vsw;     /* blank is the whole blanking, so htotal = hactive + hblank; so is the sync offset from the end of active */
    uint8_t  hpos, vpos, interlaced;
};
static inline int nme_edid_sum_ok(const uint8_t *e) { unsigned i, s = 0; for (i = 0; i < 128; i++) s += e[i]; return (s & 0xFFu) == 0; }
/* The preferred mode: the first detailed timing descriptor (bytes 54 to 71). 0, or -1 for a bad header or checksum, -2 for no timing. */
static inline int nme_edid_preferred(const uint8_t *e, struct nme_timing *t)
{
    static const uint8_t hdr[8] = { 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0 };
    const uint8_t *d = e + 54; unsigned i;
    for (i = 0; i < 8; i++) if (e[i] != hdr[i]) return -1;
    if (!nme_edid_sum_ok(e)) return -1;
    if (!(d[0] | d[1])) return -2;                                      /* a display descriptor, not a timing */
    t->clock_khz = ((uint32_t)d[1] << 8 | d[0]) * 10u;
    t->hactive = (uint16_t)(d[2] | ((d[4] & 0xF0u) << 4)); t->hblank = (uint16_t)(d[3] | ((d[4] & 0x0Fu) << 8));
    t->vactive = (uint16_t)(d[5] | ((d[7] & 0xF0u) << 4)); t->vblank = (uint16_t)(d[6] | ((d[7] & 0x0Fu) << 8));
    t->hso = (uint16_t)(d[8] | ((d[11] & 0xC0u) << 2)); t->hsw = (uint16_t)(d[9] | ((d[11] & 0x30u) << 4));
    t->vso = (uint16_t)(((d[10] >> 4) & 0xFu) | ((d[11] & 0x0Cu) << 2)); t->vsw = (uint16_t)((d[10] & 0xFu) | ((d[11] & 0x03u) << 4));
    t->interlaced = (d[17] >> 7) & 1; t->hpos = (d[17] >> 1) & 1; t->vpos = (d[17] >> 2) & 1;
    if (!t->clock_khz || !t->hactive || !t->vactive || t->interlaced) return -2;       /* interlaced modes are not set here */
    return 0;
}

/* ---- DPCD receiver capabilities (bytes 0 to 2) */
struct nme_caps { uint32_t max_rate; unsigned max_lanes; unsigned enhanced; };       /* rate in 10 kbit/s units: 162000, 270000, 540000, 810000 */
static inline int nme_caps(const uint8_t *dpcd, struct nme_caps *c)
{
    const unsigned code = dpcd[1];
    if (code != 0x06 && code != 0x0A && code != 0x14 && code != 0x1E) return -1;
    c->max_rate = code * 27000u; c->max_lanes = dpcd[2] & 0x1Fu; c->enhanced = (dpcd[2] >> 7) & 1u;
    return (c->max_lanes == 1 || c->max_lanes == 2 || c->max_lanes == 4) ? 0 : -1;
}

/* ---- the link */
static inline uint32_t nme_max_data_rate(uint32_t rate, unsigned lanes) { return rate * lanes; }              /* kB/s with 8b/10b: rate x 10 kbit x 0.8 / 8 */
static inline uint32_t nme_required(uint32_t clock_khz, unsigned bpp) { return (clock_khz * bpp + 7u) / 8u; }       /* kB/s */
/* intel_dp_compute_link_config, slow and wide: the lowest rate, then the fewest lanes, that carries the mode. 0 on success. */
static inline int nme_link(const struct nme_caps *c, const struct nme_timing *t, unsigned bpp, uint32_t *rate, unsigned *lanes)
{
    static const uint32_t rates[4] = { 162000, 270000, 540000, 810000 };
    unsigned i, l;
    for (i = 0; i < 4; i++) {
        if (rates[i] > c->max_rate) break;
        for (l = 1; l <= c->max_lanes; l <<= 1)
            if (nme_required(t->clock_khz, bpp) <= nme_max_data_rate(rates[i], l)) { *rate = rates[i]; *lanes = l; return 0; }
    }
    return -1;
}

/* ---- M and N (intel_link_compute_m_n, compute_m_n, intel_reduce_m_n_ratio) */
struct nme_mn { uint32_t tu, data_m, data_n, link_m, link_n; };
static inline uint32_t nme_pow2_up(uint32_t v) { uint32_t p = 1; while (p < v) p <<= 1; return p; }
static inline void nme_compute_mn(uint32_t *ret_m, uint32_t *ret_n, uint32_t m, uint32_t n, uint32_t constant_n)
{
    uint32_t rn = constant_n ? constant_n : (nme_pow2_up(n) < 0x8000000u ? nme_pow2_up(n) : 0x8000000u);
    uint32_t rm = (uint32_t)(((uint64_t)m * rn) / n);
    while (rm > 0xFFFFFFu || rn > 0xFFFFFFu) { rm >>= 1; rn >>= 1; }       /* intel_reduce_m_n_ratio, DATA_LINK_M_N_MASK */
    if (!rm) rm = 1;
    *ret_m = rm; *ret_n = rn;
}
static inline void nme_mn(uint32_t clock_khz, unsigned bpp, unsigned lanes, uint32_t rate, struct nme_mn *mn)
{
    mn->tu = 64;
    nme_compute_mn(&mn->data_m, &mn->data_n, nme_required(clock_khz, bpp), nme_max_data_rate(rate, lanes), 0x8000000u);
    nme_compute_mn(&mn->link_m, &mn->link_n, clock_khz, rate, 0x80000u);      /* the symbol clock of an 8b/10b link equals the rate value in kHz */
}

/* ---- the transcoder's registers for the mode, in the order of NMR_TIMING: total, blank and sync for the horizontal and vertical, source size,
 * data M and N, link M and N. The M and N words carry the transfer unit in bits 30:25 of data M. */
static inline void nme_trans_regs(const struct nme_timing *t, const struct nme_mn *mn, uint32_t out[11])
{
    const unsigned htot = t->hactive + t->hblank, vtot = t->vactive + t->vblank;
    const unsigned hss = t->hactive + t->hso, vss = t->vactive + t->vso;
    out[0] = ((htot - 1u) << 16) | (t->hactive - 1u);
    out[1] = ((htot - 1u) << 16) | (t->hactive - 1u);                          /* blank: starts at the end of active, ends at the end of the line */
    out[2] = ((hss + t->hsw - 1u) << 16) | (hss - 1u);
    out[3] = ((vtot - 1u) << 16) | (t->vactive - 1u);
    out[4] = ((vtot - 1u) << 16) | (t->vactive - 1u);
    out[5] = ((vss + t->vsw - 1u) << 16) | (vss - 1u);
    out[6] = ((t->hactive - 1u) << 16) | (t->vactive - 1u);
    out[7] = ((mn->tu - 1u) << 25) | mn->data_m; out[8] = mn->data_n; out[9] = mn->link_m; out[10] = mn->link_n;
}
/* TRANS_DDI_FUNC_CTL: enable, port select (port + 1), DisplayPort single stream, 8 bits per colour, sync polarities, lane count. */
static inline uint32_t nme_func_ctl(unsigned ddi, const struct nme_timing *t, unsigned lanes)
{
    return (1u << 31) | ((ddi + 1u) << NMR_TRANS_PORT_SHIFT) | (2u << 24) | (t->hpos ? (1u << 16) : 0u) | (t->vpos ? (1u << 17) : 0u) | ((lanes - 1u) << 1);        /* TRANS_DDI_PORT_WIDTH */
}
#define NME_MSA_MISC_8BPC_SYNC 0x21u        /* synchronous clock, 8 bits per colour */

#endif

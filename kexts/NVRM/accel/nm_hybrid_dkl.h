/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * The Dekel PHY's PLL for a DisplayPort link on a Type-C port (display versions 12 and 13): the divider search for a link rate, the
 * register values it gives, the frequency those registers describe (the inverse, used to check the first), and the sequence that writes
 * them and turns the PLL on. Linux's i915 (intel_dpll_mgr.c icl_mg_pll_find_divisors, icl_calc_mg_pll_state, icl_ddi_mg_pll_get_freq,
 * dkl_pll_write, mg_pll_enable; intel_dkl_phy.c; intel_dkl_phy_regs.h; MIT), DisplayPort only, no spread spectrum.
 *
 * The Dekel PHY's registers are banked: a write to the HIP index register selects the bank (one byte per port), then the register is
 * reached at the port's PHY base plus the offset inside the bank.
 * Plain C: tools/test_hybrid_ext.c.
 */
#ifndef NM_HYBRID_DKL_H
#define NM_HYBRID_DKL_H

#include "nm_hybrid_resume.h"

#define NMK_PHY_BASE(tc)   (0x168000u + 0x1000u * (tc))
#define NMK_HIP_INDEX(tc)  ((tc) < 4 ? 0x1010A0u : 0x1010A4u)
#define NMK_REG_OFF(off)   ((off) & 0xFFFu)
#define NMK_REG_BANK(off)  (((off) >> 12) & 0xFu)
#define NMK_PLL_DIV0       0x2200u
#define NMK_PLL_DIV1       0x2204u
#define NMK_PLL_SSC        0x2210u
#define NMK_PLL_BIAS       0x2214u
#define NMK_PLL_TDC        0x2218u
#define NMK_REFCLKIN_CTL   0x212Cu
#define NMK_HSCLKCTL       0x20D4u
#define NMK_CORECLKCTL1    0x20D8u

#define NMK_DIV0_MASK      ((0x1Fu << 16) | (0xFu << 12) | (0xFu << 8) | 0xFFu)
#define NMK_AFC_MASK       (7u << 25)
#define NMK_DIV1_MASK      ((0x1Fu << 16) | 0xFFu)
#define NMK_SSC_MASK       ((7u << 29) | (0xFFu << 16) | (7u << 11) | (1u << 9))
#define NMK_BIAS_MASK      ((1u << 30) | (0x3FFFFFu << 8))
#define NMK_TDC_MASK       ((0xFFu << 8) | 0xFFu)
#define NMK_REFCLK_OD2_MASK (7u << 8)
#define NMK_CORE_A_MASK    (0xFFu << 8)
#define NMK_HSCLK_MASK     ((3u << 14) | (1u << 16) | (3u << 12) | (0xFu << 8))

#define NMK_PLL_ENABLE_REG(ver, tc) ((ver) == 13 ? 0x46038u + 8u * (tc) : 0x46030u + 4u * (tc))   /* ADLP_PORTTC_PLL_ENABLE / MG_PLL_ENABLE */
#define NMK_PLL_ENABLE     (1u << 31)
#define NMK_PLL_LOCK       (1u << 30)
#define NMK_PLL_PWR_ENABLE (1u << 27)
#define NMK_PLL_PWR_STATE  (1u << 26)

struct nmk_pll { uint32_t refclkin, coreclk, hsclk, div0, div1, ssc, bias, tdc; };

/* The divider search and the register values for a DisplayPort link clock (kHz, 162000 to 810000) and a reference clock (kHz, 19200, 24000 or
 * 38400). 0 on success, -1 when no divider fits. */
static inline int nmk_calc(uint32_t refclk_khz, uint32_t clock_khz, struct nmk_pll *p)
{
    static const unsigned div1_vals[] = { 7, 5, 3, 2 };
    unsigned i, hsdiv_code = 0, tlinedrv, a_divratio; int div2, found = 0;
    uint32_t dco = 0, m1div, m2int, m2rem, m2frac, iref_ndiv, iref_trim, tdc_target, feedfwd, prop, integ;
    uint64_t tmp;
    p->refclkin = p->coreclk = p->hsclk = p->div0 = p->div1 = p->ssc = p->bias = p->tdc = 0;
    for (i = 0; i < 4 && !found; i++) {
        const unsigned div1 = div1_vals[i];
        for (div2 = 10; div2 > 0; div2--) {
            const uint64_t d = (uint64_t)div1 * (unsigned)div2 * clock_khz * 5u;
            if (d != 8100000u) continue;                              /* DisplayPort: the DCO is fixed at 8.1 GHz */
            a_divratio = div2 >= 2 ? 10u : 5u; tlinedrv = div2 >= 2 ? 1u : 0u;
            hsdiv_code = div1 == 7 ? 3u : (div1 == 5 ? 2u : (div1 == 3 ? 1u : 0u));
            dco = (uint32_t)d;
            p->refclkin = 1u << 8;                                    /* OD_2_MUX(1) */
            p->coreclk = a_divratio << 8;
            p->hsclk = (tlinedrv << 14) | (0u << 16) | (hsdiv_code << 12) | ((unsigned)div2 << 8);
            found = 1; break;
        }
    }
    if (!found) return -1;
    m1div = 2; m2int = dco / (refclk_khz * m1div);
    if (m2int > 255) return -1;
    m2rem = dco % (refclk_khz * m1div);
    tmp = (uint64_t)m2rem * (1u << 22); tmp /= (uint64_t)refclk_khz * m1div; m2frac = (uint32_t)tmp;
    switch (refclk_khz) { case 19200: iref_ndiv = 1; iref_trim = 28; break; case 24000: iref_ndiv = 1; iref_trim = 25; break; case 38400: iref_ndiv = 2; iref_trim = 28; break; default: return -1; }
    tdc_target = (2u * 1000u * 100000u * 10u / (132u * refclk_khz) + 5u) / 10u;
    feedfwd = m2rem > 0 ? (uint32_t)((uint64_t)m1div * 1000000u * 100u / ((uint64_t)dco * 3u / 10u)) : 0u;
    if (dco >= 9000000u) { prop = 5; integ = 10; } else { prop = 4; integ = 8; }
    p->div0 = (integ << 16) | (prop << 12) | (m1div << 8) | m2int;
    p->div1 = (iref_trim << 16) | tdc_target;
    p->ssc = (iref_ndiv << 29) | (4u << 11);                         /* step number log 4, no spread spectrum */
    p->bias = (m2frac ? (1u << 30) : 0u) | (m2frac << 8);
    p->tdc = feedfwd;
    return 0;
}
/* The link clock these registers describe, kHz (icl_ddi_mg_pll_get_freq, Dekel branch). */
static inline uint32_t nmk_freq(uint32_t refclk_khz, const struct nmk_pll *p)
{
    const uint32_t m1 = (p->div0 >> 8) & 0xFu, m2int = p->div0 & 0xFFu;
    const uint32_t m2frac = (p->bias & (1u << 30)) ? ((p->bias >> 8) & 0x3FFFFFu) : 0u;
    static const uint32_t div1v[4] = { 2, 3, 5, 7 };
    uint32_t div1 = div1v[(p->hsclk >> 12) & 3u], div2 = (p->hsclk >> 8) & 0xFu;
    uint64_t t;
    if (!div2) div2 = 1;
    t = (uint64_t)m1 * m2int * refclk_khz + (((uint64_t)m1 * m2frac * refclk_khz) >> 22);
    return (uint32_t)(t / (5ull * div1 * div2));
}

/* One banked register: select the bank, then touch it. The index register carries one byte per port. */
static inline void nmk_sel(const struct nmr_ops *o, unsigned tc, unsigned off) { o->wr(o->ctx, NMK_HIP_INDEX(tc), NMK_REG_BANK(off) << (8 * (tc % 4))); }
static inline uint32_t nmk_rd(const struct nmr_ops *o, unsigned tc, unsigned off) { nmk_sel(o, tc, off); return o->rd(o->ctx, NMK_PHY_BASE(tc) + NMK_REG_OFF(off)); }
static inline void nmk_wr(const struct nmr_ops *o, unsigned tc, unsigned off, uint32_t v) { nmk_sel(o, tc, off); o->wr(o->ctx, NMK_PHY_BASE(tc) + NMK_REG_OFF(off), v); }
static inline void nmk_rmw(const struct nmr_ops *o, unsigned tc, unsigned off, uint32_t clear, uint32_t set) { nmk_wr(o, tc, off, (nmk_rd(o, tc, off) & ~clear) | set); }

/* dkl_pll_write: every register is a read-modify-write, in this order. */
static inline void nmk_write(const struct nmr_ops *o, unsigned tc, const struct nmk_pll *p)
{
    nmk_rmw(o, tc, NMK_REFCLKIN_CTL, NMK_REFCLK_OD2_MASK, p->refclkin);
    nmk_rmw(o, tc, NMK_CORECLKCTL1, NMK_CORE_A_MASK, p->coreclk);
    nmk_rmw(o, tc, NMK_HSCLKCTL, NMK_HSCLK_MASK, p->hsclk);
    nmk_rmw(o, tc, NMK_PLL_DIV0, NMK_DIV0_MASK, p->div0);
    nmk_rmw(o, tc, NMK_PLL_DIV1, NMK_DIV1_MASK, p->div1);
    nmk_rmw(o, tc, NMK_PLL_SSC, NMK_SSC_MASK, p->ssc);
    nmk_rmw(o, tc, NMK_PLL_BIAS, NMK_BIAS_MASK, p->bias);
    nmk_rmw(o, tc, NMK_PLL_TDC, NMK_TDC_MASK, p->tdc);
    (void)nmk_rd(o, tc, NMK_PLL_TDC);                                 /* posting read */
}
/* mg_pll_enable: power the PLL, wait for it, program it, enable it, wait for lock. 0 on success. */
static inline int nmk_enable(const struct nmr_ops *o, unsigned ver, unsigned tc, const struct nmk_pll *p)
{
    const uint32_t en = NMK_PLL_ENABLE_REG(ver, tc);
    o->wr(o->ctx, en, o->rd(o->ctx, en) | NMK_PLL_PWR_ENABLE);
    if (!nmr_wait(o, en, NMK_PLL_PWR_STATE, NMK_PLL_PWR_STATE, 1000)) return -1;
    nmk_write(o, tc, p);
    o->wr(o->ctx, en, o->rd(o->ctx, en) | NMK_PLL_ENABLE);
    if (!nmr_wait(o, en, NMK_PLL_LOCK, NMK_PLL_LOCK, 5000)) return -2;
    return 0;
}

/* ---- the transmitter lanes (intel_ddi.c tgl_dkl_phy_set_signal_levels; tables from intel_ddi_buf_trans.c). Each of the two PHY lanes serves two of
 * the four link lanes through TX_DPCNTL0 and TX_DPCNTL1; the lane suspend register is cleared first, and from version 13 the load-generator select
 * is zeroed for DisplayPort. */
#define NMK_TX_DPCNTL0(ln)    ((ln) ? 0x12C0u : 0x02C0u)
#define NMK_TX_DPCNTL1(ln)    ((ln) ? 0x12C4u : 0x02C4u)
#define NMK_TX_DPCNTL2(ln)    ((ln) ? 0x12C8u : 0x02C8u)
#define NMK_TX_PMD_LANE_SUS(ln) ((ln) ? 0x1D00u : 0x0D00u)
#define NMK_PCS_DW5(ln)       ((ln) ? 0x1014u : 0x0014u)
#define NMK_PCS_SOFTRESET     (1u << 11)
#define NMK_DPCNTL_MASK       ((0x1Fu << 13) | (0x1Fu << 8) | 7u)
#define NMK_DP20BITMODE       (1u << 2)
#define NMK_LOADGEN_MASK      ((3u << 3) | (3u << 5))
struct nmk_trans { uint8_t vswing, preshoot, de_emphasis; };
static const struct nmk_trans nmk_tgl_hbr[10] = { {7,0,0x00},{5,0,0x05},{2,0,0x0B},{0,0,0x18},{5,0,0x00},{2,0,0x08},{0,0,0x14},{2,0,0x00},{0,0,0x0B},{0,0,0x00} };
static const struct nmk_trans nmk_tgl_hbr2[10] = { {7,0,0x00},{5,0,0x05},{2,0,0x0B},{0,0,0x19},{5,0,0x00},{2,0,0x08},{0,0,0x14},{2,0,0x00},{0,0,0x0B},{0,0,0x00} };
static const struct nmk_trans nmk_adlp_hbr[10] = { {7,0,0x01},{5,0,0x06},{2,0,0x0B},{0,0,0x17},{5,0,0x00},{2,0,0x08},{0,0,0x14},{2,0,0x00},{0,0,0x0B},{0,0,0x00} };
static const struct nmk_trans nmk_adlp_hbr2[10] = { {7,0,0x00},{5,0,0x04},{2,0,0x0A},{0,0,0x18},{5,0,0x00},{2,0,0x06},{0,0,0x14},{2,0,0x00},{0,0,0x09},{0,0,0x00} };
static inline const struct nmk_trans *nmk_table(unsigned ver, uint32_t rate)
{
    if (ver == 13) return rate > 270000 ? nmk_adlp_hbr2 : nmk_adlp_hbr;
    return rate > 270000 ? nmk_tgl_hbr2 : nmk_tgl_hbr;
}
/* Set the swing (0 to 3) and pre-emphasis (0 to 3) on every lane. 0, or -1 for a combination DisplayPort does not allow. */
static inline int nmk_set_levels(const struct nmr_ops *o, unsigned ver, unsigned tc, uint32_t rate, unsigned v, unsigned p)
{
    const int idx = nmr_level_index(v, p);
    const struct nmk_trans *t;
    unsigned ln;
    if (idx < 0) return -1;
    t = &nmk_table(ver, rate)[idx];
    for (ln = 0; ln < 2; ln++) {
        const uint32_t set = ((uint32_t)t->preshoot << 13) | ((uint32_t)t->de_emphasis << 8) | t->vswing;
        nmk_wr(o, tc, NMK_TX_PMD_LANE_SUS(ln), 0);
        nmk_rmw(o, tc, NMK_TX_DPCNTL0(ln), NMK_DPCNTL_MASK, set);
        nmk_rmw(o, tc, NMK_TX_DPCNTL1(ln), NMK_DPCNTL_MASK, set);
        nmk_rmw(o, tc, NMK_TX_DPCNTL2(ln), NMK_DP20BITMODE, 0);
        if (ver == 13) nmk_rmw(o, tc, NMK_TX_DPCNTL2(ln), NMK_LOADGEN_MASK, 0);
    }
    return 0;
}
/* The soft-reset bit of both lanes' PCS register cleared (adlp_tbt_to_dp_alt_switch_wa), needed when a port moves from Thunderbolt to DisplayPort-alt. */
static inline void nmk_lane_unreset(const struct nmr_ops *o, unsigned tc) { unsigned ln; for (ln = 0; ln < 2; ln++) nmk_rmw(o, tc, NMK_PCS_DW5(ln), NMK_PCS_SOFTRESET, 0); }

/* ---- routing the PLL to the port and the port to the transcoder (icl_ddi_tc_enable_clock, intel_ddi_enable_transcoder_clock) */
#define NMK_PORT_CLK_SEL(ddi)  (0x46100u + 4u * (ddi))
#define NMK_CLK_SEL_MG         (0x8u << 28)
#define NMK_DPCLKA_CFGCR0      0x164280u
#define NMK_TC_CLK_OFF(tc)     (1u << ((tc) < 4 ? (tc) + 12 : (tc) - 4 + 21))
#define NMK_TRANS_CLK_SEL(t)   (0x46140u + 4u * (t))
static inline void nmk_clock_on(const struct nmr_ops *o, unsigned ddi, unsigned tc)
{
    o->wr(o->ctx, NMK_PORT_CLK_SEL(ddi), NMK_CLK_SEL_MG);
    o->wr(o->ctx, NMK_DPCLKA_CFGCR0, o->rd(o->ctx, NMK_DPCLKA_CFGCR0) & ~NMK_TC_CLK_OFF(tc));
}
static inline void nmk_clock_off(const struct nmr_ops *o, unsigned ddi, unsigned tc)
{
    o->wr(o->ctx, NMK_DPCLKA_CFGCR0, o->rd(o->ctx, NMK_DPCLKA_CFGCR0) | NMK_TC_CLK_OFF(tc));
    o->wr(o->ctx, NMK_PORT_CLK_SEL(ddi), 0);
}
static inline void nmk_trans_clock(const struct nmr_ops *o, unsigned t, unsigned ddi, int on) { o->wr(o->ctx, NMK_TRANS_CLK_SEL(t), on ? ((ddi + 1u) << 28) : 0u); }

#endif

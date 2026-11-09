/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Relighting a laptop panel after sleep on display version 14 (Meteor Lake, Arrow Lake): the Cx0 (C10) PHY.
 *
 * Versions 12 and 13 have a combo PHY whose registers are plain MMIO (nm_hybrid_resume.h). From version 14 the PHY is a Synopsys
 * C10 block reached only through a per-port message bus, with a handshake for every power state and for the PLL. This header
 * is that part: the message bus (read, write, bus reset, retries), the capture of the PHY's PLL and lane registers while the
 * firmware's mode is lit, and the enable sequence in the order Linux's i915 uses (intel_cx0_phy.c, intel_ddi.c
 * mtl_ddi_pre_enable_dp, intel_pmdemand.c; MIT): clock control, lane reset, power state ready, PLL registers, lane
 * transmitters, clock frequency, PLL request and acknowledge, then the D2D link, signal levels, port buffer width and the
 * DDI buffer. Everything else (power wells, display clock, panel power, pipe and plane registers, link training, transcoder)
 * is the same code as versions 12 and 13.
 *
 * The PLL is not computed from link-rate tables: the firmware's own C10 register file is read back over the message bus while
 * the panel is lit and written back on wake. Only the swing and pre-emphasis levels a sink may ask for during training come
 * from a table (mtl_c10_trans_dp14).
 *
 * Display version 14 only, eDP on a C10 port (DDI A or B). Version 20 and later (Lunar Lake, Panther Lake) keep these registers
 * elsewhere and are refused. Plain C: tools/test_hybrid_resume14.c runs this against a model of the message bus and PHY.
 */
#ifndef NM_HYBRID_RESUME14_H
#define NM_HYBRID_RESUME14_H

#include "nm_hybrid_resume.h"
#include "nm_hybrid_c20tables.h"

/* ---- registers. Version 14: ports A and B, stride 0x100 from 0x64000. Version 20 and later: the same blocks sit in the second range
 * Linux's __xe2lpd_port_idx selects, at 0x16F200 + 0x200 * (4 + port). A "port" argument carries two flags: NMX_X2 (the version 20
 * layout) and NMX_X3 (version 30: the message bus is not reset after every transaction). */
#define NMX_X2             0x10u
#define NMX_X3             0x20u
#define NMX_PORT(p)        ((p) & 0xFu)
#define NMX_BLK(p)         (0x16F200u + 0x200u * (4u + NMX_PORT(p)))
#define NMX_OFF(p, a, b)   (((p) & NMX_X2) ? NMX_BLK(p) + (b) : (a) + 0x100u * NMX_PORT(p))
#define NMX_BUF_CTL1(p)    NMX_OFF(p, 0x64004u, 0x00u)
#define NMX_BUF_CTL2(p)    NMX_OFF(p, 0x64008u, 0x04u)
#define NMX_BUF_CTL3(p)    NMX_OFF(p, 0x6400Cu, 0x08u)
#define NMX_CLK_VALFREQ(p) (0x64030u + 0x100u * NMX_PORT(p))
#define NMX_M2P_CTL(p, l)  (NMX_OFF(p, 0x64040u, 0x40u) + 4u * (l))
#define NMX_P2M_STAT(p, l) (NMX_OFF(p, 0x64048u, 0x48u) + 4u * (l))
#define NMX_MB_TIMER(p, l) (NMX_OFF(p, 0x640D8u, 0x58u) + 4u * (l))
#define NMX_CLOCK_CTL(p)   NMX_OFF(p, 0x640E0u, 0x60u)
#define NMX_PMDEMAND(d)    (0x45230u + 4u * (d))
#define NMX_DCPR_STATUS_1  0x46440u

#define NMX_M2P_PENDING    (1u << 31)
#define NMX_M2P_WRITE_UNC  (1u << 27)
#define NMX_M2P_WRITE_COM  (2u << 27)
#define NMX_M2P_READ       (3u << 27)
#define NMX_M2P_RESET      (1u << 15)
#define NMX_P2M_READY      (1u << 31)
#define NMX_P2M_ERROR      (1u << 15)
#define NMX_P2M_READ_ACK   4u
#define NMX_P2M_WRITE_ACK  5u
#define NMX_TIMER_VAL      0xA000u

#define NMX_C10_PLL(i)     (0xC00u + (i))          /* 20 bytes */
#define NMX_C10_CMN(i)     (0xC20u + (i))
#define NMX_C10_TX(i)      (0xC30u + (i))
#define NMX_C10_CONTROL(i) (0xC70u + (i) - 1u)
#define NMX_C10_WIDTH      0xD02u
#define NMX_C10_OVRD       0xD71u
#define NMX_C10_VDROVRD(lane, tx, c) (0xD80u + (((lane) ^ (tx)) * 0x10u) + (c))
#define NMX_TX_CONTROL(tx, c) (0x400u + ((tx) - 1u) * 0x200u + (c))
#define NMX_CTRL_MSGBUS    0x04u
#define NMX_CTRL_MASTER    0x02u
#define NMX_CTRL_UPDATE    0x01u
#define NMX_DISABLE_TX     0x40u

enum { NMX_LANE0 = 1, NMX_LANE1 = 2, NMX_BOTH = 3 };
#define NMX_P2_READY 0x2u
#define NMX_P2_RESET 0x2u

/* ---- the message bus */
static inline void nmx_clear_ready(const struct nmr_ops *o, unsigned port, unsigned lane)
{
    o->wr(o->ctx, NMX_P2M_STAT(port, lane), o->rd(o->ctx, NMX_P2M_STAT(port, lane)) | NMX_P2M_READY | NMX_P2M_ERROR);
}
static inline void nmx_bus_reset(const struct nmr_ops *o, unsigned port, unsigned lane)
{
    o->wr(o->ctx, NMX_M2P_CTL(port, lane), NMX_M2P_RESET);
    if (!nmr_wait(o, NMX_M2P_CTL(port, lane), NMX_M2P_RESET, 0, 1000)) return;
    nmx_clear_ready(o, port, lane);
}
static inline int nmx_wait_ack(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned want, uint32_t *val)
{
    unsigned t;
    for (t = 0; t <= 1000; t += 2) {
        *val = o->rd(o->ctx, NMX_P2M_STAT(port, lane));
        if (*val & NMX_P2M_READY) break;
        o->delay_us(o->ctx, 2);
    }
    if (!(*val & NMX_P2M_READY)) { nmx_bus_reset(o, port, lane); return -1; }
    if (*val & NMX_P2M_ERROR) { nmx_bus_reset(o, port, lane); return -1; }
    if (((*val >> 27) & 0xF) != want) { nmx_bus_reset(o, port, lane); return -1; }
    return 0;
}
static inline int nmx_read_once(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned addr)
{
    uint32_t v;
    if (!nmr_wait(o, NMX_M2P_CTL(port, lane), NMX_M2P_PENDING, 0, 1000)) { nmx_bus_reset(o, port, lane); return -1; }
    o->wr(o->ctx, NMX_M2P_CTL(port, lane), NMX_M2P_PENDING | NMX_M2P_READ | (addr & 0xFFFu));
    if (nmx_wait_ack(o, port, lane, NMX_P2M_READ_ACK, &v)) return -1;
    nmx_clear_ready(o, port, lane);
    if (!(port & NMX_X3)) nmx_bus_reset(o, port, lane);          /* Linux settles the bus after every transaction below display 30 */
    return (int)((v >> 16) & 0xFF);
}
static inline int nmx_read(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned addr, uint8_t *out)
{
    int i, r;
    for (i = 0; i < 3; i++) { r = nmx_read_once(o, port, lane, addr); if (r >= 0) { *out = (uint8_t)r; return 0; } }
    return -1;
}
static inline int nmx_write_once(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned addr, unsigned data, int committed)
{
    uint32_t v;
    if (!nmr_wait(o, NMX_M2P_CTL(port, lane), NMX_M2P_PENDING, 0, 1000)) { nmx_bus_reset(o, port, lane); return -1; }
    o->wr(o->ctx, NMX_M2P_CTL(port, lane), NMX_M2P_PENDING | (committed ? NMX_M2P_WRITE_COM : NMX_M2P_WRITE_UNC) | ((data & 0xFFu) << 16) | (addr & 0xFFFu));
    if (!nmr_wait(o, NMX_M2P_CTL(port, lane), NMX_M2P_PENDING, 0, 1000)) { nmx_bus_reset(o, port, lane); return -1; }
    if (committed) { if (nmx_wait_ack(o, port, lane, NMX_P2M_WRITE_ACK, &v)) return -1; }
    else if (o->rd(o->ctx, NMX_P2M_STAT(port, lane)) & NMX_P2M_ERROR) { nmx_bus_reset(o, port, lane); return -1; }
    nmx_clear_ready(o, port, lane);
    if (!(port & NMX_X3)) nmx_bus_reset(o, port, lane);
    return 0;
}
static inline int nmx_write(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned addr, unsigned data, int committed)
{
    int i;
    for (i = 0; i < 3; i++) if (!nmx_write_once(o, port, lane, addr, data, committed)) return 0;
    return -1;
}
/* a write to every lane in the mask */
static inline int nmx_write_m(const struct nmr_ops *o, unsigned port, unsigned mask, unsigned addr, unsigned data, int committed)
{
    int r = 0, l;
    for (l = 0; l < 2; l++) if (mask & (1u << l)) r |= nmx_write(o, port, (unsigned)l, addr, data, committed);
    return r;
}
static inline int nmx_rmw_m(const struct nmr_ops *o, unsigned port, unsigned mask, unsigned addr, unsigned clear, unsigned set, int committed)
{
    int r = 0, l;
    for (l = 0; l < 2; l++) if (mask & (1u << l)) {
        uint8_t old;
        if (nmx_read(o, port, (unsigned)l, addr, &old)) { r = -1; continue; }
        if ((uint8_t)((old & ~clear) | set) != old) r |= nmx_write(o, port, (unsigned)l, addr, (old & ~clear) | set, committed);
    }
    return r;
}
static inline void nmx_program_timer(const struct nmr_ops *o, unsigned port)
{
    unsigned l;
    for (l = 0; l < 2; l++) o->wr(o->ctx, NMX_MB_TIMER(port, l), (o->rd(o->ctx, NMX_MB_TIMER(port, l)) & ~0xFFFFFFu) | NMX_TIMER_VAL);
}

/* ---- the C20 PHY (Linux intel_cx0_phy.c: intel_c20_sram_write/read, intel_c20pll_readout_hw_state, intel_c20_pll_program). Its PLL
 * state is 16-bit words in the PHY's own SRAM, two contexts (A and B); programming writes the idle context and toggles. */
#define NMC20_WR_ADDR_L 0xC02u
#define NMC20_WR_ADDR_H 0xC03u
#define NMC20_WR_DATA_L 0xC04u
#define NMC20_WR_DATA_H 0xC05u
#define NMC20_RD_ADDR_L 0xC06u
#define NMC20_RD_ADDR_H 0xC07u
#define NMC20_RD_DATA_L 0xC08u
#define NMC20_RD_DATA_H 0xC09u
#define NMC20_SERDES    0xD00u
#define NMC20_WIDTH     0xD02u
#define NMC20_CNTX_TOGGLE 0x01u
#define NMC20_SERDES_MASK 0x1Eu
#define NMC20_USE_MPLLB 0x0080u
/* SRAM word addresses for display versions 14, 20 and 30 (A context, B context); word i is at base - i */
#define NMC20_TX(b, i)   ((b) ? 0xCF2Au - (i) : 0xCF2Eu - (i))
#define NMC20_CMN(b, i)  ((b) ? 0xCDA5u - (i) : 0xCDAAu - (i))
#define NMC20_MPLLA(b, i) ((b) ? 0xCCE5u - (i) : 0xCCF0u - (i))
#define NMC20_MPLLB(b, i) ((b) ? 0xCB4Eu - (i) : 0xCB5Au - (i))

static inline int nmc20_sram_write(const struct nmr_ops *o, unsigned port, unsigned mask, unsigned addr, unsigned data)
{
    int r = 0;
    r |= nmx_write_m(o, port, mask, NMC20_WR_ADDR_H, addr >> 8, 0);
    r |= nmx_write_m(o, port, mask, NMC20_WR_ADDR_L, addr & 0xFFu, 0);
    r |= nmx_write_m(o, port, mask, NMC20_WR_DATA_H, data >> 8, 0);
    r |= nmx_write_m(o, port, mask, NMC20_WR_DATA_L, data & 0xFFu, 1);
    return r;
}
static inline int nmc20_sram_read(const struct nmr_ops *o, unsigned port, unsigned lane, unsigned addr, uint16_t *out)
{
    uint8_t h, l; const unsigned mask = 1u << lane;
    if (nmx_write_m(o, port, mask, NMC20_RD_ADDR_H, addr >> 8, 0)) return -1;
    if (nmx_write_m(o, port, mask, NMC20_RD_ADDR_L, addr & 0xFFu, 1)) return -1;
    if (nmx_read(o, port, lane, NMC20_RD_DATA_H, &h) || nmx_read(o, port, lane, NMC20_RD_DATA_L, &l)) return -1;
    *out = (uint16_t)((h << 8) | l);
    return 0;
}
/* The port clock in kHz that a C20 state describes (intel_c20pll_calc_port_clock). */
static inline uint32_t nmc20_clock(const uint16_t *tx, const uint16_t *mp)
{
    const int b = (tx[0] & NMC20_USE_MPLLB) != 0;
    uint64_t frac_en, frac_quot, frac_rem, frac_den, mult, tx_div, refdiv, div4, frac, ref, vco, txrate_mult;
    const unsigned tx_rate = tx[0] & 0x7u;
    if (b) { txrate_mult = 1; frac_en = (mp[6] >> 13) & 1; frac_quot = mp[8]; frac_rem = mp[9]; frac_den = mp[7]; mult = mp[0] & 0xFFFu;
             tx_div = (mp[0] >> 13) & 7; refdiv = (mp[6] >> 10) & 7; div4 = 0; }
    else   { txrate_mult = 2; frac_en = (mp[6] >> 14) & 1; frac_quot = mp[8]; frac_rem = mp[9]; frac_den = mp[7]; mult = mp[0] & 0xFFFu;
             tx_div = (mp[1] >> 8) & 7; refdiv = (mp[6] >> 10) & 7; div4 = (mp[0] >> 13) & 1; }
    frac = frac_en ? frac_quot + (frac_den ? (frac_rem + frac_den / 2) / frac_den : 0) : 0;
    ref = (38400ull * (1ull << (1 + div4)) + ((1ull << refdiv) >> 1)) >> refdiv;       /* round to closest, as DIV_ROUND_CLOSEST */
    vco = ((((ref * ((mult << 15) + frac)) >> 17) + 5) / 10);
    return (uint32_t)(((vco << txrate_mult) >> tx_div) >> tx_rate);
}
/* The register file as the firmware left it, over the bus (intel_c20pll_readout_hw_state). Lane 0 only. */
static inline int nmc20_snapshot_regs(const struct nmr_ops *o, struct nmx_snap *s, unsigned port)
{
    uint8_t sr, wd; unsigned i, l, t, c; const unsigned b = 0;
    int cntx;
    if (nmx_read(o, port, 0, NMC20_SERDES, &sr) || nmx_read(o, port, 0, NMC20_WIDTH, &wd)) return NMR_E_NO_SNAPSHOT;
    cntx = (sr & NMC20_CNTX_TOGGLE) != 0; (void)b;
    s->c20_serdes = sr; s->c20_width = wd;
    for (i = 0; i < 3; i++) if (nmc20_sram_read(o, port, 0, NMC20_TX(cntx, i), &s->c20_tx[i])) return NMR_E_NO_SNAPSHOT;
    for (i = 0; i < 4; i++) if (nmc20_sram_read(o, port, 0, NMC20_CMN(cntx, i), &s->c20_cmn[i])) return NMR_E_NO_SNAPSHOT;
    if (s->c20_tx[0] & NMC20_USE_MPLLB) { for (i = 0; i < 11; i++) if (nmc20_sram_read(o, port, 0, NMC20_MPLLB(cntx, i), &s->c20_mp[i])) return NMR_E_NO_SNAPSHOT; }
    else { for (i = 0; i < 10; i++) if (nmc20_sram_read(o, port, 0, NMC20_MPLLA(cntx, i), &s->c20_mp[i])) return NMR_E_NO_SNAPSHOT; s->c20_mp[10] = 0; }
    s->c20_clock = nmc20_clock(s->c20_tx, s->c20_mp);
    /* the signal-level override registers are shared with the C10 layout */
    for (l = 0; l < 2; l++) {
        if (nmx_read(o, port, l, NMX_C10_OVRD, &s->ovrd[l])) return NMR_E_NO_SNAPSHOT;
        for (t = 0; t < 2; t++) for (c = 0; c < 3; c++) if (nmx_read(o, port, l, NMX_C10_VDROVRD(l, t, c), &s->vdrovrd[l][t][c])) return NMR_E_NO_SNAPSHOT;
    }
    s->valid = 1;
    return NMR_OK;
}
static inline unsigned nmc20_dp_rate_code(uint32_t clock)
{
    switch (clock) {
    case 162000: return 0;  case 270000: return 1;  case 540000: return 2;  case 810000: return 3;  case 216000: return 4;
    case 243000: return 5;  case 324000: return 6;  case 432000: return 7;  case 1000000: return 8; case 1350000: return 9;
    case 2000000: return 10; case 648000: return 11; case 675000: return 12;
    default: return 0;
    }
}
static inline unsigned nmc20_width_code(uint32_t clock) { return (clock == 1000000 || clock == 1350000 || clock == 2000000) ? 2u : 0u; }
/* Which table Linux uses for a DisplayPort or eDP output of this version: the eDP list for version 30, else the plain DisplayPort list. */
static inline const struct nmc20_state *const *nmc20_table_for(unsigned display_version) { return display_version >= 30 ? nmc20_tab_xe3lpd : nmc20_tab_mtl_dp; }
static inline const struct nmc20_state *nmc20_find(const struct nmc20_state *const *tab, uint32_t clock)
{
    unsigned i; for (i = 0; tab[i]; i++) if (tab[i]->clock == clock) return tab[i]; return 0;
}
/* Load one state into the idle context and switch to it (intel_c20_pll_program, DisplayPort, not a Type-C port). */
static inline int nmc20_program_state(const struct nmr_ops *o, unsigned port, const uint16_t *tx, const uint16_t *cmn, const uint16_t *mp, uint32_t clock)
{
    uint8_t sr; unsigned i, nmp; int r = 0, cntx, to;
    if (nmx_read(o, port, 0, NMC20_SERDES, &sr)) return -1;
    cntx = sr & NMC20_CNTX_TOGGLE; to = !cntx;                       /* write the context that is not in use */
    for (i = 0; i < 3; i++) r |= nmc20_sram_write(o, port, NMX_LANE0, NMC20_TX(to, i), tx[i]);
    for (i = 0; i < 4; i++) r |= nmc20_sram_write(o, port, NMX_LANE0, NMC20_CMN(to, i), cmn[i]);
    nmp = (tx[0] & NMC20_USE_MPLLB) ? 11u : 10u;
    for (i = 0; i < nmp; i++) r |= nmc20_sram_write(o, port, NMX_LANE0, (tx[0] & NMC20_USE_MPLLB) ? NMC20_MPLLB(to, i) : NMC20_MPLLA(to, i), mp[i]);
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMC20_WIDTH, 0x3u, nmc20_width_code(clock), 1);
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMC20_SERDES, 0x40u | NMC20_SERDES_MASK, 0x40u | (nmc20_dp_rate_code(clock) << 1), 1);
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMC20_SERDES, NMC20_CNTX_TOGGLE, cntx ? 0u : NMC20_CNTX_TOGGLE, 1);
    return r ? -1 : 0;
}
static inline int nmc20_program(const struct nmr_ops *o, const struct nmx_snap *s, unsigned port)
{
    return nmc20_program_state(o, port, s->c20_tx, s->c20_cmn, s->c20_mp, s->c20_clock);
}

static inline int nmx_snapshot(const struct nmr_ops *o, struct nmx_snap *s, unsigned port, unsigned lanes)
{
    unsigned i, l, t, c; uint8_t b;
    s->valid = 0; s->lanes = (uint8_t)lanes;
    nmx_program_timer(o, port);
    s->clock_ctl = o->rd(o->ctx, NMX_CLOCK_CTL(port)); s->valfreq = o->rd(o->ctx, NMX_CLK_VALFREQ(port));
    s->buf1 = o->rd(o->ctx, NMX_BUF_CTL1(port)); s->buf2 = o->rd(o->ctx, NMX_BUF_CTL2(port)); s->buf3 = o->rd(o->ctx, NMX_BUF_CTL3(port));
    s->pmdemand[0] = o->rd(o->ctx, NMX_PMDEMAND(0)); s->pmdemand[1] = o->rd(o->ctx, NMX_PMDEMAND(1));
    s->reversal = (s->buf1 >> 16) & 1;
    /* "to read the PHY's internal registers over the message bus" the access bit of VDR control 1 is set first */
    if (s->is_c20) return nmc20_snapshot_regs(o, s, port);
    if (nmx_rmw_m(o, port, NMX_LANE0, NMX_C10_CONTROL(1), 0, NMX_CTRL_MSGBUS, 1)) return NMR_E_NO_SNAPSHOT;
    for (i = 0; i < 20; i++) if (nmx_read(o, port, 0, NMX_C10_PLL(i), &s->pll[i])) return NMR_E_NO_SNAPSHOT;
    for (i = 0; i < 4; i++) if (nmx_read(o, port, 0, NMX_C10_CMN(i), &s->cmn[i])) return NMR_E_NO_SNAPSHOT;
    for (i = 0; i < 2; i++) if (nmx_read(o, port, 0, NMX_C10_TX(i), &s->tx[i])) return NMR_E_NO_SNAPSHOT;
    if (nmx_read(o, port, 0, NMX_C10_WIDTH, &s->width)) return NMR_E_NO_SNAPSHOT;
    for (l = 0; l < 2; l++) {
        if (nmx_read(o, port, l, NMX_C10_OVRD, &s->ovrd[l])) return NMR_E_NO_SNAPSHOT;
        for (t = 0; t < 2; t++) for (c = 0; c < 3; c++) if (nmx_read(o, port, l, NMX_C10_VDROVRD(l, t, c), &s->vdrovrd[l][t][c])) return NMR_E_NO_SNAPSHOT;
    }
    if (nmx_read(o, port, 0, NMX_C10_CONTROL(1), &b)) return NMR_E_NO_SNAPSHOT;
    s->ctl1 = b;
    s->valid = 1;
    return NMR_OK;
}

/* ---- the enable sequence */
static inline uint32_t nmx_lane_bits(unsigned mask, uint32_t (*f)(unsigned))
{
    uint32_t v = 0; unsigned l;
    for (l = 0; l < 2; l++) if (mask & (1u << l)) v |= f(l);
    return v;
}
static inline uint32_t nmx_pipe_reset(unsigned l)   { return l ? (1u << 30) : (1u << 31); }
static inline uint32_t nmx_phy_status(unsigned l)   { return l ? (1u << 28) : (1u << 29); }
static inline uint32_t nmx_pd_update(unsigned l)    { return l ? (1u << 24) : (1u << 25); }
static inline uint32_t nmx_pclk_pll_req(unsigned l) { return 1u << (31 - 4 * l); }
static inline uint32_t nmx_pclk_pll_ack(unsigned l) { return 1u << (30 - 4 * l); }
static inline uint32_t nmx_refclk_req(unsigned l)   { return 1u << (29 - 4 * l); }
static inline uint32_t nmx_refclk_ack(unsigned l)   { return 1u << (28 - 4 * l); }
static inline uint32_t nmx_pd_new(unsigned l, unsigned st) { return l ? ((st & 0xFu) << 16) : ((st & 0xFu) << 20); }

static inline void nmx_powerdown_change(const struct nmr_ops *o, unsigned port, unsigned mask, unsigned state)
{
    unsigned l; uint32_t all = 0, set = 0, upd_all = 0, upd = 0;
    for (l = 0; l < 2; l++) { all |= nmx_pd_new(l, 0xF); upd_all |= nmx_pd_update(l); if (mask & (1u << l)) { set |= nmx_pd_new(l, state); upd |= nmx_pd_update(l); } }
    o->wr(o->ctx, NMX_BUF_CTL2(port), (o->rd(o->ctx, NMX_BUF_CTL2(port)) & ~all) | set);
    for (l = 0; l < 2; l++) if (mask & (1u << l))
        if (!nmr_wait(o, NMX_M2P_CTL(port, l), NMX_M2P_PENDING, 0, 1000)) nmx_bus_reset(o, port, l);
    o->wr(o->ctx, NMX_BUF_CTL2(port), (o->rd(o->ctx, NMX_BUF_CTL2(port)) & ~upd_all) | upd);
    nmr_wait(o, NMX_BUF_CTL2(port), upd, 0, 100);
}
static inline void nmx_setup_powerdown(const struct nmr_ops *o, unsigned port)
{
    o->wr(o->ctx, NMX_BUF_CTL2(port), (o->rd(o->ctx, NMX_BUF_CTL2(port)) & ~(0xFu << 4)) | (NMX_P2_READY << 4));
    o->wr(o->ctx, NMX_BUF_CTL3(port), (o->rd(o->ctx, NMX_BUF_CTL3(port)) & ~((0xFFu << 8) | 0xFu)) | 0u);     /* active state P0, no lane staggering */
}
static inline int nmx_lane_reset(const struct nmr_ops *o, unsigned port, int reversal)
{
    const unsigned owned = NMX_BOTH, lane = reversal ? NMX_LANE1 : NMX_LANE0;
    const uint32_t pipe_reset = nmx_lane_bits(owned, nmx_pipe_reset), status = nmx_lane_bits(owned, nmx_phy_status);
    if (!nmr_wait(o, NMX_BUF_CTL1(port), 1u << 24, 1u << 24, 100)) return -1;                  /* SOC out of reset */
    o->wr(o->ctx, NMX_BUF_CTL2(port), o->rd(o->ctx, NMX_BUF_CTL2(port)) | pipe_reset);
    if (!nmr_wait(o, NMX_BUF_CTL2(port), status, status, 100)) return -1;                       /* lanes out of reset */
    o->wr(o->ctx, NMX_CLOCK_CTL(port), (o->rd(o->ctx, NMX_CLOCK_CTL(port)) & ~nmx_lane_bits(owned, nmx_refclk_req)) | nmx_lane_bits(lane, nmx_refclk_req));
    if (!nmr_wait(o, NMX_CLOCK_CTL(port), nmx_lane_bits(owned, nmx_refclk_ack), nmx_lane_bits(lane, nmx_refclk_ack), 100)) return -1;
    nmx_powerdown_change(o, port, NMX_BOTH, NMX_P2_RESET);
    nmx_setup_powerdown(o, port);
    o->wr(o->ctx, NMX_BUF_CTL2(port), o->rd(o->ctx, NMX_BUF_CTL2(port)) & ~pipe_reset);
    if (!nmr_wait(o, NMX_BUF_CTL2(port), status, 0, 15000)) return -1;
    return 0;
}

/* Step 5 of Linux's enable: the PLL registers, over the bus, lane 0 as master. Then the transmitters. */
static inline int nmx_pll_program(const struct nmr_ops *o, const struct nmx_snap *s, unsigned port)
{
    unsigned i; int r = 0;
    if (s->is_c20) return nmc20_program(o, s, port);
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_MSGBUS, 1);
    for (i = 0; i < 20; i++) r |= nmx_write(o, port, 0, NMX_C10_PLL(i), s->pll[i], (i % 4) == 0);
    r |= nmx_write(o, port, 0, NMX_C10_CMN(0), s->cmn[0], 1);
    r |= nmx_write(o, port, 0, NMX_C10_TX(0), s->tx[0], 1);
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_WIDTH, 0x3, 0, 1);                                  /* custom width 8/10 on both lanes */
    r |= nmx_rmw_m(o, port, NMX_LANE0, NMX_C10_CONTROL(1), 0, NMX_CTRL_MASTER | NMX_CTRL_UPDATE, 1);
    return r;
}
static inline int nmx_program_lanes(const struct nmr_ops *o, unsigned port, unsigned lane_count, int reversal, int c20)
{
    unsigned i; uint8_t disables; int r = 0;
    if (!c20) r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_MSGBUS, 1);
    disables = reversal ? (uint8_t)(0xFu >> lane_count) : (uint8_t)((0xFu << lane_count) & 0xFu);
    for (i = 0; i < 4; i++) {
        const unsigned tx = i % 2 + 1, mask = i < 2 ? NMX_LANE0 : NMX_LANE1;
        r |= nmx_rmw_m(o, port, mask, NMX_TX_CONTROL(tx, 2), NMX_DISABLE_TX, (disables & (1u << i)) ? NMX_DISABLE_TX : 0, 1);
    }
    if (!c20) r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_UPDATE, 1);
    return r;
}
/* The whole of intel_cx0pll_enable for a C10 port. Returns 0, or a negative step number (-1 clock, -2 lane reset, -3 PLL, -4 lanes, -5 lock). */
static inline int nmx_pll_enable(const struct nmr_ops *o, const struct nmx_snap *s, unsigned port)
{
    const unsigned maxpclk = s->reversal ? NMX_LANE1 : NMX_LANE0;
    const uint32_t keep = (1u << 10) | (1u << 8) | ((s->ver >= 30 ? 0x1Fu : 0xFu) << 12) | (1u << 1) | (1u << 0);     /* forward clock ungate, lane-1 clock select, DDI clock select, SSC */
    nmx_program_timer(o, port);
    /* 1. clock muxes, gating and SSC; port reversal */
    o->wr(o->ctx, NMX_BUF_CTL1(port), (o->rd(o->ctx, NMX_BUF_CTL1(port)) & ~(1u << 16)) | (s->reversal ? (1u << 16) : 0));
    o->wr(o->ctx, NMX_CLOCK_CTL(port), (o->rd(o->ctx, NMX_CLOCK_CTL(port)) & ~keep) | (s->clock_ctl & keep));
    /* 2. out of reset, 3. power state ready */
    if (nmx_lane_reset(o, port, s->reversal)) return -2;
    nmx_powerdown_change(o, port, NMX_BOTH, NMX_P2_READY);
    /* 5. the PLL's registers, 6. the transmitters */
    if (nmx_pll_program(o, s, port)) return -3;
    if (nmx_program_lanes(o, port, s->lanes, s->reversal, s->is_c20)) return -4;
    /* 8. clock frequency */
    o->wr(o->ctx, NMX_CLK_VALFREQ(port), s->valfreq);
    /* 9. PLL request on the max-pclk lane, 10. wait for the acknowledge */
    {   uint32_t all = nmx_lane_bits(NMX_BOTH, nmx_pclk_pll_req);
        o->wr(o->ctx, NMX_CLOCK_CTL(port), (o->rd(o->ctx, NMX_CLOCK_CTL(port)) & ~all) | nmx_lane_bits(maxpclk, nmx_pclk_pll_req));
        if (!nmr_wait(o, NMX_CLOCK_CTL(port), nmx_lane_bits(NMX_BOTH, nmx_pclk_pll_ack), nmx_lane_bits(maxpclk, nmx_pclk_pll_ack), 3200)) return -5; }
    return 0;
}

/* ---- source levels: the C10 PHY's override registers, from the table of Linux's mtl_c10_trans_dp14 (swing, pre-cursor, post-cursor) */
struct nmx_trans { uint8_t vswing, pre, post; };
static const struct nmx_trans nmx_dp14[10] = {
    {26,0,0},{33,0,6},{38,0,11},{43,0,19},{39,0,0},{45,0,7},{46,0,13},{46,0,0},{55,0,7},{62,0,0} };
/* mtl_c20_trans_dp14 */
static const struct nmx_trans nmx_c20_dp14[10] = {
    {20,0,0},{24,0,4},{30,0,9},{34,0,14},{29,0,0},{34,0,5},{38,0,10},{36,0,0},{40,0,6},{48,0,0} };
/* the firmware's own signal levels, written back as Linux's intel_cx0_phy_set_signal_levels leaves them */
static inline int nmx_restore_levels(const struct nmr_ops *o, const struct nmx_snap *s, unsigned port)
{
    unsigned lane, tx, c; int r = 0;
    if (!s->is_c20) {
        r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_MSGBUS, 1);
        r |= nmx_write_m(o, port, NMX_BOTH, NMX_C10_CMN(3), s->cmn[3], 0);
        r |= nmx_write_m(o, port, NMX_BOTH, NMX_C10_TX(1), s->tx[1], 1);
    }
    for (lane = 0; lane < 2; lane++) for (tx = 0; tx < 2; tx++) for (c = 0; c < 3; c++)
        r |= nmx_write(o, port, lane, NMX_C10_VDROVRD(lane, tx, c), s->vdrovrd[lane][tx][c], 1);
    for (lane = 0; lane < 2; lane++) r |= nmx_write(o, port, lane, NMX_C10_OVRD, s->ovrd[lane], 1);
    if (!s->is_c20) r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_UPDATE, 1);
    return r ? -1 : 0;
}
static inline int nmx_apply_level(const struct nmr_ops *o, unsigned port, unsigned lanes, unsigned v, unsigned p, int c20)
{
    int idx = nmr_level_index(v, p), r = 0; unsigned ln;
    const struct nmx_trans *tab = c20 ? nmx_c20_dp14 : nmx_dp14;
    if (idx < 0) return -1;
    if (idx == 0) return 0;                              /* the firmware's own level 0 stays as replayed */
    if (!c20) r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_MSGBUS, 1);
    for (ln = 0; ln < lanes; ln++) {
        const unsigned lane = ln / 2, tx = ln % 2, mask = lane ? NMX_LANE1 : NMX_LANE0;
        r |= nmx_rmw_m(o, port, mask, NMX_C10_VDROVRD(lane, tx, 0), 0x3F, tab[idx].pre & 0x3F, 1);
        r |= nmx_rmw_m(o, port, mask, NMX_C10_VDROVRD(lane, tx, 1), 0x3F, tab[idx].vswing & 0x3F, 1);
        r |= nmx_rmw_m(o, port, mask, NMX_C10_VDROVRD(lane, tx, 2), 0x3F, tab[idx].post & 0x3F, 1);
    }
    r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_OVRD, 0, 0x05, 1);               /* TX1 and TX2 override enables */
    if (!c20) r |= nmx_rmw_m(o, port, NMX_BOTH, NMX_C10_CONTROL(1), 0, NMX_CTRL_UPDATE, 1);
    return r ? -1 : 0;
}

/* ---- display version 14 resume */
#define NMX_AUX_POWER_REQ (1u << 19)
#define NMX_AUX_POWER_STAT (1u << 18)
#define NMX_PMD_REQ (1u << 31)
#define NMX_PMD_INFLIGHT (1u << 26)

static inline int nmx_level_cb(const struct nmr_ops *o, const struct nmr_snap *s, unsigned v, unsigned p)
{
    return nmx_apply_level(o, s->topo.port | (s->cx0.ver >= 20 ? NMX_X2 : 0) | (s->cx0.ver >= 30 ? NMX_X3 : 0), s->dpcd[1] & 0x1Fu, v, p, s->cx0.is_c20);
}
static inline int nmx_aux_power(const struct nmr_ops *o, unsigned port, int on)
{
    const uint32_t reg = NMR_AUX_CTL(port);
    if (!on) { o->wr(o->ctx, reg, o->rd(o->ctx, reg) & ~NMX_AUX_POWER_REQ); return 1; }
    o->wr(o->ctx, reg, o->rd(o->ctx, reg) | NMX_AUX_POWER_REQ);
    return nmr_wait(o, reg, NMX_AUX_POWER_STAT, NMX_AUX_POWER_STAT, 1000);
}

/* Capture while the panel is lit. The AUX channel needs its power request on for the sink reads. Display versions 14 (Meteor Lake, Arrow
 * Lake), 20 (Lunar Lake) and 30 (Panther Lake). */
static inline int nmx_phy_is_c20(unsigned display_version, unsigned port)
{
    /* Linux intel_encoder_is_c10phy: Lunar Lake and Meteor Lake have C10 on ports A and B; Panther Lake only on A. */
    if (display_version >= 30) return port != 0;
    return 0;
}
static inline unsigned nmx_port_arg(unsigned display_version, unsigned port)
{
    return port | (display_version >= 20 ? NMX_X2 : 0u) | (display_version >= 30 ? NMX_X3 : 0u);
}
static inline int nmr20_snapshot(const struct nmr_ops *o, struct nmr_snap *s, unsigned pipe, const struct nmr_edp *edp, unsigned display_version)
{
    struct nmr_ops o2 = *o; int rc; unsigned port, xp;
    o2.aux_ctl_or = NMX_AUX_POWER_REQ;
    /* the port is read from the transcoder first; the sink reads follow inside nmr_snap_impl, so power the channel here */
    {   unsigned sel = (o->rd(o->ctx, NMR_FUNC_CTL(pipe > 3 ? 0 : pipe)) & NMR_TRANS_PORT_MASK) >> NMR_TRANS_PORT_SHIFT;
        port = sel ? sel - 1 : 0; if (port > 1) port = 0; }
    if (!nmx_aux_power(o, port, 1)) return NMR_E_SINK;
    rc = nmr_snap_impl(&o2, s, pipe, display_version, edp, 0);
    nmx_aux_power(o, port, 0);
    if (rc != NMR_OK) return rc;
    s->pmdemand[0] = o->rd(o->ctx, NMX_PMDEMAND(0)); s->pmdemand[1] = o->rd(o->ctx, NMX_PMDEMAND(1));
    s->cx0.ver = (uint8_t)display_version;
    s->cx0.is_c20 = (uint8_t)nmx_phy_is_c20(display_version, s->topo.port);
    xp = nmx_port_arg(display_version, s->topo.port);
    rc = nmx_snapshot(o, &s->cx0, xp, s->dpcd[1] & 0x1Fu);
    if (rc == NMR_OK) {
        s->cx0.reversal = (o->rd(o->ctx, NMX_BUF_CTL1(xp)) >> 16) & 1;
        if (s->cx0.is_c20 && !nmc20_find(nmc20_table_for(display_version), s->cx0.c20_clock)) rc = NMR_E_NO_SNAPSHOT;    /* a link rate Linux has no table for: not guessed */
    }
    if (rc != NMR_OK) { s->valid = 0; return rc; }
    return NMR_OK;
}
static inline int nmr14_snapshot(const struct nmr_ops *o, struct nmr_snap *s, unsigned pipe, const struct nmr_edp *edp)
{
    return nmr20_snapshot(o, s, pipe, edp, 14);
}

static inline int nmr20_resume(const struct nmr_ops *o, const struct nmr_snap *s)
{
    struct nmr_ops o2; int rc; uint32_t d1; unsigned ver, xp;
    const unsigned port = s ? s->topo.port : 0;
    if (!s || !s->valid || !s->cx0.valid) return NMR_E_NO_SNAPSHOT;
    ver = s->cx0.ver ? s->cx0.ver : 14u; xp = nmx_port_arg(ver, port);
    o2 = *o;
    if (nmr_st_lit(o, s)) return NMR_ALREADY_LIT;
    o2.aux_ctl_or = NMX_AUX_POWER_REQ; o2.apply_level = nmx_level_cb;
    nmr_st_core_early(o, s, ver);
    if ((rc = nmr_st_power(o, s))) return rc;
    /* Linux icl_display_core_init: on version 14 the PHY latches that firmware holds across sleep are released once power well 1 is up */
    if (ver == 14) o->wr(o->ctx, NMR_DC_STATE, o->rd(o->ctx, NMR_DC_STATE) & ~NMR_DC_HOLD);
    if ((rc = nmr_st_cdclk(o, s, ver))) return rc;
    if ((rc = nmr_st_core_late(o, s, ver))) return rc;
    nmr_note(o, "power-management demand");
    if (!nmr_wait(o, NMX_PMDEMAND(1), NMX_PMD_REQ, 0, 10000) || !nmr_wait(o, NMX_DCPR_STATUS_1, NMX_PMD_INFLIGHT, 0, 10000)) return NMR_E_PMDEMAND;
    d1 = s->pmdemand[1] & ~NMX_PMD_REQ;
    o->wr(o->ctx, NMX_PMDEMAND(0), s->pmdemand[0]); o->wr(o->ctx, NMX_PMDEMAND(1), d1);
    o->wr(o->ctx, NMX_PMDEMAND(1), d1 | NMX_PMD_REQ);
    if (!nmr_wait(o, NMX_PMDEMAND(1), NMX_PMD_REQ, 0, 10000)) return NMR_E_PMDEMAND;
    if ((rc = nmr_st_panel(o, s))) return rc;
    /* mtl_ddi_pre_enable_dp step 3: the port is not on Thunderbolt */
    o->wr(o->ctx, NMX_BUF_CTL1(xp), o->rd(o->ctx, NMX_BUF_CTL1(xp)) & ~(1u << 11));
    nmr_note(o, s->cx0.is_c20 ? "Cx0 PHY (C20)" : "Cx0 PHY");
    if (nmx_pll_enable(o, &s->cx0, xp)) return NMR_E_PHY;
    if (nmx_restore_levels(o, &s->cx0, xp)) return NMR_E_PHY;
    if (!nmx_aux_power(o, port, 1)) return NMR_E_SINK;
    if ((rc = nmr_st_sink(&o2, s))) return rc;
    nmr_st_pipe(o, s);
    nmr_note(o, "transcoder function and DDI buffer");
    o->wr(o->ctx, NMR_FUNC_CTL(s->topo.pipe), s->func);
    o->wr(o->ctx, NMR_DP_TP_CTL(s->topo.pipe), (s->tp & ~NMR_TP_PAT_MASK) | NMR_TP_ENABLE | NMR_TP_PAT1);
    /* 6.f the D2D link (version 20 and later: in the DDI buffer control register), 6.h the port buffer width and data width, 6.i the DDI buffer, 6.j wait for it to leave idle */
    if (ver >= 20) {
        o->wr(o->ctx, NMR_DDI_BUF_CTL(port), o->rd(o->ctx, NMR_DDI_BUF_CTL(port)) | NMR_XE2_D2D_ENABLE);
        if (!nmr_wait(o, NMR_DDI_BUF_CTL(port), NMR_XE2_D2D_STATE, NMR_XE2_D2D_STATE, 100)) return NMR_E_PHY;
    } else {
        o->wr(o->ctx, NMX_BUF_CTL1(xp), o->rd(o->ctx, NMX_BUF_CTL1(xp)) | (1u << 29));
        if (!nmr_wait(o, NMX_BUF_CTL1(xp), 1u << 28, 1u << 28, 100)) return NMR_E_PHY;
    }
    {   const uint32_t m = (7u << 1) | (3u << 18) | (1u << 16);
        o->wr(o->ctx, NMX_BUF_CTL1(xp), (o->rd(o->ctx, NMX_BUF_CTL1(xp)) & ~m) | (s->cx0.buf1 & m)); }
    o->wr(o->ctx, NMR_DDI_BUF_CTL(port), s->ddi | NMR_DDI_ENABLE);
    if (!nmr_wait(o, NMX_BUF_CTL1(xp), 1u << 7, 0, 10000)) { o->wr(o->ctx, NMR_DDI_BUF_CTL(port), s->ddi & ~NMR_DDI_ENABLE); return NMR_E_PHY; }
    rc = nmr_st_tail(&o2, s, ver);
    nmx_aux_power(o, port, 0);
    return rc;
}
static inline int nmr14_resume(const struct nmr_ops *o, const struct nmr_snap *s) { return nmr20_resume(o, s); }

#endif

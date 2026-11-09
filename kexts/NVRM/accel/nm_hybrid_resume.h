/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Relighting an Intel laptop panel after sleep (S3), for the panel driver NMIntelFB.
 *
 * After S3 the Intel display engine comes back with every register zero and the panel's own link state gone; restoring
 * the registers alone leaves the panel dark, because the power wells, the PLLs, the panel power sequencer and the
 * embedded DisplayPort link all need their handshakes. The firmware's mode is therefore captured while the panel is
 * lit (a snapshot of the registers that make up the mode, plus the sink's link settings read over the AUX channel),
 * and on wake it is replayed in the order Linux's i915 driver enables an eDP panel, waiting for each handshake and
 * stopping at the first one that does not come:
 *
 *   power wells -> display clock PLL -> DPLL and its clock select -> combo PHY block -> panel power sequence ->
 *   sink to D0 -> transcoder timing, plane registers (the plane's surface register last) -> transcoder function and DDI
 *   buffer with training pattern 1 -> link training (clock recovery, channel equalisation) -> idle pattern, normal
 *   operation -> transcoder on -> backlight.
 *
 * Register offsets are those of display versions 12 and 13 (Tiger Lake, Rocket Lake, Alder Lake, Raptor Lake) with the
 * panel on DDI A, pipe A, transcoder A, AUX channel A: the layout of Linux's i915 headers (MIT). Other generations and
 * ports are refused, never guessed. This header is plain C with no kernel headers so tools/test_hybrid_resume.c drives
 * the same code against a model of the hardware.
 *
 * Switch: boot-arg nmhybridresume=1 (default off). With it off, sleep behaves as before.
 */
#ifndef NM_HYBRID_RESUME_H
#define NM_HYBRID_RESUME_H

#include <stdint.h>

struct nmr_ops {
    void *ctx;
    uint32_t (*rd)(void *ctx, uint32_t off);
    void (*wr)(void *ctx, uint32_t off, uint32_t v);
    void (*delay_us)(void *ctx, unsigned us);
    void (*log)(void *ctx, const char *step);                /* one line per step reached; may be NULL */
    int (*set_drive)(void *ctx, unsigned vswing, unsigned preemph);   /* 0 applied, -1 not supported */
};

/* ---- registers (display 12 and 13, DDI A / pipe A / transcoder A / AUX A) */
#define NMR_PW_DRIVER      0x45404u
#define NMR_PW_AUX_A       0x45440u
#define NMR_PW_DDI_A       0x45450u
#define NMR_DE_PLL_ENABLE  0x46070u
#define NMR_CDCLK_CTL      0x46000u
#define NMR_DPLL0_ENABLE   0x46010u
#define NMR_DPLL0_CFGCR0   0x164284u
#define NMR_DPLL0_CFGCR1   0x164288u
#define NMR_DPLL0_DIV0     0x164B00u
#define NMR_DPCLKA_CFGCR0  0x164280u
#define NMR_PHY_A_BASE     0x162000u
#define NMR_PHY_A_WORDS    256u
#define NMR_PP_STATUS      0xC7200u
#define NMR_PP_CONTROL     0xC7204u
#define NMR_PP_ON_DELAYS   0xC7208u
#define NMR_PP_OFF_DELAYS  0xC720Cu
#define NMR_PP_DIVISOR     0xC7210u
#define NMR_TRANS_CLK_SEL  0x46140u
#define NMR_PIPE_MISC      0x70030u
#define NMR_WM_LINETIME    0x45270u
#define NMR_MSA_MISC       0x60410u
#define NMR_FUNC_CTL       0x60400u
#define NMR_DP_TP_CTL      0x64040u
#define NMR_DP_TP_STATUS   0x64044u
#define NMR_DDI_BUF_CTL    0x64000u
#define NMR_TRANSCONF      0x70008u
#define NMR_FRMCOUNT       0x70040u
#define NMR_PLANE_CTL      0x70180u
#define NMR_PLANE_SURF     0x7019Cu
#define NMR_PLANE_WM_BASE  0x70240u      /* plane 1 watermarks and buffer allocation, 0x70240..0x7027F */
#define NMR_PLANE_WM_WORDS 16u
#define NMR_AUX_CTL        0x64010u
#define NMR_AUX_DATA       0x64014u
#define NMR_BLC_PWM_CTL    0xC8250u
#define NMR_BLC_PWM_FREQ   0xC8254u
#define NMR_BLC_PWM_DUTY   0xC8258u

#define NMR_PLL_ENABLE  (1u << 31)
#define NMR_PLL_LOCK    (1u << 30)
#define NMR_PLL_POWER   (1u << 27)
#define NMR_PLL_PSTATE  (1u << 26)
#define NMR_PP_ON       (1u << 31)
#define NMR_PP_ON_MASK  ((1u << 31) | (3u << 28) | 0xFu)
#define NMR_PP_ON_VALUE ((1u << 31) | 0x8u)
#define NMR_PP_UNLOCK   0xABCD0000u
#define NMR_PP_POWER_ON 1u
#define NMR_DDI_ENABLE  (1u << 31)
#define NMR_DDI_IDLE    (1u << 7)
#define NMR_TP_ENABLE   (1u << 31)
#define NMR_TP_PAT_MASK (7u << 8)
#define NMR_TP_PAT1     (0u << 8)
#define NMR_TP_PAT2     (1u << 8)
#define NMR_TP_IDLE     (2u << 8)
#define NMR_TP_NORMAL   (3u << 8)
#define NMR_TP_IDLE_DONE (1u << 25)
#define NMR_TRANS_ENABLE (1u << 31)

/* DPCD (DisplayPort specification) */
#define NMR_DPCD_MAX_LANE      0x002u
#define NMR_DPCD_DOWNSPREAD    0x003u
#define NMR_DPCD_AUX_INTERVAL  0x00Eu
#define NMR_DPCD_LINK_BW       0x100u
#define NMR_DPCD_LANE_COUNT    0x101u
#define NMR_DPCD_TRAIN_PAT     0x102u
#define NMR_DPCD_LANE0_SET     0x103u
#define NMR_DPCD_DOWNSPREAD_CT 0x107u
#define NMR_DPCD_CODING        0x108u
#define NMR_DPCD_LINK_RATE_SET 0x115u
#define NMR_DPCD_LANE01_STATUS 0x202u
#define NMR_DPCD_ALIGN_STATUS  0x204u
#define NMR_DPCD_ADJUST01      0x206u
#define NMR_DPCD_SET_POWER     0x600u

enum nmr_result {
    NMR_OK = 0, NMR_ALREADY_LIT, NMR_E_NO_SNAPSHOT, NMR_E_POWER, NMR_E_CDCLK, NMR_E_PLL, NMR_E_PANEL, NMR_E_SINK,
    NMR_E_TRAIN, NMR_E_NO_SCANOUT, NMR_E_GEN
};
static inline const char *nmr_result_name(int r)
{
    switch (r) {
    case NMR_OK: return "panel relit";
    case NMR_ALREADY_LIT: return "panel was already lit";
    case NMR_E_NO_SNAPSHOT: return "no snapshot of the firmware mode";
    case NMR_E_POWER: return "a power well did not come up";
    case NMR_E_CDCLK: return "the display clock PLL did not lock";
    case NMR_E_PLL: return "the DPLL did not come up or lock";
    case NMR_E_PANEL: return "the panel power sequence did not complete";
    case NMR_E_SINK: return "the sink did not answer on the AUX channel";
    case NMR_E_TRAIN: return "link training failed";
    case NMR_E_NO_SCANOUT: return "the pipe is not scanning out after the link came up";
    case NMR_E_GEN: return "display generation or port not supported for resume";
    default: return "?";
    }
}

/* ---- the snapshot */
#define NMR_N_TIMING 11
#define NMR_N_PLANE  5
struct nmr_snap {
    uint8_t  valid, have_sink, no_handshake;
    uint32_t pw[3];
    uint32_t de_pll, cdclk;
    uint32_t dpll_en, cfgcr0, cfgcr1, div0, dpclka;
    uint32_t phy[NMR_PHY_A_WORDS];
    uint32_t pp_ctl, pp_on, pp_off, pp_div;
    uint32_t timing[NMR_N_TIMING];
    uint32_t trans_clk_sel, pipe_misc, wm_linetime, msa;
    uint32_t plane[NMR_N_PLANE];                 /* CTL STRIDE POS SIZE OFFSET */
    uint32_t plane_wm[NMR_PLANE_WM_WORDS];
    uint32_t surf, func, tp, ddi, transconf;
    uint32_t blc[3];
    uint8_t  dpcd[8];                            /* 0x100 0x101 0x107 0x108 0x115, max lane (0x002), downspread cap (0x003), interval (0x00E) */
    uint32_t stride_reg_unused;
};
static const uint32_t nmr_timing_regs[NMR_N_TIMING] = {
    0x60000, 0x60004, 0x60008, 0x6000C, 0x60010, 0x60014, 0x6001C, 0x60030, 0x60034, 0x60040, 0x60044 };
static const uint32_t nmr_plane_regs[NMR_N_PLANE] = { 0x70180, 0x70188, 0x7018C, 0x70190, 0x701A4 };

static inline int nmr_wait(const struct nmr_ops *o, uint32_t reg, uint32_t mask, uint32_t val, unsigned total_us)
{
    unsigned t;
    for (t = 0; t <= total_us; t += 100) {
        if ((o->rd(o->ctx, reg) & mask) == val) return 1;
        o->delay_us(o->ctx, 100);
    }
    return 0;
}

/* ---- AUX channel A, native transactions (Linux intel_dp_aux.c) */
#define NMR_AUX_NATIVE_WRITE 8u
#define NMR_AUX_NATIVE_READ  9u
/* One transaction of up to 16 bytes. Returns the number of bytes read or written, or -1. */
static inline int nmr_aux_xfer(const struct nmr_ops *o, int write, uint32_t addr, uint8_t *buf, unsigned len)
{
    unsigned i, tries, got = 0;
    uint8_t msg[20];
    if (len == 0 || len > 16) return -1;
    msg[0] = (uint8_t)(((write ? NMR_AUX_NATIVE_WRITE : NMR_AUX_NATIVE_READ) << 4) | ((addr >> 16) & 0xF));
    msg[1] = (uint8_t)(addr >> 8); msg[2] = (uint8_t)addr; msg[3] = (uint8_t)(len - 1);
    unsigned send = 4 + (write ? len : 0);
    if (write) for (i = 0; i < len; i++) msg[4 + i] = buf[i];
    for (tries = 0; tries < 5; tries++) {
        uint32_t ctl, v;
        for (i = 0; i < 40; i++) { if (!(o->rd(o->ctx, NMR_AUX_CTL) & (1u << 31))) break; o->delay_us(o->ctx, 100); }
        if (o->rd(o->ctx, NMR_AUX_CTL) & (1u << 31)) return -1;
        for (i = 0; i < send; i += 4) {
            uint32_t w = 0; unsigned j;
            for (j = 0; j < 4 && i + j < send; j++) w |= (uint32_t)msg[i + j] << (24 - 8 * j);
            o->wr(o->ctx, NMR_AUX_DATA + i, w);
        }
        ctl = (1u << 31) | (1u << 30) | (1u << 29) | (1u << 28) | (3u << 26) | (1u << 25) | ((uint32_t)send << 20) |
              ((18u - 1u) << 5) | (32u - 1u);
        o->wr(o->ctx, NMR_AUX_CTL, ctl);
        for (i = 0; i < 40; i++) { v = o->rd(o->ctx, NMR_AUX_CTL); if (!(v & (1u << 31))) break; o->delay_us(o->ctx, 100); }
        v = o->rd(o->ctx, NMR_AUX_CTL);
        o->wr(o->ctx, NMR_AUX_CTL, v | (1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));   /* write-1-to-clear the status bits */
        if ((v & (1u << 31)) || (v & ((1u << 28) | (1u << 25))) || !(v & (1u << 30))) { o->delay_us(o->ctx, 400); continue; }
        unsigned n = (v >> 20) & 0x1F;
        if (n == 0) return -1;
        {   /* reply: first byte is the command/ack nibble, 0 = ACK */
            uint8_t rep[20] = {0}; unsigned k;
            for (k = 0; k < n && k < 20; k += 4) {
                uint32_t w = o->rd(o->ctx, NMR_AUX_DATA + k); unsigned j;
                for (j = 0; j < 4 && k + j < n; j++) rep[k + j] = (uint8_t)(w >> (24 - 8 * j));
            }
            if ((rep[0] >> 4) != 0) { o->delay_us(o->ctx, 400); continue; }        /* NACK or DEFER: try again */
            if (!write) { for (got = 0; got < n - 1 && got < len; got++) buf[got] = rep[1 + got]; return (int)got; }
            return (int)len;
        }
    }
    return -1;
}
static inline int nmr_dpcd_rd(const struct nmr_ops *o, uint32_t a, uint8_t *b, unsigned n) { return nmr_aux_xfer(o, 0, a, b, n); }
static inline int nmr_dpcd_wr(const struct nmr_ops *o, uint32_t a, const uint8_t *b, unsigned n)
{
    uint8_t t[16]; unsigned i; if (n > 16) return -1;
    for (i = 0; i < n; i++) t[i] = b[i];
    return nmr_aux_xfer(o, 1, a, t, n);
}
static inline int nmr_dpcd_wr1(const struct nmr_ops *o, uint32_t a, uint8_t v) { return nmr_dpcd_wr(o, a, &v, 1); }

/* ---- capture, while the firmware's mode is lit */
static inline int nmr_snapshot(const struct nmr_ops *o, struct nmr_snap *s)
{
    unsigned i; uint8_t b[4];
    s->valid = s->have_sink = s->no_handshake = 0;
    if (!(o->rd(o->ctx, NMR_TRANSCONF) & NMR_TRANS_ENABLE) || !(o->rd(o->ctx, NMR_DDI_BUF_CTL) & NMR_DDI_ENABLE)) return NMR_E_NO_SNAPSHOT;
    s->pw[0] = o->rd(o->ctx, NMR_PW_DRIVER); s->pw[1] = o->rd(o->ctx, NMR_PW_AUX_A); s->pw[2] = o->rd(o->ctx, NMR_PW_DDI_A);
    s->de_pll = o->rd(o->ctx, NMR_DE_PLL_ENABLE); s->cdclk = o->rd(o->ctx, NMR_CDCLK_CTL);
    s->dpll_en = o->rd(o->ctx, NMR_DPLL0_ENABLE); s->cfgcr0 = o->rd(o->ctx, NMR_DPLL0_CFGCR0);
    s->cfgcr1 = o->rd(o->ctx, NMR_DPLL0_CFGCR1); s->div0 = o->rd(o->ctx, NMR_DPLL0_DIV0); s->dpclka = o->rd(o->ctx, NMR_DPCLKA_CFGCR0);
    for (i = 0; i < NMR_PHY_A_WORDS; i++) s->phy[i] = o->rd(o->ctx, NMR_PHY_A_BASE + 4 * i);
    s->pp_ctl = o->rd(o->ctx, NMR_PP_CONTROL); s->pp_on = o->rd(o->ctx, NMR_PP_ON_DELAYS);
    s->pp_off = o->rd(o->ctx, NMR_PP_OFF_DELAYS); s->pp_div = o->rd(o->ctx, NMR_PP_DIVISOR);
    for (i = 0; i < NMR_N_TIMING; i++) s->timing[i] = o->rd(o->ctx, nmr_timing_regs[i]);
    s->trans_clk_sel = o->rd(o->ctx, NMR_TRANS_CLK_SEL); s->pipe_misc = o->rd(o->ctx, NMR_PIPE_MISC);
    s->wm_linetime = o->rd(o->ctx, NMR_WM_LINETIME); s->msa = o->rd(o->ctx, NMR_MSA_MISC);
    for (i = 0; i < NMR_N_PLANE; i++) s->plane[i] = o->rd(o->ctx, nmr_plane_regs[i]);
    for (i = 0; i < NMR_PLANE_WM_WORDS; i++) s->plane_wm[i] = o->rd(o->ctx, NMR_PLANE_WM_BASE + 4 * i);
    s->surf = o->rd(o->ctx, NMR_PLANE_SURF); s->func = o->rd(o->ctx, NMR_FUNC_CTL); s->tp = o->rd(o->ctx, NMR_DP_TP_CTL);
    s->ddi = o->rd(o->ctx, NMR_DDI_BUF_CTL); s->transconf = o->rd(o->ctx, NMR_TRANSCONF);
    s->blc[0] = o->rd(o->ctx, NMR_BLC_PWM_CTL); s->blc[1] = o->rd(o->ctx, NMR_BLC_PWM_FREQ); s->blc[2] = o->rd(o->ctx, NMR_BLC_PWM_DUTY);
    /* the sink: its capabilities and the link settings the firmware gave it. The AUX channel needs its power well, which the
     * firmware may have left off once the panel was lit; ask for it for the reads and put it back. */
    { uint32_t aux_before = o->rd(o->ctx, NMR_PW_AUX_A);
      if ((aux_before & 3u) != 3u) {
          o->wr(o->ctx, NMR_PW_AUX_A, aux_before | 1u);
          nmr_wait(o, NMR_PW_AUX_A, 2u, 2u, 20000);
      }
    if (nmr_dpcd_rd(o, NMR_DPCD_LINK_BW, b, 2) == 2) { s->dpcd[0] = b[0]; s->dpcd[1] = b[1]; s->have_sink = 1; }
    if (nmr_dpcd_rd(o, NMR_DPCD_DOWNSPREAD_CT, b, 2) == 2) { s->dpcd[2] = b[0]; s->dpcd[3] = b[1]; } else s->have_sink = 0;
    if (nmr_dpcd_rd(o, NMR_DPCD_LINK_RATE_SET, b, 1) == 1) s->dpcd[4] = b[0];
    if (nmr_dpcd_rd(o, NMR_DPCD_MAX_LANE, b, 2) == 2) { s->dpcd[5] = b[0]; s->dpcd[6] = b[1]; s->no_handshake = (b[1] >> 6) & 1; }
    if (nmr_dpcd_rd(o, NMR_DPCD_AUX_INTERVAL, b, 1) == 1) s->dpcd[7] = b[0];
      if ((aux_before & 3u) != 3u) o->wr(o->ctx, NMR_PW_AUX_A, aux_before); }
    s->valid = s->have_sink;
    return s->valid ? NMR_OK : NMR_E_NO_SNAPSHOT;
}

static inline void nmr_note(const struct nmr_ops *o, const char *s) { if (o->log) o->log(o->ctx, s); }

/* ---- link training: the DisplayPort clock recovery and channel equalisation procedure */
static inline int nmr_train(const struct nmr_ops *o, const struct nmr_snap *s, unsigned lanes)
{
    unsigned lane_mask = (1u << lanes) - 1, tries, loops, i;
    uint8_t lane_set[4] = {0, 0, 0, 0}, st[2], adj[2], al, cur_v = 0, cur_p = 0, last_v = 0xFF, same;
    uint32_t tp;
    uint8_t pat;
    unsigned cr_us = 100, eq_us = (s->dpcd[7] & 0x7F) ? (unsigned)(s->dpcd[7] & 0x7F) * 4000u : 400u;
    /* sink link settings, as the firmware had them */
    if (nmr_dpcd_wr1(o, NMR_DPCD_LINK_BW, s->dpcd[0]) != 1) return 0;
    if (nmr_dpcd_wr1(o, NMR_DPCD_LANE_COUNT, s->dpcd[1]) != 1) return 0;
    if (nmr_dpcd_wr1(o, NMR_DPCD_DOWNSPREAD_CT, s->dpcd[2]) != 1) return 0;
    if (nmr_dpcd_wr1(o, NMR_DPCD_CODING, s->dpcd[3]) != 1) return 0;
    if (s->dpcd[4] && nmr_dpcd_wr1(o, NMR_DPCD_LINK_RATE_SET, s->dpcd[4]) != 1) return 0;
    /* clock recovery: pattern 1 on both ends, scrambling off */
    tp = o->rd(o->ctx, NMR_DP_TP_CTL);
    o->wr(o->ctx, NMR_DP_TP_CTL, (tp & ~NMR_TP_PAT_MASK) | NMR_TP_ENABLE | NMR_TP_PAT1);
    pat = 0x21;   /* TPS1, scrambling disabled */
    if (nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, pat) != 1) return 0;
    for (loops = 0, same = 0; loops < 10; loops++) {
        for (i = 0; i < lanes; i++) lane_set[i] = (uint8_t)((cur_v & 3) | ((cur_v == 3) ? 4 : 0) | ((cur_p & 3) << 3) | ((cur_p == 3) ? 0x20 : 0));
        if (nmr_dpcd_wr(o, NMR_DPCD_LANE0_SET, lane_set, lanes) != (int)lanes) return 0;
        o->delay_us(o->ctx, cr_us);
        if (nmr_dpcd_rd(o, NMR_DPCD_LANE01_STATUS, st, 2) != 2 || nmr_dpcd_rd(o, NMR_DPCD_ADJUST01, adj, 2) != 2) return 0;
        {   /* CR_DONE of every lane */
            unsigned ok = 0;
            for (i = 0; i < lanes; i++) ok += (((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 1);
            if (ok == lanes) break;
        }
        {   /* the sink's request: the highest swing / pre-emphasis over the lanes */
            uint8_t rv = 0, rp = 0;
            for (i = 0; i < lanes; i++) {
                uint8_t a = (i < 2) ? adj[0] : adj[1]; unsigned sh = 4 * (i & 1);
                uint8_t v = (a >> sh) & 3, p = (a >> (sh + 2)) & 3;
                if (v > rv) rv = v; if (p > rp) rp = p;
            }
            if (rv == cur_v && rp == cur_p) { if (++same >= 5) { loops = 99; break; } }
            else same = 0;
            if (rv != cur_v || rp != cur_p) {
                if (!o->set_drive || o->set_drive(o->ctx, rv, rp) != 0) { loops = 99; break; }   /* cannot give what it asks */
                cur_v = rv; cur_p = rp;
            }
            if (last_v == rv && loops > 5) { loops = 99; break; }
            last_v = rv;
        }
    }
    if (loops >= 10) { nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, 0); return 0; }
    nmr_note(o, "link training: clock recovery done");
    /* channel equalisation: pattern 2 */
    o->wr(o->ctx, NMR_DP_TP_CTL, (o->rd(o->ctx, NMR_DP_TP_CTL) & ~NMR_TP_PAT_MASK) | NMR_TP_PAT2);
    if (nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, 0x22) != 1) return 0;     /* TPS2, scrambling disabled */
    for (tries = 0; tries < 6; tries++) {
        o->delay_us(o->ctx, eq_us);
        if (nmr_dpcd_rd(o, NMR_DPCD_LANE01_STATUS, st, 2) != 2 || nmr_dpcd_rd(o, NMR_DPCD_ALIGN_STATUS, &al, 1) != 1 ||
            nmr_dpcd_rd(o, NMR_DPCD_ADJUST01, adj, 2) != 2) return 0;
        {
            unsigned ok = 0;
            for (i = 0; i < lanes; i++) ok += ((((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 7) == 7);
            if (ok == lanes && (al & 1)) { nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, 0); (void)lane_mask; return 1; }
            for (i = 0; i < lanes; i++) if (!((((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 1))) {   /* lost clock recovery: start over */
                nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, 0); return 0; }
        }
        {
            uint8_t rv = 0, rp = 0;
            for (i = 0; i < lanes; i++) { uint8_t a = (i < 2) ? adj[0] : adj[1]; unsigned sh = 4 * (i & 1);
                if (((a >> sh) & 3) > rv) rv = (a >> sh) & 3; if (((a >> (sh + 2)) & 3) > rp) rp = (a >> (sh + 2)) & 3; }
            if (rv != cur_v || rp != cur_p) {
                if (!o->set_drive || o->set_drive(o->ctx, rv, rp) != 0) break;
                cur_v = rv; cur_p = rp;
                for (i = 0; i < lanes; i++) lane_set[i] = (uint8_t)((cur_v & 3) | ((cur_p & 3) << 3));
                if (nmr_dpcd_wr(o, NMR_DPCD_LANE0_SET, lane_set, lanes) != (int)lanes) return 0;
            }
        }
    }
    nmr_dpcd_wr1(o, NMR_DPCD_TRAIN_PAT, 0);
    return 0;
}

/* ---- the replay */
static inline int nmr_resume(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    unsigned i, lanes;
    uint32_t cur, req, fc;
    if (display_version != 12 && display_version != 13) return NMR_E_GEN;
    if (!s || !s->valid) return NMR_E_NO_SNAPSHOT;
    /* a panel that is still running needs nothing */
    if ((o->rd(o->ctx, NMR_TRANSCONF) & NMR_TRANS_ENABLE) && (o->rd(o->ctx, NMR_DDI_BUF_CTL) & NMR_DDI_ENABLE)) return NMR_ALREADY_LIT;

    nmr_note(o, "power wells");
    { static const uint32_t pw[3] = { NMR_PW_DRIVER, NMR_PW_AUX_A, NMR_PW_DDI_A };
      for (i = 0; i < 3; i++) {
          req = (s->pw[i] & 0x55555555u) | (i > 0 ? 1u : 0u);   /* the AUX and DDI wells of port A are needed whatever the firmware left */
          cur = o->rd(o->ctx, pw[i]);
          o->wr(o->ctx, pw[i], cur | req);
          if (!nmr_wait(o, pw[i], req << 1, req << 1, 20000)) return NMR_E_POWER;
      } }
    nmr_note(o, "display clock");
    if (s->de_pll & NMR_PLL_ENABLE) {
        o->wr(o->ctx, NMR_DE_PLL_ENABLE, (s->de_pll & ~NMR_PLL_LOCK) | NMR_PLL_ENABLE);   /* the lock bit is a status */
        if (!nmr_wait(o, NMR_DE_PLL_ENABLE, NMR_PLL_LOCK, NMR_PLL_LOCK, 5000)) return NMR_E_CDCLK;
    }
    o->wr(o->ctx, NMR_CDCLK_CTL, s->cdclk);
    nmr_note(o, "DPLL");
    if (s->dpll_en & NMR_PLL_ENABLE) {
        o->wr(o->ctx, NMR_DPLL0_ENABLE, NMR_PLL_POWER);
        if (!nmr_wait(o, NMR_DPLL0_ENABLE, NMR_PLL_PSTATE, NMR_PLL_PSTATE, 1000)) return NMR_E_PLL;
        o->wr(o->ctx, NMR_DPLL0_CFGCR0, s->cfgcr0); o->wr(o->ctx, NMR_DPLL0_CFGCR1, s->cfgcr1); o->wr(o->ctx, NMR_DPLL0_DIV0, s->div0);
        o->wr(o->ctx, NMR_DPLL0_ENABLE, NMR_PLL_POWER | NMR_PLL_ENABLE);
        if (!nmr_wait(o, NMR_DPLL0_ENABLE, NMR_PLL_LOCK, NMR_PLL_LOCK, 5000)) return NMR_E_PLL;
    }
    o->wr(o->ctx, NMR_DPCLKA_CFGCR0, s->dpclka);
    nmr_note(o, "combo PHY");
    for (i = 0; i < NMR_PHY_A_WORDS; i++) o->wr(o->ctx, NMR_PHY_A_BASE + 4 * i, s->phy[i]);
    nmr_note(o, "panel power");
    o->wr(o->ctx, NMR_PP_ON_DELAYS, s->pp_on); o->wr(o->ctx, NMR_PP_OFF_DELAYS, s->pp_off); o->wr(o->ctx, NMR_PP_DIVISOR, s->pp_div);
    o->wr(o->ctx, NMR_PP_CONTROL, NMR_PP_UNLOCK | (s->pp_ctl & 0xFFFFu) | NMR_PP_POWER_ON);
    if (!nmr_wait(o, NMR_PP_STATUS, NMR_PP_ON_MASK, NMR_PP_ON_VALUE, 2000000)) return NMR_E_PANEL;
    nmr_note(o, "sink power");
    if (nmr_dpcd_wr1(o, NMR_DPCD_SET_POWER, 1) != 1) return NMR_E_SINK;
    o->delay_us(o->ctx, 1000);
    nmr_note(o, "pipe and plane registers");
    o->wr(o->ctx, NMR_TRANS_CLK_SEL, s->trans_clk_sel);
    for (i = 0; i < NMR_N_TIMING; i++) o->wr(o->ctx, nmr_timing_regs[i], s->timing[i]);
    o->wr(o->ctx, NMR_PIPE_MISC, s->pipe_misc); o->wr(o->ctx, NMR_WM_LINETIME, s->wm_linetime); o->wr(o->ctx, NMR_MSA_MISC, s->msa);
    for (i = 0; i < NMR_PLANE_WM_WORDS; i++) o->wr(o->ctx, NMR_PLANE_WM_BASE + 4 * i, s->plane_wm[i]);
    for (i = 0; i < NMR_N_PLANE; i++) o->wr(o->ctx, nmr_plane_regs[i], s->plane[i]);
    o->wr(o->ctx, NMR_PLANE_SURF, s->surf);       /* the surface register arms the plane: last */
    nmr_note(o, "transcoder function and DDI buffer");
    o->wr(o->ctx, NMR_FUNC_CTL, s->func);
    o->wr(o->ctx, NMR_DP_TP_CTL, (s->tp & ~NMR_TP_PAT_MASK) | NMR_TP_ENABLE | NMR_TP_PAT1);
    o->wr(o->ctx, NMR_DDI_BUF_CTL, s->ddi | NMR_DDI_ENABLE);
    o->delay_us(o->ctx, 600);                      /* buffer enable to first pattern */
    lanes = s->dpcd[1] & 0x1F;
    if (lanes == 0 || lanes > 4) { o->wr(o->ctx, NMR_DDI_BUF_CTL, s->ddi & ~NMR_DDI_ENABLE); return NMR_E_TRAIN; }
    if (s->no_handshake) {
        nmr_note(o, "link training skipped (the panel does not use the AUX handshake)");
        if (nmr_dpcd_wr1(o, NMR_DPCD_LINK_BW, s->dpcd[0]) != 1 || nmr_dpcd_wr1(o, NMR_DPCD_LANE_COUNT, s->dpcd[1]) != 1) return NMR_E_SINK;
        o->delay_us(o->ctx, 1000);
        o->wr(o->ctx, NMR_DP_TP_CTL, (o->rd(o->ctx, NMR_DP_TP_CTL) & ~NMR_TP_PAT_MASK) | NMR_TP_PAT2);
        o->delay_us(o->ctx, 1000);
    } else {
        nmr_note(o, "link training");
        if (!nmr_train(o, s, lanes)) {
            o->wr(o->ctx, NMR_DDI_BUF_CTL, s->ddi & ~NMR_DDI_ENABLE);
            o->wr(o->ctx, NMR_DP_TP_CTL, s->tp & ~NMR_TP_ENABLE);
            return NMR_E_TRAIN;
        }
    }
    o->wr(o->ctx, NMR_DP_TP_CTL, (o->rd(o->ctx, NMR_DP_TP_CTL) & ~NMR_TP_PAT_MASK) | NMR_TP_IDLE);
    nmr_wait(o, NMR_DP_TP_STATUS, NMR_TP_IDLE_DONE, NMR_TP_IDLE_DONE, 5000);
    o->wr(o->ctx, NMR_DP_TP_CTL, (o->rd(o->ctx, NMR_DP_TP_CTL) & ~NMR_TP_PAT_MASK) | NMR_TP_NORMAL);
    nmr_note(o, "transcoder on");
    o->wr(o->ctx, NMR_TRANSCONF, s->transconf | NMR_TRANS_ENABLE);
    fc = o->rd(o->ctx, NMR_FRMCOUNT);
    for (i = 0; i < 200; i++) { if (o->rd(o->ctx, NMR_FRMCOUNT) != fc) break; o->delay_us(o->ctx, 1000); }
    if (o->rd(o->ctx, NMR_FRMCOUNT) == fc) return NMR_E_NO_SCANOUT;
    nmr_note(o, "backlight");
    o->wr(o->ctx, NMR_BLC_PWM_FREQ, s->blc[1]); o->wr(o->ctx, NMR_BLC_PWM_DUTY, s->blc[2]); o->wr(o->ctx, NMR_BLC_PWM_CTL, s->blc[0]);
    return NMR_OK;
}

/* nmhybridresume=1 */
static inline int nmr_enabled(int present, long long value) { return present && value == 1; }

#endif

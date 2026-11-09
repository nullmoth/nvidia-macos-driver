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
 * panel on a combo-PHY port (DDI A or B), any pipe A to D, its transcoder, and that port's AUX channel: the layout of
 * Linux's i915 headers (MIT). Other generations and Type-C ports are refused, never guessed. This header is plain C with no kernel headers so tools/test_hybrid_resume.c drives
 * the same code against a model of the hardware.
 *
 * Switch: boot-arg nmhybridresume=1 (default off). With it off, sleep behaves as before.
 */
#ifndef NM_HYBRID_RESUME_H
#define NM_HYBRID_RESUME_H

#include <stdint.h>

struct nmr_snap;
struct nmr_ops {
    void *ctx;
    uint32_t (*rd)(void *ctx, uint32_t off);
    void (*wr)(void *ctx, uint32_t off, uint32_t v);
    void (*delay_us)(void *ctx, unsigned us);
    void (*log)(void *ctx, const char *step);                /* one line per step reached; may be NULL */
    int (*set_drive)(void *ctx, unsigned vswing, unsigned preemph);   /* 0 applied, -1 not supported */
    /* display version 14 and later (nm_hybrid_resume14.h): bits OR-ed into every AUX control word (the channel's power request),
     * and the way a swing / pre-emphasis level reaches the PHY. Zero / NULL for versions 12 and 13. */
    uint32_t aux_ctl_or;
    int (*apply_level)(const struct nmr_ops *o, const struct nmr_snap *s, unsigned vswing, unsigned preemph);
};

/* ---- registers (display 12 and 13, DDI A / pipe A / transcoder A / AUX A) */
#define NMR_PW_DRIVER      0x45404u
#define NMR_PW_AUX         0x45440u
#define NMR_PW_DDI         0x45450u
#define NMR_DE_PLL_ENABLE  0x46070u
#define NMR_CDCLK_CTL      0x46000u
#define NMR_CDCLK_SQUASH   0x46008u
#define NMR_DSSM           0x51004u
/* display core init (Linux icl_display_core_init): the state the firmware's own core init left, which sleep also clears */
#define NMR_NDE_RSTWRN     0x46408u      /* HSW_NDE_RSTWRN_OPT: the reset handshake with the PCH (and, from version 14, with the PICA) */
#define NMR_DBUF_CTL(i)    ((i) == 0 ? 0x45008u : ((i) == 1 ? 0x44FE8u : ((i) == 2 ? 0x44300u : 0x44304u)))
#define NMR_DBUF_REQ       (1u << 31)
#define NMR_DBUF_STATE     (1u << 30)
#define NMR_MBUS_ABOX(i)   ((i) == 0 ? 0x45038u : ((i) == 1 ? 0x45048u : 0x4504Cu))
#define NMR_BW_BUDDY_CTL(i)  ((i) == 0 ? 0x45130u : 0x45140u)
#define NMR_BW_BUDDY_MASK(i) ((i) == 0 ? 0x45134u : 0x45144u)
#define NMR_PHY_MISC(p)    (0x64C00u + 4u * (p))
#define NMR_DCPR2          0x46434u
#define NMR_MBUS_CTL       0x4438Cu       /* version 20 and later: MDCLK to CDCLK ratio throttle and MBUS join */
#define NMR_DC_STATE       0x45504u
#define NMR_DC_HOLD        (3u << 20)     /* HOLD_PHY_PG1_LATCH and HOLD_PHY_CLKREQ_PG1_LATCH */
#define NMR_XE2_D2D_ENABLE (1u << 29)     /* version 20 and later: the D2D link is controlled in the DDI buffer register */
#define NMR_XE2_D2D_STATE  (1u << 28)
#define NMR_GMBUS_GATE     (1u << 31)     /* SOUTH_DSPCLK_GATE_D: GMBUS unit clock gate disable */
#define NMR_ERR_FATAL      0x4421Cu
#define NMR_DSPCLK_GATE    0xC2020u
#define NMR_PCODE_MAILBOX  0x138124u
#define NMR_PCODE_DATA     0x138128u
#define NMR_PCODE_DATA1    0x13812Cu
#define NMR_PCODE_READY    (1u << 31)
#define NMR_PCODE_CDCLK    0x7u
#define NMR_PCODE_PREPARE  0x3u
#define NMR_PCODE_READY_FOR_CHANGE 0x1u
#define NMR_DPLL_ENABLE(n) (0x46010u + 4u * (n))
#define NMR_DPLL_CFGCR0(n) (0x164284u + 8u * (n))
#define NMR_DPLL_CFGCR1(n) (0x164288u + 8u * (n))
#define NMR_DPLL_DIV0(n)   (0x164B00u + 0x100u * (n))
#define NMR_DPCLKA_CFGCR0  0x164280u
#define NMR_PHY_A_BASE     0x162000u
#define NMR_PHY_B_BASE     0x6C000u
#define NMR_PHY_C_BASE     0x160000u
#define NMR_PHY_WORDS      768u       /* 0xC00 bytes: CL, COMP, AUX, PCS and TX groups and the four lanes */
#define NMR_PP_STATUS      0xC7200u
#define NMR_PP_CONTROL     0xC7204u
#define NMR_PP_ON_DELAYS   0xC7208u
#define NMR_PP_OFF_DELAYS  0xC720Cu
#define NMR_PP_DIVISOR     0xC7210u
/* per pipe (stride 0x1000), per transcoder (stride 0x1000), per port (stride 0x100) */
#define NMR_TRANS_CLK_SEL(t) (0x46140u + 4u * (t))
#define NMR_PIPE_MISC(p)     (0x70030u + 0x1000u * (p))
#define NMR_WM_LINETIME(p)   (0x45270u + 4u * (p))
#define NMR_MSA_MISC(t)      (0x60410u + 0x1000u * (t))
#define NMR_FUNC_CTL(t)      (0x60400u + 0x1000u * (t))
/* display 12 and later: the DisplayPort transport registers are per transcoder (Linux TGL_DP_TP_CTL / TGL_DP_TP_STATUS) */
#define NMR_DP_TP_CTL(t)     (0x60540u + 0x1000u * (t))
#define NMR_DP_TP_STATUS(t)  (0x60544u + 0x1000u * (t))
#define NMR_DDI_BUF_CTL(d)   (0x64000u + 0x100u * (d))
#define NMR_TRANSCONF(p)     (0x70008u + 0x1000u * (p))
#define NMR_FRMCOUNT(p)      (0x70040u + 0x1000u * (p))
#define NMR_PLANE_BASE(p)    (0x70180u + 0x1000u * (p))      /* plane 1: CTL +0x00 STRIDE +0x08 POS +0x0C SIZE +0x10 SURF +0x1C OFFSET +0x24 */
#define NMR_PLANE_WM_BASE(p) (0x70240u + 0x1000u * (p))      /* plane 1 watermarks and buffer allocation, +0xC0..+0xFF of the plane block */
#define NMR_PLANE_WM_WORDS 16u
#define NMR_AUX_CTL(d)       (0x64010u + 0x100u * (d))
#define NMR_AUX_DATA(d)      (0x64014u + 0x100u * (d))
#define NMR_TRANS_PORT_SHIFT 27u
#define NMR_TRANS_PORT_MASK  (0xFu << NMR_TRANS_PORT_SHIFT)
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
    NMR_E_TRAIN, NMR_E_NO_SCANOUT, NMR_E_GEN, NMR_E_PMDEMAND, NMR_E_PHY
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
    case NMR_E_PMDEMAND: return "the power-management demand request was not acknowledged";
    case NMR_E_PHY: return "the PHY did not come up";
    default: return "?";
    }
}

static inline int nmr_wait(const struct nmr_ops *o, uint32_t reg, uint32_t mask, uint32_t val, unsigned total_us)
{
    unsigned t;
    for (t = 0; t <= total_us; t += 100) {
        if ((o->rd(o->ctx, reg) & mask) == val) return 1;
        o->delay_us(o->ctx, 100);
    }
    return 0;
}

struct nmr_edp { uint8_t known, low_vswing, hobl; };     /* from the video BIOS (nm_hybrid_vbt.h); known = 0: not known */
/* ---- where the panel is: pipe A to D, its transcoder (same index), a combo port (DDI A or B) with its PHY and AUX channel */
struct nmr_topo { uint8_t pipe, port; };
static inline uint32_t nmr_phy_base(unsigned port) { return port == 0 ? NMR_PHY_A_BASE : (port == 1 ? NMR_PHY_B_BASE : NMR_PHY_C_BASE); }
/* Combo ports: DDI A to C on display 12 (Tiger Lake, Rocket Lake, Alder Lake S), A and B on display 13 (Alder Lake P, whose C and
 * later are Type-C). Type-C ports need the port's PHY ownership handshake and the Type-C subsystem; an internal panel is never
 * wired to one on any laptop, so they are refused rather than driven blind. */
static inline unsigned nmr_last_combo_port(unsigned display_version) { return display_version == 12 ? 2u : 1u; }

/* ---- display version 14: the C10 PHY's register file as the firmware left it (read over the message bus, nm_hybrid_resume14.h) */
struct nmx_snap {
    uint8_t  valid, reversal, lanes;
    uint8_t  ver, is_c20, c20_serdes, c20_width;   /* display version this was captured on; 1 when the port's PHY is a C20 */
    uint16_t c20_tx[3], c20_cmn[4], c20_mp[11]; uint32_t c20_clock;     /* the C20 register file (kHz port clock computed from it) */
    uint8_t  pll[20], cmn[4], tx[2], width, ovrd[2];
    uint8_t  vdrovrd[2][2][3];                   /* [lane][tx][control] */
    uint8_t  ctl1;
    uint32_t clock_ctl, valfreq, buf1, buf2, buf3, pmdemand[2];
};

/* ---- the display clock and the voltage level the platform controller is told for it (Linux intel_cdclk.c bxt_get_cdclk,
 * tgl_calc_voltage_level). The voltage level cannot be read back, so it is computed from the clock, as Linux does. */
static inline unsigned nmr_cdclk_khz(uint32_t dssm, uint32_t de_pll, uint32_t ctl, uint32_t squash)
{
    const unsigned sel = (dssm >> 29) & 7;
    const unsigned ref = sel == 1 ? 19200u : (sel == 2 ? 38400u : 24000u);
    const unsigned long vco = (unsigned long)ref * (de_pll & 0xFFu);
    const unsigned divs[4] = { 2, 3, 4, 8 }, div = divs[(ctl >> 22) & 3];
    if (!(de_pll & (1u << 31)) || !(de_pll & (1u << 30)) || !vco) return ref / 2;          /* PLL off: the bypass clock */
    if (squash & (1u << 31)) {
        const unsigned size = ((squash >> 24) & 0xF) + 1;
        unsigned wave = (squash & 0xFFFFu) >> (16 - size), pop = 0;
        while (wave) { pop += wave & 1; wave >>= 1; }
        return (unsigned)((pop * vco + (size * div) / 2) / (size * div));
    }
    return (unsigned)((vco + div / 2) / div);
}
static inline uint8_t nmr_cdclk_voltage(unsigned khz)
{
    static const unsigned max[4] = { 312000, 326400, 556800, 652800 };
    unsigned l;
    for (l = 0; l < 4; l++) if (khz <= max[l]) return (uint8_t)l;
    return 3;
}
/* One platform-controller mailbox transaction (Linux __snb_pcode_rw): wait for the mailbox to be free, write data and command,
 * wait for it to be taken, read the data back, and check the status byte. 0 on success. */
static inline int nmr_pcode(const struct nmr_ops *o, uint32_t mbox, uint32_t *val, int is_read)
{
    uint32_t m;
    if (o->rd(o->ctx, NMR_PCODE_MAILBOX) & NMR_PCODE_READY) return -1;
    o->wr(o->ctx, NMR_PCODE_DATA, *val); o->wr(o->ctx, NMR_PCODE_DATA1, 0);
    o->wr(o->ctx, NMR_PCODE_MAILBOX, NMR_PCODE_READY | mbox);
    if (!nmr_wait(o, NMR_PCODE_MAILBOX, NMR_PCODE_READY, 0, 20000)) return -1;
    m = o->rd(o->ctx, NMR_PCODE_MAILBOX);
    if (is_read) *val = o->rd(o->ctx, NMR_PCODE_DATA);
    return (m & 0xFFu) ? -1 : 0;
}
/* ---- the snapshot */
#define NMR_N_TIMING 11
#define NMR_N_PLANE  5
struct nmr_snap {
    uint8_t  valid, have_sink, no_handshake, n_dpll;
    struct nmr_topo topo;
    uint32_t pw_driver, pw_aux, pw_ddi;
    uint32_t de_pll, cdclk, squash, dssm;
    uint32_t nde, dbuf[4], abox[3], buddy[4], phymisc, dcpr2, err_fatal, dspclk, mbus;   /* display core init state */
    uint8_t  volt;                               /* the voltage level the platform controller was told for this display clock */
    uint32_t dpll_en[2], cfgcr0[2], cfgcr1[2], div0[2], dpclka;
    uint32_t phy[NMR_PHY_WORDS];
    uint32_t pp_ctl, pp_on, pp_off, pp_div;
    uint32_t timing[NMR_N_TIMING];
    uint32_t trans_clk_sel, pipe_misc, wm_linetime, msa;
    uint32_t plane[NMR_N_PLANE];                 /* CTL STRIDE POS SIZE OFFSET */
    uint32_t plane_wm[NMR_PLANE_WM_WORDS];
    uint32_t surf, func, tp, ddi, transconf;
    uint32_t blc[3];
    uint8_t  dpcd[8];                            /* 0x100 0x101 0x107 0x108 0x115, max lane (0x002), downspread cap (0x003), interval (0x00E) */
    struct nmr_edp edp;                          /* the video BIOS's word on the panel's drive table; known = 0 keeps the DisplayPort tables */
    struct nmx_snap cx0;                         /* display version 14 only */
    uint32_t pmdemand[2];                        /* display version 14 only: the power-management demand the firmware had requested */
};
static const uint32_t nmr_timing_off[NMR_N_TIMING] = {
    0x000, 0x004, 0x008, 0x00C, 0x010, 0x014, 0x01C, 0x030, 0x034, 0x040, 0x044 };   /* from the transcoder's 0x60000 + 0x1000 * t */
static const uint32_t nmr_plane_off[NMR_N_PLANE] = { 0x00, 0x08, 0x0C, 0x10, 0x24 };
#define NMR_TIMING(t, i) (0x60000u + 0x1000u * (t) + nmr_timing_off[i])
#define NMR_PLANE(p, i)  (NMR_PLANE_BASE(p) + nmr_plane_off[i])
#define NMR_SURF(p)      (NMR_PLANE_BASE(p) + 0x1Cu)

/* ---- AUX channel, native transactions (Linux intel_dp_aux.c) */
#define NMR_AUX_NATIVE_WRITE 8u
#define NMR_AUX_NATIVE_READ  9u
/* One AUX transaction. cmd is the 4-bit request: 8 native write, 9 native read, 0 I2C write, 1 I2C read, with 4 added for
 * "middle of transaction" (the I2C bus is kept). Writes carry buf out, reads bring it in. */
static inline int nmr_aux_raw(const struct nmr_ops *o, unsigned port, unsigned cmd, uint32_t addr, uint8_t *buf, unsigned len)
{
    const int write = !(cmd & 1);
    unsigned i, tries, got = 0;
    const uint32_t CTL = NMR_AUX_CTL(port), DATA = NMR_AUX_DATA(port);
    uint8_t msg[20];
    if (len == 0 || len > 16) return -1;
    msg[0] = (uint8_t)((cmd << 4) | ((addr >> 16) & 0xF));
    msg[1] = (uint8_t)(addr >> 8); msg[2] = (uint8_t)addr; msg[3] = (uint8_t)(len - 1);
    unsigned send = 4 + (write ? len : 0);
    if (write) for (i = 0; i < len; i++) msg[4 + i] = buf[i];
    for (tries = 0; tries < 5; tries++) {
        uint32_t ctl, v;
        for (i = 0; i < 40; i++) { if (!(o->rd(o->ctx, CTL) & (1u << 31))) break; o->delay_us(o->ctx, 100); }
        if (o->rd(o->ctx, CTL) & (1u << 31)) return -1;
        for (i = 0; i < send; i += 4) {
            uint32_t w = 0; unsigned j;
            for (j = 0; j < 4 && i + j < send; j++) w |= (uint32_t)msg[i + j] << (24 - 8 * j);
            o->wr(o->ctx, DATA + i, w);
        }
        ctl = (1u << 31) | (1u << 30) | (1u << 29) | (1u << 28) | (3u << 26) | (1u << 25) | ((uint32_t)send << 20) |
              ((18u - 1u) << 5) | (32u - 1u) | o->aux_ctl_or;
        o->wr(o->ctx, CTL, ctl);
        for (i = 0; i < 40; i++) { v = o->rd(o->ctx, CTL); if (!(v & (1u << 31))) break; o->delay_us(o->ctx, 100); }
        v = o->rd(o->ctx, CTL);
        o->wr(o->ctx, CTL, v | (1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));   /* write-1-to-clear the status bits */
        if ((v & (1u << 31)) || (v & ((1u << 28) | (1u << 25))) || !(v & (1u << 30))) { o->delay_us(o->ctx, 400); continue; }
        unsigned n = (v >> 20) & 0x1F;
        if (n == 0) return -1;
        {   /* reply: first byte is the command/ack nibble, 0 = ACK */
            uint8_t rep[20] = {0}; unsigned k;
            for (k = 0; k < n && k < 20; k += 4) {
                uint32_t w = o->rd(o->ctx, DATA + k); unsigned j;
                for (j = 0; j < 4 && k + j < n; j++) rep[k + j] = (uint8_t)(w >> (24 - 8 * j));
            }
            if ((rep[0] >> 4) != 0 || (cmd < 8 && (rep[0] & 0xC))) { o->delay_us(o->ctx, 400); continue; }        /* NACK or DEFER: try again */
            if (!write) { for (got = 0; got < n - 1 && got < len; got++) buf[got] = rep[1 + got]; return (int)got; }
            return (int)len;
        }
    }
    return -1;
}
static inline int nmr_aux_xfer(const struct nmr_ops *o, unsigned port, int write, uint32_t addr, uint8_t *buf, unsigned len)
{
    return nmr_aux_raw(o, port, write ? NMR_AUX_NATIVE_WRITE : NMR_AUX_NATIVE_READ, addr, buf, len);
}
/* The panel's identifier: bytes 8 to 17 of its EDID (manufacturer, product code, serial, week, year), read over I2C-over-AUX from
 * address 0x50 with a repeated start. These are the 10 bytes the video BIOS's panel table holds. Returns 0 or -1. */
static inline int nmr_edid_id(const struct nmr_ops *o, unsigned port, uint8_t *id10)
{
    uint8_t off = 8;
    if (nmr_aux_raw(o, port, 4, 0x50, &off, 1) != 1) return -1;       /* I2C write, offset 8, bus kept */
    if (nmr_aux_raw(o, port, 1, 0x50, id10, 10) != 10) return -1;     /* I2C read, bus released */
    return 0;
}
static inline int nmr_dpcd_rd(const struct nmr_ops *o, unsigned port, uint32_t a, uint8_t *b, unsigned n) { return nmr_aux_xfer(o, port, 0, a, b, n); }
static inline int nmr_dpcd_wr(const struct nmr_ops *o, unsigned port, uint32_t a, const uint8_t *b, unsigned n)
{
    uint8_t t[16]; unsigned i; if (n > 16) return -1;
    for (i = 0; i < n; i++) t[i] = b[i];
    return nmr_aux_xfer(o, port, 1, a, t, n);
}
static inline int nmr_dpcd_wr1(const struct nmr_ops *o, unsigned port, uint32_t a, uint8_t v) { return nmr_dpcd_wr(o, port, a, &v, 1); }

/* ---- source drive levels: the combo PHY's swing and pre-emphasis for DisplayPort voltage swing 0-3 x pre-emphasis 0-3
 * (Linux intel_ddi_buf_trans.c tgl_combo_phy_trans_dp_hbr, _hbr2 and adlp_combo_phy_trans_dp_hbr, _hbr2_hbr3; the ten
 * valid combinations). Registers: TX_DW2 swing select and RCOMP scalar, TX_DW4 cursor coefficient and post-cursor, TX_DW7
 * n-scalar, each per lane (intel_ddi.c icl_ddi_combo_vswing_program). eDP-specific tables (low voltage swing, HOBL), chosen
 * by the video BIOS, are not applied: level 0 is whatever the firmware programmed and is never rewritten here. */
struct nmr_trans { uint8_t sw, ns, cc, pc2, pc1; };
static const struct nmr_trans nmr_tgl_hbr[10] = {
    {0xA,0x32,0x3F,0,0x00},{0xA,0x4F,0x37,0,0x08},{0xC,0x71,0x2F,0,0x10},{0x6,0x7D,0x2B,0,0x14},{0xA,0x4C,0x3F,0,0x00},
    {0xC,0x73,0x34,0,0x0B},{0x6,0x7F,0x2F,0,0x10},{0xC,0x6C,0x3C,0,0x03},{0x6,0x7F,0x35,0,0x0A},{0x6,0x7F,0x3F,0,0x00} };
static const struct nmr_trans nmr_tgl_hbr2[10] = {
    {0xA,0x35,0x3F,0,0x00},{0xA,0x4F,0x37,0,0x08},{0xC,0x63,0x2F,0,0x10},{0x6,0x7F,0x2B,0,0x14},{0xA,0x47,0x3F,0,0x00},
    {0xC,0x63,0x34,0,0x0B},{0x6,0x7F,0x2F,0,0x10},{0xC,0x61,0x3C,0,0x03},{0x6,0x7B,0x35,0,0x0A},{0x6,0x7F,0x3F,0,0x00} };
static const struct nmr_trans nmr_adlp_hbr[10] = {
    {0xA,0x35,0x3F,0,0x00},{0xA,0x4F,0x37,0,0x08},{0xC,0x71,0x31,0,0x0E},{0x6,0x7F,0x2C,0,0x13},{0xA,0x4C,0x3F,0,0x00},
    {0xC,0x73,0x34,0,0x0B},{0x6,0x7F,0x2F,0,0x10},{0xC,0x7C,0x3C,0,0x03},{0x6,0x7F,0x35,0,0x0A},{0x6,0x7F,0x3F,0,0x00} };
static const struct nmr_trans nmr_adlp_hbr2[10] = {
    {0xA,0x35,0x3F,0,0x00},{0xA,0x4F,0x37,0,0x08},{0xC,0x71,0x30,0,0x0F},{0x6,0x7F,0x2B,0,0x14},{0xA,0x4C,0x3F,0,0x00},
    {0xC,0x73,0x34,0,0x0B},{0x6,0x7F,0x30,0,0x0F},{0xC,0x63,0x3F,0,0x00},{0x6,0x7F,0x38,0,0x07},{0x6,0x7F,0x3F,0,0x00} };

/* DisplayPort signal level -> table row (intel_ddi.c index_to_dp_signal_levels); -1 for a combination DP does not allow */
static inline int nmr_level_index(unsigned v, unsigned p)
{
    if (v > 3 || p > 3 || v + p > 3) return -1;
    switch (v) { case 0: return (int)p; case 1: return 4 + (int)p; case 2: return 7 + (int)p; default: return 9; }
}
static const struct nmr_trans nmr_icl_dp_edp_hbr3[10] = {
    {0xA,0x35,0x3F,0x00,0x00},{0xA,0x4F,0x37,0x00,0x08},{0xC,0x71,0x2F,0x00,0x10},{0x6,0x7F,0x2B,0x00,0x14},{0xA,0x4C,0x3F,0x00,0x00},{0xC,0x73,0x34,0x00,0x0B},{0x6,0x7F,0x2F,0x00,0x10},{0xC,0x6C,0x3C,0x00,0x03},{0x6,0x7F,0x35,0x00,0x0A},{0x6,0x7F,0x3F,0x00,0x00} };
static const struct nmr_trans nmr_icl_edp_hbr2[10] = {
    {0x0,0x7F,0x3F,0x00,0x00},{0x8,0x7F,0x38,0x00,0x07},{0x1,0x7F,0x33,0x00,0x0C},{0x9,0x7F,0x31,0x00,0x0E},{0x8,0x7F,0x3F,0x00,0x00},{0x1,0x7F,0x38,0x00,0x07},{0x9,0x7F,0x35,0x00,0x0A},{0x1,0x7F,0x3F,0x00,0x00},{0x9,0x7F,0x38,0x00,0x07},{0x9,0x7F,0x3F,0x00,0x00} };
static const struct nmr_trans nmr_tgl_edp_hobl[9] = {
    {0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00},{0x6,0x7F,0x3F,0x00,0x00} };
static const struct nmr_trans nmr_adlp_edp_hbr2[10] = {
    {0x4,0x50,0x38,0x00,0x07},{0x4,0x58,0x35,0x00,0x0A},{0x4,0x60,0x34,0x00,0x0B},{0x4,0x6A,0x32,0x00,0x0D},{0x4,0x5E,0x38,0x00,0x07},{0x4,0x61,0x36,0x00,0x09},{0x4,0x6B,0x34,0x00,0x0B},{0x4,0x69,0x39,0x00,0x06},{0x4,0x73,0x37,0x00,0x08},{0x4,0x7A,0x38,0x00,0x07} };
static const struct nmr_trans nmr_adlp_edp_hbr3[10] = {
    {0xA,0x35,0x3F,0x00,0x00},{0xA,0x4F,0x37,0x00,0x08},{0xC,0x71,0x30,0x00,0x0f},{0x6,0x7F,0x2B,0x00,0x14},{0xA,0x4C,0x3F,0x00,0x00},{0xC,0x73,0x34,0x00,0x0B},{0x6,0x7F,0x30,0x00,0x0F},{0xC,0x63,0x3F,0x00,0x00},{0x6,0x7F,0x38,0x00,0x07},{0x6,0x7F,0x3F,0x00,0x00} };
/* eDP panels use their own tables when the video BIOS says so (low voltage swing, reduced-power), and above 5.4 Gb/s a table of
 * their own (Linux tgl_get_combo_buf_trans_edp, adlp_get_combo_buf_trans_edp). 'n' is the number of rows: a row past it is a
 * level the table does not have, which caps the swing the source can offer. */
struct nmr_sel { const struct nmr_trans *t; unsigned n; int hobl; };
static inline struct nmr_sel nmr_select(unsigned display_version, unsigned rate_khz, const struct nmr_edp *edp)
{
    struct nmr_sel r;
    const int is_edp = edp && edp->known;
    r.hobl = 0; r.n = 10;
    if (display_version == 13) {
        r.t = rate_khz > 270000 ? nmr_adlp_hbr2 : nmr_adlp_hbr;
        if (is_edp) {
            if (rate_khz > 540000) r.t = nmr_adlp_edp_hbr3;
            else if (edp->hobl) { r.t = nmr_tgl_edp_hobl; r.n = 9; r.hobl = 1; }
            else if (edp->low_vswing) r.t = nmr_adlp_edp_hbr2;
        }
    } else {
        r.t = rate_khz > 270000 ? nmr_tgl_hbr2 : nmr_tgl_hbr;
        if (is_edp) {
            if (rate_khz > 540000) r.t = nmr_icl_dp_edp_hbr3;
            else if (edp->hobl) { r.t = nmr_tgl_edp_hobl; r.n = 9; r.hobl = 1; }
            else if (edp->low_vswing) r.t = nmr_icl_edp_hbr2;
        }
    }
    return r;
}
static inline const struct nmr_trans *nmr_table(unsigned display_version, unsigned rate_khz) { return nmr_select(display_version, rate_khz, 0).t; }

/* the field edit of one register, as Linux's intel_de_rmw */
static inline void nmr_rmw(const struct nmr_ops *o, uint32_t reg, uint32_t mask, uint32_t val) { o->wr(o->ctx, reg, (o->rd(o->ctx, reg) & ~mask) | (val & mask)); }
/* Program all four lanes for (v, p). Level 0 with the firmware's own table is left as it is. Returns 0, or -1 for a pair DP does
 * not allow or one the chosen table has no row for. */
static inline int nmr_drive_apply(const struct nmr_ops *o, unsigned port, unsigned display_version, unsigned rate_khz,
                                  const struct nmr_edp *edp, unsigned v, unsigned p)
{
    int idx = nmr_level_index(v, p), ln;
    const uint32_t base = nmr_phy_base(port);
    struct nmr_sel sel = nmr_select(display_version, rate_khz, edp);
    if (idx < 0 || (unsigned)idx >= sel.n) return -1;
    if (sel.hobl) nmr_rmw(o, base + 10 * 4, (1u << 3) | (1u << 2), (1u << 3) | (1u << 2));   /* EDP4K2K_MODE_OVRD_EN | OPTIMIZED in PORT_CL_DW10 */
    if (idx == 0) return 0;
    {
        const struct nmr_trans *t = &sel.t[idx];
        for (ln = 0; ln < 4; ln++) {
            const uint32_t lane = base + 0x880u + 0x100u * (unsigned)ln;
            nmr_rmw(o, lane + 2 * 4, (1u << 15) | (7u << 11) | 0xFFu, ((uint32_t)(t->sw >> 3) << 15) | ((uint32_t)(t->sw & 7) << 11) | 0x98u);
            nmr_rmw(o, lane + 4 * 4, (0x3Fu << 12) | (0x3Fu << 6) | 0x3Fu, ((uint32_t)t->pc1 << 12) | ((uint32_t)t->pc2 << 6) | t->cc);
            nmr_rmw(o, lane + 7 * 4, 0x7Fu << 24, (uint32_t)t->ns << 24);
        }
    }
    return 0;
}

/* ---- capture, while the firmware's mode is lit. 'pipe' is the pipe the adopted plane belongs to. */
static inline int nmr_snap_impl(const struct nmr_ops *o, struct nmr_snap *s, unsigned pipe, unsigned display_version, const struct nmr_edp *edp, int with_combo)
{
    unsigned i, port, t = pipe; uint8_t b[4];
    s->valid = s->have_sink = s->no_handshake = 0;
    if (edp) s->edp = *edp; else { s->edp.known = s->edp.low_vswing = s->edp.hobl = 0; }
    if (pipe > 3) return NMR_E_GEN;
    if (!(o->rd(o->ctx, NMR_TRANSCONF(pipe)) & NMR_TRANS_ENABLE)) return NMR_E_NO_SNAPSHOT;
    s->func = o->rd(o->ctx, NMR_FUNC_CTL(t));
    { unsigned sel = (s->func & NMR_TRANS_PORT_MASK) >> NMR_TRANS_PORT_SHIFT;    /* port + 1, 0 = none */
      if (!sel) return NMR_E_NO_SNAPSHOT;
      port = sel - 1; }
    if (!((display_version >= 12 && display_version <= 14) || display_version == 20 || display_version >= 30) || port > nmr_last_combo_port(display_version)) return NMR_E_GEN;   /* Type-C and later ports: refused */
    if (!(o->rd(o->ctx, NMR_DDI_BUF_CTL(port)) & NMR_DDI_ENABLE)) return NMR_E_NO_SNAPSHOT;
    s->topo.pipe = (uint8_t)pipe; s->topo.port = (uint8_t)port;
    s->pw_driver = o->rd(o->ctx, NMR_PW_DRIVER); s->pw_aux = o->rd(o->ctx, NMR_PW_AUX); s->pw_ddi = o->rd(o->ctx, NMR_PW_DDI);
    s->de_pll = o->rd(o->ctx, NMR_DE_PLL_ENABLE); s->cdclk = o->rd(o->ctx, NMR_CDCLK_CTL);
    if (display_version >= 20) s->mbus = o->rd(o->ctx, NMR_MBUS_CTL);
    s->nde = o->rd(o->ctx, NMR_NDE_RSTWRN); s->dspclk = o->rd(o->ctx, NMR_DSPCLK_GATE);
    for (i = 0; i < 4; i++) s->dbuf[i] = o->rd(o->ctx, NMR_DBUF_CTL(i));
    if (display_version <= 13) {
        for (i = 0; i < 3; i++) s->abox[i] = o->rd(o->ctx, NMR_MBUS_ABOX(i));
        for (i = 0; i < 2; i++) { s->buddy[2 * i] = o->rd(o->ctx, NMR_BW_BUDDY_CTL(i)); s->buddy[2 * i + 1] = o->rd(o->ctx, NMR_BW_BUDDY_MASK(i)); }
        s->phymisc = o->rd(o->ctx, NMR_PHY_MISC(port)); s->dcpr2 = o->rd(o->ctx, NMR_DCPR2); s->err_fatal = o->rd(o->ctx, NMR_ERR_FATAL);
    }
    s->dssm = o->rd(o->ctx, NMR_DSSM); s->squash = display_version >= 13 ? o->rd(o->ctx, NMR_CDCLK_SQUASH) : 0;
    s->volt = nmr_cdclk_voltage(nmr_cdclk_khz(s->dssm, s->de_pll, s->cdclk, s->squash));
    s->n_dpll = 2;
    if (with_combo) for (i = 0; i < 2; i++) {
        s->dpll_en[i] = o->rd(o->ctx, NMR_DPLL_ENABLE(i)); s->cfgcr0[i] = o->rd(o->ctx, NMR_DPLL_CFGCR0(i));
        s->cfgcr1[i] = o->rd(o->ctx, NMR_DPLL_CFGCR1(i)); s->div0[i] = o->rd(o->ctx, NMR_DPLL_DIV0(i));
    }
    if (with_combo) {
        s->dpclka = o->rd(o->ctx, NMR_DPCLKA_CFGCR0);
        for (i = 0; i < NMR_PHY_WORDS; i++) s->phy[i] = o->rd(o->ctx, nmr_phy_base(port) + 4 * i);
    }
    s->pp_ctl = o->rd(o->ctx, NMR_PP_CONTROL); s->pp_on = o->rd(o->ctx, NMR_PP_ON_DELAYS);
    s->pp_off = o->rd(o->ctx, NMR_PP_OFF_DELAYS); s->pp_div = o->rd(o->ctx, NMR_PP_DIVISOR);
    for (i = 0; i < NMR_N_TIMING; i++) s->timing[i] = o->rd(o->ctx, NMR_TIMING(t, i));
    s->trans_clk_sel = o->rd(o->ctx, NMR_TRANS_CLK_SEL(t)); s->pipe_misc = o->rd(o->ctx, NMR_PIPE_MISC(pipe));
    s->wm_linetime = o->rd(o->ctx, NMR_WM_LINETIME(pipe)); s->msa = o->rd(o->ctx, NMR_MSA_MISC(t));
    for (i = 0; i < NMR_N_PLANE; i++) s->plane[i] = o->rd(o->ctx, NMR_PLANE(pipe, i));
    for (i = 0; i < NMR_PLANE_WM_WORDS; i++) s->plane_wm[i] = o->rd(o->ctx, NMR_PLANE_WM_BASE(pipe) + 4 * i);
    s->surf = o->rd(o->ctx, NMR_SURF(pipe)); s->tp = o->rd(o->ctx, NMR_DP_TP_CTL(t));
    s->ddi = o->rd(o->ctx, NMR_DDI_BUF_CTL(port)); s->transconf = o->rd(o->ctx, NMR_TRANSCONF(pipe));
    s->blc[0] = o->rd(o->ctx, NMR_BLC_PWM_CTL); s->blc[1] = o->rd(o->ctx, NMR_BLC_PWM_FREQ); s->blc[2] = o->rd(o->ctx, NMR_BLC_PWM_DUTY);
    /* the sink: its capabilities and the link settings the firmware gave it. The AUX channel needs its power well, which the
     * firmware may have left off once the panel was lit; ask for it for the reads and put it back. */
    { const uint32_t auxbit = 1u << (2 * port);
      uint32_t aux_before = o->rd(o->ctx, NMR_PW_AUX);
      if (!(aux_before & (auxbit << 1))) {
          o->wr(o->ctx, NMR_PW_AUX, aux_before | auxbit);
          nmr_wait(o, NMR_PW_AUX, auxbit << 1, auxbit << 1, 20000);
      }
      if (nmr_dpcd_rd(o, port, NMR_DPCD_LINK_BW, b, 2) == 2) { s->dpcd[0] = b[0]; s->dpcd[1] = b[1]; s->have_sink = 1; }
      if (nmr_dpcd_rd(o, port, NMR_DPCD_DOWNSPREAD_CT, b, 2) == 2) { s->dpcd[2] = b[0]; s->dpcd[3] = b[1]; } else s->have_sink = 0;
      if (nmr_dpcd_rd(o, port, NMR_DPCD_LINK_RATE_SET, b, 1) == 1) s->dpcd[4] = b[0];
      if (nmr_dpcd_rd(o, port, NMR_DPCD_MAX_LANE, b, 2) == 2) { s->dpcd[5] = b[0]; s->dpcd[6] = b[1]; s->no_handshake = (b[1] >> 6) & 1; }
      if (nmr_dpcd_rd(o, port, NMR_DPCD_AUX_INTERVAL, b, 1) == 1) s->dpcd[7] = b[0];
      if (!(aux_before & (auxbit << 1))) o->wr(o->ctx, NMR_PW_AUX, aux_before); }
    s->valid = s->have_sink;
    return s->valid ? NMR_OK : NMR_E_NO_SNAPSHOT;
}

static inline int nmr_snapshot(const struct nmr_ops *o, struct nmr_snap *s, unsigned pipe, unsigned display_version, const struct nmr_edp *edp)
{
    return (display_version == 12 || display_version == 13) ? nmr_snap_impl(o, s, pipe, display_version, edp, 1) : NMR_E_GEN;
}

static inline void nmr_note(const struct nmr_ops *o, const char *s) { if (o->log) o->log(o->ctx, s); }

/* ---- link training: the DisplayPort clock recovery and channel equalisation procedure (DP 1.4 section 3.5.1.2, as Linux
 * intel_dp_link_training.c): the source repeats a level until the sink reports lock, follows the sink's adjust requests, gives
 * up after five tries at one voltage swing, ten loops in all, and at the maximum swing and pre-emphasis the sink can ask for.
 * Levels are applied to the source first (nmr_drive_apply), then written to the sink's TRAINING_LANEx_SET. */
static inline uint8_t nmr_lane_set(unsigned v, unsigned p, unsigned vmax)
{
    return (uint8_t)((v & 3) | (v >= vmax ? 4 : 0) | ((p & 3) << 3) | (p >= 3 || v + p >= 3 ? 0x20 : 0));
}
static inline int nmr_apply_level(const struct nmr_ops *o, const struct nmr_snap *s, unsigned v, unsigned p, unsigned display_version)
{
    const unsigned rate = (s->dpcd[0] ? s->dpcd[0] : 0x14) * 27000u;
    if (o->apply_level) { if (o->apply_level(o, s, v, p)) return -1; }
    else if (nmr_drive_apply(o, s->topo.port, display_version, rate, &s->edp, v, p)) return -1;
    if (o->set_drive && o->set_drive(o->ctx, v, p)) return -1;     /* an observer (tests) may refuse */
    return 0;
}
/* the sink's request, the highest over the lanes in use; combinations DP forbids are cut back to swing + pre-emphasis <= 3 */
static inline void nmr_request(const uint8_t adj[2], unsigned lanes, unsigned *v, unsigned *p)
{
    unsigned i, rv = 0, rp = 0;
    for (i = 0; i < lanes; i++) {
        uint8_t a = (i < 2) ? adj[0] : adj[1]; unsigned sh = 4 * (i & 1);
        unsigned lv = (a >> sh) & 3, lp = (a >> (sh + 2)) & 3;
        if (lv > rv) rv = lv;
        if (lp > rp) rp = lp;
    }
    if (rv + rp > 3) rp = 3 - rv;
    *v = rv; *p = rp;
}
static inline int nmr_train(const struct nmr_ops *o, const struct nmr_snap *s, unsigned lanes, unsigned display_version)
{
    const unsigned port = s->topo.port, t = s->topo.pipe;     /* the transport registers belong to the transcoder, which has the pipe's index */
    unsigned tries, loops, i, cur_v = 0, cur_p = 0, same_v_tries = 0;
    uint8_t lane_set[4], st[2], adj[2], al;
    uint32_t tp;
    const unsigned cr_us = 100u;
    /* the highest swing the source's table has a row for: the sink is told when it asks for more */
    const struct nmr_sel sel = nmr_select(display_version, (s->dpcd[0] ? s->dpcd[0] : 0x14) * 27000u, &s->edp);
    const unsigned vmax = sel.n >= 10 ? 3u : (sel.n >= 9 ? 2u : (sel.n >= 7 ? 1u : 0u));
    const unsigned eq_us = (s->dpcd[7] & 0x7F) ? (unsigned)(s->dpcd[7] & 0x7F) * 4000u : 400u;
    /* sink link settings, as the firmware had them */
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_LINK_BW, s->dpcd[0]) != 1) return 0;
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_LANE_COUNT, s->dpcd[1]) != 1) return 0;
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_DOWNSPREAD_CT, s->dpcd[2]) != 1) return 0;
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_CODING, s->dpcd[3]) != 1) return 0;
    if (s->dpcd[4] && nmr_dpcd_wr1(o, port, NMR_DPCD_LINK_RATE_SET, s->dpcd[4]) != 1) return 0;
    /* clock recovery: pattern 1 on both ends, scrambling off */
    tp = o->rd(o->ctx, NMR_DP_TP_CTL(t));
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (tp & ~NMR_TP_PAT_MASK) | NMR_TP_ENABLE | NMR_TP_PAT1);
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0x21) != 1) return 0;
    for (loops = 0; loops < 10; loops++) {
        for (i = 0; i < lanes; i++) lane_set[i] = nmr_lane_set(cur_v, cur_p, vmax);
        if (nmr_dpcd_wr(o, port, NMR_DPCD_LANE0_SET, lane_set, lanes) != (int)lanes) return 0;
        o->delay_us(o->ctx, cr_us);
        if (nmr_dpcd_rd(o, port, NMR_DPCD_LANE01_STATUS, st, 2) != 2 || nmr_dpcd_rd(o, port, NMR_DPCD_ADJUST01, adj, 2) != 2) return 0;
        {
            unsigned ok = 0;
            for (i = 0; i < lanes; i++) ok += (((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 1);
            if (ok == lanes) break;
        }
        {
            unsigned rv, rp;
            nmr_request(adj, lanes, &rv, &rp);
            if (rv == cur_v) { if (++same_v_tries >= 5) { loops = 99; break; } } else same_v_tries = 0;
            if (rv == cur_v && rp == cur_p && cur_v >= 3) { loops = 99; break; }          /* maximum swing already requested */
            if (rv != cur_v || rp != cur_p) {
                if (nmr_apply_level(o, s, rv, rp, display_version)) { loops = 99; break; }  /* cannot give what it asks */
                cur_v = rv; cur_p = rp;
            }
        }
    }
    if (loops >= 10) { nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0); return 0; }
    nmr_note(o, "link training: clock recovery done");
    /* channel equalisation: pattern 2 (scrambling off); a lost lock restarts clock recovery, up to five times in all */
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_PAT2);
    if (nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0x22) != 1) return 0;
    for (tries = 0; tries < 6; tries++) {
        unsigned ok = 0;
        o->delay_us(o->ctx, eq_us);
        if (nmr_dpcd_rd(o, port, NMR_DPCD_LANE01_STATUS, st, 2) != 2 || nmr_dpcd_rd(o, port, NMR_DPCD_ALIGN_STATUS, &al, 1) != 1 ||
            nmr_dpcd_rd(o, port, NMR_DPCD_ADJUST01, adj, 2) != 2) return 0;
        for (i = 0; i < lanes; i++) ok += ((((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 7) == 7);
        if (ok == lanes && (al & 1)) { nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0); return 1; }
        for (i = 0; i < lanes; i++) if (!((((i < 2 ? st[0] : st[1]) >> (4 * (i & 1))) & 1))) {
            nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0); return 0; }
        {
            unsigned rv, rp;
            nmr_request(adj, lanes, &rv, &rp);
            if (rv != cur_v || rp != cur_p) {
                if (nmr_apply_level(o, s, rv, rp, display_version)) break;
                cur_v = rv; cur_p = rp;
                for (i = 0; i < lanes; i++) lane_set[i] = nmr_lane_set(cur_v, cur_p, vmax);
                if (nmr_dpcd_wr(o, port, NMR_DPCD_LANE0_SET, lane_set, lanes) != (int)lanes) return 0;
            }
        }
    }
    nmr_dpcd_wr1(o, port, NMR_DPCD_TRAIN_PAT, 0);
    return 0;
}

/* ---- the replay, in steps: versions 12 and 13 run them in this file, version 14 shares the ones that do not touch the PHY */
static inline int nmr_st_lit(const struct nmr_ops *o, const struct nmr_snap *s)
{
    return (o->rd(o->ctx, NMR_TRANSCONF(s->topo.pipe)) & NMR_TRANS_ENABLE) && (o->rd(o->ctx, NMR_DDI_BUF_CTL(s->topo.port)) & NMR_DDI_ENABLE);
}
static inline int nmr_st_power(const struct nmr_ops *o, const struct nmr_snap *s)
{
    const unsigned port = s->topo.port; unsigned i; uint32_t cur, req;
    const uint32_t regs[3] = { NMR_PW_DRIVER, NMR_PW_AUX, NMR_PW_DDI };
    const uint32_t vals[3] = { s->pw_driver, s->pw_aux, s->pw_ddi };
    nmr_note(o, "power wells");
    for (i = 0; i < 3; i++) {
        req = vals[i] & 0x55555555u;
        if (i > 0) req |= 1u << (2 * port);          /* this port's AUX and DDI wells are needed whatever the firmware left */
        cur = o->rd(o->ctx, regs[i]);
        o->wr(o->ctx, regs[i], cur | req);
        if (!nmr_wait(o, regs[i], req << 1, req << 1, 20000)) return NMR_E_POWER;
    }
    return NMR_OK;
}
static inline int nmr_st_cdclk(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    const int pcode = display_version >= 11 && display_version < 14;       /* versions 14 and later have no mailbox handshake */
    unsigned t;
    nmr_note(o, "display clock");
    if (pcode) {
        /* Linux skl_pcode_request: ask until the platform controller says it is ready for a display clock change */
        int ready = 0;
        for (t = 0; t < 60 && !ready; t++) {
            uint32_t v = NMR_PCODE_PREPARE;
            if (nmr_pcode(o, NMR_PCODE_CDCLK, &v, 1) == 0 && (v & NMR_PCODE_READY_FOR_CHANGE) == NMR_PCODE_READY_FOR_CHANGE) ready = 1;
            else o->delay_us(o->ctx, 1000);
        }
        if (!ready) return NMR_E_CDCLK;
    }
    if (s->de_pll & NMR_PLL_ENABLE) {
        o->wr(o->ctx, NMR_DE_PLL_ENABLE, (s->de_pll & ~NMR_PLL_LOCK) | NMR_PLL_ENABLE);   /* the lock bit is a status */
        if (!nmr_wait(o, NMR_DE_PLL_ENABLE, NMR_PLL_LOCK, NMR_PLL_LOCK, 5000)) return NMR_E_CDCLK;
    }
    if (display_version >= 13) o->wr(o->ctx, NMR_CDCLK_SQUASH, s->squash);                 /* before the clock control, as Linux */
    o->wr(o->ctx, NMR_CDCLK_CTL, s->cdclk);
    if (pcode) { uint32_t v = s->volt; if (nmr_pcode(o, NMR_PCODE_CDCLK, &v, 0)) return NMR_E_CDCLK; }
    return NMR_OK;
}
/* The display core init steps that come before and after the power wells and the display clock. Registers are written back as the
 * firmware's own core init left them. */
static inline void nmr_st_core_early(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    nmr_note(o, "display core init");
    o->wr(o->ctx, NMR_DSPCLK_GATE, s->dspclk);
    o->wr(o->ctx, NMR_NDE_RSTWRN, s->nde);                                  /* the reset handshake first */
    if (display_version <= 13) o->wr(o->ctx, NMR_PHY_MISC(s->topo.port), s->phymisc);   /* combo PHY out of power-down */
}
static inline int nmr_st_core_late(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    unsigned i;
    for (i = 0; i < 4; i++) {                                               /* display buffer slices: request, then wait for the state */
        uint32_t cur;
        if (!(s->dbuf[i] & NMR_DBUF_REQ)) continue;
        cur = o->rd(o->ctx, NMR_DBUF_CTL(i));
        o->wr(o->ctx, NMR_DBUF_CTL(i), (s->dbuf[i] & ~NMR_DBUF_STATE) | (cur & 0) | NMR_DBUF_REQ);
        if (!nmr_wait(o, NMR_DBUF_CTL(i), NMR_DBUF_STATE, NMR_DBUF_STATE, 1000)) return NMR_E_POWER;
    }
    if (display_version <= 13) {
        for (i = 0; i < 3; i++) if (s->abox[i]) o->wr(o->ctx, NMR_MBUS_ABOX(i), s->abox[i]);
        for (i = 0; i < 2; i++) { o->wr(o->ctx, NMR_BW_BUDDY_MASK(i), s->buddy[2 * i + 1]); o->wr(o->ctx, NMR_BW_BUDDY_CTL(i), s->buddy[2 * i]); }
        o->wr(o->ctx, NMR_DCPR2, s->dcpr2);
        if (display_version == 13) o->wr(o->ctx, NMR_ERR_FATAL, s->err_fatal);
    }
    if (display_version >= 20) {          /* the MDCLK to CDCLK ratio throttle and the MBUS join the firmware had */
        o->wr(o->ctx, NMR_MBUS_CTL, s->mbus);
        if (display_version == 20) {      /* Wa_15013987218: pulse the GMBUS unit clock gate */
            o->wr(o->ctx, NMR_DSPCLK_GATE, o->rd(o->ctx, NMR_DSPCLK_GATE) | NMR_GMBUS_GATE);
            o->wr(o->ctx, NMR_DSPCLK_GATE, o->rd(o->ctx, NMR_DSPCLK_GATE) & ~NMR_GMBUS_GATE);
        }
    }
    return NMR_OK;
}
static inline int nmr_st_combo(const struct nmr_ops *o, const struct nmr_snap *s)
{
    unsigned i, k; const uint32_t phy = nmr_phy_base(s->topo.port);
    nmr_note(o, "DPLL");
    for (k = 0; k < 2; k++) {
        if (!(s->dpll_en[k] & NMR_PLL_ENABLE)) continue;
        o->wr(o->ctx, NMR_DPLL_ENABLE(k), NMR_PLL_POWER);
        if (!nmr_wait(o, NMR_DPLL_ENABLE(k), NMR_PLL_PSTATE, NMR_PLL_PSTATE, 1000)) return NMR_E_PLL;
        o->wr(o->ctx, NMR_DPLL_CFGCR0(k), s->cfgcr0[k]); o->wr(o->ctx, NMR_DPLL_CFGCR1(k), s->cfgcr1[k]); o->wr(o->ctx, NMR_DPLL_DIV0(k), s->div0[k]);
        o->wr(o->ctx, NMR_DPLL_ENABLE(k), NMR_PLL_POWER | NMR_PLL_ENABLE);
        if (!nmr_wait(o, NMR_DPLL_ENABLE(k), NMR_PLL_LOCK, NMR_PLL_LOCK, 5000)) return NMR_E_PLL;
    }
    o->wr(o->ctx, NMR_DPCLKA_CFGCR0, s->dpclka);
    nmr_note(o, "combo PHY");
    for (i = 0; i < NMR_PHY_WORDS; i++) o->wr(o->ctx, phy + 4 * i, s->phy[i]);
    return NMR_OK;
}
static inline int nmr_st_panel(const struct nmr_ops *o, const struct nmr_snap *s)
{
    nmr_note(o, "panel power");
    o->wr(o->ctx, NMR_PP_ON_DELAYS, s->pp_on); o->wr(o->ctx, NMR_PP_OFF_DELAYS, s->pp_off); o->wr(o->ctx, NMR_PP_DIVISOR, s->pp_div);
    o->wr(o->ctx, NMR_PP_CONTROL, NMR_PP_UNLOCK | (s->pp_ctl & 0xFFFFu) | NMR_PP_POWER_ON);
    if (!nmr_wait(o, NMR_PP_STATUS, NMR_PP_ON_MASK, NMR_PP_ON_VALUE, 2000000)) return NMR_E_PANEL;
    return NMR_OK;
}
static inline int nmr_st_sink(const struct nmr_ops *o, const struct nmr_snap *s)
{
    nmr_note(o, "sink power");
    if (nmr_dpcd_wr1(o, s->topo.port, NMR_DPCD_SET_POWER, 1) != 1) return NMR_E_SINK;
    o->delay_us(o->ctx, 1000);
    return NMR_OK;
}
static inline void nmr_st_pipe(const struct nmr_ops *o, const struct nmr_snap *s)
{
    const unsigned pipe = s->topo.pipe, t = pipe; unsigned i;
    nmr_note(o, "pipe and plane registers");
    o->wr(o->ctx, NMR_TRANS_CLK_SEL(t), s->trans_clk_sel);
    for (i = 0; i < NMR_N_TIMING; i++) o->wr(o->ctx, NMR_TIMING(t, i), s->timing[i]);
    o->wr(o->ctx, NMR_PIPE_MISC(pipe), s->pipe_misc); o->wr(o->ctx, NMR_WM_LINETIME(pipe), s->wm_linetime); o->wr(o->ctx, NMR_MSA_MISC(t), s->msa);
    for (i = 0; i < NMR_PLANE_WM_WORDS; i++) o->wr(o->ctx, NMR_PLANE_WM_BASE(pipe) + 4 * i, s->plane_wm[i]);
    for (i = 0; i < NMR_N_PLANE; i++) o->wr(o->ctx, NMR_PLANE(pipe, i), s->plane[i]);
    o->wr(o->ctx, NMR_SURF(pipe), s->surf);       /* the surface register arms the plane: last */
}
/* from "the buffer is running" to the backlight: link training, idle and normal pattern, transcoder, backlight */
static inline int nmr_st_tail(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    const unsigned pipe = s->topo.pipe, t = pipe, port = s->topo.port; unsigned i, lanes; uint32_t fc;
    lanes = s->dpcd[1] & 0x1F;
    if (lanes == 0 || lanes > 4) { o->wr(o->ctx, NMR_DDI_BUF_CTL(port), s->ddi & ~NMR_DDI_ENABLE); return NMR_E_TRAIN; }
    if (s->no_handshake) {
        nmr_note(o, "link training skipped (the panel does not use the AUX handshake)");
        if (nmr_dpcd_wr1(o, port, NMR_DPCD_LINK_BW, s->dpcd[0]) != 1 || nmr_dpcd_wr1(o, port, NMR_DPCD_LANE_COUNT, s->dpcd[1]) != 1) return NMR_E_SINK;
        o->delay_us(o->ctx, 1000);
        o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_PAT2);
        o->delay_us(o->ctx, 1000);
    } else {
        nmr_note(o, "link training");
        if (!nmr_train(o, s, lanes, display_version)) {
            o->wr(o->ctx, NMR_DDI_BUF_CTL(port), s->ddi & ~NMR_DDI_ENABLE);
            o->wr(o->ctx, NMR_DP_TP_CTL(t), s->tp & ~NMR_TP_ENABLE);
            return NMR_E_TRAIN;
        }
    }
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_IDLE);
    nmr_wait(o, NMR_DP_TP_STATUS(t), NMR_TP_IDLE_DONE, NMR_TP_IDLE_DONE, 5000);
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_NORMAL);
    nmr_note(o, "transcoder on");
    o->wr(o->ctx, NMR_TRANSCONF(pipe), s->transconf | NMR_TRANS_ENABLE);
    fc = o->rd(o->ctx, NMR_FRMCOUNT(pipe));
    for (i = 0; i < 200; i++) { if (o->rd(o->ctx, NMR_FRMCOUNT(pipe)) != fc) break; o->delay_us(o->ctx, 1000); }
    if (o->rd(o->ctx, NMR_FRMCOUNT(pipe)) == fc) return NMR_E_NO_SCANOUT;
    nmr_note(o, "backlight");
    o->wr(o->ctx, NMR_BLC_PWM_FREQ, s->blc[1]); o->wr(o->ctx, NMR_BLC_PWM_DUTY, s->blc[2]); o->wr(o->ctx, NMR_BLC_PWM_CTL, s->blc[0]);
    return NMR_OK;
}

static inline int nmr_resume(const struct nmr_ops *o, const struct nmr_snap *s, unsigned display_version)
{
    int rc;
    if (display_version != 12 && display_version != 13) return NMR_E_GEN;
    if (!s || !s->valid) return NMR_E_NO_SNAPSHOT;
    if (nmr_st_lit(o, s)) return NMR_ALREADY_LIT;                       /* a panel that is still running needs nothing */
    nmr_st_core_early(o, s, display_version);
    if ((rc = nmr_st_power(o, s))) return rc;
    if ((rc = nmr_st_cdclk(o, s, display_version))) return rc;
    if ((rc = nmr_st_core_late(o, s, display_version))) return rc;
    if ((rc = nmr_st_combo(o, s))) return rc;
    if ((rc = nmr_st_panel(o, s))) return rc;
    if ((rc = nmr_st_sink(o, s))) return rc;
    nmr_st_pipe(o, s);
    nmr_note(o, "transcoder function and DDI buffer");
    o->wr(o->ctx, NMR_FUNC_CTL(s->topo.pipe), s->func);
    o->wr(o->ctx, NMR_DP_TP_CTL(s->topo.pipe), (s->tp & ~NMR_TP_PAT_MASK) | NMR_TP_ENABLE | NMR_TP_PAT1);
    o->wr(o->ctx, NMR_DDI_BUF_CTL(s->topo.port), s->ddi | NMR_DDI_ENABLE);
    o->delay_us(o->ctx, 600);                      /* buffer enable to first pattern */
    return nmr_st_tail(o, s, display_version);
}

/* nmhybridresume=1 */
static inline int nmr_enabled(int present, long long value) { return present && value == 1; }

#endif

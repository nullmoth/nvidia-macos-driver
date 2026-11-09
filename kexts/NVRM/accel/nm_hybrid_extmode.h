/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Lighting an external display on a Type-C port (display versions 12 and 13), start to finish, as a sequence that can be stopped at any
 * step and undone in reverse: hot-plug, PHY ownership, power wells, the sink's capabilities and EDID, the mode and the link, the display-buffer
 * and watermark fit, the Dekel PLL and lanes, the clocks, the pipe and plane, the transcoder and DDI buffer, link training, the transcoder.
 * Every step asks whether the cable is still there; an unplug at any point (including in the middle of link training, where it shows
 * as AUX failures) takes the teardown path for exactly the steps that were done.
 *
 * Built from the pieces in nm_hybrid_tc.h, nm_hybrid_ext.h, nm_hybrid_dkl.h, nm_hybrid_wm.h and the training loop of nm_hybrid_resume.h. The order is
 * Linux's i915 (intel_ddi.c tgl_ddi_pre_enable_dp / intel_ddi_post_disable, intel_dp_link_training). The backlight and the other
 * panel registers are never touched. Plain C: tools/test_hybrid_extmode.c.
 */
#ifndef NM_HYBRID_EXTMODE_H
#define NM_HYBRID_EXTMODE_H

#include <string.h>
#include "nm_hybrid_tc.h"
#include "nm_hybrid_ext.h"
#include "nm_hybrid_dkl.h"
#include "nm_hybrid_wm.h"

enum { NMO_IDLE = 0, NMO_UP, NMO_FAILED };
enum { NMO_OK = 0, NMO_E_PHY, NMO_E_POWER, NMO_E_SINK, NMO_E_EDID, NMO_E_MODE, NMO_E_PLL, NMO_E_TRAIN, NMO_E_UNPLUGGED, NMO_E_SCANOUT, NMO_E_BUSY };
enum { NMO_D_PHY = 1, NMO_D_POWER = 2, NMO_D_PLL = 4, NMO_D_CLOCK = 8, NMO_D_TRANSCLK = 16, NMO_D_PIPE = 32, NMO_D_DDI = 64, NMO_D_SINK = 128, NMO_D_TRANSCONF = 256 };

#define NMO_PW_AUX_DRIVER 0x45444u    /* ICL_PWR_WELL_CTL_AUX2 */
#define NMO_PW_DDI_DRIVER 0x45454u    /* ICL_PWR_WELL_CTL_DDI2 */

struct nmo {
    struct nmt_port tc;
    uint8_t  ver, pipe, ddi, state, err, refclk_set;
    uint32_t refclk_khz;
    uint32_t done;                        /* NMO_D_* bits: what teardown must undo */
    struct nme_timing t; struct nme_caps c; uint32_t rate; unsigned lanes;
    struct nmw_level wm[NMW_LEVELS]; unsigned ddb_start, ddb_end;
    uint32_t surf, pitch;                 /* the scan-out surface: a graphics-table address and its pitch */
    struct nmr_snap s;                    /* the context link training works in (only the link fields are used) */
};
static inline const char *nmo_err_name(int e)
{
    static const char *n[] = { "ok", "the PHY could not be taken", "a power well did not come up", "the sink did not answer", "no usable EDID", "the mode does not fit the link or the display buffer",
                               "the Dekel PLL did not come up", "link training failed", "the cable was unplugged", "the pipe is not scanning out", "already running" };
    return e >= 0 && e <= NMO_E_BUSY ? n[e] : "?";
}

/* ---- helpers */
static inline int nmo_alive(const struct nmr_ops *o, struct nmo *m)
{
    return nmt_live(o, &m->tc) != 0;          /* any cable at all; a Thunderbolt one is turned away at the PHY step */
}
static inline int nmo_power(const struct nmr_ops *o, struct nmo *m, int on)
{
    const unsigned idx = 3u + m->tc.tc;
    const uint32_t req = 1u << (2 * idx), st = req << 1;
    if (on) {
        o->wr(o->ctx, NMO_PW_DDI_DRIVER, o->rd(o->ctx, NMO_PW_DDI_DRIVER) | req);
        o->wr(o->ctx, NMO_PW_AUX_DRIVER, o->rd(o->ctx, NMO_PW_AUX_DRIVER) | req);
        if (!nmr_wait(o, NMO_PW_DDI_DRIVER, st, st, 20000) || !nmr_wait(o, NMO_PW_AUX_DRIVER, st, st, 20000)) return -1;
        return 0;                                   /* versions 12 and 13 power the AUX channel through its power well only */
    }
    o->wr(o->ctx, NMO_PW_AUX_DRIVER, o->rd(o->ctx, NMO_PW_AUX_DRIVER) & ~req);
    o->wr(o->ctx, NMO_PW_DDI_DRIVER, o->rd(o->ctx, NMO_PW_DDI_DRIVER) & ~req);
    return 0;
}
static inline int nmo_level_cb(const struct nmr_ops *o, const struct nmr_snap *s, unsigned v, unsigned p)
{
    return nmk_set_levels(o, s->cx0.ver, s->topo.port - 3u, (uint32_t)s->dpcd[0] * 27000u, v, p);
}

/* ---- undo, in reverse, exactly what was done. Safe to call at any time and more than once. */
static inline void nmo_teardown(const struct nmr_ops *o, struct nmo *m)
{
    const unsigned t = m->pipe, p = m->pipe;
    nmr_note(o, "external display: tearing down");
    if (m->done & NMO_D_TRANSCONF) o->wr(o->ctx, NMR_TRANSCONF(p), o->rd(o->ctx, NMR_TRANSCONF(p)) & ~NMR_TRANS_ENABLE);
    if (m->done & NMO_D_DDI) {
        o->wr(o->ctx, NMR_DDI_BUF_CTL(m->ddi), o->rd(o->ctx, NMR_DDI_BUF_CTL(m->ddi)) & ~NMR_DDI_ENABLE);
        o->wr(o->ctx, NMR_DP_TP_CTL(t), o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_ENABLE);
    }
    if (m->done & NMO_D_PIPE) {
        o->wr(o->ctx, NMR_FUNC_CTL(t), 0);
        o->wr(o->ctx, NMR_PLANE(p, 0), 0); o->wr(o->ctx, NMR_SURF(p), 0);
        { unsigned i; for (i = 0; i < NMW_LEVELS; i++) o->wr(o->ctx, NMR_PLANE_WM_BASE(p) + 4u * i, 0); }
        o->wr(o->ctx, NMR_PLANE_WM_BASE(p) + 0x3Cu, 0);                              /* PLANE_BUF_CFG */
    }
    if (m->done & NMO_D_SINK) { uint8_t d3 = 2; nmr_dpcd_wr(o, m->ddi, NMR_DPCD_SET_POWER, &d3, 1); }
    if (m->done & NMO_D_TRANSCLK) nmk_trans_clock(o, t, m->ddi, 0);
    if (m->done & NMO_D_CLOCK) nmk_clock_off(o, m->ddi, m->tc.tc);
    if (m->done & NMO_D_PLL) { const uint32_t en = NMK_PLL_ENABLE_REG(m->ver, m->tc.tc); o->wr(o->ctx, en, o->rd(o->ctx, en) & ~(NMK_PLL_ENABLE | NMK_PLL_PWR_ENABLE)); }
    if (m->done & NMO_D_POWER) nmo_power(o, m, 0);
    if (m->done & NMO_D_PHY) nmt_disconnect(o, &m->tc);
    m->done = 0;
    m->state = NMO_IDLE;
}
static inline int nmo_fail(const struct nmr_ops *o, struct nmo *m, int err)
{
    if (!nmo_alive(o, m)) err = NMO_E_UNPLUGGED;
    nmo_teardown(o, m);
    m->err = (uint8_t)err; m->state = NMO_FAILED;
    return err;
}

/* ---- bring it up. surf is the scan-out surface's graphics-table address and capacity its size in bytes (the pitch follows from the mode); ddb_start/ddb_end the display-buffer range this pipe may use (nmw_free_run). Returns 0 or NMO_E_*. */
static inline int nmo_init(const struct nmr_ops *o, struct nmo *m, unsigned ver, unsigned tc, unsigned pipe, unsigned refclk_khz)
{
    memset(m, 0, sizeof *m);
    if (nmt_init(o, &m->tc, ver, tc, 0)) return NMO_E_PHY;
    m->ver = (uint8_t)ver; m->pipe = (uint8_t)pipe; m->ddi = (uint8_t)(3 + tc); m->refclk_khz = refclk_khz;
    return NMO_OK;
}
static inline int nmo_enable(const struct nmr_ops *o, struct nmo *m, uint32_t surf, uint32_t capacity, unsigned ddb_start, unsigned ddb_end)
{
    uint32_t pitch = 0;
    struct nmr_ops o2 = *o;
    const unsigned p = m->pipe, t = m->pipe, ddi = m->ddi, tc = m->tc.tc;
    uint8_t dp[16], edid[128]; uint32_t regs[11]; struct nme_mn mn; struct nmw_in win; unsigned i;
    if (m->state == NMO_UP) return NMO_E_BUSY;
    m->done = 0; m->err = 0; m->surf = surf; m->ddb_start = ddb_start; m->ddb_end = ddb_end;

    nmr_note(o, "external display: PHY");
    if (!nmt_connect(o, &m->tc, 1)) return nmo_fail(o, m, NMO_E_PHY);
    m->done |= NMO_D_PHY;
    if (m->tc.mode != NMT_DPALT && m->tc.mode != NMT_LEGACY) return nmo_fail(o, m, NMO_E_PHY);        /* a Thunderbolt tunnel is another path */
    if (!nmo_alive(o, m)) return nmo_fail(o, m, NMO_E_UNPLUGGED);

    nmr_note(o, "external display: power");
    m->done |= NMO_D_POWER;
    if (nmo_power(o, m, 1)) return nmo_fail(o, m, NMO_E_POWER);

    nmr_note(o, "external display: sink");
    if (nmr_dpcd_rd(o, ddi, 0, dp, 16) != 16 || nme_caps(dp, &m->c)) return nmo_fail(o, m, NMO_E_SINK);
    if (nme_edid_read(o, ddi, edid) || nme_edid_preferred(edid, &m->t)) return nmo_fail(o, m, NMO_E_EDID);
    if (!nmo_alive(o, m)) return nmo_fail(o, m, NMO_E_UNPLUGGED);

    nmr_note(o, "external display: mode");
    pitch = ((uint32_t)m->t.hactive * 4u + 63u) & ~63u; m->pitch = pitch;                                // linear XRGB8888, rows on 64-byte units
    if ((uint64_t)pitch * m->t.vactive > capacity) return nmo_fail(o, m, NMO_E_MODE);                  // the buffer given is too small for the preferred mode
    if (nme_link(&m->c, &m->t, 24, &m->rate, &m->lanes)) return nmo_fail(o, m, NMO_E_MODE);
    if (nmt_max_lanes(o, &m->tc) < m->lanes) return nmo_fail(o, m, NMO_E_MODE);                       /* the cable gives fewer lanes than the link needs */
    memset(&win, 0, sizeof win); win.pixel_khz = m->t.clock_khz; win.hactive = m->t.hactive; win.htotal = (uint16_t)(m->t.hactive + m->t.hblank); win.ver = m->ver;
    if (nmw_read_latency(o, m->ver, win.latency)) return nmo_fail(o, m, NMO_E_MODE);
    nmw_compute(&win, m->wm);
    if (nmw_fit(m->wm, ddb_end - ddb_start)) return nmo_fail(o, m, NMO_E_MODE);                       /* level 0 does not fit: the pipe would underrun */
    nme_mn(m->t.clock_khz, 24, m->lanes, m->rate, &mn);
    nme_trans_regs(&m->t, &mn, regs);

    nmr_note(o, "external display: PLL");
    { struct nmk_pll pll;
      if (nmk_calc(m->refclk_khz, m->rate, &pll)) return nmo_fail(o, m, NMO_E_PLL);
      m->done |= NMO_D_PLL;
      if (nmk_enable(o, m->ver, tc, &pll)) return nmo_fail(o, m, NMO_E_PLL); }
    nmk_lane_unreset(o, tc);
    nmk_clock_on(o, ddi, tc); m->done |= NMO_D_CLOCK;
    nmk_trans_clock(o, t, ddi, 1); m->done |= NMO_D_TRANSCLK;
    if (!nmo_alive(o, m)) return nmo_fail(o, m, NMO_E_UNPLUGGED);

    nmr_note(o, "external display: pipe and plane");
    m->done |= NMO_D_PIPE;
    for (i = 0; i < NMR_N_TIMING; i++) o->wr(o->ctx, NMR_TIMING(t, i), regs[i]);
    o->wr(o->ctx, NMR_MSA_MISC(t), NME_MSA_MISC_8BPC_SYNC);
    o->wr(o->ctx, NMR_PIPE_MISC(p), 0);
    nmw_program(o, p, &win, m->wm, ddb_start, ddb_end);
    o->wr(o->ctx, NMR_PLANE(p, 1), pitch / 64u);
    o->wr(o->ctx, NMR_PLANE(p, 2), 0);
    o->wr(o->ctx, NMR_PLANE(p, 3), ((uint32_t)(m->t.vactive - 1u) << 16) | (m->t.hactive - 1u));
    o->wr(o->ctx, NMR_PLANE(p, 0), (1u << 31) | (4u << 24) | (1u << 13));                                 /* enabled, XRGB8888, linear, pipe gamma off */
    o->wr(o->ctx, NMR_SURF(p), surf);                                                                 /* arms the plane: last */

    nmr_note(o, "external display: transcoder function and DDI buffer");
    o->wr(o->ctx, NMR_FUNC_CTL(t), nme_func_ctl(ddi, &m->t, m->lanes));
    o->wr(o->ctx, NMR_DP_TP_CTL(t), NMR_TP_ENABLE | NMR_TP_PAT1);
    { uint8_t d0 = 1; if (nmr_dpcd_wr(o, ddi, NMR_DPCD_SET_POWER, &d0, 1) != 1) return nmo_fail(o, m, NMO_E_SINK); m->done |= NMO_D_SINK; }
    nmk_set_levels(o, m->ver, tc, m->rate, 0, 0);
    m->done |= NMO_D_DDI;
    o->wr(o->ctx, NMR_DDI_BUF_CTL(ddi), NMR_DDI_ENABLE | ((m->lanes - 1u) << 1) | (m->ver == 13 ? NMT_DDI_OWNER : 0u));
    if (!nmo_alive(o, m)) return nmo_fail(o, m, NMO_E_UNPLUGGED);

    nmr_note(o, "external display: link training");
    memset(&m->s, 0, sizeof m->s);
    m->s.valid = 1; m->s.topo.port = (uint8_t)ddi; m->s.topo.pipe = (uint8_t)p;
    m->s.dpcd[0] = (uint8_t)(m->rate / 27000u); m->s.dpcd[1] = (uint8_t)(m->lanes | (m->c.enhanced ? 0x80u : 0u)); m->s.dpcd[2] = 0; m->s.dpcd[3] = 1; m->s.dpcd[7] = dp[0x0E];
    m->s.cx0.ver = m->ver;
    o2.apply_level = nmo_level_cb;
    if (!nmr_train(&o2, &m->s, m->lanes, m->ver)) return nmo_fail(o, m, NMO_E_TRAIN);
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_IDLE);
    nmr_wait(o, NMR_DP_TP_STATUS(t), NMR_TP_IDLE_DONE, NMR_TP_IDLE_DONE, 5000);
    o->wr(o->ctx, NMR_DP_TP_CTL(t), (o->rd(o->ctx, NMR_DP_TP_CTL(t)) & ~NMR_TP_PAT_MASK) | NMR_TP_NORMAL);
    nmr_note(o, "external display: transcoder on");
    o->wr(o->ctx, NMR_TRANSCONF(p), NMR_TRANS_ENABLE);
    m->done |= NMO_D_TRANSCONF;
    { uint32_t fc = o->rd(o->ctx, NMR_FRMCOUNT(p));
      for (i = 0; i < 200; i++) { if (o->rd(o->ctx, NMR_FRMCOUNT(p)) != fc) break; o->delay_us(o->ctx, 1000); }
      if (o->rd(o->ctx, NMR_FRMCOUNT(p)) == fc) return nmo_fail(o, m, NMO_E_SCANOUT); }
    m->state = NMO_UP;
    return NMO_OK;
}
/* Hot-plug: call with the result of nmt_poll. A plug brings the display up; an unplug takes it down. Returns the NMO_E_* of the attempt, or 0. */
static inline int nmo_event(const struct nmr_ops *o, struct nmo *m, int ev, uint32_t surf, uint32_t capacity, unsigned ddb_start, unsigned ddb_end)
{
    if ((ev == 1 || ev == 3) && m->state != NMO_UP) return nmo_enable(o, m, surf, capacity, ddb_start, ddb_end);
    if (ev == 2 && (m->state == NMO_UP || m->done)) { nmo_teardown(o, m); return 0; }
    return 0;
}

#endif

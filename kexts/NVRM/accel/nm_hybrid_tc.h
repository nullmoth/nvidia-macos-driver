/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Type-C ports on Intel display versions 12 (Tiger Lake) and 13 (Alder Lake P): live status (hot-plug), the mode of the port, and
 * the PHY ownership handshake that hands the port's lanes from the Type-C subsystem to the display engine and back.
 *
 * A Type-C port is not usable the moment a cable is seen: its PHY is shared with USB and Thunderbolt, owned by the Type-C
 * subsystem's firmware until display asks for it. The register sequences are those of Linux's i915 (intel_tc.c
 * icl/tgl/adlp phy ops, intel_display_power_well.c tgl_tc_cold_request, i915_reg.h; MIT):
 *   version 12: block the TC-cold power state with the platform controller, check the PHY-ready flag in the FIA, set the
 *               "not safe" flag (display owns the PHY) in the FIA, then check the cable's mode is still there and has the lanes;
 *   version 13: set the PHY-ownership bit in the DDI buffer control register, check the ready flag in the TCSS status register,
 *               then block TC-cold, and check the mode and the lanes.
 * Disconnect clears the ownership and releases the TC-cold block.
 *
 * This is the connect and hot-plug half of an external Type-C display. It does not by itself light one: that also needs the
 * Type-C PHY's PLL and lane programming for the mode being set (the DKL PHY on these versions), EDID over the port's AUX channel,
 * a mode set for a second head and a second head published to the window system. NMIntelFB drives the internal panel only, so none of
 * that exists yet; docs/HYBRID-LAPTOPS.md lists exactly what is missing. Plain C: tools/test_hybrid_tc.c runs this against a model.
 */
#ifndef NM_HYBRID_TC_H
#define NM_HYBRID_TC_H

#include "nm_hybrid_resume.h"

enum { NMT_DISCONNECTED = 0, NMT_TBT = 1, NMT_DPALT = 2, NMT_LEGACY = 3 };

#define NMT_FIA_BASE(f)      ((f) == 0 ? 0x163000u : ((f) == 1 ? 0x16E000u : 0x16F000u))
#define NMT_DFLEXDPSP(f)     (NMT_FIA_BASE(f) + 0x8A0u)      /* live state, lane assignment, modular-FIA flag */
#define NMT_DFLEXDPPMS(f)    (NMT_FIA_BASE(f) + 0x890u)      /* PHY mode status: ready */
#define NMT_DFLEXDPCSSS(f)   (NMT_FIA_BASE(f) + 0x894u)      /* PHY mode status: not safe (display owns the PHY) */
#define NMT_MODULAR_FIA      (1u << 4)
#define NMT_LIVE_TBT(i)      (1u << ((i) * 8 + 6))
#define NMT_LIVE_TC(i)       (1u << ((i) * 8 + 5))
#define NMT_LANES(v, i)      (((v) >> ((i) * 8)) & 0xFu)
#define NMT_PMS_READY(i)     (1u << (i))
#define NMT_CSSS_NOT_SAFE(i) (1u << (i))
#define NMT_TCSS_STATUS(tc)  (0x161500u + 4u * (tc))         /* version 13 */
#define NMT_TCSS_READY       (1u << 2)
#define NMT_DE_HPD_ISR       0x44470u                        /* version 13: DisplayPort-alt (bit 16 + tc) and Thunderbolt (bit tc) live state */
#define NMT_SDEISR           0xC4000u                        /* legacy ports: bit 24 + tc */
#define NMT_DDI_OWNER        (1u << 6)                       /* version 13: DDI_BUF_CTL, display owns the PHY */
#define NMT_PCODE_TCCOLD     0x26u

struct nmt_port {
    uint8_t ver;          /* 12 or 13 */
    uint8_t tc;           /* Type-C port index 0..5 (the n-th Type-C port, not the DDI letter) */
    uint8_t legacy;       /* the video BIOS says this port is a legacy (always-display) Type-C port */
    uint8_t ddi;          /* DDI index for DDI_BUF_CTL: 3 + tc */
    uint8_t fia, fia_idx; /* which FIA instance and which of its two ports */
    uint8_t mode;         /* NMT_* the port is in while connected */
    uint8_t cold;         /* the TC-cold block is held */
};

/* TC-cold power state block or unblock through the platform controller (tgl_tc_cold_request): three tries; a block that answers "exit failed" is a failure. */
static inline int nmt_cold(const struct nmr_ops *o, struct nmt_port *p, int block)
{
    unsigned tries;
    for (tries = 0; tries < 3; tries++) {
        uint32_t v = block ? 0u : 1u;
        if (nmr_pcode(o, NMT_PCODE_TCCOLD, &v, 1) == 0 && !(block && (v & 1u))) { p->cold = (uint8_t)block; return 0; }
        o->delay_us(o->ctx, 1000);
    }
    return -1;
}

/* The FIA instance and index (tgl_tc_phy_init, adlp_tc_phy_init). Version 12 asks the hardware whether the FIA is modular. */
static inline int nmt_init(const struct nmr_ops *o, struct nmt_port *p, unsigned ver, unsigned tc, int legacy)
{
    int modular = 1;
    if (ver != 12 && ver != 13) return -1;
    p->ver = (uint8_t)ver; p->tc = (uint8_t)tc; p->legacy = (uint8_t)(legacy != 0); p->ddi = (uint8_t)(3 + tc); p->mode = NMT_DISCONNECTED; p->cold = 0;
    if (ver == 12) {
        uint32_t v;
        if (nmt_cold(o, p, 1)) return -1;
        v = o->rd(o->ctx, NMT_DFLEXDPSP(0));
        nmt_cold(o, p, 0);
        if (v == 0xFFFFFFFFu) return -1;
        modular = (v & NMT_MODULAR_FIA) != 0;
    }
    if (modular) { p->fia = (uint8_t)(tc / 2); p->fia_idx = (uint8_t)(tc % 2); } else { p->fia = 0; p->fia_idx = (uint8_t)tc; }
    return 0;
}

/* The live status of the cable as a mask of (1 << mode): Thunderbolt, DisplayPort-alt, legacy (hpd_live_status). 0 when nothing is connected. */
static inline uint32_t nmt_live(const struct nmr_ops *o, struct nmt_port *p)
{
    uint32_t mask = 0, a, b;
    if (p->ver == 12) {
        int own = !p->cold;
        if (own && nmt_cold(o, p, 1)) return 0;
        a = o->rd(o->ctx, NMT_DFLEXDPSP(p->fia)); b = o->rd(o->ctx, NMT_SDEISR);
        if (own) nmt_cold(o, p, 0);
        if (a == 0xFFFFFFFFu) return 0;                 /* PHY in TC-cold: nothing connected */
        if (a & NMT_LIVE_TBT(p->fia_idx)) mask |= 1u << NMT_TBT;
        if (a & NMT_LIVE_TC(p->fia_idx)) mask |= 1u << NMT_DPALT;
        if (b & (1u << (24 + p->tc))) mask |= 1u << NMT_LEGACY;
    } else {
        a = o->rd(o->ctx, NMT_DE_HPD_ISR); b = o->rd(o->ctx, NMT_SDEISR);
        if (a & (1u << (16 + p->tc))) mask |= 1u << NMT_DPALT;
        if (a & (1u << p->tc)) mask |= 1u << NMT_TBT;
        if (b & (1u << (24 + p->tc))) mask |= 1u << NMT_LEGACY;
    }
    return mask;
}
static inline unsigned nmt_mask_to_mode(uint32_t mask) { unsigned m = 0; while (mask >> (m + 1)) m++; return mask ? m : (unsigned)NMT_DISCONNECTED; }

static inline int nmt_ready(const struct nmr_ops *o, const struct nmt_port *p)
{
    uint32_t v;
    if (p->ver == 12) { v = o->rd(o->ctx, NMT_DFLEXDPPMS(p->fia)); return v != 0xFFFFFFFFu && (v & NMT_PMS_READY(p->fia_idx)); }
    v = o->rd(o->ctx, NMT_TCSS_STATUS(p->tc));
    return v != 0xFFFFFFFFu && (v & NMT_TCSS_READY);
}
static inline int nmt_owned(const struct nmr_ops *o, const struct nmt_port *p)
{
    uint32_t v;
    if (p->ver == 12) { v = o->rd(o->ctx, NMT_DFLEXDPCSSS(p->fia)); return v != 0xFFFFFFFFu && (v & NMT_CSSS_NOT_SAFE(p->fia_idx)); }
    v = o->rd(o->ctx, NMR_DDI_BUF_CTL(p->ddi));
    return v != 0xFFFFFFFFu && (v & NMT_DDI_OWNER);
}
/* 0 on success; -1 when the register reads as all ones (the PHY or the port is powered down). */
static inline int nmt_take(const struct nmr_ops *o, const struct nmt_port *p, int take)
{
    uint32_t v;
    if (p->ver == 12) {
        v = o->rd(o->ctx, NMT_DFLEXDPCSSS(p->fia));
        if (v == 0xFFFFFFFFu) return -1;
        v &= ~NMT_CSSS_NOT_SAFE(p->fia_idx); if (take) v |= NMT_CSSS_NOT_SAFE(p->fia_idx);
        o->wr(o->ctx, NMT_DFLEXDPCSSS(p->fia), v);
        return 0;
    }
    v = o->rd(o->ctx, NMR_DDI_BUF_CTL(p->ddi));
    if (v == 0xFFFFFFFFu) return -1;
    v &= ~NMT_DDI_OWNER; if (take) v |= NMT_DDI_OWNER;
    o->wr(o->ctx, NMR_DDI_BUF_CTL(p->ddi), v);
    return 0;
}

/* The lanes the cable gives display in DisplayPort-alt mode (intel_tc_port_get_max_lane_count); 4 in the other modes. */
static inline unsigned nmt_max_lanes(const struct nmr_ops *o, const struct nmt_port *p)
{
    uint32_t v;
    if (p->mode != NMT_DPALT) return 4;
    v = NMT_LANES(o->rd(o->ctx, NMT_DFLEXDPSP(p->fia)), p->fia_idx);
    switch (v) { case 3: case 0xC: return 2; case 0xF: return 4; default: return 1; }
}

/* intel_tc.c tc_phy_verify_legacy_or_dp_alt_mode: a DisplayPort-alt cable must still be there and have the lanes asked for. */
static inline int nmt_verify(const struct nmr_ops *o, struct nmt_port *p, unsigned required_lanes)
{
    if (p->mode == NMT_LEGACY) return 1;
    if (!(nmt_live(o, p) & (1u << NMT_DPALT))) return 0;               /* sudden disconnect */
    return nmt_max_lanes(o, p) >= required_lanes;
}

/* One connect attempt in the given mode. Returns 1 when the port is connected. */
static inline int nmt_connect_one(const struct nmr_ops *o, struct nmt_port *p, unsigned required_lanes)
{
    if (p->ver == 12) {
        if (nmt_cold(o, p, 1)) return 0;
        if (p->mode == NMT_TBT) return 1;                               /* Thunderbolt owns the PHY: nothing to take */
        if ((!nmt_ready(o, p) || nmt_take(o, p, 1)) && p->mode != NMT_LEGACY) goto out_unblock;
        if (!nmt_verify(o, p, required_lanes)) { nmt_take(o, p, 0); goto out_unblock; }
        return 1;
out_unblock:
        nmt_cold(o, p, 0);
        return 0;
    }
    if (p->mode == NMT_TBT) return nmt_cold(o, p, 1) == 0;
    if (nmt_take(o, p, 1) && p->mode != NMT_LEGACY) return 0;
    if (!nmt_ready(o, p) && p->mode != NMT_LEGACY) { nmt_take(o, p, 0); return 0; }
    if (nmt_cold(o, p, 1)) { nmt_take(o, p, 0); return 0; }
    if (!nmt_verify(o, p, required_lanes)) { nmt_cold(o, p, 0); nmt_take(o, p, 0); return 0; }
    return 1;
}
static inline unsigned nmt_default_mode(const struct nmt_port *p) { return p->legacy ? NMT_LEGACY : NMT_TBT; }

/* tc_phy_connect: trust the live status over the video BIOS legacy flag when they disagree, pick the target mode from it, try, and fall back
 * to the default mode (Thunderbolt-alt for a non-legacy port) if the target does not connect. Returns 1 when connected. */
static inline int nmt_connect(const struct nmr_ops *o, struct nmt_port *p, unsigned required_lanes)
{
    uint32_t live = nmt_live(o, p);
    unsigned target = nmt_mask_to_mode(live);
    int ok;
    if (live && (live & (live - 1)) == 0) {                             /* exactly one live bit: fix the legacy flag if it contradicts */
        const uint32_t valid = p->legacy ? (1u << NMT_LEGACY) : ((1u << NMT_DPALT) | (1u << NMT_TBT));
        if (live & ~valid) p->legacy = !p->legacy;
    }
    p->mode = (uint8_t)(target != NMT_DISCONNECTED ? target : nmt_default_mode(p));
    ok = nmt_connect_one(o, p, required_lanes);
    if (!ok && p->mode != nmt_default_mode(p)) { p->mode = (uint8_t)nmt_default_mode(p); ok = nmt_connect_one(o, p, required_lanes); }
    if (!ok) p->mode = NMT_DISCONNECTED;
    return ok;
}
static inline void nmt_disconnect(const struct nmr_ops *o, struct nmt_port *p)
{
    if (p->mode == NMT_DISCONNECTED) return;
    if (p->ver == 12) {
        if (p->mode == NMT_LEGACY || p->mode == NMT_DPALT) nmt_take(o, p, 0);
        nmt_cold(o, p, 0);
    } else {
        nmt_cold(o, p, 0);
        if (p->mode == NMT_LEGACY || p->mode == NMT_DPALT) nmt_take(o, p, 0);
    }
    p->mode = NMT_DISCONNECTED;
}
/* Hot-plug: compare the live status with the last one. Returns 1 for a plug, 2 for an unplug, 3 for a change of cable type, 0 for no change. */
static inline int nmt_poll(const struct nmr_ops *o, struct nmt_port *p, uint32_t *last)
{
    uint32_t now = nmt_live(o, p); int ev = 0;
    if (now != *last) ev = !*last ? 1 : (!now ? 2 : 3);
    *last = now;
    return ev;
}

#endif

/* S3 resume on display versions 20 and 30 (Lunar Lake, Panther Lake) and on the C20 PHY (kexts/NVRM/accel/nm_hybrid_resume14.h,
 * nm_hybrid_c20tables.h), against a model of the hardware built from Linux's i915 display code (intel_cx0_phy.c, intel_cx0_phy_regs.h,
 * intel_ddi.c, intel_cdclk.c, intel_display_power.c; MIT).
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_resume20.c -o /tmp/t && /tmp/t     -> 0 failed
 * Three layouts run through the same suite: display 20 with a C10 port (the Lunar Lake eDP case), display 30 with a C10 port A, and
 * display 30 with a C20 port B. It checks the ORDER, the handshakes, the register block addresses of the second port range, the
 * C20 context switch and that the PHY ends with the register file it started with; it cannot show that a panel lights. */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "nm_hybrid_resume14.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static unsigned T_PIPE = 0, T_PORT = 0, VER = 20, IS_C20 = 0;
#define XP nmx_port_arg(VER, T_PORT)
static uint32_t reg[NREG];
static uint8_t phy[2][4096];          /* the C10 register file (or the C20 vendor registers), one per lane */
static uint16_t sram[2][65536];       /* the C20 PHY's own SRAM */
static uint16_t sram_want[65536];
static uint8_t dpcd[0x800];
static struct { uint32_t off, v; } wlog[20000]; static int nw;
static unsigned long now_us;
static int dbuf_fail, sram_writes, bus_resets;
static int pw_fail, bus_fail, pll_ack_fail, sink_deaf, soc_fail, d2d_fail, idle_fail, pmd_fail, train_fail, sink_needs_v, sink_needs_p;
static int bus_errs_left;             /* inject N bus errors then recover */
static int nsteps; static const char *steps[40];
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

/* the training level the PHY currently carries on lane 0 */
static void phy_level(unsigned *v, unsigned *p)
{
    unsigned vv, pp; *v = *p = 0;
    for (vv = 0; vv < 4; vv++) for (pp = 0; pp + vv < 4; pp++) {
        int idx = nmr_level_index(vv, pp);
        if (idx <= 0) continue;
        if (phy[0][NMX_C10_VDROVRD(0, 0, 0)] == (IS_C20 ? nmx_c20_dp14 : nmx_dp14)[idx].pre && phy[0][NMX_C10_VDROVRD(0, 0, 1)] == (IS_C20 ? nmx_c20_dp14 : nmx_dp14)[idx].vswing && phy[0][NMX_C10_VDROVRD(0, 0, 2)] == (IS_C20 ? nmx_c20_dp14 : nmx_dp14)[idx].post) { *v = vv; *p = pp; }
    }
}

static void aux_process(void)
{
    const uint32_t CTL = NMR_AUX_CTL(T_PORT), DATA = NMR_AUX_DATA(T_PORT);
    uint8_t m[20] = {0}; unsigned i, send = (*R(CTL) >> 20) & 0x1F;
    uint32_t ctl = *R(CTL) & ~((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));
    for (i = 0; i < send; i += 4) { uint32_t w = *R(DATA + i); for (unsigned j = 0; j < 4 && i + j < send; j++) m[i + j] = (uint8_t)(w >> (24 - 8 * j)); }
    if (!(*R(CTL) & NMX_AUX_POWER_REQ) || !(*R(NMR_PW_AUX) & (2u << (2 * T_PORT))) || sink_deaf || (*R(NMR_PP_STATUS) & NMR_PP_ON_MASK) != NMR_PP_ON_VALUE) {
        *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }
    unsigned cmd = m[0] >> 4, len = m[3] + 1; uint32_t addr = ((m[0] & 0xF) << 16) | (m[1] << 8) | m[2];
    uint8_t rep[20] = {0};
    if (cmd == 8) { for (i = 0; i < len; i++) dpcd[addr + i] = m[4 + i]; *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (1u << 20); *R(DATA) = 0; return; }
    for (i = 0; i < len; i++) {
        uint32_t a = addr + i; uint8_t v = dpcd[a];
        if (a == 0x202 || a == 0x203 || a == 0x204) {
            unsigned pat = dpcd[0x102] & 3, srcpat = (*R(NMR_DP_TP_CTL(T_PIPE)) >> 8) & 7, dv, dp; int cr, eq;
            phy_level(&dv, &dp);
            cr = pat >= 1 && srcpat == (pat == 1 ? 0u : 1u) && !train_fail && dv >= (unsigned)sink_needs_v && (dpcd[0x103] & 3) == dv && (*R(NMR_DDI_BUF_CTL(T_PORT)) & NMR_DDI_ENABLE);
            eq = cr && pat == 2 && dp >= (unsigned)sink_needs_p;
            if (a == 0x202 || a == 0x203) v = (uint8_t)((cr ? 1 : 0) | (eq ? 6 : 0) | (cr ? 0x10 : 0) | (eq ? 0x60 : 0));
            if (a == 0x204) v = (uint8_t)(eq ? 1 : 0);
        }
        if (a == 0x206 || a == 0x207) { uint8_t l = (uint8_t)((sink_needs_v & 3) | ((sink_needs_p & 3) << 2)); v = (uint8_t)(l | (l << 4)); }
        rep[1 + i] = v;
    }
    *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | ((len + 1) << 20);
    for (i = 0; i < len + 1; i += 4) *R(DATA + i) = ((uint32_t)rep[i] << 24) | ((uint32_t)rep[i + 1] << 16) | ((uint32_t)rep[i + 2] << 8) | rep[i + 3];
}

static int phy_powered(unsigned lane)   /* the PHY answers on the bus when its lane is out of reset (status bit clear) and its reference clock is acknowledged */
{
    (void)lane;
    return !(*R(NMX_BUF_CTL2(XP)) & (lane ? (1u << 28) : (1u << 29))) && (*R(NMX_CLOCK_CTL(XP)) & nmx_refclk_ack(0));
}
static void msgbus(unsigned lane, uint32_t v)
{
    unsigned addr = v & 0xFFF, cmd = (v >> 27) & 0xF, data = (v >> 16) & 0xFF;
    uint32_t st = 0;
    if (v & NMX_M2P_RESET) { *R(NMX_M2P_CTL(XP, lane)) = 0; bus_resets++; return; }
    if (bus_fail || !phy_powered(lane) || bus_errs_left > 0) {
        if (bus_errs_left > 0) bus_errs_left--;
        *R(NMX_M2P_CTL(XP, lane)) = v & ~NMX_M2P_PENDING; *R(NMX_P2M_STAT(XP, lane)) = NMX_P2M_READY | NMX_P2M_ERROR; return; }
    if (cmd == 3) { st = NMX_P2M_READY | (NMX_P2M_READ_ACK << 27) | ((uint32_t)phy[lane][addr] << 16); }
    else {
        phy[lane][addr] = (uint8_t)data; st = (cmd == 2) ? (NMX_P2M_READY | (NMX_P2M_WRITE_ACK << 27)) : 0;
        if (IS_C20 && addr == NMC20_WR_DATA_L) { unsigned a = (phy[lane][NMC20_WR_ADDR_H] << 8) | phy[lane][NMC20_WR_ADDR_L]; sram[lane][a] = (uint16_t)((phy[lane][NMC20_WR_DATA_H] << 8) | data); sram_writes++; }
        if (IS_C20 && addr == NMC20_RD_ADDR_L) { unsigned a = (phy[lane][NMC20_RD_ADDR_H] << 8) | data; phy[lane][NMC20_RD_DATA_H] = (uint8_t)(sram[lane][a] >> 8); phy[lane][NMC20_RD_DATA_L] = (uint8_t)sram[lane][a]; }
    }
    *R(NMX_M2P_CTL(XP, lane)) = v & ~NMX_M2P_PENDING;
    *R(NMX_P2M_STAT(XP, lane)) = st;
}
static uint32_t m_rd(void *c, uint32_t o)
{
    (void)c;
    if (o == NMR_FRMCOUNT(T_PIPE) && (*R(NMR_TRANSCONF(T_PIPE)) & NMR_TRANS_ENABLE) && (*R(NMR_DDI_BUF_CTL(T_PORT)) & NMR_DDI_ENABLE)) (*R(o))++;
    if (o == NMX_BUF_CTL1(XP) && !soc_fail) return *R(o) | (1u << 24);       /* the SoC side reports the PHY ready once the port is powered */
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    unsigned p = T_PORT; (void)c;
    if (nw < 20000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    if (o == NMR_AUX_CTL(p)) {
        if (v & (1u << 31)) { *R(o) = v; aux_process(); return; }
        *R(o) &= ~(v & ((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25)));
        *R(o) = (*R(o) & ~(NMX_AUX_POWER_REQ | NMX_AUX_POWER_STAT)) | (v & NMX_AUX_POWER_REQ) | ((v & NMX_AUX_POWER_REQ) && (*R(NMR_PW_AUX) & (2u << (2 * p))) ? NMX_AUX_POWER_STAT : 0);
        return; }
    for (unsigned l = 0; l < 2; l++) {
        if (o == NMX_M2P_CTL(XP, l)) { if (v & (NMX_M2P_PENDING | NMX_M2P_RESET)) msgbus(l, v); else *R(o) = v; return; }
        if (o == NMX_P2M_STAT(XP, l)) { *R(o) &= ~(v & (NMX_P2M_READY | NMX_P2M_ERROR)); return; }
    }
    if (o == NMX_BUF_CTL1(XP)) {
        uint32_t nv = v & ~((1u << 28) | (1u << 24) | (1u << 7));
        nv |= (*R(o) & ((1u << 28) | (1u << 24) | (1u << 7)));
        if (VER < 20 && (v & (1u << 29)) && !d2d_fail) nv |= (1u << 28);
        *R(o) = nv; return; }
    if (o == NMX_BUF_CTL2(XP)) {
        uint32_t cur = *R(o), nv = v;
        /* lane reset: pipe reset set -> phy status; pipe reset cleared (after power state reset) -> status cleared */
        if (!soc_fail) { if (v & (1u << 31)) nv |= (1u << 29); if (v & (1u << 30)) nv |= (1u << 28); }
        if (!(v & (1u << 31)) && (cur & (1u << 31))) nv &= ~(1u << 29);
        if (!(v & (1u << 30)) && (cur & (1u << 30))) nv &= ~(1u << 28);
        /* a powerdown update completes: the update bits read back as zero */
        nv &= ~((1u << 25) | (1u << 24));
        *R(o) = nv; return; }
    if (o == NMX_CLOCK_CTL(XP)) {
        uint32_t nv = v & ~((1u << 30) | (1u << 28) | (1u << 26) | (1u << 24));
        for (unsigned l = 0; l < 2; l++) {
            if (v & (1u << (29 - 4 * l))) nv |= (1u << (28 - 4 * l));                       /* refclk request -> ack */
            if ((v & (1u << (31 - 4 * l))) && !pll_ack_fail && (IS_C20 ? (phy[0][NMC20_SERDES] & 0x40) : (phy[0][NMX_C10_CONTROL(1)] & NMX_CTRL_UPDATE))) nv |= (1u << (30 - 4 * l));    /* pll request -> ack once programmed */
        }
        *R(o) = nv; return; }
    if (o == NMR_DDI_BUF_CTL(p)) {
        const int d2d_up = VER >= 20 ? ((v & NMR_XE2_D2D_ENABLE) && !d2d_fail) : ((*R(NMX_BUF_CTL1(XP)) & (1u << 28)) != 0);
        *R(o) = VER >= 20 ? ((v & ~NMR_XE2_D2D_STATE) | (d2d_up ? NMR_XE2_D2D_STATE : 0)) : v;
        if (v & NMR_DDI_ENABLE) { if (!idle_fail && d2d_up) *R(NMX_BUF_CTL1(XP)) &= ~(1u << 7); else *R(NMX_BUF_CTL1(XP)) |= (1u << 7); }
        return; }
    *R(o) = v;
    if (o == NMR_PW_DRIVER || o == NMR_PW_AUX || o == NMR_PW_DDI) { if (!pw_fail) *R(o) = v | ((v & 0x55555555u) << 1); }
    for (unsigned k = 0; k < 4; k++) if (o == NMR_DBUF_CTL(k) && (v & NMR_DBUF_REQ) && !dbuf_fail) *R(o) |= NMR_DBUF_STATE;
    if (o == NMR_DE_PLL_ENABLE && (v & NMR_PLL_ENABLE)) *R(o) |= NMR_PLL_LOCK;
    if (o == NMX_PMDEMAND(1) && (v & NMX_PMD_REQ) && !pmd_fail) *R(o) &= ~NMX_PMD_REQ;
    if (o == NMR_PP_CONTROL && (v & 1)) *R(NMR_PP_STATUS) = NMR_PP_ON_VALUE;
    if (o == NMR_DP_TP_CTL(T_PIPE) && ((v >> 8) & 7) == 2) *R(NMR_DP_TP_STATUS(T_PIPE)) |= NMR_TP_IDLE_DONE;
}
static void m_delay(void *c, unsigned us) { (void)c; now_us += us; }
static void m_log(void *c, const char *s) { (void)c; if (nsteps < 40) steps[nsteps++] = s; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, 0, 0, 0 };

static void lit_state(void)
{
    const unsigned t = T_PIPE, p = T_PIPE, port = T_PORT;
    memset(reg, 0, sizeof reg); memset(dpcd, 0, sizeof dpcd); memset(phy, 0, sizeof phy);
    reg[NMR_DBUF_CTL(0) / 4] = NMR_DBUF_REQ | NMR_DBUF_STATE; reg[NMR_NDE_RSTWRN / 4] = 0x10u;
    reg[NMR_PW_DRIVER / 4] = 0x1F | 0x2A; reg[NMR_PW_AUX / 4] = 3u << (2 * port); reg[NMR_PW_DDI / 4] = 3u << (2 * port);
    reg[NMR_DE_PLL_ENABLE / 4] = NMR_PLL_ENABLE | NMR_PLL_LOCK | 0x23; reg[NMR_CDCLK_CTL / 4] = 0xBB8;
    reg[NMX_PMDEMAND(0) / 4] = 0x00A31204; reg[NMX_PMDEMAND(1) / 4] = 0x0004F1A0;
    reg[NMR_PP_CONTROL / 4] = 5; reg[NMR_PP_STATUS / 4] = NMR_PP_ON_VALUE; reg[NMR_PP_ON_DELAYS / 4] = 0x19001900;
    reg[NMR_PP_OFF_DELAYS / 4] = 0x01900190; reg[NMR_PP_DIVISOR / 4] = 0x0001E00A;
    for (unsigned i = 0; i < NMR_N_TIMING; i++) reg[NMR_TIMING(t, i) / 4] = 0x1000 + i * 7;
    reg[NMR_TRANS_CLK_SEL(t) / 4] = 1; reg[NMR_PIPE_MISC(p) / 4] = 0x40; reg[NMR_WM_LINETIME(p) / 4] = 0x1f4; reg[NMR_MSA_MISC(t) / 4] = 0x21;
    for (unsigned i = 0; i < NMR_N_PLANE; i++) reg[NMR_PLANE(p, i) / 4] = 0x8000u + i;
    for (unsigned i = 0; i < NMR_PLANE_WM_WORDS; i++) reg[(NMR_PLANE_WM_BASE(p) / 4) + i] = 0x80000000u + i;
    reg[NMR_SURF(p) / 4] = 0x00800000; reg[NMR_FUNC_CTL(t) / 4] = 0x80000000u | ((port + 1u) << NMR_TRANS_PORT_SHIFT);
    reg[NMR_DP_TP_CTL(t) / 4] = NMR_TP_ENABLE | NMR_TP_NORMAL; reg[NMR_DDI_BUF_CTL(port) / 4] = NMR_DDI_ENABLE | (VER >= 20 ? (NMR_XE2_D2D_ENABLE | NMR_XE2_D2D_STATE) : 0);
    if (VER >= 20) reg[NMR_MBUS_CTL / 4] = 0x80000000u | (2u << 13);
    reg[NMR_TRANSCONF(p) / 4] = NMR_TRANS_ENABLE | 4; reg[NMR_BLC_PWM_CTL / 4] = 0x80000000u; reg[NMR_BLC_PWM_FREQ / 4] = 0x4B0; reg[NMR_BLC_PWM_DUTY / 4] = 0x258;
    reg[NMX_BUF_CTL1(XP) / 4] = (1u << 24) | (VER < 20 ? (1u << 28) : 0) | (3u << 1) | (0u << 18);            /* running: SOC ready, D2D up, width 4 lanes, 10-bit data */
    reg[NMX_BUF_CTL2(XP) / 4] = (NMX_P2_READY << 4);
    reg[NMX_CLOCK_CTL(XP) / 4] = nmx_pclk_pll_req(0) | nmx_pclk_pll_ack(0) | nmx_refclk_req(0) | nmx_refclk_ack(0) | nmx_refclk_req(1) | nmx_refclk_ack(1) | (1u << 10) | (8u << 12) | 1u;   /* ddi clock select: max pclk */
    reg[NMX_CLK_VALFREQ(XP) / 4] = 162000;
    memset(sram, 0, sizeof sram);
    for (unsigned l = 0; l < 2; l++) {
        if (!IS_C20) {
        for (unsigned i = 0; i < 20; i++) phy[l][NMX_C10_PLL(i)] = (uint8_t)(0x30 + i + l);
        for (unsigned i = 0; i < 4; i++) phy[l][NMX_C10_CMN(i)] = (uint8_t)(0x50 + i);
        for (unsigned i = 0; i < 2; i++) phy[l][NMX_C10_TX(i)] = (uint8_t)(0x60 + i);
        phy[l][NMX_C10_WIDTH] = 0; phy[l][NMX_C10_CONTROL(1)] = 0x07;
        } else {
            const struct nmc20_state *st = nmc20_tab_mtl_dp[0];           /* 1.62 Gb/s, context A is the one in use */
            phy[l][NMC20_SERDES] = (uint8_t)(0x40 | (nmc20_dp_rate_code(st->clock) << 1)); phy[l][NMC20_WIDTH] = 0;
            for (unsigned i = 0; i < 3; i++) sram[l][NMC20_TX(0, i)] = st->tx[i];
            for (unsigned i = 0; i < 4; i++) sram[l][NMC20_CMN(0, i)] = st->cmn[i];
            for (unsigned i = 0; i < 11; i++) sram[l][(st->tx[0] & NMC20_USE_MPLLB) ? NMC20_MPLLB(0, i) : NMC20_MPLLA(0, i)] = st->mp[i];
        }
        phy[l][NMX_C10_OVRD] = 0x05;
        for (unsigned t2 = 0; t2 < 2; t2++) for (unsigned c = 0; c < 3; c++) phy[l][NMX_C10_VDROVRD(l, t2, c)] = (uint8_t)(0x10 + l * 4 + t2 * 3 + c);
    }
    dpcd[0x100] = 0x06; dpcd[0x101] = 0x84; dpcd[0x107] = 0x10; dpcd[0x108] = 1; dpcd[0x002] = 0x84; dpcd[0x003] = 0x01; dpcd[0x600] = 1;
    pw_fail = bus_fail = pll_ack_fail = sink_deaf = soc_fail = d2d_fail = idle_fail = pmd_fail = train_fail = 0; sink_needs_v = sink_needs_p = 0; bus_errs_left = 0;
}
static void s3(void)
{
    pw_fail = bus_fail = pll_ack_fail = sink_deaf = soc_fail = d2d_fail = idle_fail = pmd_fail = train_fail = dbuf_fail = 0; sink_needs_v = sink_needs_p = 0; bus_errs_left = 0;
    memset(reg, 0, sizeof reg); memset(phy, 0, sizeof phy); memset(sram, 0, sizeof sram); memset(dpcd, 0, sizeof dpcd); dpcd[0x002] = 0x84; dpcd[0x003] = 0x01; nw = 0; nsteps = 0;
    sram_writes = bus_resets = 0; reg[NMR_DC_STATE / 4] = NMR_DC_HOLD | 1u;        /* the latches firmware leaves held */
}
static int first_write(uint32_t off) { for (int i = 0; i < nw; i++) if (wlog[i].off == off) return i; return -1; }

static void suite(void)
{
    static struct nmr_snap snap, saved; static uint8_t phy_want[2][4096]; static uint32_t want[NREG];
    const unsigned p = T_PIPE, port = T_PORT; int rc;
    lit_state(); memcpy(want, reg, sizeof want); memcpy(phy_want, phy, sizeof phy); memcpy(sram_want, sram[0], sizeof sram_want);
    rc = nmr20_snapshot(&ops, &snap, p, 0, VER);
    ARM("a lit panel and its PHY are captured over the message bus", rc == NMR_OK && snap.valid && snap.cx0.valid);
    if (!IS_C20) ARM("the PLL register file is read exactly", memcmp(snap.cx0.pll, phy_want[0] + NMX_C10_PLL(0), 20) == 0 && snap.cx0.cmn[3] == 0x53 && snap.cx0.tx[1] == 0x61);
    else ARM("the C20 state is read from the active context and its port clock is 1.62 Gb/s", snap.cx0.is_c20 && snap.cx0.c20_clock == 162000 &&
             memcmp(snap.cx0.c20_tx, nmc20_tab_mtl_dp[0]->tx, 6) == 0 && memcmp(snap.cx0.c20_cmn, nmc20_tab_mtl_dp[0]->cmn, 8) == 0 && memcmp(snap.cx0.c20_mp, nmc20_tab_mtl_dp[0]->mp, 22) == 0);
    ARM("the PHY kind follows Linux: C20 only on Panther Lake ports after A", (int)snap.cx0.is_c20 == IS_C20 && nmx_phy_is_c20(VER, T_PORT) == (unsigned)IS_C20);
    ARM("the port is read from the transcoder", snap.topo.port == port);
    ARM("the sink's link settings are read over AUX with the channel's power request", snap.dpcd[0] == 0x06 && snap.have_sink);
    ARM("the AUX power request is released afterwards", !(*R(NMR_AUX_CTL(port)) & NMX_AUX_POWER_REQ));
    memcpy(&saved, &snap, sizeof snap);

    s3(); rc = nmr20_resume(&ops, &snap);
    if (rc) printf("   rc %d %s\n", rc, nmr_result_name(rc)); ARM("resume after S3 lights the panel", rc == NMR_OK);
    { int ok = 1; for (unsigned l = 0; l < 2; l++) for (unsigned i = 0; i < 4096; i++) {
          /* the PLL, common and transmitter registers, the width and the level overrides must match; control bytes are left as the sequence leaves them */
          int checked = IS_C20 ? ((i >= 0xD80 && i < 0xDA0) || i == NMX_C10_OVRD) : ((i >= 0xC00 && i < 0xC14) || i == 0xC20 || i == 0xC23 || i == 0xC30 || i == 0xC31 || (i >= 0xD80 && i < 0xDA0) || i == NMX_C10_OVRD);
          if (l == 1 && ((i >= 0xC00 && i < 0xC14) || i == 0xC20 || i == 0xC30)) continue;      /* the PLL, common and transmitter registers are written to the master lane only */
          if (checked && phy[l][i] != phy_want[l][i]) { ok = 0; printf("   differs: lane %u 0x%x (%02x, want %02x)\n", l, i, phy[l][i], phy_want[l][i]); } }
      ARM("the PHY's register file is back as the firmware left it", ok); }
    if (IS_C20) {
        const unsigned act = phy[0][NMC20_SERDES] & NMC20_CNTX_TOGGLE; int ok = 1; unsigned i;
        const struct nmc20_state *st = nmc20_tab_mtl_dp[0];
        for (i = 0; i < 3; i++) ok &= sram[0][NMC20_TX(act, i)] == st->tx[i];
        for (i = 0; i < 4; i++) ok &= sram[0][NMC20_CMN(act, i)] == st->cmn[i];
        for (i = 0; i < 11; i++) ok &= sram[0][(st->tx[0] & NMC20_USE_MPLLB) ? NMC20_MPLLB(act, i) : NMC20_MPLLA(act, i)] == st->mp[i];
        ARM("the C20 PLL words are in the context that is now active", ok);
        ARM("the words were written to the idle context and the context bit was toggled (A was active in the zeroed PHY)", act == 1 && sram[0][NMC20_TX(0, 0)] == 0 && sram_writes == 3 + 4 + 11);
        ARM("the serdes rate carries the DisplayPort bit and the rate code, the width is the plain-DisplayPort one", (phy[0][NMC20_SERDES] & 0x5E) == (0x40 | (nmc20_dp_rate_code(162000) << 1)) && (phy[0][NMC20_WIDTH] & 3) == 0);
        ARM("the C10 vendor control register is never touched on a C20 port", phy[0][NMX_C10_CONTROL(1)] == 0 && phy[1][NMX_C10_CONTROL(1)] == 0);
    }
    ARM("the block of registers is where Linux's register macros put it", VER >= 20 ? (NMX_BUF_CTL1(XP) == 0x16FA00u + 0x200u * port && NMX_M2P_CTL(XP, 0) == 0x16FA40u + 0x200u * port && NMX_P2M_STAT(XP, 1) == 0x16FA4Cu + 0x200u * port &&
          NMX_MB_TIMER(XP, 0) == 0x16FA58u + 0x200u * port && NMX_CLOCK_CTL(XP) == 0x16FA60u + 0x200u * port && NMX_BUF_CTL2(XP) == 0x16FA04u + 0x200u * port && NMX_CLK_VALFREQ(XP) == 0x64030u + 0x100u * port)
          : (NMX_BUF_CTL1(XP) == 0x64004u + 0x100u * port && NMX_CLOCK_CTL(XP) == 0x640E0u + 0x100u * port));
    ARM("version 30 does not reset the message bus after every transaction, earlier versions do", VER >= 30 ? bus_resets == 0 : bus_resets > 10);
    { unsigned k, gm = 0, set = 0, clr = 0; for (k = 0; k < (unsigned)nw; k++) if (wlog[k].off == NMR_DSPCLK_GATE) { gm++; if (wlog[k].v & NMR_GMBUS_GATE) set++; else clr++; }
      ARM("version 20 pulses the GMBUS clock gate (set, then cleared); other versions write the gate once", VER == 20 ? (set == 1 && gm == 3 && clr == 2 && ({ int a = -1, b = -1, k2; for (k2 = 0; k2 < nw; k2++) if (wlog[k2].off == NMR_DSPCLK_GATE) { if ((wlog[k2].v & NMR_GMBUS_GATE) && a < 0) a = k2; else if (a >= 0 && b < 0) b = k2; } a > 0 && b > a; })) : gm == 1); }
    ARM("the unit-bus throttle register is replayed from version 20", VER >= 20 ? (*R(NMR_MBUS_CTL) == want[NMR_MBUS_CTL / 4]) : first_write(NMR_MBUS_CTL) < 0);
    ARM("the D2D link is enabled in the DDI buffer register from version 20, before the buffer is enabled", VER >= 20 ? ({ int k = 0, f = -1; for (; k < nw; k++) if (wlog[k].off == NMR_DDI_BUF_CTL(port) && (wlog[k].v & NMR_XE2_D2D_ENABLE) && !(wlog[k].v & NMR_DDI_ENABLE)) { f = k; break; }
                       f >= 0 && f < first_write(NMR_TRANSCONF(p)) && (*R(NMR_DDI_BUF_CTL(port)) & NMR_XE2_D2D_STATE); }) : 1);
    ARM("the DDI buffer and the transcoder are running", (*R(NMR_DDI_BUF_CTL(port)) & NMR_DDI_ENABLE) && (*R(NMR_TRANSCONF(p)) & NMR_TRANS_ENABLE));
    ARM("the D2D link is up and the port buffer has the firmware's width", (VER >= 20 || (*R(NMX_BUF_CTL1(XP)) & (1u << 28))) && (*R(NMX_BUF_CTL1(XP)) & (7u << 1)) == (want[NMX_BUF_CTL1(XP) / 4] & (7u << 1)));
    ARM("the frequency register and the clock control are back", *R(NMX_CLK_VALFREQ(XP)) == 162000 && (*R(NMX_CLOCK_CTL(XP)) & nmx_pclk_pll_ack(0)));
    ARM("the power-management demand is requested and acknowledged", *R(NMX_PMDEMAND(0)) == want[NMX_PMDEMAND(0) / 4] && !(*R(NMX_PMDEMAND(1)) & NMX_PMD_REQ));
    ARM("the link ends in normal operation", ((*R(NMR_DP_TP_CTL(p)) >> 8) & 7) == 3);
    { int pw = first_write(NMR_PW_DRIVER), cd = first_write(NMR_CDCLK_CTL), pm = first_write(NMX_PMDEMAND(0)), pp = first_write(NMR_PP_CONTROL),
          ck = first_write(NMX_CLOCK_CTL(XP)), vf = first_write(NMX_CLK_VALFREQ(XP)), au = first_write(NMR_AUX_CTL(port)), fn = first_write(NMR_FUNC_CTL(p)),
          d2 = first_write(NMX_BUF_CTL1(XP)), db = first_write(NMR_DDI_BUF_CTL(port)), tc = first_write(NMR_TRANSCONF(p)), bl = first_write(NMR_BLC_PWM_DUTY);
      ARM("order: power wells, display clock, power demand, panel power, then the PHY", pw < cd && cd < pm && pm < pp && pp < ck);
      ARM("order: clock frequency written before the PLL is requested", vf > ck);
      ARM("order: sink and pipe registers after the PLL, function register, then the DDI buffer, then the transcoder, then the backlight", au > vf && fn > au && db > fn && tc > db && bl > tc);
      ARM("order: the D2D link is enabled before the DDI buffer", d2 > 0 && d2 < db); }
    { const char *w[] = { "display core init", "power wells", "display clock", "power-management demand", "panel power", IS_C20 ? "Cx0 PHY (C20)" : "Cx0 PHY", "sink power", "pipe and plane registers",
                          "transcoder function and DDI buffer", "link training", "link training: clock recovery done", "transcoder on", "backlight" };
      int ok = nsteps == 13; for (int i = 0; ok && i < 13; i++) if (strcmp(steps[i], w[i])) ok = 0;
      ARM("each step is logged in order", ok); }

    /* failures */
    s3(); lit_state(); nmr20_snapshot(&ops, &snap, p, 0, VER); nw = 0; ARM("a panel still lit is left alone", nmr20_resume(&ops, &snap) == NMR_ALREADY_LIT && nw == 0);
    memcpy(&snap, &saved, sizeof snap);
    s3(); rc = nmr20_resume(&ops, &snap);
    ARM("the platform reset handshake is replayed before the power wells", rc == NMR_OK && first_write(NMR_NDE_RSTWRN) >= 0 && first_write(NMR_NDE_RSTWRN) < first_write(NMR_PW_DRIVER));
    ARM("the display buffer slice is requested after the display clock and before the PHY", first_write(NMR_DBUF_CTL(0)) > first_write(NMR_CDCLK_CTL) && first_write(NMR_DBUF_CTL(0)) < first_write(NMX_CLOCK_CTL(XP)));
    ARM("the PHY latches that firmware holds are left alone on versions 20 and 30 (only version 14 releases them)", (*R(NMR_DC_STATE) & NMR_DC_HOLD) == NMR_DC_HOLD);
    s3(); dbuf_fail = 1; ARM("a display buffer slice that never powers stops the resume before the PHY", nmr20_resume(&ops, &snap) == NMR_E_POWER && first_write(NMX_CLOCK_CTL(XP)) < 0);
    s3(); pw_fail = 1; ARM("a power well that never answers stops the resume before the PHY", nmr20_resume(&ops, &snap) == NMR_E_POWER && first_write(NMX_CLOCK_CTL(XP)) < 0);
    s3(); pmd_fail = 1; ARM("a power-management demand that is not acknowledged stops the resume before the panel is powered", nmr20_resume(&ops, &snap) == NMR_E_PMDEMAND && first_write(NMR_PP_CONTROL) < 0);
    s3(); soc_fail = 1; ARM("a PHY that never leaves reset stops the resume before any PLL register is written", nmr20_resume(&ops, &snap) == NMR_E_PHY && first_write(NMR_DDI_BUF_CTL(port)) < 0);
    s3(); pll_ack_fail = 1; ARM("a PLL that never acknowledges stops the resume before the DDI buffer", nmr20_resume(&ops, &snap) == NMR_E_PHY && first_write(NMR_DDI_BUF_CTL(port)) < 0);
    s3(); d2d_fail = 1; { int rc2 = nmr20_resume(&ops, &snap), en = 0; for (int k = 0; k < nw; k++) if (wlog[k].off == NMR_DDI_BUF_CTL(port) && (wlog[k].v & NMR_DDI_ENABLE)) en = 1;
      ARM("a D2D link that never comes up stops the resume before the DDI buffer is enabled", rc2 == NMR_E_PHY && !en); }
    s3(); idle_fail = 1; ARM("a port buffer that stays idle turns the DDI buffer off again", nmr20_resume(&ops, &snap) == NMR_E_PHY && !(*R(NMR_DDI_BUF_CTL(port)) & NMR_DDI_ENABLE));
    s3(); sink_deaf = 1; ARM("a sink that does not answer on AUX stops the resume", nmr20_resume(&ops, &snap) == NMR_E_SINK && first_write(NMR_DDI_BUF_CTL(port)) < 0);
    s3(); bus_errs_left = 2; ARM("two message-bus errors are retried and the resume still lights the panel", nmr20_resume(&ops, &snap) == NMR_OK);
    s3(); train_fail = 1; ARM("a sink that never trains fails within a bounded time and turns the buffer off", nmr20_resume(&ops, &snap) == NMR_E_TRAIN && !(*R(NMR_DDI_BUF_CTL(port)) & NMR_DDI_ENABLE));
    s3(); sink_needs_v = 2; sink_needs_p = 1;
    { unsigned dv, dp; rc = nmr20_resume(&ops, &snap); phy_level(&dv, &dp);
      ARM("the sink's swing and pre-emphasis request reaches the C10 override registers", rc == NMR_OK && dv == 2 && dp == 1); }
    s3(); sink_needs_v = 3; { unsigned dv, dp; rc = nmr20_resume(&ops, &snap); phy_level(&dv, &dp); ARM("the maximum swing is reachable", rc == NMR_OK && dv == 3); }
    if (IS_C20) {
        lit_state(); sram[0][NMC20_MPLLB(0, 0)] ^= 0x3; sram[0][NMC20_MPLLA(0, 0)] ^= 0x3;
        ARM("a C20 state whose link rate is in none of Linux's tables is refused, not guessed", nmr20_snapshot(&ops, &snap, p, 0, VER) == NMR_E_NO_SNAPSHOT);
    }
    /* the bus itself */
    lit_state();
    { uint8_t b = 0; phy[0][0xC05] = 0x5A; ARM("a message-bus read returns the register", nmx_read(&ops, XP, 0, 0xC05, &b) == 0 && b == 0x5A);
      ARM("a write lands", nmx_write(&ops, XP, 0, 0xC06, 0xA7, 1) == 0 && phy[0][0xC06] == 0xA7);
      bus_fail = 1; ARM("a bus that never answers fails after three tries, and is reset", nmx_read(&ops, XP, 0, 0xC05, &b) == -1);
      bus_fail = 0; }
}

int main(void)
{
    static const struct { unsigned ver, c20, port; } L[] = { { 20, 0, 0 }, { 20, 0, 1 }, { 30, 0, 0 }, { 30, 1, 1 } };
    for (unsigned c = 0; c < 4; c++) {
        VER = L[c].ver; IS_C20 = L[c].c20; T_PORT = L[c].port; T_PIPE = L[c].port;
        printf("-- display version %u, %s PHY, panel on pipe %c, DDI %c\n", VER, IS_C20 ? "C20" : "C10", 'A' + T_PIPE, 'A' + T_PORT);
        suite();
    }
    /* the tables copied from Linux describe the clocks they are filed under: the port clock computed from the register words (the readout
     * formula of intel_c20pll_calc_port_clock) equals each entry's clock */
    { const struct nmc20_state *const *tabs[4] = { nmc20_tab_mtl_dp, nmc20_tab_xe2hpd_edp, nmc20_tab_xe2hpd_dp, nmc20_tab_xe3lpd }; unsigned n = 0, bad = 0, k, i;
      for (k = 0; k < 4; k++) for (i = 0; tabs[k][i]; i++) { n++; if (nmc20_clock(tabs[k][i]->tx, tabs[k][i]->mp) != tabs[k][i]->clock) { bad++; printf("   table %u entry %u: clock %u, computed %u\n", k, i, tabs[k][i]->clock, nmc20_clock(tabs[k][i]->tx, tabs[k][i]->mp)); } }
      ARM("every C20 table entry's register words give back the link rate it is filed under", n > 30 && bad == 0); }
    ARM("version 30 offers the eDP rates in between the DisplayPort ones, earlier parts do not", nmc20_find(nmc20_table_for(30), 216000) && nmc20_find(nmc20_table_for(30), 675000) && !nmc20_find(nmc20_table_for(20), 216000) && nmc20_find(nmc20_table_for(20), 810000));
    ARM("the plain MMIO entry point still refuses display version 20", ({ struct nmr_snap sx; memset(&sx, 0, sizeof sx); T_PORT = 0; T_PIPE = 0; VER = 20; IS_C20 = 0; lit_state(); nmr_snapshot(&ops, &sx, 0, 20, 0) == NMR_E_GEN; }));
    printf("%d failed\n", fails);
    return fails != 0;
}

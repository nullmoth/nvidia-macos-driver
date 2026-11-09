/* S3 resume of an Intel laptop panel (kexts/NVRM/accel/nm_hybrid_resume.h) against a model of the display engine, the
 * panel power sequencer, the combo PHY drive registers, the AUX channel and an eDP sink.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_resume.c -o /tmp/t && /tmp/t     -> 0 failed
 *   cc -DNAIVE_RESTORE ...   (a plain register restore: values written back, no handshakes, no link training)  -> failures
 * The model is ours, built from the register documentation in Linux's i915 headers; it checks the ORDER and the handshakes
 * the engine performs and that the drive levels it programs are the ones the sink asked for, not that a given panel lights
 * (that needs hardware). The whole suite runs for a panel on pipe A / DDI A, pipe B / DDI B and pipe D / DDI A. */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "nm_hybrid_resume.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static unsigned T_PIPE = 0, T_PORT = 0, T_VER = 12;
static struct nmr_edp T_EDP;                /* what the video BIOS says about the panel's drive table */
static uint32_t reg[NREG];
static uint8_t dpcd[0x800];
static uint8_t edid[128]; static unsigned edid_off;      /* the panel's EDID, read over I2C-over-AUX */
static struct { uint32_t off; uint32_t v; } wlog[6000]; static int nw;
static unsigned long now_us;
static int pw_fail, pll_fail, panel_fail, sink_deaf, train_fail_level, sink_needs_v, sink_needs_p, no_drive;
static int drive_calls;
static int dbuf_fail;
static int pcode_fail, pcode_prepare_at, pcode_volt_at, pcode_volt = -1;      /* the platform controller's display-clock mailbox */
static const char *steps[32]; static int nsteps;
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

/* the level the PHY registers of lane 0 encode: matched against the table for this generation and the link rate */
static void phy_level(unsigned *v, unsigned *p)
{
    const uint32_t lane = nmr_phy_base(T_PORT) + 0x880;
    const struct nmr_sel sel = nmr_select(T_VER, dpcd[0x100] * 27000u, &T_EDP);
    const struct nmr_trans *tab = sel.t;
    uint32_t d2 = *R(lane + 8), d4 = *R(lane + 16), d7 = *R(lane + 28);
    *v = *p = 0;
    for (unsigned vv = 0; vv < 4; vv++) for (unsigned pp = 0; pp + vv < 4; pp++) {
        int idx = nmr_level_index(vv, pp);
        const struct nmr_trans *t = &tab[idx];
        if (idx == 0 || (unsigned)idx >= sel.n) continue;
        if ((((d2 >> 15) & 1) << 3 | ((d2 >> 11) & 7)) == t->sw && ((d7 >> 24) & 0x7F) == t->ns && (d4 & 0x3F) == t->cc && ((d4 >> 12) & 0x3F) == t->pc1) { *v = vv; *p = pp; }
    }
}

static void aux_process(void)
{
    const uint32_t CTL = NMR_AUX_CTL(T_PORT), DATA = NMR_AUX_DATA(T_PORT);
    uint8_t m[20] = {0}; unsigned i, send = (*R(CTL) >> 20) & 0x1F;
    uint32_t ctl = *R(CTL) & ~((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));   /* status bits written as 1 are cleared */
    for (i = 0; i < send; i += 4) { uint32_t w = *R(DATA + i); for (unsigned j = 0; j < 4 && i + j < send; j++) m[i + j] = (uint8_t)(w >> (24 - 8 * j)); }
    int panel_on = (*R(NMR_PP_STATUS) & NMR_PP_ON_MASK) == NMR_PP_ON_VALUE;
    if (!(*R(NMR_PW_AUX) & (2u << (2 * T_PORT)))) { *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }   /* the AUX well is off */
    if (sink_deaf || !panel_on) { *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }       /* time out */
    unsigned cmd = m[0] >> 4, len = m[3] + 1; uint32_t addr = ((m[0] & 0xF) << 16) | (m[1] << 8) | m[2];
    uint8_t rep[20] = {0};
    if (cmd == 0 || cmd == 4) {          /* I2C write: sets the EDID read offset */
        if (addr != 0x50) { *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }
        edid_off = m[4]; *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (1u << 20); *R(DATA) = 0; return; }
    if (cmd == 1 || cmd == 5) {          /* I2C read of the EDID */
        for (i = 0; i < len; i++) rep[1 + i] = edid[(edid_off + i) & 127];
        *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | ((len + 1) << 20);
        for (i = 0; i < len + 1; i += 4) *R(DATA + i) = ((uint32_t)rep[i] << 24) | ((uint32_t)rep[i + 1] << 16) | ((uint32_t)rep[i + 2] << 8) | rep[i + 3];
        return; }
    if (cmd == 8) {
        for (i = 0; i < len; i++) dpcd[addr + i] = m[4 + i];
        *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (1u << 20);
        *R(DATA) = 0;
        return;
    }
    for (i = 0; i < len; i++) {
        uint32_t a = addr + i; uint8_t v = dpcd[a];
        if (a == 0x202 || a == 0x203 || a == 0x204) {
            unsigned pat = dpcd[0x102] & 3, srcpat = (*R(NMR_DP_TP_CTL(T_PIPE)) >> 8) & 7, dv, dp; int cr, eq;
            phy_level(&dv, &dp);
            cr = pat >= 1 && srcpat == (pat == 1 ? 0u : 1u) && !train_fail_level && dv >= (unsigned)sink_needs_v && (dpcd[0x103] & 3) == dv;
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
static uint32_t m_rd(void *c, uint32_t o)
{
    (void)c;
    if (o == NMR_FRMCOUNT(T_PIPE) && (*R(NMR_TRANSCONF(T_PIPE)) & NMR_TRANS_ENABLE) && (*R(NMR_DDI_BUF_CTL(T_PORT)) & NMR_DDI_ENABLE)) (*R(o))++;
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    (void)c;
    if (nw < 6000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    if (o == NMR_AUX_CTL(T_PORT)) {
        if (v & (1u << 31)) { *R(o) = v; aux_process(); return; }
        *R(o) &= ~(v & ((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25))); return;
    }
    if (o == NMR_PCODE_MAILBOX) {
        *R(o) = v;
        if ((v & NMR_PCODE_READY) && (v & 0xFF) == NMR_PCODE_CDCLK && !pcode_fail) {
            uint32_t d = *R(NMR_PCODE_DATA);
            if (d == NMR_PCODE_PREPARE) { *R(NMR_PCODE_DATA) = NMR_PCODE_READY_FOR_CHANGE; if (!pcode_prepare_at) pcode_prepare_at = nw; }
            else { pcode_volt = (int)d; pcode_volt_at = nw; }
            *R(o) = v & ~NMR_PCODE_READY & ~0xFFu;      /* done: the low byte now carries the status, 0 = success */
        }
        return;
    }
    *R(o) = v;
    if (o == NMR_PW_DRIVER || o == NMR_PW_AUX || o == NMR_PW_DDI) { if (!pw_fail) *R(o) = v | ((v & 0x55555555u) << 1); }
    for (unsigned k = 0; k < 4; k++) if (o == NMR_DBUF_CTL(k) && (v & NMR_DBUF_REQ) && !dbuf_fail) *R(o) |= NMR_DBUF_STATE;
    if (o == NMR_DE_PLL_ENABLE && (v & NMR_PLL_ENABLE) && !pll_fail) *R(o) |= NMR_PLL_LOCK;
    for (unsigned k = 0; k < 2; k++) if (o == NMR_DPLL_ENABLE(k)) { if (v & NMR_PLL_POWER) *R(o) |= NMR_PLL_PSTATE; if ((v & NMR_PLL_ENABLE) && !pll_fail) *R(o) |= NMR_PLL_LOCK; }
    if (o == NMR_PP_CONTROL && (v & 1) && !panel_fail) *R(NMR_PP_STATUS) = NMR_PP_ON_VALUE;
    if (o == NMR_DP_TP_CTL(T_PIPE) && ((v >> 8) & 7) == 2) *R(NMR_DP_TP_STATUS(T_PIPE)) |= NMR_TP_IDLE_DONE;
}
static void m_delay(void *c, unsigned us) { (void)c; now_us += us; }
static void m_log(void *c, const char *s) { (void)c; if (nsteps < 32) steps[nsteps++] = s; }
static int m_drive(void *c, unsigned v, unsigned p) { (void)c; (void)v; (void)p; drive_calls++; return no_drive ? -1 : 0; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, m_drive };

static void lit_state(void)
{
    memset(reg, 0, sizeof reg); memset(dpcd, 0, sizeof dpcd);
    const unsigned t = T_PIPE, p = T_PIPE, port = T_PORT;
    reg[NMR_PW_DRIVER / 4] = 0x0000001Fu | 0x2Au; reg[NMR_PW_AUX / 4] = 3u << (2 * port); reg[NMR_PW_DDI / 4] = 3u << (2 * port);
    reg[NMR_DBUF_CTL(0) / 4] = NMR_DBUF_REQ | NMR_DBUF_STATE; reg[NMR_NDE_RSTWRN / 4] = 0x10u;
    reg[NMR_DSSM / 4] = 2u << 29;                                          /* 38.4 MHz reference */
    reg[NMR_DE_PLL_ENABLE / 4] = NMR_PLL_ENABLE | NMR_PLL_LOCK | 25; reg[NMR_CDCLK_CTL / 4] = 0x00000BB8;      /* vco 960 MHz / 2 = 480 MHz */
    if (T_VER >= 13) reg[NMR_CDCLK_SQUASH / 4] = (1u << 31) | (7u << 24) | 0xFF00u;       /* 8 of 8: the same 480 MHz */
    reg[NMR_DPLL_ENABLE(port & 1) / 4] = NMR_PLL_POWER | NMR_PLL_PSTATE | NMR_PLL_ENABLE | NMR_PLL_LOCK;
    for (unsigned k = 0; k < 2; k++) {
        reg[NMR_DPLL_CFGCR0(k) / 4] = 0x1234567 + k; reg[NMR_DPLL_CFGCR1(k) / 4] = 0x89AB + k; reg[NMR_DPLL_DIV0(k) / 4] = 0x5 + k;
    }
    reg[NMR_DPCLKA_CFGCR0 / 4] = 0x3F0;
    for (unsigned i = 0; i < NMR_PHY_WORDS; i++) reg[nmr_phy_base(port) / 4 + i] = 0xC0DE0000u + i;
    reg[NMR_PP_CONTROL / 4] = 0x00000001u | 0x4; reg[NMR_PP_STATUS / 4] = NMR_PP_ON_VALUE;
    reg[NMR_PP_ON_DELAYS / 4] = 0x19001900; reg[NMR_PP_OFF_DELAYS / 4] = 0x01900190; reg[NMR_PP_DIVISOR / 4] = 0x0001E00A;
    for (unsigned i = 0; i < NMR_N_TIMING; i++) reg[NMR_TIMING(t, i) / 4] = 0x1000 + i * 7;
    reg[NMR_TRANS_CLK_SEL(t) / 4] = 0; reg[NMR_PIPE_MISC(p) / 4] = 0x40; reg[NMR_WM_LINETIME(p) / 4] = 0x1f4; reg[NMR_MSA_MISC(t) / 4] = 0x21;
    for (unsigned i = 0; i < NMR_N_PLANE; i++) reg[NMR_PLANE(p, i) / 4] = 0x8000u + i;
    for (unsigned i = 0; i < NMR_PLANE_WM_WORDS; i++) reg[(NMR_PLANE_WM_BASE(p) / 4) + i] = 0x80000000u + i;
    reg[NMR_SURF(p) / 4] = 0x00800000;
    reg[NMR_FUNC_CTL(t) / 4] = 0x80000000u | ((port + 1u) << NMR_TRANS_PORT_SHIFT) | (1u << 8);
    reg[NMR_DP_TP_CTL(p) / 4] = NMR_TP_ENABLE | NMR_TP_NORMAL; reg[NMR_DDI_BUF_CTL(port) / 4] = NMR_DDI_ENABLE | (1u << 1);
    reg[NMR_TRANSCONF(p) / 4] = NMR_TRANS_ENABLE | 0x4; reg[NMR_BLC_PWM_CTL / 4] = 0x80000000u; reg[NMR_BLC_PWM_FREQ / 4] = 0x4B0; reg[NMR_BLC_PWM_DUTY / 4] = 0x258;
    dpcd[0x100] = 0x14; dpcd[0x101] = 0x82; dpcd[0x107] = 0x10; dpcd[0x108] = 1; dpcd[0x115] = 0;
    dpcd[0x002] = 0x82; dpcd[0x003] = 0x01; dpcd[0x00E] = 0; dpcd[0x600] = 1;
    /* lane registers hold the firmware's level 0, which is not one of the table rows */
    for (int ln = 0; ln < 4; ln++) { uint32_t l = nmr_phy_base(port) + 0x880 + 0x100 * ln; reg[(l + 8) / 4] = 0; reg[(l + 16) / 4] = 0; reg[(l + 28) / 4] = 0; }
    sink_needs_v = sink_needs_p = 0; train_fail_level = 0; pw_fail = pll_fail = panel_fail = sink_deaf = no_drive = 0;
    pcode_fail = 0; dbuf_fail = 0; pcode_prepare_at = pcode_volt_at = 0; pcode_volt = -1;
}
static void s3(void)   /* the display engine and the sink lose everything */
{
    memset(reg, 0, sizeof reg); memset(dpcd, 0, sizeof dpcd); dpcd[0x002] = 0x82; dpcd[0x003] = 0x01; nw = 0; nsteps = 0; drive_calls = 0;
    pcode_prepare_at = pcode_volt_at = 0; pcode_volt = -1; dbuf_fail = 0;
}
static int first_write(uint32_t off) { for (int i = 0; i < nw; i++) if (wlog[i].off == off) return i; return -1; }
static int last_write(uint32_t off) { int r = -1; for (int i = 0; i < nw; i++) if (wlog[i].off == off) r = i; return r; }

#ifdef NAIVE_RESTORE
static int naive_resume(const struct nmr_ops *o, const struct nmr_snap *s, unsigned v)
{
    (void)v; unsigned i;
    o->wr(o->ctx, NMR_PW_DRIVER, s->pw_driver); o->wr(o->ctx, NMR_DPLL_CFGCR0(0), s->cfgcr0[0]); o->wr(o->ctx, NMR_DPLL_ENABLE(0), s->dpll_en[0]);
    for (i = 0; i < NMR_N_TIMING; i++) o->wr(o->ctx, NMR_TIMING(s->topo.pipe, i), s->timing[i]);
    o->wr(o->ctx, NMR_SURF(s->topo.pipe), s->surf); o->wr(o->ctx, NMR_FUNC_CTL(s->topo.pipe), s->func);
    o->wr(o->ctx, NMR_DDI_BUF_CTL(s->topo.port), s->ddi); o->wr(o->ctx, NMR_TRANSCONF(s->topo.pipe), s->transconf);
    return NMR_OK;
}
#define nmr_resume naive_resume
#endif

static void edp_suite(void);
static void suite(void)
{
    static struct nmr_snap snap; static uint32_t want[NREG];
    const unsigned t = T_PIPE, p = T_PIPE, port = T_PORT;
    lit_state(); memcpy(want, reg, sizeof want);
    ARM("a lit panel is captured", nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP) == NMR_OK && snap.valid);
    ARM("the port is read from the transcoder's function register", snap.topo.port == port && snap.topo.pipe == p);
    ARM("the sink's link settings are captured over the AUX channel", snap.dpcd[0] == 0x14 && snap.dpcd[1] == 0x82 && snap.have_sink);
    ARM("a panel that uses the AUX handshake is recognised", !snap.no_handshake);
    lit_state(); reg[NMR_PW_AUX / 4] = 0; { static struct nmr_snap s2; ARM("a firmware that left the AUX well off still gets its sink captured", nmr_snapshot(&ops, &s2, p, T_VER, &T_EDP) == NMR_OK && s2.have_sink && reg[NMR_PW_AUX / 4] == 0); }
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP);
    { uint8_t id[10] = {0}; for (unsigned i = 0; i < 128; i++) edid[i] = (uint8_t)(0x40 + i);
      ARM("the panel's identifier (EDID bytes 8 to 17) is read over I2C-over-AUX", nmr_edid_id(&ops, port, id) == 0 && !memcmp(id, edid + 8, 10));
      sink_deaf = 1; ARM("a panel that does not answer gives no identifier", nmr_edid_id(&ops, port, id) == -1); sink_deaf = 0; }

    s3(); int rc = nmr_resume(&ops, &snap, T_VER);
    ARM("resume after S3 lights the panel", rc == NMR_OK);
    { int ok = 1; uint32_t chk[] = { NMR_DPLL_CFGCR0(port & 1), NMR_DPLL_CFGCR1(port & 1), NMR_DPLL_DIV0(port & 1), NMR_DPCLKA_CFGCR0, nmr_phy_base(port), nmr_phy_base(port) + 0x880 + 8,
        nmr_phy_base(port) + 4 * (NMR_PHY_WORDS - 1), NMR_PP_ON_DELAYS, NMR_TIMING(t, 0), NMR_TIMING(t, 6), NMR_TIMING(t, 10), NMR_SURF(p), NMR_PLANE(p, 1), NMR_PLANE_WM_BASE(p),
        NMR_PLANE_WM_BASE(p) + 0x3C, NMR_FUNC_CTL(t), NMR_PIPE_MISC(p), NMR_BLC_PWM_DUTY, NMR_CDCLK_CTL, NMR_TRANS_CLK_SEL(t), NMR_MSA_MISC(t) };
      for (unsigned i = 0; i < sizeof chk / sizeof *chk; i++) if (reg[chk[i] / 4] != want[chk[i] / 4]) { ok = 0; printf("   differs: 0x%x\n", chk[i]); }
      ARM("every captured configuration register is back to its firmware value, including the whole PHY block", ok); }
    ARM("the transcoder and the DDI buffer are running", (reg[NMR_TRANSCONF(p) / 4] & NMR_TRANS_ENABLE) && (reg[NMR_DDI_BUF_CTL(port) / 4] & NMR_DDI_ENABLE));
    ARM("the link ends in normal operation", ((reg[NMR_DP_TP_CTL(p) / 4] >> 8) & 7) == 3);
    ARM("the sink is told the link settings and powered to D0", dpcd[0x100] == 0x14 && dpcd[0x101] == 0x82 && dpcd[0x600] == 1);
    ARM("the training pattern is switched off at the sink afterwards", dpcd[0x102] == 0);
    { int pw = first_write(NMR_PW_DRIVER), cd = first_write(NMR_DE_PLL_ENABLE), pl = first_write(NMR_DPLL_ENABLE(port & 1)), ph = first_write(nmr_phy_base(port)),
          pp = first_write(NMR_PP_CONTROL), aux = first_write(NMR_AUX_CTL(port)), tim = first_write(NMR_TIMING(t, 0)), sf = last_write(NMR_SURF(p)),
          pl7 = last_write(NMR_PLANE(p, 3)), fn = first_write(NMR_FUNC_CTL(t)), dd = first_write(NMR_DDI_BUF_CTL(port)), tc = first_write(NMR_TRANSCONF(p)), bl = first_write(NMR_BLC_PWM_DUTY);
      ARM("order: power wells, display clock, DPLL, PHY block, panel power", pw < cd && cd < pl && pl < ph && ph < pp);
      ARM("order: the AUX channel is used only after the panel is powered", aux > pp);
      ARM("order: timing and plane registers, the surface register after the other plane registers", tim > aux && sf > pl7);
      ARM("order: the transcoder function and the DDI buffer after the pipe registers, the transcoder last of the three", fn > sf && dd > fn && tc > dd);
      ARM("order: the backlight only after the transcoder is on", bl > tc); }
    { const char *want_steps[] = { "display core init", "power wells", "display clock", "DPLL", "combo PHY", "panel power", "sink power", "pipe and plane registers",
                                   "transcoder function and DDI buffer", "link training", "link training: clock recovery done", "transcoder on", "backlight" };
      int ok = nsteps == 13; for (int i = 0; ok && i < 13; i++) if (strcmp(steps[i], want_steps[i])) ok = 0;
      ARM("each step is logged in order (these are the lines a tester sends)", ok); }
    ARM("the other port's registers are never written", port == 0 ? first_write(NMR_DDI_BUF_CTL(1)) < 0 : first_write(NMR_DDI_BUF_CTL(0)) < 0);


    /* the display clock: the platform controller's handshake and the squash register */
    ARM("the display clock and its voltage level are computed from the registers (480 MHz is level 2)", snap.volt == 2 && nmr_cdclk_khz(snap.dssm, snap.de_pll, snap.cdclk, snap.squash) == 480000);
    { s3(); rc = nmr_resume(&ops, &snap, T_VER); int dp = first_write(NMR_DE_PLL_ENABLE), cc = first_write(NMR_CDCLK_CTL);
      ARM("the platform controller is asked to prepare before the display clock PLL is touched", rc == NMR_OK && pcode_prepare_at > 0 && pcode_prepare_at <= dp);
      ARM("the voltage level is sent after the display clock is set", pcode_volt == 2 && pcode_volt_at > cc);
      ARM("the squash register is replayed from version 13, between the PLL and the clock control", T_VER >= 13 ? (first_write(NMR_CDCLK_SQUASH) > dp && first_write(NMR_CDCLK_SQUASH) < cc) : first_write(NMR_CDCLK_SQUASH) < 0); }
    { s3(); pcode_fail = 1; rc = nmr_resume(&ops, &snap, T_VER);
      ARM("a platform controller that never says ready stops the resume before the PLL and the PHY", rc == NMR_E_CDCLK && first_write(NMR_DE_PLL_ENABLE) < 0 && first_write(nmr_phy_base(port)) < 0); pcode_fail = 0; }
    { s3(); rc = nmr_resume(&ops, &snap, T_VER); int ch = first_write(NMR_NDE_RSTWRN), db = first_write(NMR_DBUF_CTL(0)), pw = first_write(NMR_PW_DRIVER), tm = first_write(NMR_TIMING(t, 0));
      ARM("the platform reset handshake is replayed before the power wells", rc == NMR_OK && ch >= 0 && ch < pw);
      ARM("the display buffer slice is requested after the display clock and before any pipe register", db > first_write(NMR_CDCLK_CTL) && db < tm && (reg[NMR_DBUF_CTL(0) / 4] & NMR_DBUF_STATE)); }
    { s3(); dbuf_fail = 1; rc = nmr_resume(&ops, &snap, T_VER);
      ARM("a display buffer slice that never powers stops the resume before the PHY and the pipe", rc == NMR_E_POWER && first_write(nmr_phy_base(port)) < 0 && first_write(NMR_TIMING(t, 0)) < 0); dbuf_fail = 0; }

    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); nw = 0;
    ARM("a panel that is still lit is not touched", nmr_resume(&ops, &snap, T_VER) == NMR_ALREADY_LIT && nw == 0);

    s3(); ARM("display version 14 is refused with no register written", nmr_resume(&ops, &snap, 14) == NMR_E_GEN && nw == 0);
    { static struct nmr_snap none; memset(&none, 0, sizeof none); ARM("no snapshot, no resume, nothing written", nmr_resume(&ops, &none, T_VER) == NMR_E_NO_SNAPSHOT && nw == 0); }
    memset(reg, 0, sizeof reg);
    ARM("a dark panel is not captured", nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP) == NMR_E_NO_SNAPSHOT && !snap.valid);
    lit_state(); reg[NMR_FUNC_CTL(t) / 4] = 0x80000000u | (4u << NMR_TRANS_PORT_SHIFT);   /* DDI D */
    ARM("a panel on DDI D or later (Type-C) is refused, not guessed", nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP) == NMR_E_GEN);

    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); pw_fail = 1;
    ARM("a power well that never acknowledges stops the resume", nmr_resume(&ops, &snap, T_VER) == NMR_E_POWER);
    ARM("...before any PLL, panel or DDI register is written", first_write(NMR_DPLL_ENABLE(port & 1)) < 0 && first_write(NMR_PP_CONTROL) < 0 && first_write(NMR_DDI_BUF_CTL(port)) < 0);
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); pll_fail = 1;
    ARM("a display clock PLL that never locks stops the resume", nmr_resume(&ops, &snap, T_VER) == NMR_E_CDCLK && first_write(NMR_DDI_BUF_CTL(port)) < 0);
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); panel_fail = 1;
    ARM("a panel power sequence that never completes stops the resume", nmr_resume(&ops, &snap, T_VER) == NMR_E_PANEL && first_write(NMR_DDI_BUF_CTL(port)) < 0 && first_write(NMR_TRANSCONF(p)) < 0);
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_deaf = 1;
    ARM("a sink that does not answer on AUX stops the resume", nmr_resume(&ops, &snap, T_VER) == NMR_E_SINK && first_write(NMR_DDI_BUF_CTL(port)) < 0);

    /* link training: the adjust-request loop and the drive registers */
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 2; sink_needs_p = 1;
    { rc = nmr_resume(&ops, &snap, T_VER); unsigned dv, dp; phy_level(&dv, &dp);
      ARM("a sink that asks for swing 2 and pre-emphasis 1 gets exactly that in the PHY registers", rc == NMR_OK && dv == 2 && dp == 1);
      ARM("...and in the sink's lane setting", (dpcd[0x103] & 3) == 2 || dpcd[0x102] == 0); }
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 3;
    { rc = nmr_resume(&ops, &snap, T_VER); unsigned dv, dp; phy_level(&dv, &dp);
      ARM("the maximum swing is reachable", rc == NMR_OK && dv == 3 && dp == 0); }
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 0; sink_needs_p = 3;
    { rc = nmr_resume(&ops, &snap, T_VER); unsigned dv, dp; phy_level(&dv, &dp);
      ARM("the maximum pre-emphasis is reachable", rc == NMR_OK && dv == 0 && dp == 3); }
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 2; no_drive = 1;
    ARM("an observer that refuses the level makes the training fail", nmr_resume(&ops, &snap, T_VER) == NMR_E_TRAIN);
    ARM("a failed training turns the DDI buffer off and leaves the transcoder off", !(reg[NMR_DDI_BUF_CTL(port) / 4] & NMR_DDI_ENABLE) && !(reg[NMR_TRANSCONF(p) / 4] & NMR_TRANS_ENABLE));
    lit_state(); nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); train_fail_level = 1; now_us = 0;
    ARM("a sink that never reports clock recovery fails within a bounded time", nmr_resume(&ops, &snap, T_VER) == NMR_E_TRAIN && now_us < 5000000ul);
    lit_state(); dpcd[0x003] = 0x41; nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP);
    ARM("a panel flagged as not using the AUX handshake is recognised", snap.no_handshake);
    s3(); train_fail_level = 1;
    ARM("such a panel is not trained over AUX, so a sink that cannot train still relights", nmr_resume(&ops, &snap, T_VER) == NMR_OK && (reg[NMR_DDI_BUF_CTL(port) / 4] & NMR_DDI_ENABLE));
}

static void edp_suite(void)
{
    static struct nmr_snap snap; unsigned dv, dp; int rc;
    const unsigned p = T_PIPE, port = T_PORT;
    lit_state(); T_EDP.known = 1; T_EDP.low_vswing = 1; T_EDP.hobl = 0; nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 2; sink_needs_p = 1;
    rc = nmr_resume(&ops, &snap, T_VER); phy_level(&dv, &dp);
    ARM("the panel's own table is used: low swing, the sink's request reaches the PHY", rc == NMR_OK && dv == 2 && dp == 1);
    { const struct nmr_trans *low = nmr_select(T_VER, 540000, &T_EDP).t, *dpt = nmr_select(T_VER, 540000, 0).t;
      ARM("and it is not the DisplayPort row", low != dpt && memcmp(&low[8], &dpt[8], sizeof low[8]) != 0); }
    lit_state(); T_EDP.known = 1; T_EDP.low_vswing = 0; T_EDP.hobl = 1; nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 2;
    rc = nmr_resume(&ops, &snap, T_VER);
    ARM("reduced-power panel: up to swing 2 is trained", rc == NMR_OK);
    ARM("reduced-power panel: the PHY's reduced-power override bits are set", (*R(nmr_phy_base(port) + 40) & 0xC) == 0xC);
    lit_state(); T_EDP.known = 1; T_EDP.hobl = 1; nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 3;
    ARM("reduced-power panel: swing 3 has no row, so the training fails rather than guess", nmr_resume(&ops, &snap, T_VER) == NMR_E_TRAIN);
    ARM("a table with 9 rows offers swing 2 at most", nmr_select(T_VER, 270000, &T_EDP).n == 9);
    lit_state(); T_EDP.known = 0; T_EDP.hobl = 0; T_EDP.low_vswing = 1; nmr_snapshot(&ops, &snap, p, T_VER, &T_EDP); s3(); sink_needs_v = 2; sink_needs_p = 1;
    rc = nmr_resume(&ops, &snap, T_VER); phy_level(&dv, &dp);
    ARM("a panel the video BIOS did not identify keeps the DisplayPort table, whatever the flags say", rc == NMR_OK && nmr_select(T_VER, 270000, &T_EDP).t == nmr_select(T_VER, 270000, 0).t);
    ARM("above 5.4 Gb/s an eDP panel has a table of its own", ({ struct nmr_edp e = {1, 0, 0}; nmr_select(T_VER, 810000, &e).t != nmr_select(T_VER, 810000, 0).t; }));
    memset(&T_EDP, 0, sizeof T_EDP);
}

int main(void)
{
    static const struct { unsigned pipe, port, ver; } cases[] = { {0, 0, 12}, {1, 1, 12}, {3, 0, 13}, {2, 1, 13}, {0, 2, 12}, {2, 2, 12} };
    for (unsigned c = 0; c < sizeof cases / sizeof *cases; c++) {
        T_PIPE = cases[c].pipe; T_PORT = cases[c].port; T_VER = cases[c].ver;
        printf("-- panel on pipe %c, DDI %c, display version %u\n", 'A' + T_PIPE, 'A' + T_PORT, T_VER);
        suite();
        if (c == 0 || c == 2) { printf("-- the same panel with a video BIOS that names its drive table\n"); edp_suite(); }
    }
    T_PIPE = 0; T_PORT = 0; T_VER = 12;

    /* the drive tables and the register edit, direct */
    ARM("the DisplayPort level index covers exactly the ten valid pairs", nmr_level_index(0, 0) == 0 && nmr_level_index(1, 2) == 6 && nmr_level_index(2, 1) == 8 && nmr_level_index(3, 0) == 9 && nmr_level_index(3, 1) == -1 && nmr_level_index(2, 2) == -1 && nmr_level_index(4, 0) == -1);
    ARM("the table follows the generation and the link rate", nmr_table(12, 270000) == nmr_tgl_hbr && nmr_table(12, 540000) == nmr_tgl_hbr2 && nmr_table(13, 270000) == nmr_adlp_hbr && nmr_table(13, 810000) == nmr_adlp_hbr2);
    lit_state();
    { const uint32_t l1 = nmr_phy_base(0) + 0x880 + 0x100;
      *R(l1 + 8) = 0xFFFF0700u; *R(l1 + 16) = 0xFFFC0000u; *R(l1 + 28) = 0x80FFFFFFu; nw = 0;
      ARM("level 0 writes nothing", nmr_drive_apply(&ops, 0, 12, 540000, 0, 0, 0) == 0 && nw == 0);
      ARM("an invalid pair is refused and writes nothing", nmr_drive_apply(&ops, 0, 12, 540000, 0, 3, 2) == -1 && nw == 0);
      ARM("a level programs all four lanes", nmr_drive_apply(&ops, 0, 12, 540000, 0, 1, 2) == 0 && nw == 12);
      uint32_t d2 = *R(l1 + 8), d4 = *R(l1 + 16), d7 = *R(l1 + 28); const struct nmr_trans *t = &nmr_tgl_hbr2[6];
      ARM("DW2 holds the swing select and the rcomp scalar 0x98, the other bits kept", (((d2 >> 15) & 1) << 3 | ((d2 >> 11) & 7)) == t->sw && (d2 & 0xFF) == 0x98 && (d2 & 0xFFFF0000u) == 0xFFFF0000u && (d2 & 0x700u) == 0x700u);
      ARM("DW4 holds cursor, post-cursor 1 and 2, the other bits kept", (d4 & 0x3F) == t->cc && ((d4 >> 12) & 0x3F) == t->pc1 && ((d4 >> 6) & 0x3F) == 0 && (d4 & 0xFFFC0000u) == 0xFFFC0000u);
      ARM("DW7 holds the n-scalar, the other bits kept", ((d7 >> 24) & 0x7F) == t->ns && (d7 & 0x80FFFFFFu) == 0x80FFFFFFu); }
    printf("%d failed\n", fails);
    return fails != 0;
}

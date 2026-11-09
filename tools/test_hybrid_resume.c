/* S3 resume of an Intel laptop panel (kexts/NVRM/accel/nm_hybrid_resume.h) against a model of the display engine, the
 * panel power sequencer, the AUX channel and an eDP sink.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_resume.c -o /tmp/t && /tmp/t     -> 0 failed
 *   cc -DNAIVE_RESTORE ...   (a plain register restore: values written back, no handshakes, no link training)  -> failures
 * The model is ours, built from the register documentation in Linux's i915 headers; it checks the ORDER and the handshakes
 * the engine performs, not that a given panel lights (that needs hardware). */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "nm_hybrid_resume.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static uint32_t reg[NREG];
static uint8_t dpcd[0x800];
static struct { uint32_t off; uint32_t v; } wlog[4000]; static int nw;
static unsigned long now_us;
static int pw_fail, pll_fail, panel_fail, sink_deaf, train_fail_level, sink_needs_v, sink_needs_p, drive_v, drive_p, no_set_drive;
static int drive_calls;
static const char *steps[32]; static int nsteps;
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

static void aux_process(void)
{
    uint8_t m[20] = {0}; unsigned i, send = (*R(NMR_AUX_CTL) >> 20) & 0x1F;
    uint32_t ctl = *R(NMR_AUX_CTL) & ~((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));   /* status bits written as 1 are cleared */
    for (i = 0; i < send; i += 4) { uint32_t w = *R(NMR_AUX_DATA + i); for (unsigned j = 0; j < 4 && i + j < send; j++) m[i + j] = (uint8_t)(w >> (24 - 8 * j)); }
    int panel_on = (*R(NMR_PP_STATUS) & NMR_PP_ON_MASK) == NMR_PP_ON_VALUE;
    if (!(*R(NMR_PW_AUX_A) & 2u)) { *R(NMR_AUX_CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }   /* the AUX well is off */
    if (sink_deaf || !panel_on) { *R(NMR_AUX_CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }       /* time out */
    unsigned cmd = m[0] >> 4, len = m[3] + 1; uint32_t addr = ((m[0] & 0xF) << 16) | (m[1] << 8) | m[2];
    uint8_t rep[20] = {0};
    if (cmd == 8) {
        for (i = 0; i < len; i++) dpcd[addr + i] = m[4 + i];
        *R(NMR_AUX_CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (1u << 20);
        *R(NMR_AUX_DATA) = 0;
        return;
    }
    for (i = 0; i < len; i++) {
        uint32_t a = addr + i; uint8_t v = dpcd[a];
        if (a == 0x202 || a == 0x203 || a == 0x204) {
            unsigned pat = dpcd[0x102] & 3, srcpat = (*R(NMR_DP_TP_CTL) >> 8) & 7; int cr, eq;
            cr = pat >= 1 && srcpat == (pat == 1 ? 0u : 1u) && !train_fail_level && drive_v >= sink_needs_v && (dpcd[0x103] & 3) == drive_v;
            eq = cr && pat == 2 && drive_p >= sink_needs_p;
            if (a == 0x202 || a == 0x203) v = (uint8_t)((cr ? 1 : 0) | (eq ? 6 : 0) | (cr ? 0x10 : 0) | (eq ? 0x60 : 0));
            if (a == 0x204) v = (uint8_t)(eq ? 1 : 0);
        }
        if (a == 0x206 || a == 0x207) { uint8_t l = (uint8_t)((sink_needs_v & 3) | ((sink_needs_p & 3) << 2)); v = (uint8_t)(l | (l << 4)); }
        rep[1 + i] = v;
    }
    *R(NMR_AUX_CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | ((len + 1) << 20);
    for (i = 0; i < len + 1; i += 4) *R(NMR_AUX_DATA + i) = ((uint32_t)rep[i] << 24) | ((uint32_t)rep[i + 1] << 16) | ((uint32_t)rep[i + 2] << 8) | rep[i + 3];
}
static uint32_t m_rd(void *c, uint32_t o)
{
    (void)c;
    if (o == NMR_FRMCOUNT && (*R(NMR_TRANSCONF) & NMR_TRANS_ENABLE) && (*R(NMR_DDI_BUF_CTL) & NMR_DDI_ENABLE)) (*R(o))++;
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    (void)c;
    if (nw < 4000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    if (o == NMR_AUX_CTL) {
        if (v & (1u << 31)) { *R(o) = v; aux_process(); return; }
        *R(o) &= ~(v & ((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25))); return;
    }
    *R(o) = v;
    if (o == NMR_PW_DRIVER || o == NMR_PW_AUX_A || o == NMR_PW_DDI_A) { if (!pw_fail) *R(o) = v | ((v & 0x55555555u) << 1); }
    if (o == NMR_DE_PLL_ENABLE && (v & NMR_PLL_ENABLE) && !pll_fail) *R(o) |= NMR_PLL_LOCK;
    if (o == NMR_DPLL0_ENABLE) { if (v & NMR_PLL_POWER) *R(o) |= NMR_PLL_PSTATE; if ((v & NMR_PLL_ENABLE) && !pll_fail) *R(o) |= NMR_PLL_LOCK; }
    if (o == NMR_PP_CONTROL && (v & 1) && !panel_fail) *R(NMR_PP_STATUS) = NMR_PP_ON_VALUE;
    if (o == NMR_DP_TP_CTL && ((v >> 8) & 7) == 2) *R(NMR_DP_TP_STATUS) |= NMR_TP_IDLE_DONE;
}
static void m_delay(void *c, unsigned us) { (void)c; now_us += us; }
static void m_log(void *c, const char *s) { (void)c; if (nsteps < 32) steps[nsteps++] = s; }
static int m_drive(void *c, unsigned v, unsigned p) { (void)c; drive_calls++; if (no_set_drive) return -1; drive_v = (int)v; drive_p = (int)p; return 0; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, m_drive };

static void lit_state(void)
{
    memset(reg, 0, sizeof reg); memset(dpcd, 0, sizeof dpcd);
    reg[NMR_PW_DRIVER / 4] = 0x0000001Fu | 0x2Au; reg[NMR_PW_AUX_A / 4] = 3; reg[NMR_PW_DDI_A / 4] = 3;
    reg[NMR_DE_PLL_ENABLE / 4] = NMR_PLL_ENABLE | NMR_PLL_LOCK | 0x23; reg[NMR_CDCLK_CTL / 4] = 0x00000BB8;
    reg[NMR_DPLL0_ENABLE / 4] = NMR_PLL_POWER | NMR_PLL_PSTATE | NMR_PLL_ENABLE | NMR_PLL_LOCK;
    reg[NMR_DPLL0_CFGCR0 / 4] = 0x1234567; reg[NMR_DPLL0_CFGCR1 / 4] = 0x89AB; reg[NMR_DPLL0_DIV0 / 4] = 0x5; reg[NMR_DPCLKA_CFGCR0 / 4] = 0x3F0;
    for (unsigned i = 0; i < NMR_PHY_A_WORDS; i++) reg[NMR_PHY_A_BASE / 4 + i] = 0xC0DE0000u + i;
    reg[NMR_PP_CONTROL / 4] = 0x00000001u | 0x4; reg[NMR_PP_STATUS / 4] = NMR_PP_ON_VALUE;
    reg[NMR_PP_ON_DELAYS / 4] = 0x19001900; reg[NMR_PP_OFF_DELAYS / 4] = 0x01900190; reg[NMR_PP_DIVISOR / 4] = 0x0001E00A;
    for (unsigned i = 0; i < NMR_N_TIMING; i++) reg[nmr_timing_regs[i] / 4] = 0x1000 + i * 7;
    reg[NMR_TRANS_CLK_SEL / 4] = 0; reg[NMR_PIPE_MISC / 4] = 0x40; reg[NMR_WM_LINETIME / 4] = 0x1f4; reg[NMR_MSA_MISC / 4] = 0x21;
    for (unsigned i = 0; i < NMR_N_PLANE; i++) reg[nmr_plane_regs[i] / 4] = 0x8000u + i;
    for (unsigned i = 0; i < NMR_PLANE_WM_WORDS; i++) reg[NMR_PLANE_WM_BASE / 4 + i] = 0x80000000u + i;
    reg[NMR_PLANE_SURF / 4] = 0x00800000; reg[NMR_FUNC_CTL / 4] = 0x80000000u | (1u << 28);
    reg[NMR_DP_TP_CTL / 4] = NMR_TP_ENABLE | NMR_TP_NORMAL; reg[NMR_DDI_BUF_CTL / 4] = NMR_DDI_ENABLE | (1u << 1);
    reg[NMR_TRANSCONF / 4] = NMR_TRANS_ENABLE | 0x4; reg[NMR_BLC_PWM_CTL / 4] = 0x80000000u; reg[NMR_BLC_PWM_FREQ / 4] = 0x4B0; reg[NMR_BLC_PWM_DUTY / 4] = 0x258;
    dpcd[0x100] = 0x14; dpcd[0x101] = 0x82; dpcd[0x107] = 0x10; dpcd[0x108] = 1; dpcd[0x115] = 0;
    dpcd[0x002] = 0x82; dpcd[0x003] = 0x01; dpcd[0x00E] = 0; dpcd[0x600] = 1;
    drive_v = drive_p = 0; sink_needs_v = sink_needs_p = 0; train_fail_level = 0; pw_fail = pll_fail = panel_fail = sink_deaf = no_set_drive = 0;
}
static void s3(void)   /* the display engine and the sink lose everything */
{
    memset(reg, 0, sizeof reg); memset(dpcd, 0, sizeof dpcd); dpcd[0x002] = 0x82; dpcd[0x003] = 0x01; drive_v = drive_p = 0; nw = 0; nsteps = 0; drive_calls = 0;
}
static int first_write(uint32_t off) { for (int i = 0; i < nw; i++) if (wlog[i].off == off) return i; return -1; }
static int last_write(uint32_t off) { int r = -1; for (int i = 0; i < nw; i++) if (wlog[i].off == off) r = i; return r; }

#ifdef NAIVE_RESTORE
/* what a plain register restore does: write the captured values back in table order, no handshakes, no sink */
static int naive_resume(const struct nmr_ops *o, const struct nmr_snap *s, unsigned v)
{
    (void)v; unsigned i;
    o->wr(o->ctx, NMR_PW_DRIVER, s->pw[0]); o->wr(o->ctx, NMR_DPLL0_CFGCR0, s->cfgcr0); o->wr(o->ctx, NMR_DPLL0_ENABLE, s->dpll_en);
    for (i = 0; i < NMR_N_TIMING; i++) o->wr(o->ctx, nmr_timing_regs[i], s->timing[i]);
    o->wr(o->ctx, NMR_PLANE_SURF, s->surf); o->wr(o->ctx, NMR_FUNC_CTL, s->func);
    o->wr(o->ctx, NMR_DDI_BUF_CTL, s->ddi); o->wr(o->ctx, NMR_TRANSCONF, s->transconf);
    return NMR_OK;
}
#define nmr_resume naive_resume
#endif

int main(void)
{
    static struct nmr_snap snap; static uint32_t want[NREG];
    lit_state(); memcpy(want, reg, sizeof want);
    ARM("a lit panel is captured", nmr_snapshot(&ops, &snap) == NMR_OK && snap.valid);
    ARM("the sink's link settings are captured over the AUX channel", snap.dpcd[0] == 0x14 && snap.dpcd[1] == 0x82 && snap.have_sink);
    ARM("a panel that uses the AUX handshake is recognised", !snap.no_handshake);
    lit_state(); reg[NMR_PW_AUX_A / 4] = 0; { static struct nmr_snap s2; ARM("a firmware that left the AUX well off still gets its sink captured", nmr_snapshot(&ops, &s2) == NMR_OK && s2.have_sink && reg[NMR_PW_AUX_A / 4] == 0); }
    lit_state(); nmr_snapshot(&ops, &snap);

    s3(); int rc = nmr_resume(&ops, &snap, 12);
    ARM("resume after S3 lights the panel", rc == NMR_OK);
    { int ok = 1; uint32_t chk[] = { NMR_DPLL0_CFGCR0, NMR_DPLL0_CFGCR1, NMR_DPCLKA_CFGCR0, NMR_PHY_A_BASE, NMR_PHY_A_BASE + 4 * 255, NMR_PP_ON_DELAYS,
        0x60000, 0x6001C, 0x60044, NMR_PLANE_SURF, 0x70188, 0x70240, 0x7027C, NMR_FUNC_CTL, NMR_PIPE_MISC, NMR_BLC_PWM_DUTY, NMR_CDCLK_CTL };
      for (unsigned i = 0; i < sizeof chk / sizeof *chk; i++) if (reg[chk[i] / 4] != want[chk[i] / 4]) { ok = 0; printf("   differs: 0x%x\n", chk[i]); }
      ARM("every captured configuration register is back to its firmware value", ok); }
    ARM("the transcoder and the DDI buffer are running", (reg[NMR_TRANSCONF / 4] & NMR_TRANS_ENABLE) && (reg[NMR_DDI_BUF_CTL / 4] & NMR_DDI_ENABLE));
    ARM("the link ends in normal operation", ((reg[NMR_DP_TP_CTL / 4] >> 8) & 7) == 3);
    ARM("the sink is told the link settings and powered to D0", dpcd[0x100] == 0x14 && dpcd[0x101] == 0x82 && dpcd[0x600] == 1);
    ARM("the training pattern is switched off at the sink afterwards", dpcd[0x102] == 0);
    { int pw = first_write(NMR_PW_DRIVER), cd = first_write(NMR_DE_PLL_ENABLE), pl = first_write(NMR_DPLL0_ENABLE), ph = first_write(NMR_PHY_A_BASE),
          pp = first_write(NMR_PP_CONTROL), aux = first_write(NMR_AUX_CTL), tim = first_write(0x60000), sf = last_write(NMR_PLANE_SURF),
          pl7 = last_write(0x70190), fn = first_write(NMR_FUNC_CTL), dd = first_write(NMR_DDI_BUF_CTL), tc = first_write(NMR_TRANSCONF), bl = first_write(NMR_BLC_PWM_DUTY);
      ARM("order: power wells, display clock, DPLL, PHY block, panel power", pw < cd && cd < pl && pl < ph && ph < pp);
      ARM("order: the AUX channel is used only after the panel is powered", aux > pp);
      ARM("order: timing and plane registers, the surface register after the other plane registers", tim > aux && sf > pl7);
      ARM("order: the transcoder function and the DDI buffer after the pipe registers, the transcoder last of the three", fn > sf && dd > fn && tc > dd);
      ARM("order: the backlight only after the transcoder is on", bl > tc); }
    { const char *want_steps[] = { "power wells", "display clock", "DPLL", "combo PHY", "panel power", "sink power", "pipe and plane registers",
                                   "transcoder function and DDI buffer", "link training", "link training: clock recovery done", "transcoder on", "backlight" };
      int ok = nsteps == 12; for (int i = 0; ok && i < 12; i++) if (strcmp(steps[i], want_steps[i])) ok = 0;
      ARM("each step is logged in order (these are the lines a tester sends)", ok); }

    /* a panel that is still lit is left alone */
    lit_state(); nmr_snapshot(&ops, &snap); nw = 0;
    ARM("a panel that is still lit is not touched", nmr_resume(&ops, &snap, 12) == NMR_ALREADY_LIT && nw == 0);

    /* refusals */
    s3(); ARM("display version 14 is refused with no register written", nmr_resume(&ops, &snap, 14) == NMR_E_GEN && nw == 0);
    { static struct nmr_snap none; memset(&none, 0, sizeof none); ARM("no snapshot, no resume, nothing written", nmr_resume(&ops, &none, 12) == NMR_E_NO_SNAPSHOT && nw == 0); }
    memset(reg, 0, sizeof reg);
    ARM("a dark panel is not captured", nmr_snapshot(&ops, &snap) == NMR_E_NO_SNAPSHOT && !snap.valid);

    /* failures stop at the first handshake that does not come */
    lit_state(); nmr_snapshot(&ops, &snap); s3(); pw_fail = 1;
    ARM("a power well that never acknowledges stops the resume", nmr_resume(&ops, &snap, 12) == NMR_E_POWER);
    ARM("...before any PLL, panel or DDI register is written", first_write(NMR_DPLL0_ENABLE) < 0 && first_write(NMR_PP_CONTROL) < 0 && first_write(NMR_DDI_BUF_CTL) < 0);
    lit_state(); nmr_snapshot(&ops, &snap); s3(); pll_fail = 1;
    ARM("a display clock PLL that never locks stops the resume", nmr_resume(&ops, &snap, 12) == NMR_E_CDCLK && first_write(NMR_DDI_BUF_CTL) < 0);
    lit_state(); nmr_snapshot(&ops, &snap); s3(); panel_fail = 1;
    ARM("a panel power sequence that never completes stops the resume", nmr_resume(&ops, &snap, 12) == NMR_E_PANEL && first_write(NMR_DDI_BUF_CTL) < 0 && first_write(NMR_TRANSCONF) < 0);
    lit_state(); nmr_snapshot(&ops, &snap); s3(); sink_deaf = 1;
    ARM("a sink that does not answer on AUX stops the resume", nmr_resume(&ops, &snap, 12) == NMR_E_SINK && first_write(NMR_DDI_BUF_CTL) < 0);

    /* link training */
    lit_state(); nmr_snapshot(&ops, &snap); s3(); sink_needs_v = 2; sink_needs_p = 1;
    ARM("a sink that asks for more swing and pre-emphasis is given it", nmr_resume(&ops, &snap, 12) == NMR_OK && drive_v == 2 && drive_p == 1 && drive_calls >= 1);
    lit_state(); nmr_snapshot(&ops, &snap); s3(); sink_needs_v = 2; no_set_drive = 1;
    ARM("when the driver cannot raise the swing the training fails", nmr_resume(&ops, &snap, 12) == NMR_E_TRAIN);
    ARM("a failed training turns the DDI buffer off and leaves the transcoder off", !(reg[NMR_DDI_BUF_CTL / 4] & NMR_DDI_ENABLE) && !(reg[NMR_TRANSCONF / 4] & NMR_TRANS_ENABLE));
    lit_state(); nmr_snapshot(&ops, &snap); s3(); train_fail_level = 1; now_us = 0;
    ARM("a sink that never reports clock recovery fails within a bounded time", nmr_resume(&ops, &snap, 12) == NMR_E_TRAIN && now_us < 5000000ul);
    lit_state(); dpcd[0x003] = 0x41; nmr_snapshot(&ops, &snap);
    ARM("a panel flagged as not using the AUX handshake is recognised", snap.no_handshake);
    s3(); train_fail_level = 1;
    ARM("such a panel is not trained over AUX, so a sink that cannot train still relights", nmr_resume(&ops, &snap, 12) == NMR_OK && (reg[NMR_DDI_BUF_CTL / 4] & NMR_DDI_ENABLE));
    ARM("the switch is boot-arg nmhybridresume=1 only", nmr_enabled(1, 1) && !nmr_enabled(1, 2) && !nmr_enabled(0, 1) && !nmr_enabled(1, 0));

    /* AUX framing */
    lit_state(); *R(NMR_PP_STATUS) = NMR_PP_ON_VALUE; dpcd[0x240] = 0xAA; dpcd[0x241] = 0x55;
    { uint8_t b[2] = {0, 0}; int n = nmr_dpcd_rd(&ops, 0x240, b, 2);
      ARM("an AUX read returns the sink's bytes in order", n == 2 && b[0] == 0xAA && b[1] == 0x55); }
    { uint8_t v = 0x7E; int n = nmr_dpcd_wr(&ops, 0x300, &v, 1); ARM("an AUX write lands at its address", n == 1 && dpcd[0x300] == 0x7E); }
    { uint8_t big[17] = {0}; ARM("a 17-byte transfer is refused", nmr_dpcd_rd(&ops, 0x240, big, 17) == -1); }
    printf("%d failed\n", fails);
    return fails != 0;
}

/* Bringing up an external Type-C display, and taking it down (kexts/NVRM/accel/nm_hybrid_extmode.h), against a model of the Type-C subsystem, the
 * power wells, the sink (EDID, DPCD, link training), the Dekel PHY and the display engine's registers. Includes the unplug at every step and in the
 * middle of link training.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_extmode.c -o /tmp/t -lm && /tmp/t     -> 0 failed
 * The model follows Linux's i915; it checks the ORDER, the handshakes, and that a failure leaves nothing enabled. It cannot show that a given dock
 * and monitor work. */
#include <stdio.h>
#include <string.h>
#include "nm_hybrid_extmode.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static uint32_t reg[NREG];
static uint32_t dkl[6][16][0x1000 / 4];
static uint8_t edid[128], dpcd[0x800];
static unsigned VER = 13, TC = 1, PIPE = 1, DDI;
static unsigned edid_ptr;
static struct { uint32_t off, v; } wlog[20000]; static int nw;
static int blocked, tccold_asleep, iom_ready, exit_fail, pw_fail, pll_lock_fail, pll_pwr_fail, train_fail, aux_deaf, plugged, aux_count, unplug_after_aux, sink_needs_v, sink_needs_p, lat_fail;
static int cable_lanes;
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

static void unplug(void)
{
    plugged = 0;
    if (VER == 12) { *R(NMT_DFLEXDPSP(TC / 2)) &= ~(NMT_LIVE_TC(TC % 2) | NMT_LIVE_TBT(TC % 2)); } else { *R(NMT_DE_HPD_ISR) &= ~((1u << (16 + TC)) | (1u << TC)); }
}
static void plug(unsigned mode, unsigned lanes)
{
    const unsigned fia = TC / 2, idx = TC % 2;
    plugged = 1; cable_lanes = (int)lanes;
    *R(NMT_DFLEXDPSP(fia)) = (*R(NMT_DFLEXDPSP(fia)) & ~(0xFFu << (idx * 8))) | (lanes << (idx * 8)) | NMT_MODULAR_FIA;
    if (VER == 12) { if (mode == NMT_DPALT) *R(NMT_DFLEXDPSP(fia)) |= NMT_LIVE_TC(idx); if (mode == NMT_TBT) *R(NMT_DFLEXDPSP(fia)) |= NMT_LIVE_TBT(idx); }
    else { *R(NMT_DE_HPD_ISR) &= ~((1u << (16 + TC)) | (1u << TC)); if (mode == NMT_DPALT) *R(NMT_DE_HPD_ISR) |= 1u << (16 + TC); if (mode == NMT_TBT) *R(NMT_DE_HPD_ISR) |= 1u << TC; }
    *R(NMT_DFLEXDPPMS(fia)) = iom_ready ? NMT_PMS_READY(idx) : 0;
}
static int is_fia_reg(uint32_t o) { return (o >= 0x163000 && o < 0x164000) || (o >= 0x16E000 && o < 0x170000); }

static void level_now(unsigned *v, unsigned *p)
{
    const struct nmk_trans *tab = nmk_table(VER, (uint32_t)dpcd[0x100] * 27000u);
    uint32_t d = dkl[TC][NMK_REG_BANK(NMK_TX_DPCNTL0(0))][NMK_REG_OFF(NMK_TX_DPCNTL0(0)) / 4];
    unsigned vv, pp; *v = *p = 0;
    for (vv = 0; vv < 4; vv++) for (pp = 0; pp + vv < 4; pp++) {
        int idx = nmr_level_index(vv, pp);
        if (idx >= 0 && tab[idx].vswing == (d & 7) && tab[idx].de_emphasis == ((d >> 8) & 0x1F)) { *v = vv; *p = pp; }
    }
}
static void aux_process(void)
{
    const uint32_t CTL = NMR_AUX_CTL(DDI), DATA = NMR_AUX_DATA(DDI);
    uint8_t m[20] = {0}, rep[20] = {0}; unsigned i, send = (*R(CTL) >> 20) & 0x1F, cmd, len, nrep; uint32_t addr;
    uint32_t ctl = *R(CTL) & ~((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));
    for (i = 0; i < send; i += 4) { uint32_t w = *R(DATA + i); for (unsigned j = 0; j < 4 && i + j < send; j++) m[i + j] = (uint8_t)(w >> (24 - 8 * j)); }
    aux_count++;
    if (unplug_after_aux && aux_count == unplug_after_aux) unplug();
    if (aux_deaf || !plugged || !(*R(NMO_PW_AUX_DRIVER) & (2u << (2 * (3 + TC))))) { *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }
    cmd = m[0] >> 4; len = m[3] + 1; addr = ((m[0] & 0xF) << 16) | (m[1] << 8) | m[2]; nrep = 1;
    if (cmd == 4 || cmd == 0) { if (addr == 0x50) edid_ptr = m[4]; }                               /* I2C write: the offset */
    else if (cmd == 5 || cmd == 1) { for (i = 0; i < len; i++) rep[1 + i] = addr == 0x50 ? edid[(edid_ptr + i) & 127] : 0; edid_ptr += len; nrep = len + 1; }
    else if (cmd == 8) { for (i = 0; i < len; i++) dpcd[addr + i] = m[4 + i]; }                      /* native write */
    else {                                                                                            /* native read */
        for (i = 0; i < len; i++) {
            uint32_t a = addr + i; uint8_t v = dpcd[a];
            if (a == 0x202 || a == 0x203 || a == 0x204) {
                unsigned pat = dpcd[0x102] & 3, srcpat = (*R(NMR_DP_TP_CTL(PIPE)) >> 8) & 7, dv, dp; int cr, eq;
                level_now(&dv, &dp);
                cr = pat >= 1 && srcpat == (pat == 1 ? 0u : 1u) && !train_fail && dv >= (unsigned)sink_needs_v && (dpcd[0x103] & 3) == dv && (*R(NMR_DDI_BUF_CTL(DDI)) & NMR_DDI_ENABLE);
                eq = cr && pat == 2 && dp >= (unsigned)sink_needs_p;
                if (a == 0x202 || a == 0x203) v = (uint8_t)((cr ? 1 : 0) | (eq ? 6 : 0) | (cr ? 0x10 : 0) | (eq ? 0x60 : 0));
                if (a == 0x204) v = (uint8_t)(eq ? 1 : 0);
            }
            if (a == 0x206 || a == 0x207) { uint8_t l = (uint8_t)((sink_needs_v & 3) | ((sink_needs_p & 3) << 2)); v = (uint8_t)(l | (l << 4)); }
            rep[1 + i] = v;
        }
        nrep = len + 1;
    }
    *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (nrep << 20);
    for (i = 0; i < nrep; i += 4) *R(DATA + i) = ((uint32_t)rep[i] << 24) | ((uint32_t)rep[i + 1] << 16) | ((uint32_t)rep[i + 2] << 8) | rep[i + 3];
}
static uint32_t lat[2] = { 0x1B140C03u, 0x46382D21u };
static uint32_t m_rd(void *c, uint32_t o)
{
    unsigned tc;
    (void)c;
    if (VER == 12 && is_fia_reg(o) && tccold_asleep && !blocked) return 0xFFFFFFFFu;
    if (VER == 13 && o >= 0x64000 && o < 0x64800 && 0) return 0xFFFFFFFFu;
    if (VER == 13 && o >= 0x161500 && o < 0x161520) return *R(o) | (iom_ready ? NMT_TCSS_READY : 0);
    for (tc = 0; tc < 6; tc++) if (o >= NMK_PHY_BASE(tc) && o < NMK_PHY_BASE(tc) + 0x1000) return dkl[tc][(*R(NMK_HIP_INDEX(tc)) >> (8 * (tc % 4))) & 15][(o - NMK_PHY_BASE(tc)) / 4];
    if (o == NMR_FRMCOUNT(PIPE) && (*R(NMR_TRANSCONF(PIPE)) & NMR_TRANS_ENABLE) && (*R(NMR_DDI_BUF_CTL(DDI)) & NMR_DDI_ENABLE)) (*R(o))++;
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    unsigned tc;
    (void)c;
    if (nw < 20000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    for (tc = 0; tc < 6; tc++) if (o >= NMK_PHY_BASE(tc) && o < NMK_PHY_BASE(tc) + 0x1000) { dkl[tc][(*R(NMK_HIP_INDEX(tc)) >> (8 * (tc % 4))) & 15][(o - NMK_PHY_BASE(tc)) / 4] = v; return; }
    if (o == NMR_PCODE_MAILBOX) {
        if (v & NMR_PCODE_READY) {
            unsigned cmd = v & 0xFF;
            if (cmd == NMT_PCODE_TCCOLD) { if (!(*R(NMR_PCODE_DATA) & 1u)) { if (!exit_fail) blocked = 1; *R(NMR_PCODE_DATA) = exit_fail ? 1u : 0u; } else { blocked = 0; *R(NMR_PCODE_DATA) = 0; } }
            else if (cmd == NMW_READ_MEM_LATENCY) *R(NMR_PCODE_DATA) = lat[*R(NMR_PCODE_DATA) & 1];
            *R(o) = (v & ~NMR_PCODE_READY & ~0xFFu) | (cmd == NMW_READ_MEM_LATENCY && lat_fail ? 1u : 0u); return; }
        *R(o) = v; return; }
    if (o == NMR_AUX_CTL(DDI)) { *R(o) = v; if (v & (1u << 31)) aux_process(); return; }
    if (o == NMO_PW_DDI_DRIVER || o == NMO_PW_AUX_DRIVER) { *R(o) = v; if (!pw_fail) *R(o) = v | ((v & 0x55555555u) << 1); else *R(o) = v & 0x55555555u; if (!(v & 0x55555555u)) *R(o) = 0; return; }
    for (tc = 0; tc < 6; tc++) {
        const uint32_t en = NMK_PLL_ENABLE_REG(VER, tc);
        if (o == en) { uint32_t nv = v & ~(NMK_PLL_PWR_STATE | NMK_PLL_LOCK); if ((v & NMK_PLL_PWR_ENABLE) && !pll_pwr_fail) nv |= NMK_PLL_PWR_STATE; if ((v & NMK_PLL_ENABLE) && !pll_lock_fail && (dkl[tc][NMK_REG_BANK(NMK_PLL_DIV0)][NMK_REG_OFF(NMK_PLL_DIV0) / 4] & 0xFF)) nv |= NMK_PLL_LOCK; *R(o) = nv; return; }
    }
    if (o == NMR_DDI_BUF_CTL(DDI)) { *R(o) = v; return; }
    *R(o) = v;
    if (o == NMR_DP_TP_CTL(PIPE) && ((v >> 8) & 7) == 2) *R(NMR_DP_TP_STATUS(PIPE)) |= NMR_TP_IDLE_DONE;
}
static void m_delay(void *c, unsigned us) { (void)c; (void)us; }
static void m_log(void *c, const char *s) { (void)c; (void)s; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, 0, 0, 0 };
static int first_write(uint32_t off) { for (int i = 0; i < nw; i++) if (wlog[i].off == off) return i; return -1; }
static int last_write(uint32_t off) { int r = -1; for (int i = 0; i < nw; i++) if (wlog[i].off == off) r = i; return r; }

static void make_edid(void)
{
    unsigned i, s = 0;
    static const uint8_t dtd[18] = { 0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0xC4, 0x8E, 0x21, 0x00, 0x00, 0x1E };
    memset(edid, 0, sizeof edid); edid[1] = edid[2] = edid[3] = edid[4] = edid[5] = edid[6] = 0xFF; memcpy(edid + 54, dtd, 18);
    for (i = 0; i < 127; i++) s += edid[i];
    edid[127] = (uint8_t)(256 - (s & 255));
}
static void world(unsigned ver)
{
    VER = ver; DDI = 3 + TC;
    memset(reg, 0, sizeof reg); memset(dkl, 0, sizeof dkl); memset(dpcd, 0, sizeof dpcd); nw = 0;
    make_edid(); edid_ptr = 0;
    blocked = 0; tccold_asleep = (ver == 12); iom_ready = 1; exit_fail = pw_fail = pll_lock_fail = pll_pwr_fail = train_fail = aux_deaf = aux_count = unplug_after_aux = sink_needs_v = sink_needs_p = lat_fail = 0;
    dpcd[0] = 0x14; dpcd[1] = 0x14; dpcd[2] = 0x84; dpcd[0x0E] = 0; dpcd[0x600] = 2;                  /* DPCD 1.4, 5.4 Gb/s, 4 lanes with enhanced framing */
    lat[0] = 0x1B140C03u; lat[1] = 0x46382D21u;
    if (ver == 12) *R(NMT_DFLEXDPSP(0)) = NMT_MODULAR_FIA;
    *R(NMK_DPCLKA_CFGCR0) = 0xFFFFFFFFu;
    plug(NMT_DPALT, 0xF);
}
static int all_off(unsigned pipe, unsigned ddi, unsigned tc)
{
    const uint32_t en = NMK_PLL_ENABLE_REG(VER, tc);
    return !(*R(NMR_DDI_BUF_CTL(ddi)) & NMR_DDI_ENABLE) && !(*R(NMR_TRANSCONF(pipe)) & NMR_TRANS_ENABLE) && *R(NMR_FUNC_CTL(pipe)) == 0 && *R(NMR_PLANE(pipe, 0)) == 0 && *R(NMR_SURF(pipe)) == 0 &&
           !(*R(en) & (NMK_PLL_ENABLE | NMK_PLL_PWR_ENABLE)) && *R(NMK_PORT_CLK_SEL(ddi)) == 0 && *R(NMK_TRANS_CLK_SEL(pipe)) == 0 && !blocked &&
           !(*R(NMO_PW_DDI_DRIVER) & (1u << (2 * (3 + tc)))) && !(*R(NMO_PW_AUX_DRIVER) & (1u << (2 * (3 + tc)))) &&
           (VER == 12 ? !(*R(NMT_DFLEXDPCSSS(tc / 2)) & NMT_CSSS_NOT_SAFE(tc % 2)) : !(*R(NMR_DDI_BUF_CTL(ddi)) & NMT_DDI_OWNER)) && (*R(NMK_DPCLKA_CFGCR0) & NMK_TC_CLK_OFF(tc));
}

static void suite(unsigned ver)
{
    struct nmo m; int rc; const unsigned ddb_s = 1024, ddb_e = 2048; const uint32_t surf = 0x00400000u, pitch = 1920 * 4, cap = 1920 * 4 * 1080;
    printf("-- display version %u, Type-C port %u, pipe %c\n", ver, TC + 1, 'A' + PIPE);
    world(ver);
    ARM("init finds the FIA and names the DDI", nmo_init(&ops, &m, ver, TC, PIPE, 38400) == 0 && m.ddi == 3 + TC);
    rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e);
    if (rc) printf("   rc %d: %s\n", rc, nmo_err_name(rc));
    ARM("a DisplayPort-alt monitor comes up", rc == 0 && m.state == NMO_UP);
    ARM("the mode is the EDID's 1080p60 on the cheapest link (1.62 Gb/s, 4 lanes)", m.t.hactive == 1920 && m.t.clock_khz == 148500 && m.rate == 162000 && m.lanes == 4);
    ARM("the transcoder and DDI buffer run, and the PHY is owned by display", (*R(NMR_TRANSCONF(PIPE)) & NMR_TRANS_ENABLE) && (*R(NMR_DDI_BUF_CTL(DDI)) & NMR_DDI_ENABLE) && m.tc.mode == NMT_DPALT && blocked);
    ARM("the plane scans out the surface: XRGB8888, stride in 64-byte units, size, surface last", *R(NMR_PLANE(PIPE, 0)) == ((1u << 31) | (4u << 24) | (1u << 13)) && *R(NMR_PLANE(PIPE, 1)) == pitch / 64 && *R(NMR_PLANE(PIPE, 3)) == ((1079u << 16) | 1919u) &&
        *R(NMR_SURF(PIPE)) == surf && last_write(NMR_SURF(PIPE)) > last_write(NMR_PLANE(PIPE, 3)) && last_write(NMR_SURF(PIPE)) > last_write(NMR_PLANE(PIPE, 0)));
    ARM("the display-buffer range sits above the other pipe's and level 0 of the watermarks is on", *R(0x7027Cu + 0x1000u * PIPE) == nmw_buf_cfg(ddb_s, ddb_e) && (*R(NMR_PLANE_WM_BASE(PIPE)) & (1u << 31)));
    ARM("the Dekel PLL is locked at the 1.62 Gb/s dividers and the port clock is the PLL's", (*R(NMK_PLL_ENABLE_REG(ver, TC)) & NMK_PLL_LOCK) && *R(NMK_PORT_CLK_SEL(DDI)) == (0x8u << 28) && *R(NMK_TRANS_CLK_SEL(PIPE)) == ((DDI + 1u) << 28));
    ARM("the sink was told to power up and the link settings are 1.62 Gb/s x 4 with enhanced framing", dpcd[0x600] == 1 && dpcd[0x100] == 6 && dpcd[0x101] == 0x84);
    { int pw = first_write(NMO_PW_DDI_DRIVER), pl = first_write(NMK_PLL_ENABLE_REG(ver, TC)), pe = first_write(NMR_FUNC_CTL(PIPE)), db = -1, tr = last_write(NMR_TRANSCONF(PIPE)), cs = first_write(NMK_PORT_CLK_SEL(DDI));
      { int k; for (k = 0; k < nw; k++) if (wlog[k].off == NMR_DDI_BUF_CTL(DDI) && (wlog[k].v & NMR_DDI_ENABLE)) { db = k; break; } }
      ARM("order: power wells, PLL, clocks, pipe, DDI buffer, transcoder", pw >= 0 && pw < pl && pl < cs && cs < pe && pe < db && db < tr); }
    ARM("the internal panel's backlight and panel power registers are never written", first_write(NMR_BLC_PWM_CTL) < 0 && first_write(NMR_BLC_PWM_DUTY) < 0 && first_write(NMR_PP_CONTROL) < 0);
    ARM("the pitch follows the mode: 1920 pixels x 4 bytes", m.pitch == pitch);
    ARM("a second enable while up is refused", nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e) == NMO_E_BUSY);
    unplug(); { uint32_t last = 0xFFFFFFFFu; (void)last; }
    nmo_event(&ops, &m, 2, 0, 0, 0, 0);
    ARM("an unplug takes everything down in reverse", m.state == NMO_IDLE && all_off(PIPE, DDI, TC));
    { int tr = last_write(NMR_TRANSCONF(PIPE)), db = last_write(NMR_DDI_BUF_CTL(DDI)), pe = last_write(NMR_FUNC_CTL(PIPE)), pl = last_write(NMK_PLL_ENABLE_REG(ver, TC)), pw = last_write(NMO_PW_DDI_DRIVER);
      { int k; db = -1; for (k = tr + 1; k < nw; k++) if (wlog[k].off == NMR_DDI_BUF_CTL(DDI)) { db = k; break; } }
      ARM("teardown order: transcoder, DDI buffer, pipe, PLL, power", tr < db && db < pe && pe < pl && pl < pw); }

    /* replug works again */
    world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); plug(NMT_DPALT, 0xF);
    ARM("a plug event brings it up through the event entry", nmo_event(&ops, &m, 1, surf, cap, ddb_s, ddb_e) == 0 && m.state == NMO_UP);
    nmo_event(&ops, &m, 1, surf, cap, ddb_s, ddb_e); ARM("a second plug event while up changes nothing", m.state == NMO_UP);
    nmo_teardown(&ops, &m); nmo_teardown(&ops, &m); ARM("teardown twice is harmless", all_off(PIPE, DDI, TC));

    /* failures, each leaving nothing enabled */
#define FAIL_CASE(name, setup, want) do { world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); setup; rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e); \
        ARM(name, rc == (want) && m.state == NMO_FAILED && m.err == (want) && all_off(PIPE, DDI, TC)); } while (0)
    FAIL_CASE("a Thunderbolt cable is not driven here, and nothing is left owned", plug(NMT_TBT, 0), NMO_E_PHY);
    FAIL_CASE("a PHY that is not ready connects as Thunderbolt and is refused", iom_ready = 0; plug(NMT_DPALT, 0xF), NMO_E_PHY);
    world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); exit_fail = 1; rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e);
    ARM("a platform controller that refuses the TC-cold block fails the PHY step (on version 12 the cable cannot even be read without it)", (rc == NMO_E_PHY || (ver == 12 && rc == NMO_E_UNPLUGGED)) && m.state == NMO_FAILED && all_off(PIPE, DDI, TC));
    FAIL_CASE("a power well that never comes up", pw_fail = 1, NMO_E_POWER);
    FAIL_CASE("a sink that does not answer on AUX", aux_deaf = 1, NMO_E_SINK);
    FAIL_CASE("an EDID with a bad checksum", edid[20] ^= 1, NMO_E_EDID);
    FAIL_CASE("a cable with two lanes cannot carry a mode that needs four", plug(NMT_DPALT, 0x3); dpcd[1] = 0x06; dpcd[2] = 0x82; edid[54] = 0x02, NMO_E_MODE) ;
    FAIL_CASE("memory latencies the platform controller will not give", lat_fail = 1, NMO_E_MODE);
    world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); rc = nmo_enable(&ops, &m, surf, cap, 0, 8);
    ARM("a display-buffer range too small for level 0 would underrun: refused before anything is programmed", rc == NMO_E_MODE && all_off(PIPE, DDI, TC) && first_write(NMR_FUNC_CTL(PIPE)) < 0 && first_write(NMK_PLL_ENABLE_REG(ver, TC)) < 0);
    world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); rc = nmo_enable(&ops, &m, surf, cap - 4096, ddb_s, ddb_e);
    ARM("a scan-out buffer too small for the preferred mode is refused before the PLL is touched", rc == NMO_E_MODE && all_off(PIPE, DDI, TC) && first_write(NMK_PLL_ENABLE_REG(ver, TC)) < 0);
    FAIL_CASE("a PLL that never powers", pll_pwr_fail = 1, NMO_E_PLL);
    FAIL_CASE("a PLL that never locks", pll_lock_fail = 1, NMO_E_PLL);
    FAIL_CASE("a sink that never trains", train_fail = 1, NMO_E_TRAIN);
    /* unplug at every AUX transaction: wherever it lands, the result is an unplug and nothing stays enabled */
    { unsigned n, ok = 1, seen = 0, total; 
      world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); sink_needs_v = 1; sink_needs_p = 1; nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e); total = (unsigned)aux_count;
      for (n = 1; n < total; n++) {
          world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); sink_needs_v = 1; sink_needs_p = 1; unplug_after_aux = (int)n; aux_count = 0;
          rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e);
          if (rc != NMO_E_UNPLUGGED || m.state != NMO_FAILED || !all_off(PIPE, DDI, TC)) { ok = 0; printf("   unplug at AUX transaction %u of %u: rc %d (%s), clean %d\n", n, total, rc, nmo_err_name(rc), all_off(PIPE, DDI, TC)); }
          else seen++; }
      ARM("an unplug at any of the AUX transactions (EDID, DPCD, link training included) ends as 'unplugged' with nothing left enabled", ok && seen == total - 1 && total > 20); }
    { unsigned mid;
      world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); sink_needs_v = 2; sink_needs_p = 1; nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e);
      mid = (unsigned)aux_count - 4;
      world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); sink_needs_v = 2; sink_needs_p = 1; unplug_after_aux = (int)mid; aux_count = 0;
      rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e);
      ARM("an unplug in the middle of equalisation: the DDI buffer is switched off before the PLL, and the transcoder never came on", rc == NMO_E_UNPLUGGED && all_off(PIPE, DDI, TC) && (*R(NMR_TRANSCONF(PIPE)) & NMR_TRANS_ENABLE) == 0); }
    /* training reaches the sink's level request */
    world(ver); nmo_init(&ops, &m, ver, TC, PIPE, 38400); sink_needs_v = 2; sink_needs_p = 1;
    { unsigned dv, dp; rc = nmo_enable(&ops, &m, surf, cap, ddb_s, ddb_e); level_now(&dv, &dp); ARM("the sink's swing and pre-emphasis request reaches the Dekel transmitter registers", rc == 0 && dv == 2 && dp == 1); }
    nmo_teardown(&ops, &m);
}

int main(void)
{
    TC = 1; PIPE = 1; suite(13);
    TC = 2; PIPE = 2; suite(12);
    printf("%d failed\n", fails);
    return fails != 0;
}

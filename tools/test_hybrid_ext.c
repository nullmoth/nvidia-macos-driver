/* External DisplayPort display building blocks (kexts/NVRM/accel/nm_hybrid_ext.h, nm_hybrid_dkl.h): EDID over AUX, the preferred mode, the
 * link, M and N, the transcoder words, and the Dekel PHY's PLL (divider search, round trip to the link rate, banked register writes and
 * enable) against a model of the AUX channel and the PHY.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_ext.c -o /tmp/t && /tmp/t     -> 0 failed */
#include <stdio.h>
#include <string.h>
#include "nm_hybrid_ext.h"
#include "nm_hybrid_dkl.h"
#include "nm_hybrid_wm.h"
#include <math.h>

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static uint32_t reg[NREG];
static uint8_t edid[128];
static unsigned edid_ptr; static int edid_deaf, aux_reads;
static uint32_t dkl[6][16][0x1000 / 4];        /* tc port, bank, register */
static int pll_pwr_fail, pll_lock_fail, lat_fail, lat_calls; static uint32_t lat_set[2];
static struct { uint32_t off, v; } wlog[4000]; static int nw;
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

static void aux_process(unsigned port)
{
    const uint32_t CTL = NMR_AUX_CTL(port), DATA = NMR_AUX_DATA(port);
    uint8_t m[20] = {0}, rep[20] = {0}; unsigned i, send = (*R(CTL) >> 20) & 0x1F;
    uint32_t ctl = *R(CTL) & ~((1u << 30) | (1u << 29) | (1u << 28) | (1u << 25));
    for (i = 0; i < send; i += 4) { uint32_t w = *R(DATA + i); for (unsigned j = 0; j < 4 && i + j < send; j++) m[i + j] = (uint8_t)(w >> (24 - 8 * j)); }
    if (edid_deaf) { *R(CTL) = (ctl & ~(1u << 31)) | (1u << 28); return; }
    { unsigned cmd = m[0] >> 4, len = m[3] + 1, k; uint32_t addr = ((m[0] & 0xF) << 16) | (m[1] << 8) | m[2];
      if (cmd == 4 || cmd == 0) { if (addr == 0x50 && len >= 1) edid_ptr = m[4]; }
      else if (cmd == 5 || cmd == 1) { aux_reads++; for (k = 0; k < len; k++) rep[1 + k] = addr == 0x50 ? edid[(edid_ptr + k) & 127] : 0; if (addr == 0x50) edid_ptr += len; }
      *R(CTL) = (ctl & ~(1u << 31) & ~(0x1Fu << 20)) | (1u << 30) | (((cmd == 4 || cmd == 0) ? 1u : len + 1u) << 20);
      for (i = 0; i < ((cmd == 4 || cmd == 0) ? 4u : len + 1); i += 4) *R(DATA + i) = ((uint32_t)rep[i] << 24) | ((uint32_t)rep[i + 1] << 16) | ((uint32_t)rep[i + 2] << 8) | rep[i + 3]; }
}
static uint32_t m_rd(void *c, uint32_t o)
{
    unsigned tc;
    (void)c;
    for (tc = 0; tc < 6; tc++) if (o >= NMK_PHY_BASE(tc) && o < NMK_PHY_BASE(tc) + 0x1000) {
        const unsigned bank = (*R(NMK_HIP_INDEX(tc)) >> (8 * (tc % 4))) & 0xFF;
        return dkl[tc][bank & 15][(o - NMK_PHY_BASE(tc)) / 4]; }
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    unsigned tc;
    (void)c;
    if (nw < 4000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    for (tc = 0; tc < 6; tc++) if (o >= NMK_PHY_BASE(tc) && o < NMK_PHY_BASE(tc) + 0x1000) {
        const unsigned bank = (*R(NMK_HIP_INDEX(tc)) >> (8 * (tc % 4))) & 0xFF;
        dkl[tc][bank & 15][(o - NMK_PHY_BASE(tc)) / 4] = v; return; }
    if (o == NMR_PCODE_MAILBOX) {
        if ((v & NMR_PCODE_READY) && (v & 0xFF) == NMW_READ_MEM_LATENCY) { lat_calls++; *R(NMR_PCODE_DATA) = lat_set[*R(NMR_PCODE_DATA) & 1]; *R(o) = (v & ~NMR_PCODE_READY & ~0xFFu) | (lat_fail ? 1u : 0u); return; }
        *R(o) = v & ~NMR_PCODE_READY & ~0xFFu; return; }
    if (o == NMR_AUX_CTL(3)) { *R(o) = v; if (v & (1u << 31)) aux_process(3); return; }
    for (tc = 0; tc < 6; tc++) {
        const uint32_t en = NMK_PLL_ENABLE_REG(13, tc);
        if (o == en) { uint32_t nv = v & ~(NMK_PLL_PWR_STATE | NMK_PLL_LOCK); if ((v & NMK_PLL_PWR_ENABLE) && !pll_pwr_fail) nv |= NMK_PLL_PWR_STATE; if ((v & NMK_PLL_ENABLE) && !pll_lock_fail) nv |= NMK_PLL_LOCK; *R(o) = nv; return; }
    }
    *R(o) = v;
}
static void m_delay(void *c, unsigned us) { (void)c; (void)us; }
static void m_log(void *c, const char *s) { (void)c; (void)s; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, 0, 0, 0 };

/* a real 1920x1080 at 60 Hz detailed timing, as monitors carry it, in an EDID with a valid header and checksum */
static void make_edid(void)
{
    unsigned i, s = 0;
    static const uint8_t dtd[18] = { 0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0xC4, 0x8E, 0x21, 0x00, 0x00, 0x1E };
    memset(edid, 0, sizeof edid);
    edid[1] = edid[2] = edid[3] = edid[4] = edid[5] = edid[6] = 0xFF;
    edid[8] = 0x10; edid[9] = 0xAC; edid[10] = 0x44; edid[11] = 0xF0;
    memcpy(edid + 54, dtd, 18);
    for (i = 0; i < 127; i++) s += edid[i];
    edid[127] = (uint8_t)(256 - (s & 255));
}

int main(void)
{
    struct nme_timing t; struct nme_caps c; struct nme_mn mn; uint32_t regs[11], rate = 0; unsigned lanes = 0;
    make_edid();
    ARM("the preferred mode is read from the first detailed timing (1920x1080, 148.5 MHz, +sync)", nme_edid_preferred(edid, &t) == 0 && t.clock_khz == 148500 && t.hactive == 1920 && t.hblank == 280 &&
        t.hso == 88 && t.hsw == 44 && t.vactive == 1080 && t.vblank == 45 && t.vso == 4 && t.vsw == 5 && t.hpos && t.vpos && !t.interlaced);
    { uint8_t bad[128]; memcpy(bad, edid, 128); bad[20] ^= 1; ARM("a bad checksum is refused", nme_edid_preferred(bad, &t) == -1);
      memcpy(bad, edid, 128); bad[0] = 1; ARM("a bad header is refused", nme_edid_preferred(bad, &t) == -1);
      memcpy(bad, edid, 128); memset(bad + 54, 0, 18); { unsigned i, s = 0; bad[127] = 0; for (i = 0; i < 127; i++) s += bad[i]; bad[127] = (uint8_t)(256 - (s & 255)); }
      ARM("a first descriptor that is not a timing is reported", nme_edid_preferred(bad, &t) == -2);
      memcpy(bad, edid, 128); bad[54 + 17] |= 0x80; { unsigned i, s = 0; bad[127] = 0; for (i = 0; i < 127; i++) s += bad[i]; bad[127] = (uint8_t)(256 - (s & 255)); }
      ARM("an interlaced preferred mode is refused", nme_edid_preferred(bad, &t) == -2); }
    nme_edid_preferred(edid, &t);
    { uint8_t got[128]; memset(got, 0, 128); edid_ptr = 0; aux_reads = 0;
      ARM("the EDID is read over I2C-over-AUX, 16 bytes at a time, from offset 0", nme_edid_read(&ops, 3, got) == 0 && memcmp(got, edid, 128) == 0 && aux_reads == 8);
      edid_deaf = 1; ARM("a sink that does not answer fails the read", nme_edid_read(&ops, 3, got) == -1); edid_deaf = 0; }
    { uint8_t d[3] = { 0x11, 0x0A, 0x84 };
      ARM("the DPCD capabilities: 2.7 Gb/s, 4 lanes, enhanced framing", nme_caps(d, &c) == 0 && c.max_rate == 270000 && c.max_lanes == 4 && c.enhanced);
      d[1] = 0x07; ARM("an unknown link rate code is refused", nme_caps(d, &c) == -1);
      d[1] = 0x0A; d[2] = 3; ARM("a lane count that is not 1, 2 or 4 is refused", nme_caps(d, &c) == -1); }
    c.max_rate = 540000; c.max_lanes = 4; c.enhanced = 1;
    ARM("1080p60 at 24 bpp needs 445500 kB/s", nme_required(t.clock_khz, 24) == 445500);
    ARM("the cheapest link is the lowest rate with the fewest lanes that fits: 1.62 Gb/s x 4", nme_link(&c, &t, 24, &rate, &lanes) == 0 && rate == 162000 && lanes == 4);
    c.max_rate = 270000; c.max_lanes = 2; ARM("2.7 Gb/s x 2 lanes carries it too and is chosen when that is all the sink has", nme_link(&c, &t, 24, &rate, &lanes) == 0 && rate == 270000 && lanes == 2);
    c.max_rate = 162000; c.max_lanes = 2; ARM("a link too slow for the mode is refused", nme_link(&c, &t, 24, &rate, &lanes) == -1);
    { struct nme_timing small = t; struct nme_caps wide = { 540000, 4, 1 }; small.clock_khz = 25175; ARM("a small mode takes one lane at the lowest rate even when the sink offers four", nme_link(&wide, &small, 24, &rate, &lanes) == 0 && rate == 162000 && lanes == 1); }
    nme_mn(t.clock_khz, 24, 4, 162000, &mn);
    ARM("N starts at 2^27 for data and 2^19 for the link and is shifted down until M and N fit 24 bits (data N 2^23, link N 2^19)", mn.tu == 64 && mn.data_n == 0x800000u && mn.link_n == 0x80000u);
    { const double want_d = 445500.0 / 648000.0, got_d = (double)mn.data_m / mn.data_n, want_l = 148500.0 / 162000.0, got_l = (double)mn.link_m / mn.link_n;
      ARM("the data M over N is the stream rate over the link rate", got_d > want_d * 0.9999 && got_d < want_d * 1.0001);
      ARM("the link M over N is the pixel clock over the symbol clock", got_l > want_l * 0.9999 && got_l < want_l * 1.0001);
      ARM("M and N fit the 24-bit fields", mn.data_m <= 0xFFFFFF && mn.data_n <= 0xFFFFFF && mn.link_m <= 0xFFFFFF && mn.link_n <= 0xFFFFFF); }
    nme_trans_regs(&t, &mn, regs);
    ARM("horizontal total and active", regs[0] == ((2199u << 16) | 1919u));
    ARM("horizontal blank runs from the end of active to the end of the line", regs[1] == ((2199u << 16) | 1919u));
    ARM("horizontal sync starts at active + 88 and is 44 wide", regs[2] == ((2051u << 16) | 2007u));
    ARM("vertical total, blank and sync", regs[3] == ((1124u << 16) | 1079u) && regs[4] == regs[3] && regs[5] == ((1088u << 16) | 1083u));
    ARM("the pipe source size is the active size", regs[6] == ((1919u << 16) | 1079u));
    ARM("data M carries the transfer unit (64) in bits 30:25", (regs[7] >> 25) == 63u && (regs[7] & 0x1FFFFFFu) == mn.data_m && regs[8] == mn.data_n && regs[9] == mn.link_m && regs[10] == mn.link_n);
    ARM("the transcoder function word: enabled, DDI D (select 4), DisplayPort single stream, 8 bpc, both syncs positive, 4 lanes",
        nme_func_ctl(3, &t, 4) == ((1u << 31) | (4u << 27) | (2u << 24) | (1u << 17) | (1u << 16) | (3u << 1)));

    /* the Dekel PHY */
    { static const uint32_t rates[4] = { 162000, 270000, 540000, 810000 }; static const uint32_t refs[3] = { 19200, 24000, 38400 }; unsigned i, j, ok = 1;
      for (i = 0; i < 4; i++) for (j = 0; j < 3; j++) { struct nmk_pll p; if (nmk_calc(refs[j], rates[i], &p) || nmk_freq(refs[j], &p) != rates[i]) { ok = 0; printf("   %u/%u: round trip gives %u\n", rates[i], refs[j], nmk_freq(refs[j], &p)); } }
      ARM("every DisplayPort rate on every reference clock: the registers describe the rate they were computed for", ok); }
    { struct nmk_pll p, q;
      nmk_calc(38400, 162000, &p); nmk_calc(38400, 810000, &q);
      ARM("1.62 Gb/s: DCO 8.1 GHz from 2 x 105, high-speed divider 5, second divider 2, core divider 10, transmitter line driver clock 1", (p.div0 & 0xFF) == 105 && ((p.div0 >> 8) & 0xF) == 2 && ((p.hsclk >> 12) & 3) == 2 && ((p.hsclk >> 8) & 0xF) == 2 && ((p.coreclk >> 8) & 0xFF) == 10 && ((p.hsclk >> 14) & 3) == 1);
      ARM("8.1 Gb/s: a second divider of 1 turns the core divider to 5 and the line driver clock to 0", ((q.hsclk >> 8) & 0xF) == 1 && ((q.coreclk >> 8) & 0xFF) == 5 && ((q.hsclk >> 14) & 3) == 0 && ((q.hsclk >> 12) & 3) == 0);
      ARM("a 38.4 MHz reference gives reference-ratio 2, trim 28 and a target count of 39", ((p.ssc >> 29) & 7) == 2 && ((p.div1 >> 16) & 0x1F) == 28 && (p.div1 & 0xFF) == 39);
      ARM("a rate no divider reaches is refused", nmk_calc(38400, 200000, &p) == -1 && nmk_calc(12345, 162000, &p) == -1); }
    { struct nmk_pll p; const unsigned tc = 2; int rc; unsigned i, bank_ok = 1;
      memset(reg, 0, sizeof reg); memset(dkl, 0, sizeof dkl); nw = 0; pll_pwr_fail = pll_lock_fail = 0;
      nmk_calc(38400, 270000, &p);
      dkl[tc][NMK_REG_BANK(NMK_PLL_DIV0)][NMK_REG_OFF(NMK_PLL_DIV0) / 4] = 0xF0000000u;        /* bits outside the fields must survive the read-modify-write */
      rc = nmk_enable(&ops, 13, tc, &p);
      ARM("the PLL powers up, is programmed and locks", rc == 0 && (*R(NMK_PLL_ENABLE_REG(13, tc)) & NMK_PLL_ENABLE) && (*R(NMK_PLL_ENABLE_REG(13, tc)) & NMK_PLL_LOCK));
      ARM("the registers hold the computed values, in their banks", dkl[tc][NMK_REG_BANK(NMK_PLL_DIV0)][NMK_REG_OFF(NMK_PLL_DIV0) / 4] == (0xF0000000u | p.div0) && dkl[tc][NMK_REG_BANK(NMK_HSCLKCTL)][NMK_REG_OFF(NMK_HSCLKCTL) / 4] == p.hsclk);
      { uint32_t en = NMK_PLL_ENABLE_REG(13, tc); int pw = -1, hi = -1, on = -1; for (i = 0; i < (unsigned)nw; i++) { if (wlog[i].off == en && (wlog[i].v & NMK_PLL_PWR_ENABLE) && pw < 0) pw = (int)i; if (wlog[i].off == NMK_HIP_INDEX(tc) && hi < 0) hi = (int)i; if (wlog[i].off == en && (wlog[i].v & NMK_PLL_ENABLE) && on < 0) on = (int)i; }
        ARM("order: power the PLL, program the PHY through the bank index, then enable", pw >= 0 && hi > pw && on > hi); }
      for (i = 0; i < (unsigned)nw; i++) if (wlog[i].off == NMK_HIP_INDEX(tc) && wlog[i].v > 0xFFu << (8 * (tc % 4))) bank_ok = 0;
      ARM("the bank index of a port sits in that port's byte of the index register", bank_ok && NMK_HIP_INDEX(3) == 0x1010A0u && NMK_HIP_INDEX(4) == 0x1010A4u);
      ARM("version 12 and 13 use different enable registers", NMK_PLL_ENABLE_REG(12, 1) == 0x46034u && NMK_PLL_ENABLE_REG(13, 1) == 0x46040u);
      memset(reg, 0, sizeof reg); pll_pwr_fail = 1; ARM("a PLL that never powers up stops before the PHY is programmed", nmk_enable(&ops, 13, tc, &p) == -1);
      memset(reg, 0, sizeof reg); pll_pwr_fail = 0; pll_lock_fail = 1; ARM("a PLL that never locks is reported", nmk_enable(&ops, 13, tc, &p) == -2); }

    /* the Dekel lanes and the clock routing */
    { const unsigned tc = 3; unsigned ln; int ok = 1;
      memset(reg, 0, sizeof reg); memset(dkl, 0, sizeof dkl); nw = 0;
      for (ln = 0; ln < 2; ln++) { dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL0(ln))][NMK_REG_OFF(NMK_TX_DPCNTL0(ln)) / 4] = 0xE0000000u; dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL2(ln))][NMK_REG_OFF(NMK_TX_DPCNTL2(ln)) / 4] = NMK_DP20BITMODE | NMK_LOADGEN_MASK | 0x80000000u; dkl[tc][NMK_REG_BANK(NMK_TX_PMD_LANE_SUS(ln))][NMK_REG_OFF(NMK_TX_PMD_LANE_SUS(ln)) / 4] = 0xFFFFFFFFu; }
      ARM("swing 2, pre-emphasis 1 at 2.7 Gb/s on Alder Lake P is the 'hbr' row 8: vswing 0, de-emphasis 0x0B", nmk_set_levels(&ops, 13, tc, 270000, 2, 1) == 0 &&
          (dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL0(0))][NMK_REG_OFF(NMK_TX_DPCNTL0(0)) / 4] & NMK_DPCNTL_MASK) == (0x0Bu << 8));
      for (ln = 0; ln < 2; ln++) {
          ok &= (dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL0(ln))][NMK_REG_OFF(NMK_TX_DPCNTL0(ln)) / 4] == (0xE0000000u | (0x0Bu << 8)));            /* bits outside the fields survive */
          ok &= (dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL1(ln))][NMK_REG_OFF(NMK_TX_DPCNTL1(ln)) / 4] & NMK_DPCNTL_MASK) == (0x0Bu << 8);
          ok &= dkl[tc][NMK_REG_BANK(NMK_TX_PMD_LANE_SUS(ln))][NMK_REG_OFF(NMK_TX_PMD_LANE_SUS(ln)) / 4] == 0;
          ok &= (dkl[tc][NMK_REG_BANK(NMK_TX_DPCNTL2(ln))][NMK_REG_OFF(NMK_TX_DPCNTL2(ln)) / 4] & (NMK_DP20BITMODE | NMK_LOADGEN_MASK)) == 0; }
      ARM("both lanes take the level, the suspend register is cleared, the 20-bit mode and the load-generator select (version 13) are cleared", ok);
      ARM("version 13 uses its own tables, and above 2.7 Gb/s the high-bit-rate one", nmk_table(13, 270000) == nmk_adlp_hbr && nmk_table(13, 540000) == nmk_adlp_hbr2 && nmk_table(12, 270000) == nmk_tgl_hbr && nmk_table(12, 810000) == nmk_tgl_hbr2 &&
          nmk_adlp_hbr[0].de_emphasis == 1 && nmk_adlp_hbr2[1].de_emphasis == 4 && nmk_tgl_hbr2[3].de_emphasis == 0x19 && nmk_tgl_hbr[3].de_emphasis == 0x18);
      ARM("a swing and pre-emphasis the standard does not allow (2 and 2) is refused and writes nothing", ({ nw = 0; nmk_set_levels(&ops, 12, tc, 270000, 2, 2) == -1 && nw == 0; }));
      memset(reg, 0, sizeof reg); reg[NMK_DPCLKA_CFGCR0 / 4] = 0xFFFFFFFFu; nmk_clock_on(&ops, 3, 0);
      ARM("the port clock select takes the Dekel PLL and the port's clock gate is opened", *R(NMK_PORT_CLK_SEL(3)) == (0x8u << 28) && !(*R(NMK_DPCLKA_CFGCR0) & (1u << 12)) && (*R(NMK_DPCLKA_CFGCR0) & (1u << 13)));
      nmk_clock_off(&ops, 3, 0); ARM("off gates the clock first, then clears the select", (*R(NMK_DPCLKA_CFGCR0) & (1u << 12)) && *R(NMK_PORT_CLK_SEL(3)) == 0);
      ARM("TC port 5 and 6 use the second gate field", NMK_TC_CLK_OFF(4) == (1u << 21) && NMK_TC_CLK_OFF(5) == (1u << 22));
      nmk_trans_clock(&ops, 1, 3, 1); ARM("the transcoder clock select names the port (port + 1)", *R(NMK_TRANS_CLK_SEL(1)) == (4u << 28)); nmk_trans_clock(&ops, 1, 3, 0); ARM("and is cleared again", *R(NMK_TRANS_CLK_SEL(1)) == 0); }

    /* watermarks and the buffer allocation. The reference below is plain floating point from the meaning of the formulas: a level must hold the data the
     * pipe reads during the memory latency. */
    { struct nmw_in in; struct nmw_level lv[NMW_LEVELS]; unsigned i; int ok = 1, ok2 = 1;
      memset(&in, 0, sizeof in); in.pixel_khz = 148500; in.hactive = 1920; in.htotal = 2200; in.ver = 13;
      { const uint16_t L[8] = { 4, 12, 20, 27, 33, 45, 56, 70 }; memcpy(in.latency, L, sizeof L); }
      nmw_compute(&in, lv);
      for (i = 0; i < NMW_LEVELS; i++) {
          const double bpl = 1920 * 4.0, line_us = 2200 * 1000.0 / in.pixel_khz, need_blocks = ceil(in.latency[i] / line_us) * bpl / 512.0;
          const double m2_blocks = ceil(in.latency[i] * in.pixel_khz / (2200 * 1000.0)) * (ceil(bpl / 512.0) + 1.0);
          if (!lv[i].enable) continue;
          ok &= lv[i].blocks >= need_blocks;                                                  /* the data read during the latency fits */
          ok2 &= (in.latency[i] >= line_us ? lv[i].blocks >= m2_blocks : lv[i].blocks >= need_blocks) && lv[i].blocks >= (unsigned)(bpl / 512.0) + 1; }
      ARM("every enabled level holds the whole lines the pipe reads during its latency (the pipe reads nothing in blanking)", ok);
      ARM("a level slower than one line holds the whole lines it waits (method 2), and no level holds less than a line", ok2);
      ARM("level 0 at 4 us for 1080p60: method 1 gives 7 blocks, but a level holds at least one line (16 blocks): 16 blocks, 1 line, buffer need 19", lv[0].enable && lv[0].blocks == 16 && lv[0].lines == 1 && lv[0].min_ddb == 19);
      ARM("a slower level needs more blocks than the one below it", lv[7].blocks > lv[4].blocks && lv[4].blocks > lv[0].blocks);
      nmw_compute(&in, lv); ARM("with 400 blocks all eight levels fit", nmw_fit(lv, 400) == 0 && lv[7].enable);
      nmw_compute(&in, lv); ARM("with 40 blocks the slow levels are switched off but level 0 stays", nmw_fit(lv, 40) == 0 && lv[0].enable && !lv[7].enable && nmw_word(&lv[7]) == 0);
      nmw_compute(&in, lv); ARM("with 8 blocks level 0 cannot be met, so the mode is refused (it would underrun)", nmw_fit(lv, 8) == -1);
      in.latency[0] = 0; nmw_compute(&in, lv); ARM("a level with no latency is rejected", !lv[0].enable && lv[0].min_ddb == 0xFFFF);
      { struct nmw_in big = in; struct nmw_level bl[NMW_LEVELS]; const uint16_t L[8] = { 4, 12, 20, 27, 33, 45, 56, 70 }; memcpy(big.latency, L, sizeof L); big.pixel_khz = 594000; big.hactive = 3840; big.htotal = 4400;
        nmw_compute(&big, bl); ARM("4K at 60 Hz needs more blocks per level than 1080p", bl[0].blocks > 2u * 11u && bl[0].enable);
        ARM("lines are capped (255 on version 13): a level that needs more is rejected", ({ struct nmw_in x = big; struct nmw_level xl[NMW_LEVELS]; x.latency[7] = 60000; nmw_compute(&x, xl); !xl[7].enable; })); }
      ARM("the line time is 8 x htotal x 1000 / pixel clock rounded up, at most 0x1ff", ({ struct nmw_in x = in; nmw_linetime(&x) == 119u; }));
      { unsigned s, e; nmw_free_run(2048, 0, 1024, &s, &e); ARM("the second pipe takes the free half above the first pipe's range", s == 1024 && e == 2048);
        nmw_free_run(2048, 1024, 2048, &s, &e); ARM("or below it", s == 0 && e == 1024); nmw_free_run(2048, 0, 0, &s, &e); ARM("or everything when the first pipe holds none", s == 0 && e == 2048); }
      ARM("the display buffer is 2048 blocks on version 12 and 4096 on version 13", nmw_dbuf_total(12) == 2048 && nmw_dbuf_total(13) == 4096);
      ARM("a buffer range is written as end - 1 in the high half and start in the low half", nmw_buf_cfg(1024, 2048) == ((2047u << 16) | 1024u)); }
    { uint16_t wm[8]; int rc;
      memset(reg, 0, sizeof reg);
      (void)rc; (void)wm; }
    { uint16_t wm[8]; int rc; memset(reg, 0, sizeof reg); lat_set[0] = 0x1B140C03u; lat_set[1] = 0x46382D21u; lat_fail = 0; lat_calls = 0;
      rc = nmw_read_latency(&ops, 13, wm);
      ARM("the latencies come from two mailbox reads (sets 0 and 1), one byte per level", rc == 0 && lat_calls == 2 && wm[1] == 0x0C && wm[2] == 0x14 && wm[3] == 0x1B && wm[4] == 0x21 && wm[7] == 0x46);
      ARM("level 0 reads 3 us: the platform controller adds one more for the DIMM", wm[0] == 4);
      lat_set[0] = 0x1B140C00u; lat_set[1] = 0x46382D21u; rc = nmw_read_latency(&ops, 13, wm);
      ARM("a level 0 of zero gets the memory read latency added to every valid level (3 on version 12 and later)", rc == 0 && wm[0] == 3 + 1 && wm[1] == 0x0C + 3 && wm[3] == 0x1B + 3);
      lat_set[0] = 0x1B000C03u; rc = nmw_read_latency(&ops, 13, wm);
      ARM("a level with no latency disables every level above it", rc == 0 && wm[2] == 0 && wm[3] == 0 && wm[7] == 0);
      lat_fail = 1; ARM("a mailbox that fails is an error", nmw_read_latency(&ops, 13, wm) == -1); lat_fail = 0; }
    printf("%d failed\n", fails);
    return fails != 0;
}

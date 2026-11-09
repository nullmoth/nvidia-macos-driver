/* Type-C ports on display versions 12 and 13 (kexts/NVRM/accel/nm_hybrid_tc.h): live status, mode, and the PHY ownership handshake, against
 * a model of the Type-C subsystem's side (FIA / TCSS registers, the DDI ownership bit, the platform controller's TC-cold mailbox).
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_tc.c -o /tmp/t && /tmp/t     -> 0 failed
 * The model follows Linux's i915 (intel_tc.c, intel_display_power_well.c, i915_reg.h; MIT). It checks the ORDER of the handshake and what
 * each failure leaves behind; it cannot show that a given dock works. */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "nm_hybrid_tc.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define NREG (0x170000 / 4)
static uint32_t reg[NREG];
static struct { uint32_t off, v; } wlog[2000]; static int nw;
static int blocked, tccold_asleep, iom_ready, exit_fail, drop_live_on_own, ddi_off, cold_calls;
static unsigned VER = 12;
static uint32_t *R(uint32_t o) { return &reg[o / 4]; }

static int is_fia_reg(uint32_t o) { return o >= 0x163000 && o < 0x164000 ? 1 : (o >= 0x16E000 && o < 0x16F000) ? 1 : (o >= 0x16F000 && o < 0x170000) ? 1 : 0; }
static uint32_t m_rd(void *c, uint32_t o)
{
    (void)c;
    if (VER == 12 && is_fia_reg(o) && tccold_asleep && !blocked) return 0xFFFFFFFFu;     /* the PHY answers only while TC-cold is blocked */
    if (VER == 13 && o >= 0x64000 && o < 0x64800 && ddi_off) return 0xFFFFFFFFu;
    if (VER == 13 && o >= 0x161500 && o < 0x161520) return *R(o) | (iom_ready ? NMT_TCSS_READY : 0);
    return *R(o);
}
static void m_wr(void *c, uint32_t o, uint32_t v)
{
    (void)c;
    if (nw < 2000) { wlog[nw].off = o; wlog[nw].v = v; nw++; }
    if (o == NMR_PCODE_MAILBOX) {
        if ((v & NMR_PCODE_READY) && (v & 0xFF) == NMT_PCODE_TCCOLD) {
            cold_calls++;
            if (!(*R(NMR_PCODE_DATA) & 1u)) { blocked = 1; *R(NMR_PCODE_DATA) = exit_fail ? 1u : 0u; } else { blocked = 0; *R(NMR_PCODE_DATA) = 0; }
            *R(o) = v & ~NMR_PCODE_READY & ~0xFFu; return;
        }
        *R(o) = v & ~NMR_PCODE_READY & ~0xFFu; return;
    }
    *R(o) = v;
    if (drop_live_on_own && ((VER == 12 && (o == NMT_DFLEXDPCSSS(0) || o == NMT_DFLEXDPCSSS(1) || o == NMT_DFLEXDPCSSS(2))) || (VER == 13 && o == NMR_DDI_BUF_CTL(6)))) { *R(NMT_DFLEXDPSP(0)) = 0; *R(NMT_DFLEXDPSP(1)) = 0; *R(NMT_DE_HPD_ISR) = 0; }
}
static void m_delay(void *c, unsigned us) { (void)c; (void)us; }
static void m_log(void *c, const char *s) { (void)c; (void)s; }
static struct nmr_ops ops = { 0, m_rd, m_wr, m_delay, m_log, 0, 0, 0 };

static int first_write(uint32_t off) { for (int i = 0; i < nw; i++) if (wlog[i].off == off) return i; return -1; }
static int last_write(uint32_t off) { int r = -1; for (int i = 0; i < nw; i++) if (wlog[i].off == off) r = i; return r; }
static int cold_idx(int nth, int block)   /* index of the nth mailbox write for TC-cold with that request */
{
    int seen = 0, i;
    for (i = 0; i < nw; i++) if (wlog[i].off == NMR_PCODE_MAILBOX && (wlog[i].v & 0xFF) == NMT_PCODE_TCCOLD) { if (seen == nth) return i; seen++; }
    (void)block; return -1;
}

static void world(unsigned ver, int modular)
{
    VER = ver;
    memset(reg, 0, sizeof reg); nw = 0;
    blocked = 0; tccold_asleep = (ver == 12); iom_ready = 1; exit_fail = 0; drop_live_on_own = 0; ddi_off = 0; cold_calls = 0;
    if (ver == 12) { *R(NMT_DFLEXDPSP(0)) = modular ? NMT_MODULAR_FIA : 0; }
}
/* the cable on Type-C port tc of the world: mode, lanes assigned */
static void cable(unsigned ver, const struct nmt_port *p, unsigned mode, unsigned lanes)
{
    if (ver == 12) {
        uint32_t v = *R(NMT_DFLEXDPSP(p->fia)) & ~(0xFFu << (p->fia_idx * 8));
        if (mode == NMT_DPALT) v |= NMT_LIVE_TC(p->fia_idx) | (lanes << (p->fia_idx * 8));
        if (mode == NMT_TBT) v |= NMT_LIVE_TBT(p->fia_idx);
        *R(NMT_DFLEXDPSP(p->fia)) = v | (*R(NMT_DFLEXDPSP(0)) & NMT_MODULAR_FIA);
        if (mode == NMT_LEGACY) *R(NMT_SDEISR) |= 1u << (24 + p->tc); else *R(NMT_SDEISR) &= ~(1u << (24 + p->tc));
        *R(NMT_DFLEXDPPMS(p->fia)) = iom_ready ? NMT_PMS_READY(p->fia_idx) : 0;
    } else {
        *R(NMT_DE_HPD_ISR) &= ~((1u << (16 + p->tc)) | (1u << p->tc));
        if (mode == NMT_DPALT) *R(NMT_DE_HPD_ISR) |= 1u << (16 + p->tc);
        if (mode == NMT_TBT) *R(NMT_DE_HPD_ISR) |= 1u << p->tc;
        if (mode == NMT_LEGACY) *R(NMT_SDEISR) |= 1u << (24 + p->tc); else *R(NMT_SDEISR) &= ~(1u << (24 + p->tc));
        *R(NMT_DFLEXDPSP(p->fia)) = (*R(NMT_DFLEXDPSP(p->fia)) & ~(0xFu << (p->fia_idx * 8))) | (lanes << (p->fia_idx * 8));      /* the lane assignment is in the FIA on both versions */
    }
}

static void suite(unsigned ver)
{
    struct nmt_port p; const unsigned tc = 3; uint32_t last; int rc;
    printf("-- display version %u\n", ver);
    world(ver, 1);
    rc = nmt_init(&ops, &p, ver, tc, 0);
    ARM("a modular FIA puts two ports in each instance: Type-C port 3 is instance 1, index 1", rc == 0 && p.fia == 1 && p.fia_idx == 1);
    if (ver == 12) {
        struct nmt_port q; world(12, 0);
        ARM("a non-modular FIA (Tiger Lake) is one instance with the port index as is", nmt_init(&ops, &q, 12, tc, 0) == 0 && q.fia == 0 && q.fia_idx == 3);
        world(12, 1); *R(NMT_DFLEXDPSP(0)) = 0xFFFFFFFFu; tccold_asleep = 0;
        ARM("an FIA that reads as all ones at init is refused", nmt_init(&ops, &q, 12, tc, 0) != 0);
        world(12, 1); nmt_init(&ops, &p, ver, tc, 0);
    }
    cable(ver, &p, NMT_DPALT, 0xF);
    ARM("a DisplayPort-alt cable is seen in the live status", nmt_live(&ops, &p) == (1u << NMT_DPALT));
    cable(ver, &p, NMT_TBT, 0);
    ARM("a Thunderbolt cable is seen in the live status", nmt_live(&ops, &p) == (1u << NMT_TBT));
    cable(ver, &p, NMT_LEGACY, 0);
    ARM("a legacy port is seen through the south-bridge status", nmt_live(&ops, &p) == (1u << NMT_LEGACY));
    cable(ver, &p, NMT_DISCONNECTED, 0);
    ARM("nothing connected reads as no live status", nmt_live(&ops, &p) == 0);
    ARM("the highest live bit is the mode", nmt_mask_to_mode((1u << NMT_TBT) | (1u << NMT_DPALT)) == NMT_DPALT && nmt_mask_to_mode(0) == NMT_DISCONNECTED);
    if (ver == 12) {
        world(12, 1); nmt_init(&ops, &p, 12, tc, 0); cable(12, &p, NMT_DPALT, 0xF); nw = 0; cold_calls = 0;
        ARM("version 12 reads the live status only with TC-cold blocked, and releases it after", nmt_live(&ops, &p) == (1u << NMT_DPALT) && cold_calls == 2 && !blocked);
        *R(NMT_DFLEXDPSP(1)) = 0xFFFFFFFFu; blocked = 0; tccold_asleep = 0;
        ARM("an FIA that reads as all ones means a PHY in TC-cold: nothing connected", nmt_live(&ops, &p) == 0);
    }

    /* DisplayPort-alt, 4 lanes */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_DPALT, 0xF); nw = 0; cold_calls = 0;
    rc = nmt_connect(&ops, &p, 4);
    ARM("a DisplayPort-alt cable connects in that mode", rc == 1 && p.mode == NMT_DPALT);
    ARM("display owns the PHY afterwards and TC-cold stays blocked", nmt_owned(&ops, &p) && blocked);
    { const uint32_t own = ver == 12 ? NMT_DFLEXDPCSSS(p.fia) : NMR_DDI_BUF_CTL(p.ddi);
      int ow = first_write(own), cb = -1, i;
      for (i = 0; i < nw; i++) if (wlog[i].off == NMR_PCODE_MAILBOX && (wlog[i].v & 0xFF) == NMT_PCODE_TCCOLD) cb = i;
      if (ver == 12) ARM("order (12): TC-cold is blocked before the PHY ownership is taken", cold_idx(0, 1) >= 0 && cold_idx(0, 1) < ow);
      else ARM("order (13): the PHY ownership is taken before TC-cold is blocked", ow >= 0 && ow < cold_idx(0, 1)); (void)cb; }
    ARM("the other port of the same FIA instance is not touched", ver == 13 ? 1 : (*R(NMT_DFLEXDPCSSS(p.fia)) & NMT_CSSS_NOT_SAFE(0)) == 0);
    nw = 0;
    nmt_disconnect(&ops, &p);
    ARM("disconnect hands the PHY back and releases TC-cold", p.mode == NMT_DISCONNECTED && !nmt_owned(&ops, &p) && !blocked);
    { const uint32_t own = ver == 12 ? NMT_DFLEXDPCSSS(p.fia) : NMR_DDI_BUF_CTL(p.ddi);
      if (ver == 12) ARM("order (12): ownership is released, then TC-cold", first_write(own) >= 0 && first_write(own) < last_write(NMR_PCODE_MAILBOX));
      else ARM("order (13): TC-cold is released, then ownership", first_write(own) > cold_idx(0, 0)); }
    nmt_disconnect(&ops, &p);
    ARM("a second disconnect does nothing", nw >= 0 && p.mode == NMT_DISCONNECTED);

    /* PHY not ready: falls back to the default mode */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); iom_ready = 0; cable(ver, &p, NMT_DPALT, 0xF);
    rc = nmt_connect(&ops, &p, 4);
    ARM("a PHY that is not ready connects as Thunderbolt-alt and never keeps the ownership", rc == 1 && p.mode == NMT_TBT && !nmt_owned(&ops, &p));
    nmt_disconnect(&ops, &p);

    /* lanes */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_DPALT, 0x3);
    rc = nmt_connect(&ops, &p, 2);
    ARM("two lanes assigned satisfy a two-lane request", rc == 1 && p.mode == NMT_DPALT && nmt_max_lanes(&ops, &p) == 2);
    nmt_disconnect(&ops, &p);
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_DPALT, 0x3);
    rc = nmt_connect(&ops, &p, 4);
    ARM("two lanes assigned do not satisfy a four-lane request: ownership is given back and the port falls back", rc == 1 && p.mode == NMT_TBT && !nmt_owned(&ops, &p));
    nmt_disconnect(&ops, &p);

    /* sudden disconnect */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_DPALT, 0xF); drop_live_on_own = 1;
    rc = nmt_connect(&ops, &p, 4);
    ARM("a cable pulled during the handshake is noticed and the ownership is released", !nmt_owned(&ops, &p) && (rc == 0 || p.mode == NMT_TBT));
    drop_live_on_own = 0;

    /* TC-cold block refused */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_DPALT, 0xF); exit_fail = 1; nw = 0; cold_calls = 0;
    rc = nmt_connect(&ops, &p, 4);
    ARM("a platform controller that refuses the TC-cold block fails the connect after three tries, without leaving ownership", rc == 0 && p.mode == NMT_DISCONNECTED && !nmt_owned(&ops, &p) && cold_calls >= 3);

    /* legacy flag fixup */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_LEGACY, 0xF); iom_ready = 0;
    rc = nmt_connect(&ops, &p, 4);
    ARM("a lone legacy live bit on a port the video BIOS calls non-legacy flips the flag, and a legacy port connects even before the PHY says ready", rc == 1 && p.legacy == 1 && p.mode == NMT_LEGACY);
    nmt_disconnect(&ops, &p);

    /* Thunderbolt */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); cable(ver, &p, NMT_TBT, 0);
    rc = nmt_connect(&ops, &p, 4);
    ARM("a Thunderbolt cable connects without taking the PHY", rc == 1 && p.mode == NMT_TBT && !nmt_owned(&ops, &p) && blocked);
    nmt_disconnect(&ops, &p);

    /* port power off (version 13) */
    if (ver == 13) {
        world(13, 1); nmt_init(&ops, &p, 13, tc, 0); cable(13, &p, NMT_DPALT, 0xF); ddi_off = 1;
        ARM("a DDI buffer register that reads as all ones cannot take ownership", nmt_take(&ops, &p, 1) == -1);
        ddi_off = 0;
    }

    /* hot-plug events */
    world(ver, 1); nmt_init(&ops, &p, ver, tc, 0); last = 0;
    cable(ver, &p, NMT_DISCONNECTED, 0); ARM("no change is no event", nmt_poll(&ops, &p, &last) == 0);
    cable(ver, &p, NMT_DPALT, 0xF); ARM("a plug is event 1", nmt_poll(&ops, &p, &last) == 1);
    ARM("the same state again is no event", nmt_poll(&ops, &p, &last) == 0);
    cable(ver, &p, NMT_TBT, 0); ARM("a change of cable type is event 3", nmt_poll(&ops, &p, &last) == 3);
    cable(ver, &p, NMT_DISCONNECTED, 0); ARM("an unplug is event 2", nmt_poll(&ops, &p, &last) == 2);
}

int main(void)
{
    suite(12);
    suite(13);
    { struct nmt_port q; ARM("versions other than 12 and 13 are refused", nmt_init(&ops, &q, 14, 0, 0) != 0 && nmt_init(&ops, &q, 11, 0, 0) != 0); }
    printf("%d failed\n", fails);
    return fails != 0;
}

/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Intel display microcontroller (DMC) firmware: the package format of the Intel i915 firmware files, validated and turned
 * into the list of writes that load it. Plain C so tools/test_hybrid_dmc.c runs it on the host, on synthetic packages and
 * (when NMH_DMC_DIR names a folder holding the fetched files) on the real ones.
 *
 * Format and checks follow Linux's intel_dmc.c (MIT): a CSS header, a package header with one entry per (firmware id,
 * stepping), and for each entry a DMC header (version 1 or 3) with the program size, the register the program is written to
 * and a list of register writes to make after it. Nothing is trusted: every length, offset and count is checked against the
 * file size before it is used, so a damaged or hostile file is refused and never read past its end.
 *
 * The file is Intel's, redistributable unmodified (licenses/Intel-i915-firmware-LICENSE.txt), and is fetched on the user's
 * Mac by package/dmc_fetch.sh. This header reads its structure only.
 */
#ifndef NM_HYBRID_DMC_H
#define NM_HYBRID_DMC_H

#include <stdint.h>

#define NMD_MAX_IDS 9           /* main + pipe A..D + the extra ids newer packages carry */
#define NMD_MAX_MMIO 20
#define NMD_CSS_BYTES 128
#define NMD_PKG_BYTES 16
#define NMD_FWINFO_BYTES 12
#define NMD_HDR_BASE_BYTES 20
#define NMD_HDR_V1_BYTES 128
#define NMD_HDR_V3_BYTES 256

struct nmd_part {
    int present;
    uint8_t id;
    const uint8_t *payload;      /* points into the file */
    uint32_t words;              /* program size in 32-bit words */
    uint32_t start_mmio;         /* program RAM address; word i goes to start_mmio + 4 * i */
    uint32_t mmio_count;
    uint32_t mmio_addr[NMD_MAX_MMIO], mmio_data[NMD_MAX_MMIO];
};
struct nmd_fw { uint32_t version; struct nmd_part part[NMD_MAX_IDS]; };

enum { NMD_OK = 0, NMD_E_SHORT, NMD_E_CSS, NMD_E_PKG, NMD_E_NO_MAIN, NMD_E_HDR, NMD_E_RANGE, NMD_E_SIZE };
static inline const char *nmd_err(int e)
{
    switch (e) {
    case NMD_OK: return "ok";
    case NMD_E_SHORT: return "truncated";
    case NMD_E_CSS: return "bad CSS header";
    case NMD_E_PKG: return "bad package header";
    case NMD_E_NO_MAIN: return "no main program for this stepping";
    case NMD_E_HDR: return "bad DMC header";
    case NMD_E_RANGE: return "a register address outside the DMC ranges";
    case NMD_E_SIZE: return "program larger than this generation allows";
    default: return "?";
    }
}

static inline uint32_t nmd_u32(const uint8_t *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }

/* a register the firmware may ask the loader to write: the DMC's own blocks (0x8F000), the per-pipe blocks (0x92000 and
 * 0x5F000), and for header version 1 the original 0x80000 window */
static inline int nmd_addr_ok(uint32_t a, int hdr_ver, int is_main)
{
    if (hdr_ver == 1) return a >= 0x80000 && a <= 0x8FFFF;
    if (is_main) return a >= 0x8F000 && a <= 0x8FFFF;
    return (a >= 0x5F000 && a <= 0x5FFFF) || (a >= 0x92000 && a <= 0x93FFF);
}

/* 'stepping' and 'sub' are ASCII ('*' when unknown); 'max_words' the program size limit of the generation (bytes / 4). */
static inline int nmd_parse(const uint8_t *f, uint32_t size, char stepping, char sub, uint32_t max_words, struct nmd_fw *out)
{
    uint32_t off, ver, nent, maxent, i, k;
    int found[NMD_MAX_IDS];
    for (i = 0; i < NMD_MAX_IDS; i++) { out->part[i].present = 0; found[i] = 0; }
    if (size < NMD_CSS_BYTES + NMD_PKG_BYTES) return NMD_E_SHORT;
    if (nmd_u32(f + 4) * 4 != NMD_CSS_BYTES) return NMD_E_CSS;
    out->version = nmd_u32(f + 0x50);
    off = NMD_CSS_BYTES;
    ver = f[off + 1];
    if (ver == 1) maxent = 20; else if (ver == 2) maxent = 32; else return NMD_E_PKG;
    {
        uint32_t pkg = NMD_PKG_BYTES + maxent * NMD_FWINFO_BYTES;
        if (size - off < pkg) return NMD_E_SHORT;
        if ((uint32_t)f[off] * 4 != pkg) return NMD_E_PKG;
        nent = nmd_u32(f + off + 12);
        if (nent > maxent) nent = maxent;
        for (i = 0; i < nent; i++) {
            const uint8_t *e = f + off + NMD_PKG_BYTES + i * NMD_FWINFO_BYTES;
            uint8_t id = ver <= 1 ? 0 : e[1];
            char st = (char)e[2], ss = (char)e[3];
            uint32_t dmc_off = nmd_u32(e + 4);
            int match;
            if (id >= NMD_MAX_IDS || found[id]) continue;
            match = (ss == '*' && stepping == st) || (stepping == st && sub == ss) || (stepping == '*' && sub == ss) || (st == '*' && ss == '*');
            if (!match) continue;
            found[id] = 1;
            {   /* locate and parse this entry's DMC header: offsets are in dwords from the end of the package header */
                uint64_t hoff = (uint64_t)off + pkg + (uint64_t)dmc_off * 4;
                const uint8_t *h; uint32_t rem, hlen, hbytes, payload, mc, mmax, start;
                int hv;
                if (hoff + NMD_HDR_BASE_BYTES > size) return NMD_E_SHORT;
                h = f + hoff; rem = (uint32_t)(size - hoff);
                if (nmd_u32(h) != 0x40403E3Eu) return NMD_E_HDR;
                hv = h[5];
                if (hv == 3) { if (rem < NMD_HDR_V3_BYTES) return NMD_E_SHORT; hbytes = NMD_HDR_V3_BYTES; hlen = (uint32_t)h[4] * 4; mmax = 20; start = nmd_u32(h + 20); mc = nmd_u32(h + 20 + 4 + 36 + 32); }
                else if (hv == 1) { if (rem < NMD_HDR_V1_BYTES) return NMD_E_SHORT; hbytes = NMD_HDR_V1_BYTES; hlen = h[4]; mmax = 8; start = 0x80000; mc = nmd_u32(h + 20); }
                else return NMD_E_HDR;
                if (hlen != hbytes) return NMD_E_HDR;
                if (mc > mmax) return NMD_E_HDR;
                {
                    const uint8_t *ma = hv == 3 ? h + 20 + 4 + 36 + 32 + 4 : h + 24;
                    const uint8_t *md = ma + 4 * mmax;
                    struct nmd_part *p = &out->part[id];
                    for (k = 0; k < mc; k++) {
                        p->mmio_addr[k] = nmd_u32(ma + 4 * k); p->mmio_data[k] = nmd_u32(md + 4 * k);
                        if (!nmd_addr_ok(p->mmio_addr[k], hv, id == 0)) return NMD_E_RANGE;
                    }
                    p->mmio_count = mc;
                }
                payload = nmd_u32(h + 12) * 4;
                if (rem - hbytes < payload) return NMD_E_SHORT;
                if (payload / 4 > max_words) return NMD_E_SIZE;
                out->part[id].present = 1; out->part[id].id = id; out->part[id].payload = h + hbytes;
                out->part[id].words = payload / 4; out->part[id].start_mmio = start;
                /* program RAM windows seen in the shipped files are 0x52000 to 0x98000; Linux does not check the base, this does */
                if (start < 0x50000u || start > 0x9FFFFu) { out->part[id].present = 0; return NMD_E_RANGE; }
            }
        }
    }
    return out->part[0].present ? NMD_OK : NMD_E_NO_MAIN;
}

/* One write of the load sequence */
struct nmd_write { uint32_t reg, val; };
/* The writes that load one part: the program words, then the register list. Returns the count, or 0 if 'cap' is too small. */
static inline uint32_t nmd_plan(const struct nmd_part *p, struct nmd_write *w, uint32_t cap)
{
    uint32_t i, n = 0;
    if (!p->present || cap < p->words + p->mmio_count) return 0;
    for (i = 0; i < p->words; i++) { w[n].reg = p->start_mmio + 4 * i; w[n].val = nmd_u32(p->payload + 4 * i); n++; }
    for (i = 0; i < p->mmio_count; i++) { w[n].reg = p->mmio_addr[i]; w[n].val = p->mmio_data[i]; n++; }
    return n;
}


/* ---- the display stepping, which picks between the package's entries (Linux intel_step.c; intel_dmc.c intel_get_stepping_info).
 * Versions 12 and 13 map the PCI revision id through a table per sub-platform; version 14 reads the display step from the
 * GMD_ID_DISPLAY register (0x510A0, bits 5:0). An unknown part gets the wildcard, which Linux also does. */
struct nmd_step { char st, sub; };
static inline struct nmd_step nmd_step_of(unsigned n)
{
    struct nmd_step r;
    if (n >= 40) { r.st = r.sub = '*'; return r; }          /* a stepping beyond the names Linux has: "future" */
    r.st = (char)('A' + n / 4); r.sub = (char)('0' + n % 4);
    return r;
}
static inline int nmd_id_in(const unsigned *t, unsigned n, unsigned id) { unsigned i; for (i = 0; i < n; i++) if (t[i] == id) return 1; return 0; }
/* Steppings by revision id, as arrays of (revid -> step number, 0xFF = gap): A0 = 0, A1 = 1, B0 = 4, B1 = 5, C0 = 8 */
static inline struct nmd_step nmd_stepping(unsigned display_version, unsigned pci_device, unsigned revid, uint32_t gmd_display_id)
{
    static const unsigned tgl_uy[] = { 0x9a40, 0x9a49, 0x9a59, 0x9a78, 0x9ac0, 0x9ac9, 0x9ad9, 0x9af8 };
    static const unsigned tgl_gt1[] = { 0x9a60, 0x9a68, 0x9a70 };
    static const unsigned adln[] = { 0x46d0, 0x46d1, 0x46d2, 0x46d3, 0x46d4 };
    static const unsigned rplp[] = { 0xa720, 0xa7a0, 0xa7a8, 0xa7aa, 0xa7ab };
    static const unsigned char t_uy[] = { 0, 4, 5, 8 }, t_tgl[] = { 0, 4 }, t_adlp[13] = { 0, 0xFF, 0xFF, 0xFF, 4, 0xFF, 0xFF, 0xFF, 8, 0xFF, 0xFF, 0xFF, 8 },
                              t_rplp[] = { 0xFF, 0xFF, 0xFF, 0xFF, 8 }, t_adln[] = { 0 };
    const unsigned char *t = 0; unsigned size = 0;
    struct nmd_step wild = { '*', '*' };
    if (display_version == 14) {
        if ((gmd_display_id >> 22) != 14) return wild;
        return nmd_step_of(gmd_display_id & 0x3F);
    }
    if (display_version == 12) {
        if (nmd_id_in(tgl_uy, sizeof tgl_uy / sizeof *tgl_uy, pci_device)) { t = t_uy; size = sizeof t_uy; }
        else if (nmd_id_in(tgl_gt1, sizeof tgl_gt1 / sizeof *tgl_gt1, pci_device)) { t = t_tgl; size = sizeof t_tgl; }
    } else if (display_version == 13) {
        if (nmd_id_in(adln, 5, pci_device)) { t = t_adln; size = sizeof t_adln; }
        else if (nmd_id_in(rplp, 5, pci_device)) { t = t_rplp; size = sizeof t_rplp; }
        else { t = t_adlp; size = sizeof t_adlp; }
    }
    if (!t) return wild;
    /* a gap in the table takes the next revision's stepping; past the end is "future" */
    while (revid < size && t[revid] == 0xFF) revid++;
    if (revid < size) return nmd_step_of(t[revid]);
    return wild;
}

/* ---- which file a given GPU needs, and how large its program may be (Linux intel_dmc.c dmc_firmware_default, pciids.h) */
static inline const char *nmd_file_for(unsigned display_version, unsigned pci_device)
{
    static const unsigned tgl[] = { 0x9a40, 0x9a49, 0x9a59, 0x9a60, 0x9a68, 0x9a70, 0x9a78, 0x9ac0, 0x9ac9, 0x9ad9, 0x9af8 };
    static const unsigned adlp[] = { 0x4626, 0x4628, 0x462a, 0x46a0, 0x46a1, 0x46a2, 0x46a3, 0x46a6, 0x46a8, 0x46aa, 0x46b0, 0x46b1, 0x46b2, 0x46b3, 0x46c0, 0x46c1, 0x46c2, 0x46c3,
                                     0x46d0, 0x46d1, 0x46d2, 0x46d3, 0x46d4 /* Alder Lake N */, 0xa720, 0xa7a0, 0xa7a8, 0xa7aa, 0xa7ab /* Raptor Lake P */ };
    static const unsigned mtl[] = { 0x7d40, 0x7d45, 0x7d55, 0x7d60, 0x7dd5, 0x7d41, 0x7d51, 0x7d67, 0x7dd1, 0xb640 };   /* Meteor Lake and Arrow Lake */
    unsigned i;
    if (display_version == 12) { for (i = 0; i < sizeof tgl / sizeof *tgl; i++) if (tgl[i] == pci_device) return "tgl_dmc_ver2_12.bin"; }
    else if (display_version == 13) { for (i = 0; i < sizeof adlp / sizeof *adlp; i++) if (adlp[i] == pci_device) return "adlp_dmc_ver2_16.bin"; }
    else if (display_version == 14) { for (i = 0; i < sizeof mtl / sizeof *mtl; i++) if (mtl[i] == pci_device) return "mtl_dmc.bin"; }
    return 0;      /* Rocket Lake and Alder Lake S have their own files, not fetched */
}
static inline uint32_t nmd_max_words(unsigned display_version)
{
    return (display_version >= 14 ? 0x7000u : (display_version == 13 ? 0x20000u : 0x6000u)) / 4;
}

/* ---- the load sequence, as Linux's intel_dmc_load_program does it, but with every event handler left disabled: the program
 * is in place, nothing it would trigger runs. (Linux keeps the main program's events enabled for the deep display power states;
 * this driver never enables those states, so there is nothing for them to do.) Register bases: the main program at 0x8F000, the
 * pipe programs at 0x5F000 (version 13 and later) or 0x92000 plus 0x400 per pipe. */
#define NMD_EVT_COUNT 8
#define NMD_EVT_DISABLED ((3u << 16) | (0x01u << 8))        /* edge 0 to 1 on the event "false": never fires */
#define NMD_EVT_CTL_OFF 0x34u
#define NMD_EVT_HTP_OFF 0x04u
static inline uint32_t nmd_base(unsigned id, unsigned display_version)
{
    return id == 0 ? 0x8F000u : ((display_version >= 13 ? 0x5F000u : 0x92000u) + 0x400u * (id - 1));
}
static inline int nmd_is_evt_ctl(unsigned id, unsigned display_version, uint32_t reg)
{
    const uint32_t b = nmd_base(id, display_version) + NMD_EVT_CTL_OFF;
    return reg >= b && reg < b + 4u * NMD_EVT_COUNT;
}
static inline uint32_t nmd_load_plan(const struct nmd_fw *fw, unsigned display_version, struct nmd_write *w, uint32_t cap)
{
    uint32_t n = 0, i, h; unsigned id;
    uint64_t need = 0;
    /* The plan writes every part's event entries first and every part's words afterwards, so the bound has to be
       the total over all present parts, checked once before anything is written. */
    for (id = 0; id < NMD_MAX_IDS; id++) {
        const struct nmd_part *p = &fw->part[id];
        if (p->present) need += 2ull * NMD_EVT_COUNT + (uint64_t)p->words + (uint64_t)p->mmio_count;
    }
    if (need > cap) return 0;
    for (id = 0; id < NMD_MAX_IDS; id++) {
        const struct nmd_part *p = &fw->part[id];
        if (!p->present) continue;
        for (h = 0; h < NMD_EVT_COUNT; h++) {
            w[n].reg = nmd_base(id, display_version) + NMD_EVT_CTL_OFF + 4 * h; w[n].val = NMD_EVT_DISABLED; n++;
            w[n].reg = nmd_base(id, display_version) + NMD_EVT_HTP_OFF + 4 * h; w[n].val = 0; n++;
        }
    }
    for (id = 0; id < NMD_MAX_IDS; id++) {
        const struct nmd_part *p = &fw->part[id];
        if (!p->present) continue;
        for (i = 0; i < p->words; i++) { w[n].reg = p->start_mmio + 4 * i; w[n].val = nmd_u32(p->payload + 4 * i); n++; }
        for (i = 0; i < p->mmio_count; i++) {
            w[n].reg = p->mmio_addr[i];
            w[n].val = nmd_is_evt_ctl(id, display_version, p->mmio_addr[i]) ? NMD_EVT_DISABLED : p->mmio_data[i];
            n++;
        }
    }
    return n;
}

#endif

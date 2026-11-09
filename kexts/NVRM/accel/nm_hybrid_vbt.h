/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * The video BIOS tables (VBT) of an Intel laptop, read for the two facts about the built-in panel that pick its drive
 * level table: whether the panel uses the low voltage swing table and whether it uses the reduced-power one. Plain C so
 * tools/test_hybrid_vbt.c runs the same code on the host. Layout from Linux i915 (intel_vbt_defs.h, intel_bios.c,
 * intel_opregion.c, MIT).
 *
 * Every offset and length is checked against the size of the buffer before it is read, so a damaged or hostile table is
 * refused and never read past its end. If the VBT does not name a panel type this code does not guess one: it reports
 * "unknown" and the caller keeps the plain DisplayPort tables.
 */
#ifndef NM_HYBRID_VBT_H
#define NM_HYBRID_VBT_H

#include <stdint.h>

#define NMV_BDB_EDP        27
#define NMV_BDB_LFP_OPTS   40
#define NMV_BDB_LFP_POWER  44
#define NMV_BDB_LFP_PTRS   41
#define NMV_BDB_LFP_DATA   42

struct nmv_edp { int known; unsigned panel_type; int low_vswing; int hobl; unsigned vbt_version, bdb_version; };

static inline unsigned nmv_u16(const uint8_t *p) { return (unsigned)p[0] | (unsigned)p[1] << 8; }
static inline uint32_t nmv_u32(const uint8_t *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }
static inline uint64_t nmv_u64(const uint8_t *p) { return (uint64_t)nmv_u32(p) | (uint64_t)nmv_u32(p + 4) << 32; }

/* the BDB (data blocks) of a VBT: offset and size, or 0 */
static inline int nmv_bdb(const uint8_t *v, uint32_t size, uint32_t *off, uint32_t *len, unsigned *ver)
{
    uint32_t bdb, i;
    static const char sig[] = "BIOS_DATA_BLOCK";
    if (size < 48 || v[0] != '$' || v[1] != 'V' || v[2] != 'B' || v[3] != 'T') return 0;
    bdb = nmv_u32(v + 28);
    if (bdb > size || size - bdb < 22) return 0;
    for (i = 0; i < 15; i++) if (v[bdb + i] != (uint8_t)sig[i]) return 0;
    *ver = nmv_u16(v + bdb + 16);
    {
        unsigned hsize = nmv_u16(v + bdb + 18), bsize = nmv_u16(v + bdb + 20);
        if (hsize < 22 || bdb + hsize > size) return 0;
        if (bsize > size - bdb) bsize = size - bdb;
        *off = bdb + hsize; *len = bsize - hsize;
        if (*off > size) return 0;
        if (*len > size - *off) *len = size - *off;
    }
    return 1;
}

/* the data of block 'id' (without its 3-byte header) and its size, or NULL */
static inline const uint8_t *nmv_block(const uint8_t *v, uint32_t size, unsigned id, uint32_t *blen)
{
    uint32_t off, len, p = 0; unsigned bver;
    if (!nmv_bdb(v, size, &off, &len, &bver)) return 0;
    while (len - p >= 3) {
        unsigned bid = v[off + p], bs = nmv_u16(v + off + p + 1);
        if (bs > len - p - 3) return 0;
        if (bid == id) { *blen = bs; return v + off + p + 3; }
        p += 3 + bs;
    }
    return 0;
}

/* The panel type by the panel's own identifier (Linux pnpid_get_panel_type): the table of 16 LFP entries each carries the 10 bytes
 * the panel's EDID has at offset 8 (manufacturer, product code, serial, week, year). A full match wins; a match with the date
 * cleared is accepted when no full match exists and the entry has no date. 'id' is those 10 bytes of the EDID. Returns the entry,
 * or -1. Offsets in the pointer block are from the start of the BDB; every one is bounds-checked against the data block. */
static inline int nmv_pnp_match(const uint8_t *v, uint32_t size, const uint8_t *id)
{
    uint32_t off, len, pn, dn, i, k; unsigned bver; int best = -1;
    const uint8_t *ptrs, *data; uint8_t nodate[10];
    if (!id || !nmv_bdb(v, size, &off, &len, &bver)) return -1;
    ptrs = nmv_block(v, size, NMV_BDB_LFP_PTRS, &pn);
    data = nmv_block(v, size, NMV_BDB_LFP_DATA, &dn);
    if (!ptrs || !data || pn < 1 + 16 * 9 || ptrs[0] != 3) return -1;
    {   /* where the data block's data starts, counted from the start of the BDB */
        const int64_t base = (int64_t)(data - v) - (int64_t)nmv_u32(v + 28);      /* offsets in the pointer block count from the start of the BDB */
        for (k = 0; k < 10; k++) nodate[k] = id[k];
        nodate[8] = nodate[9] = 0;
        for (i = 0; i < 16; i++) {
            const uint8_t *e = ptrs + 1 + 9 * i;                /* fp_timing, dvo_timing, panel_pnp_id: 3 bytes each (offset u16, size u8) */
            const unsigned poff = nmv_u16(e + 6), psz = e[8];
            int64_t rel = (int64_t)poff - base;
            int full = 1, nod = 1;
            if (psz != 10 || rel < 0 || rel + 10 > (int64_t)dn) return -1;
            for (k = 0; k < 10; k++) { if (data[rel + k] != id[k]) full = 0; if (data[rel + k] != nodate[k]) nod = 0; }
            if (full) return (int)i;
            if (best < 0 && nod) best = (int)i;
        }
    }
    return best;
}

/* The built-in panel's eDP drive-table choice. The panel type comes from the LFP options block; a value of 0xff means the VBT
 * names none, and then the panel's EDID identifier (edid_id, 10 bytes, or NULL when unknown) picks the entry. */
static inline int nmv_edp(const uint8_t *v, uint32_t size, const uint8_t *edid_id, struct nmv_edp *out)
{
    uint32_t off, len, n; unsigned bver;
    const uint8_t *b;
    out->known = 0; out->panel_type = 0xff; out->low_vswing = 0; out->hobl = 0; out->vbt_version = 0; out->bdb_version = 0;
    if (!nmv_bdb(v, size, &off, &len, &bver)) return 0;
    out->vbt_version = nmv_u16(v + 20); out->bdb_version = bver;
    b = nmv_block(v, size, NMV_BDB_LFP_OPTS, &n);
    if (!b || n < 1) return 0;
    out->panel_type = b[0];
    if (b[0] > 15) {                       /* 0xff: the VBT names none; try the panel's identifier */
        int m = nmv_pnp_match(v, size, edid_id);
        if (m < 0) return 0;
        out->panel_type = (unsigned)m;
    }
    b = nmv_block(v, size, NMV_BDB_EDP, &n);
    if (!b) return 0;
    /* block 27: power_seqs[16] (10 bytes each), color_depth (4), fast_link_params[16] (2 each), sdrrs (4), s3d (2),
     * t3 (2), vswing_preemph (8, version 173+) */
    if (bver >= 173) {
        if (n < 212) return 0;
        out->low_vswing = ((nmv_u64(b + 204) >> (out->panel_type * 4)) & 0xF) == 0;
    }
    /* block 44 'hobl' (u16 bitmask at 54, version 232+) */
    if (bver >= 232) {
        const uint8_t *p = nmv_block(v, size, NMV_BDB_LFP_POWER, &n);
        if (!p || n < 56) return 0;
        out->hobl = (nmv_u16(p + 54) >> out->panel_type) & 1;
    }
    out->known = 1;
    return 1;
}

/* ---- where the VBT is: the OpRegion (Linux intel_opregion.c). 'op' is the first 8 KiB of the OpRegion, 'asls' its physical
 * address. The VBT is either the raw table the ASLE mailbox points at (version 2.1 and later: an offset from the OpRegion,
 * version 2.0: a physical address) or the mailbox at 0x400. */
struct nmv_loc { int kind; uint64_t phys; uint32_t size; };   /* kind 0 none, 1 inside the 8 KiB at 'offset', 2 elsewhere at 'phys' */
#define NMV_OPREGION_SIZE 8192u
static inline int nmv_locate(const uint8_t *op, uint64_t asls, uint32_t *inline_off, uint32_t *inline_size, struct nmv_loc *out)
{
    static const char sig[] = "IntelGraphicsMem";
    unsigned i, major, minor; uint32_t mboxes, rvds; uint64_t rvda;
    out->kind = 0; out->phys = 0; out->size = 0; *inline_off = 0; *inline_size = 0;
    for (i = 0; i < 16; i++) if (op[i] != (uint8_t)sig[i]) return 0;
    minor = op[22]; major = op[23]; mboxes = nmv_u32(op + 88);
    rvda = nmv_u64(op + 0x300 + 186); rvds = nmv_u32(op + 0x300 + 194);
    if (major >= 2 && rvda && rvds) {
        /* 2.0: a physical address. 2.1 and later: an offset from the OpRegion that never points inside it */
        if (major > 2 || minor >= 1) { if (rvda < NMV_OPREGION_SIZE) goto mailbox; rvda += asls; }
        if (rvds > (1u << 20)) goto mailbox;
        out->kind = 2; out->phys = rvda; out->size = rvds;
        return 1;
    }
mailbox:
    *inline_off = 0x400;
    *inline_size = ((mboxes & (1u << 4)) ? 0x1C00u : NMV_OPREGION_SIZE) - 0x400u;
    out->kind = 1;
    return 1;
}

#endif

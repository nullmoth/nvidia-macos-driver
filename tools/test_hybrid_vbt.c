/* The video BIOS table reader (kexts/NVRM/accel/nm_hybrid_vbt.h), on the host.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_vbt.c -o /tmp/t && /tmp/t     -> 0 failed
 * Tables are built here to the layout of Linux's intel_vbt_defs.h; under -fsanitize=address,undefined the corrupted-table
 * arms prove nothing is read outside the buffer. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nm_hybrid_vbt.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
static void p16(uint8_t *p, unsigned v) { p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); }
static void p32(uint8_t *p, uint32_t v) { p16(p, v & 0xFFFF); p16(p + 2, v >> 16); }
static void p64(uint8_t *p, uint64_t v) { p32(p, (uint32_t)v); p32(p + 4, (uint32_t)(v >> 32)); }

/* a VBT with the LFP options (panel type), eDP and LFP power blocks */
static uint32_t build(uint8_t *v, unsigned bdb_ver, unsigned panel_type, uint64_t vswing_preemph, unsigned hobl_mask)
{
    uint32_t bdb = 48, p, hs = 22;
    memset(v, 0, 4096);
    memcpy(v, "$VBT CHERRYVIEW      ", 20); p16(v + 20, 230); p16(v + 22, 48); p32(v + 28, bdb);
    memcpy(v + bdb, "BIOS_DATA_BLOCK ", 16); p16(v + bdb + 16, bdb_ver); p16(v + bdb + 18, hs);
    p = bdb + hs;
    v[p] = NMV_BDB_LFP_OPTS; p16(v + p + 1, 4); v[p + 3] = (uint8_t)panel_type; p += 3 + 4;
    v[p] = NMV_BDB_EDP; p16(v + p + 1, 300); p64(v + p + 3 + 204, vswing_preemph); p += 3 + 300;
    v[p] = NMV_BDB_LFP_POWER; p16(v + p + 1, 80); p16(v + p + 3 + 54, hobl_mask); p += 3 + 80;
    p16(v + bdb + 20, p - bdb); p16(v + 24, p);
    return p;
}
int main(void)
{
    static uint8_t v[4096]; struct nmv_edp e; uint32_t n;
    n = build(v, 245, 3, 0xFFFFFFFFFFFF0FFFull, 0);
    ARM("a well-formed table is read", nmv_edp(v, n, &e) && e.known && e.panel_type == 3);
    ARM("panel type 3 with nibble 0 in the swing word: low swing", e.low_vswing == 1);
    ARM("hobl is off when the panel's bit is clear", e.hobl == 0);
    n = build(v, 245, 3, 0xFFFFFFFFFFFFFFFFull, 0x0008);
    ARM("a non-zero nibble is the normal swing table", nmv_edp(v, n, &e) && e.low_vswing == 0);
    ARM("hobl is read from the panel's bit", e.hobl == 1);
    n = build(v, 245, 4, 0xFFFFFFFFFFFFFFFFull, 0x0008);
    ARM("another panel's hobl bit does not count", nmv_edp(v, n, &e) && e.hobl == 0);
    n = build(v, 245, 0xff, 0, 0);
    ARM("a VBT that names no panel type is not guessed", !nmv_edp(v, n, &e) && !e.known && e.panel_type == 0xff);
    n = build(v, 170, 1, 0, 0);
    ARM("a table older than the swing word keeps the normal table", nmv_edp(v, n, &e) && e.known && e.low_vswing == 0 && e.hobl == 0);
    n = build(v, 245, 2, 0xFFFFFFFFFFFFFFFFull, 0);
    memcpy(v, "$XYZ", 4);
    ARM("a table without the signature is refused", !nmv_edp(v, n, &e));
    n = build(v, 245, 2, 0, 0); p32(v + 28, 0x7FFFFFFFu);
    ARM("a block offset past the end is refused", !nmv_edp(v, n, &e));
    n = build(v, 245, 2, 0, 0);
    ARM("every cut-short length is refused or read safely", ({ int ok = 1; for (uint32_t c = 0; c < n; c += 7) { struct nmv_edp x; if (nmv_edp(v, c, &x) && c < 48 + 22 + 3 + 4 + 3 + 212) ok = 0; } ok; }));
    srand(9);
    { int bad = 0; for (int t = 0; t < 6000; t++) { n = build(v, 245, 3, 0x0F0F0F0F0F0F0F0Full, 0x00FF);
          for (int k = 0; k < 8; k++) v[rand() % n] = (uint8_t)rand();
          (void)nmv_edp(v, n, &e); if (e.known && e.panel_type > 15) bad++; }
      ARM("6000 corrupted tables: nothing is read outside the buffer and a known panel type is always 0 to 15", bad == 0); }

    /* the OpRegion */
    { static uint8_t op[8192]; struct nmv_loc loc; uint32_t io, is;
      memset(op, 0, sizeof op); memcpy(op, "IntelGraphicsMem", 16); op[23] = 2; op[22] = 1;
      ARM("a 2.1 OpRegion without a raw VBT pointer: the mailbox at 0x400", nmv_locate(op, 0xDEAD0000ull, &io, &is, &loc) && loc.kind == 1 && io == 0x400 && is == 8192 - 0x400);
      p32(op + 88, 1u << 4);
      ARM("with the extension mailbox the VBT area is shorter", nmv_locate(op, 0, &io, &is, &loc) && is == 0x1C00 - 0x400);
      p64(op + 0x300 + 186, 0x2000); p32(op + 0x300 + 194, 5000); op[22] = 1;
      ARM("version 2.1 points at the raw VBT relative to the OpRegion", nmv_locate(op, 0xA0000000ull, &io, &is, &loc) && loc.kind == 2 && loc.phys == 0xA0002000ull && loc.size == 5000);
      p64(op + 0x300 + 186, 0xB0000000ull); op[22] = 0;
      ARM("version 2.0 gives a physical address", nmv_locate(op, 0xA0000000ull, &io, &is, &loc) && loc.kind == 2 && loc.phys == 0xB0000000ull);
      p64(op + 0x300 + 186, 0x100); op[22] = 1;
      ARM("a relative address inside the OpRegion itself is not trusted: the mailbox is used", nmv_locate(op, 0xA0000000ull, &io, &is, &loc) && loc.kind == 1);
      op[22] = 1; p64(op + 0x300 + 186, 0x2000); p32(op + 0x300 + 194, 0x7FFFFFFFu);
      ARM("an absurd raw VBT size is not trusted: the mailbox is used", nmv_locate(op, 0, &io, &is, &loc) && loc.kind == 1);
      memcpy(op, "IntelGraphicsMeX", 16);
      ARM("something that is not an OpRegion is refused", !nmv_locate(op, 0, &io, &is, &loc) && loc.kind == 0); }
    printf("%d failed\n", fails);
    return fails != 0;
}

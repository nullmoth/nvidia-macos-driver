/* The Intel display firmware package reader (kexts/NVRM/accel/nm_hybrid_dmc.h), on the host.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_dmc.c -o /tmp/t && /tmp/t                     synthetic packages
 *   NMH_DMC_DIR=<folder with the fetched files> /tmp/t                                          also the real ones */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nm_hybrid_dmc.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
static void put32(uint8_t *p, uint32_t v) { p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8); p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24); }

/* a package with a main program (header v3, 'words' program words) and one pipe program, package header version 2 */
static uint32_t build(uint8_t *f, uint32_t words, uint32_t main_addr, uint32_t mmio_addr)
{
    const uint32_t pkg = NMD_PKG_BYTES + 32 * NMD_FWINFO_BYTES, h3 = NMD_HDR_V3_BYTES;
    uint32_t off = NMD_CSS_BYTES + pkg, i, total;
    memset(f, 0, 1 << 20);
    put32(f + 4, NMD_CSS_BYTES / 4); put32(f + 0x50, (2u << 16) | 12u);
    f[NMD_CSS_BYTES] = (uint8_t)(pkg / 4); f[NMD_CSS_BYTES + 1] = 2; put32(f + NMD_CSS_BYTES + 12, 2);
    /* entries: main at offset 0, pipe A (id 1) after it */
    { uint8_t *e = f + NMD_CSS_BYTES + NMD_PKG_BYTES; e[1] = 0; e[2] = '*'; e[3] = '*'; put32(e + 4, 0);
      e += NMD_FWINFO_BYTES; e[1] = 1; e[2] = '*'; e[3] = '*'; put32(e + 4, (h3 + words * 4) / 4); }
    for (int part = 0; part < 2; part++) {
        uint8_t *h = f + off + (part ? h3 + words * 4 : 0);
        put32(h, 0x40403E3Eu); h[4] = (uint8_t)(h3 / 4); h[5] = 3; put32(h + 12, words);
        put32(h + 20, part ? 0x92000u : main_addr);
        put32(h + 92, 2); put32(h + 96, part ? 0x92004u : mmio_addr); put32(h + 96 + 4, part ? 0x92100u : 0x8F010u);
        put32(h + 176, 0xAA55AA55u); put32(h + 180, 0x12345678u);
        for (i = 0; i < words; i++) put32(h + h3 + 4 * i, 0xC0000000u + (part << 16) + i);
    }
    total = off + 2 * (h3 + words * 4);
    return total;
}

int main(void)
{
    static uint8_t f[1 << 20]; struct nmd_fw fw; uint32_t n;
    n = build(f, 100, 0x8F000u, 0x8F004u);
    ARM("a well-formed package parses", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_OK);
    ARM("the main program is found with its size and address", fw.part[0].present && fw.part[0].words == 100 && fw.part[0].start_mmio == 0x8F000u);
    ARM("the pipe program is found too", fw.part[1].present && fw.part[1].words == 100 && fw.part[1].start_mmio == 0x92000u);
    ARM("the CSS version is read", fw.version == ((2u << 16) | 12u));
    { static struct nmd_write w[512]; uint32_t c = nmd_plan(&fw.part[0], w, 512);
      ARM("the load plan is the program words then the register list", c == 102 && w[0].reg == 0x8F000u && w[0].val == 0xC0000000u && w[99].reg == 0x8F000u + 4 * 99 && w[100].reg == 0x8F004u && w[100].val == 0xAA55AA55u);
      ARM("a plan that does not fit the buffer is refused", nmd_plan(&fw.part[0], w, 50) == 0); }
    ARM("a file cut short is refused at every length", ({ int ok = 1; for (uint32_t cut = 0; cut < n; cut += 97) { if (nmd_parse(f, cut, '*', '*', 0x20000, &fw) == NMD_OK) ok = 0; } ok; }));
    ARM("a program larger than the generation allows is refused", nmd_parse(f, n, '*', '*', 99, &fw) == NMD_E_SIZE);
    n = build(f, 100, 0x8F000u, 0x7000u);
    ARM("a register the firmware asks to write outside the DMC blocks is refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_RANGE);
    n = build(f, 100, 0x8F000u, 0x8F004u); f[NMD_CSS_BYTES + 1] = 9;
    ARM("an unknown package version is refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_PKG);
    n = build(f, 100, 0x8F000u, 0x8F004u); put32(f + 4, 3);
    ARM("a wrong CSS length is refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_CSS);
    n = build(f, 100, 0x8F000u, 0x8F004u); put32(f + NMD_CSS_BYTES + NMD_PKG_BYTES + 4, 0x3FFFFFFFu);
    ARM("an entry offset past the end of the file is refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_SHORT);
    n = build(f, 100, 0x8F000u, 0x8F004u); { uint32_t off = NMD_CSS_BYTES + NMD_PKG_BYTES + 32 * NMD_FWINFO_BYTES; put32(f + off, 0); }
    ARM("a bad DMC signature is refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_HDR);
    n = build(f, 100, 0x8F000u, 0x8F004u); { uint32_t off = NMD_CSS_BYTES + NMD_PKG_BYTES + 32 * NMD_FWINFO_BYTES; put32(f + off + 92, 21); }
    ARM("more register writes than the format allows are refused", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_E_HDR);
    n = build(f, 100, 0x8F000u, 0x8F004u); f[NMD_CSS_BYTES + NMD_PKG_BYTES + 2] = 'B'; f[NMD_CSS_BYTES + NMD_PKG_BYTES + 3] = '0';
    ARM("an entry for another stepping is not taken as the main program", nmd_parse(f, n, 'A', '0', 0x20000, &fw) == NMD_E_NO_MAIN);
    ARM("the matching stepping is taken", nmd_parse(f, n, 'B', '0', 0x20000, &fw) == NMD_OK);
    srand(5);
    { int bad = 0;
      for (int t = 0; t < 4000; t++) {
          n = build(f, 40, 0x8F000u, 0x8F004u);
          for (int k = 0; k < 6; k++) f[rand() % n] = (uint8_t)rand();
          (void)nmd_parse(f, n, '*', '*', 0x20000, &fw);          /* must not read outside f[0..n): run under the sanitiser to prove */
          if (fw.part[0].present && (fw.part[0].payload < f || fw.part[0].payload + fw.part[0].words * 4 > f + n)) bad++;
      }
      ARM("4000 corrupted packages: a program that parses always lies inside the file", bad == 0); }

    /* the real files, when they are at hand */
    { const char *dir = getenv("NMH_DMC_DIR");
      static const struct { const char *name; uint32_t words_max; } real[] = { {"tgl_dmc_ver2_12.bin", 0x6000 / 4}, {"adlp_dmc_ver2_16.bin", 0x20000 / 4}, {"mtl_dmc.bin", 0x7000 / 4} };
      for (unsigned r = 0; dir && r < 3; r++) {
          char path[512]; snprintf(path, sizeof path, "%s/%s", dir, real[r].name);
          FILE *fp = fopen(path, "rb"); static uint8_t big[1 << 20]; size_t got = fp ? fread(big, 1, sizeof big, fp) : 0; if (fp) fclose(fp);
          char nm[160];
          snprintf(nm, sizeof nm, "real %s parses (main program %s)", real[r].name, got ? "found" : "FILE MISSING");
          ARM(nm, got && nmd_parse(big, (uint32_t)got, '*', '*', real[r].words_max, &fw) == NMD_OK && fw.part[0].words > 100 && fw.part[0].words <= real[r].words_max);
          if (got) { static struct nmd_write w[1 << 16]; uint32_t c = nmd_plan(&fw.part[0], w, 1 << 16);
                     snprintf(nm, sizeof nm, "real %s: %u words at 0x%x, %u register writes, plan of %u writes", real[r].name, fw.part[0].words, fw.part[0].start_mmio, fw.part[0].mmio_count, c);
                     ARM(nm, c == fw.part[0].words + fw.part[0].mmio_count); }
      } }

    /* the load plan and the file choice */
    { static struct nmd_write w[1 << 15]; uint32_t c;
      n = build(f, 100, 0x80000u, 0x8F034u);
      ARM("a parse for the plan", nmd_parse(f, n, '*', '*', 0x20000, &fw) == NMD_OK);
      c = nmd_load_plan(&fw, 12, w, 1 << 15);
      ARM("the plan starts by disabling every event handler of every program", c > 0 && w[0].reg == 0x8F034u && w[0].val == NMD_EVT_DISABLED && w[1].reg == 0x8F004u && w[1].val == 0);
      { int ok = 1; for (uint32_t k = 0; k < c; k++) if (w[k].reg >= 0x8F034u && w[k].reg < 0x8F054u && w[k].val != NMD_EVT_DISABLED && w[k].val != 0) ok = 0;
        ARM("no event handler is ever enabled by the plan, including through the firmware's own register list", ok); }
      ARM("the program words follow, each at its own address", ({ int ok = 0; for (uint32_t k = 0; k < c; k++) if (w[k].reg == 0x80000u + 4 * 5 && w[k].val == 0xC0000005u) ok = 1; ok; }));
      ARM("pipe programs use the version 13 base on version 13 and the older base before", nmd_base(1, 13) == 0x5F000u && nmd_base(2, 13) == 0x5F400u && nmd_base(1, 12) == 0x92000u);
      ARM("a plan that does not fit is refused whole", nmd_load_plan(&fw, 12, w, 30) == 0);
      { /* two programs, each fitting on its own, together one entry too many: the bound must be the total */
        static struct nmd_fw two; static uint8_t pay[4 * 16]; static struct nmd_write g[4 * NMD_EVT_COUNT + 20 + 64];
        memset(&two, 0, sizeof two);
        for (unsigned k = 0; k < 2; k++) { two.part[k].present = 1; two.part[k].id = (uint8_t)k; two.part[k].payload = pay; two.part[k].words = 10; two.part[k].start_mmio = 0x80000u + 0x1000u * k; }
        const uint32_t need = 4 * NMD_EVT_COUNT + 20, gcap = need - 1;
        for (uint32_t k = 0; k < sizeof g / sizeof g[0]; k++) { g[k].reg = 0xDEADBEEFu; g[k].val = 0xDEADBEEFu; }
        uint32_t got2 = nmd_load_plan(&two, 12, g, gcap);
        int clean = 1; for (uint32_t k = gcap; k < sizeof g / sizeof g[0]; k++) if (g[k].reg != 0xDEADBEEFu) clean = 0;
        ARM("two programs that fit one at a time but not together are refused, with nothing written past the end", got2 == 0 && clean);
        ARM("the same two programs with room for both give exactly the total", nmd_load_plan(&two, 12, g, need) == need); } }
    ARM("Tiger Lake, Alder Lake P and Meteor Lake each get their own file",
        !strcmp(nmd_file_for(12, 0x9a49), "tgl_dmc_ver2_12.bin") && !strcmp(nmd_file_for(13, 0x46a6), "adlp_dmc_ver2_16.bin") && !strcmp(nmd_file_for(14, 0x7d55), "mtl_dmc.bin"));
    ARM("Arrow Lake uses the Meteor Lake file", !strcmp(nmd_file_for(14, 0x7d67), "mtl_dmc.bin"));
    ARM("a part of the same generation with no fetched file gets none", nmd_file_for(12, 0x4c8a) == 0 && nmd_file_for(12, 0x4680) == 0 && nmd_file_for(20, 0x64a0) == 0);
    ARM("program size limits follow the generation", nmd_max_words(12) == 0x6000 / 4 && nmd_max_words(13) == 0x20000 / 4 && nmd_max_words(14) == 0x7000 / 4);
    { const char *dir = getenv("NMH_DMC_DIR");
      if (dir) { static const struct { const char *name; unsigned ver; } real[] = { {"tgl_dmc_ver2_12.bin", 12}, {"adlp_dmc_ver2_16.bin", 13}, {"mtl_dmc.bin", 14} };
        for (unsigned r = 0; r < 3; r++) { char path[512], nm[200]; snprintf(path, sizeof path, "%s/%s", dir, real[r].name);
          FILE *fp = fopen(path, "rb"); static uint8_t big[1 << 20]; size_t got = fp ? fread(big, 1, sizeof big, fp) : 0; if (fp) fclose(fp);
          static struct nmd_write w[1 << 15];
          int rc = got ? nmd_parse(big, (uint32_t)got, '*', '*', nmd_max_words(real[r].ver), &fw) : -1; uint32_t c = rc == NMD_OK ? nmd_load_plan(&fw, real[r].ver, w, 1 << 15) : 0;
          int evt_ok = c > 0; for (uint32_t k = 0; k < c; k++) for (unsigned id = 0; id < NMD_MAX_IDS; id++) if (fw.part[id].present && nmd_is_evt_ctl(id, real[r].ver, w[k].reg) && w[k].val != NMD_EVT_DISABLED && w[k].val != 0) evt_ok = 0;
          snprintf(nm, sizeof nm, "real %s: load plan of %u writes, every event handler left disabled", real[r].name, c); ARM(nm, c > 0 && evt_ok); } } }

    /* the stepping */
    ARM("Tiger Lake U/Y: revision 3 is C0", ({ struct nmd_step x = nmd_stepping(12, 0x9a49, 3, 0); x.st == 'C' && x.sub == '0'; }));
    ARM("Tiger Lake H: revision 1 is B0", ({ struct nmd_step x = nmd_stepping(12, 0x9a60, 1, 0); x.st == 'B' && x.sub == '0'; }));
    ARM("Tiger Lake U/Y and H differ for the same revision 2", ({ struct nmd_step a = nmd_stepping(12, 0x9a49, 2, 0), b = nmd_stepping(12, 0x9a60, 2, 0); a.st == 'B' && a.sub == '1' && b.st == '*'; }));
    ARM("Alder Lake P: revisions 0, 4, 8 and 12 are A0, B0, C0, C0", ({ struct nmd_step a = nmd_stepping(13, 0x46a6, 0, 0), b = nmd_stepping(13, 0x46a6, 4, 0), c = nmd_stepping(13, 0x46a6, 8, 0), d = nmd_stepping(13, 0x46a6, 12, 0);
        a.st == 'A' && b.st == 'B' && c.st == 'C' && d.st == 'C' && d.sub == '0'; }));
    ARM("Alder Lake P: a gap in the table takes the next revision's stepping", ({ struct nmd_step x = nmd_stepping(13, 0x46a6, 5, 0); x.st == 'C' && x.sub == '0'; }));
    ARM("Alder Lake P: past the table is the wildcard", ({ struct nmd_step x = nmd_stepping(13, 0x46a6, 13, 0); x.st == '*' && x.sub == '*'; }));
    ARM("Raptor Lake P revision 4 is C0, Alder Lake N revision 0 is A0", ({ struct nmd_step a = nmd_stepping(13, 0xa7a0, 4, 0), b = nmd_stepping(13, 0x46d1, 0, 0); a.st == 'C' && b.st == 'A'; }));
    ARM("Meteor Lake reads the display step from the GMD register", ({ struct nmd_step x = nmd_stepping(14, 0x7d55, 0, (14u << 22) | (0u << 14) | 4); x.st == 'B' && x.sub == '0'; }));
    ARM("a GMD register that is not version 14 is the wildcard", ({ struct nmd_step x = nmd_stepping(14, 0x7d55, 0, (20u << 22) | 4); x.st == '*'; }));
    ARM("an unlisted part is the wildcard", ({ struct nmd_step x = nmd_stepping(12, 0x4c8a, 0, 0); x.st == '*' && x.sub == '*'; }));
    ARM("Raptor Lake P and Alder Lake N get the Alder Lake P file", !strcmp(nmd_file_for(13, 0xa7a0), "adlp_dmc_ver2_16.bin") && !strcmp(nmd_file_for(13, 0x46d2), "adlp_dmc_ver2_16.bin"));
    { const char *dir = getenv("NMH_DMC_DIR");
      if (dir) { char path[512]; snprintf(path, sizeof path, "%s/adlp_dmc_ver2_16.bin", dir);
        FILE *fp = fopen(path, "rb"); static uint8_t big[1 << 20]; size_t got = fp ? fread(big, 1, sizeof big, fp) : 0; if (fp) fclose(fp);
        struct nmd_fw fa, fc, fw2; int ra = got ? nmd_parse(big, (uint32_t)got, 'A', '0', nmd_max_words(13), &fa) : -1, rc2 = got ? nmd_parse(big, (uint32_t)got, 'C', '0', nmd_max_words(13), &fc) : -1;
        (void)fw2;
        ARM("real Alder Lake P file: an A0 part takes the A-stepping main program", ra == NMD_OK && fa.part[0].words == 6195);
        ARM("real Alder Lake P file: a C0 part takes the generic main program, a different one", rc2 == NMD_OK && fc.part[0].words != fa.part[0].words);
        ARM("real Alder Lake P file: the wildcard query is what the old code got, the A program", ({ struct nmd_fw fx; nmd_parse(big, (uint32_t)got, '*', '*', nmd_max_words(13), &fx) == NMD_OK && fx.part[0].words == fa.part[0].words; })); } }
    printf("%d failed\n", fails);
    return fails != 0;
}

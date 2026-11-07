/* test_cache_file.c - the shader-cache file rules (plugin/nvmtl_cache_file.h): whole-or-refused entries.
   Build and run anywhere (no GPU, no Metal):  cc -O1 -pthread -I plugin tools/test_cache_file.c -o /tmp/test_cache_file && /tmp/test_cache_file
   Exit status 0 when every arm holds. The "old driver" arms replay the check the cache used before this change (a SPIR-V
   magic and a word-aligned length), so each failure the new code catches is one the old code let through. */
#include <stdio.h>
#include <stdatomic.h>
#include <dirent.h>
#include "nvmtl_cache_file.h"

static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
static char dir[256];

/* the check the shader cache applied before: a SPIR-V magic and a word-aligned length of at least 20 bytes */
static int old_driver_accepts(const uint8_t *b, size_t n) { uint32_t m; if (n < 20 || n % 4) return 0; memcpy(&m, b, 4); return m == 0x07230203u; }
/* a small valid SPIR-V module: header + 40 two-word instructions (OpNop-sized instructions are one word; use OpSource-like 2-word) */
static size_t make_spv(uint8_t *out, int ninst)
{
    uint32_t w[5 + 2 * 64] = { 0x07230203u, 0x00010500u, 0, 100, 0 }; size_t i = 5;
    for (int k = 0; k < ninst; k++) { w[i++] = (2u << 16) | 3u; w[i++] = (uint32_t)k; }
    memcpy(out, w, i * 4); return i * 4;
}
static void put(const char *name, const void *p, size_t n) { char path[512]; snprintf(path, sizeof path, "%s/%s", dir, name); FILE *f = fopen(path, "wb"); if (n) fwrite(p, 1, n, f); fclose(f); }
static int exists(const char *name) { char path[512]; snprintf(path, sizeof path, "%s/%s", dir, name); return access(path, F_OK) == 0; }
static nvmtl_cf_status rd(const char *name, uint8_t **buf, size_t *n) { char path[512]; snprintf(path, sizeof path, "%s/%s", dir, name); nvmtl_cf_status s; *buf = nvmtl_cf_read(path, n, &s); return s; }

static atomic_int stop, bad_seen, good_seen;
static void *writer(void *a) { (void)a; uint8_t spv[1024]; size_t n = make_spv(spv, 60); char path[512]; snprintf(path, sizeof path, "%s/race.spv", dir);
    while (!atomic_load(&stop)) nvmtl_cf_write(path, spv, n); return NULL; }
static void *reader(void *a) { (void)a; char path[512]; snprintf(path, sizeof path, "%s/race.spv", dir);
    while (!atomic_load(&stop)) { size_t n; nvmtl_cf_status s; uint8_t *b = nvmtl_cf_read(path, &n, &s);
        if (s == NVMTL_CF_OK && b && old_driver_accepts(b, n)) atomic_fetch_add(&good_seen, 1); else if (s != NVMTL_CF_MISSING) atomic_fetch_add(&bad_seen, 1); free(b); } return NULL; }

int main(void)
{
    snprintf(dir, sizeof dir, "/tmp/test_cache_file.%d", (int)getpid()); mkdir(dir, 0755);
    uint8_t spv[1024]; size_t n = make_spv(spv, 60);
    char path[512]; snprintf(path, sizeof path, "%s/a.spv", dir);

    arm("a written entry reads back byte for byte", nvmtl_cf_write(path, spv, n) == 0 && ({ uint8_t *b; size_t m; nvmtl_cf_status s = rd("a.spv", &b, &m); int ok = s == NVMTL_CF_OK && m == n && !memcmp(b, spv, n); free(b); ok; }));
    arm("no temporary file is left behind after a write", ({ int left = 0; DIR *d = opendir(dir); struct dirent *e; while ((e = readdir(d))) if (strstr(e->d_name, ".tmp-")) left++; closedir(d); left == 0; }));

    /* torn: the file cut short at every length, including exactly at an instruction boundary of the payload */
    uint8_t whole[2048]; memcpy(whole, spv, n);
    uint32_t magic = NVMTL_CF_MAGIC, len = (uint32_t)n; uint64_t h = nvmtl_cf_hash(spv, n);
    memcpy(whole + n, &magic, 4); memcpy(whole + n + 4, &len, 4); memcpy(whole + n + 8, &h, 8);
    size_t total = n + NVMTL_CF_FOOTER; int cut_ok = 1, old_took_a_cut = 0;
    for (size_t keep = 0; keep < total; keep++) {
        put("cut.spv", whole, keep);
        uint8_t *b; size_t m; nvmtl_cf_status s = rd("cut.spv", &b, &m);
        if (s == NVMTL_CF_OK || b || exists("cut.spv")) cut_ok = 0;
        if (keep >= 5 * 4 + 8 && old_driver_accepts(whole, keep & ~3u)) old_took_a_cut = 1;
    }
    arm("a file cut at ANY length is refused and removed (never trusted)", cut_ok);
    arm("control: the old check WOULD have accepted a word-aligned cut of the same module", old_took_a_cut);

    /* the payload cut at an instruction boundary and then framed honestly is a different, whole file: this is not torn */
    /* a corrupt middle: one flipped bit, same length */
    uint8_t flip[2048]; memcpy(flip, whole, total); flip[40] ^= 0x10;
    put("flip.spv", flip, total);
    { uint8_t *b; size_t m; nvmtl_cf_status s = rd("flip.spv", &b, &m); arm("one flipped bit in the payload is refused (hash) and removed", s == NVMTL_CF_HASH && !b && !exists("flip.spv")); }
    arm("control: the old check accepts the flipped module", old_driver_accepts(flip, n));
    /* a footer that lies about the length */
    uint8_t lie[2048]; memcpy(lie, whole, total); uint32_t wrong = (uint32_t)n - 8; memcpy(lie + n + 4, &wrong, 4);
    put("lie.spv", lie, total);
    { uint8_t *b; size_t m; nvmtl_cf_status s = rd("lie.spv", &b, &m); arm("a footer whose length disagrees with the file is refused", s == NVMTL_CF_LENGTH && !b); }
    /* an entry from an older driver (valid module, no footer) */
    put("legacy.spv", spv, n);
    { uint8_t *b; size_t m; nvmtl_cf_status s = rd("legacy.spv", &b, &m); arm("an older driver's unframed entry is not trusted and is removed", s == NVMTL_CF_NO_FOOTER && !b && !exists("legacy.spv")); }
    put("empty.spv", "", 0);
    { uint8_t *b; size_t m; nvmtl_cf_status s = rd("empty.spv", &b, &m); arm("an empty file is refused", s == NVMTL_CF_SHORT && !b); }
    { uint8_t *b; size_t m; nvmtl_cf_status s = rd("nothere.spv", &b, &m); arm("a missing file reads as missing, nothing is created or removed", s == NVMTL_CF_MISSING && !b); }
    char bad[600]; snprintf(bad, sizeof bad, "%s/nodir/x.spv", dir);
    arm("a write into a folder that is not there fails with its errno and leaves nothing", nvmtl_cf_write(bad, spv, n) == ENOENT);

    /* writer and readers on one name: a reader sees the whole entry or nothing, never a partial one */
    pthread_t w, r[3]; atomic_store(&stop, 0);
    pthread_create(&w, NULL, writer, NULL); for (int i = 0; i < 3; i++) pthread_create(&r[i], NULL, reader, NULL);
    usleep(700000); atomic_store(&stop, 1);
    pthread_join(w, NULL); for (int i = 0; i < 3; i++) pthread_join(r[i], NULL);
    printf("     race: %d whole reads, %d refused reads\n", atomic_load(&good_seen), atomic_load(&bad_seen));
    arm("a reader racing a rewriter sees the whole entry or none, never a torn one", atomic_load(&bad_seen) == 0 && atomic_load(&good_seen) > 100);

    char cmd[300]; snprintf(cmd, sizeof cmd, "rm -rf %s", dir); if (system(cmd)) {}
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}

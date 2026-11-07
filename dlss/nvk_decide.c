/* nvk_decide - the patched nvk_cubin_module_init decision, run offline over the fatbins DLSS really uploaded.
 * Uses Mesa's nv_fatbin.c / nv_cubin.c AFTER patch_dlss_cache.py, and the same cache-path rule and checks as the patch
 * (minus the root-owner test, which a user-owned scratch cache cannot pass; that test is checked in source separately).
 * Usage: nvk_decide <device sm> <cache dir> <module.bin>...  -> one line per module + TOTAL. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nv_cubin.h"
#include "nv_fatbin.h"
#include "blake3/blake3.h"

static void *cache_read(const char *dir, const void *ptx, size_t n, unsigned sm, size_t *out)
{
    blake3_hasher h; unsigned char d[32]; char hex[65], path[1024];
    blake3_hasher_init(&h); blake3_hasher_update(&h, ptx, n); blake3_hasher_finalize(&h, d, 32);
    for (int i = 0; i < 32; i++) sprintf(hex + 2 * i, "%02x", d[i]);
    snprintf(path, sizeof path, "%s/%s.sm%u.cubin", dir, hex, sm);
    FILE *f = fopen(path, "rb"); if (!f) return NULL;
    fseek(f, 0, SEEK_END); long l = ftell(f); rewind(f);
    void *b = malloc((size_t)l); if (fread(b, 1, (size_t)l, f) != (size_t)l) { free(b); b = NULL; }
    fclose(f); *out = (size_t)l; return b;
}

int main(int argc, char **argv)
{
    unsigned sm = (unsigned)atoi(argv[1]); const char *dir = argv[2];
    int sass = 0, cache = 0, fail = 0;
    for (int a = 3; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); long n = ftell(f); rewind(f);
        void *buf = malloc((size_t)n); fread(buf, 1, (size_t)n, f); fclose(f);
        struct nv_fatbin fb; const void *data = NULL; size_t size = 0; void *owned = NULL; const char *how = "sass";
        if (!nv_fatbin_init(&fb, buf, (size_t)n)) { data = buf; size = (size_t)n; how = "cubin"; }
        else if (!nv_fatbin_get_bytecode(&fb, sm, false, &data, &size)) {
            const void *ptx; size_t pn;
            if (nv_fatbin_get_ptx(&fb, sm, &ptx, &pn)) owned = cache_read(dir, ptx, pn, sm, &size);
            data = owned; how = "cache";
        }
        struct nv_cubin_module m; memset(&m, 0, sizeof m);
        int ok = data && nv_cubin_module_init(&m, data, size) && nv_is_sm_compatible(sm, m.sm, false);
        if (!ok && !owned && nv_fatbin_init(&fb, buf, (size_t)n)) {   /* the second fallback: SASS that did not parse */
            const void *ptx; size_t pn;
            nv_cubin_module_fini(&m); memset(&m, 0, sizeof m);
            if (nv_fatbin_get_ptx(&fb, sm, &ptx, &pn) && (owned = cache_read(dir, ptx, pn, sm, &size))) {
                ok = nv_cubin_module_init(&m, owned, size) && nv_is_sm_compatible(sm, m.sm, false); how = "cache2";
            }
        }
        if (ok) { if (owned) cache++; else sass++; } else fail++;
        printf("%s %s sm_%u funcs=%u %s\n", ok ? "ok  " : "FAIL", how, ok ? m.sm : 0, ok ? m.function_count : 0, argv[a]);
        nv_cubin_module_fini(&m); free(owned); free(buf);
    }
    printf("TOTAL device sm_%u: loaded from fatbin SASS %d, from install-time cache %d, failed %d\n", sm, sass, cache, fail);
    return fail ? 1 : 0;
}

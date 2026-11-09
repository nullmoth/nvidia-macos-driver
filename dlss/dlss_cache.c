/* dlss_cache - build the DLSS kernel cache NVK falls back to (nvk/dlss/patch_dlss_cache.py), on the user's Mac, from
 * the user's own downloads: NVIDIA's DLSS runtimes and NVIDIA's nvJitLink. Nothing NVIDIA-made ships with the driver.
 *
 * For every fatbin in every runtime it takes the PTX object NVK would pick for this SM (nv_fatbin_get_ptx: the highest
 * PTX target not above the SM, else the lowest), compiles it for the SM with nvJitLink plus a trap __assertfail, and
 * writes <BLAKE3 of the PTX object as stored>.sm<SM>.cubin. NVK reads an entry only when the fatbin itself has nothing it
 * can load, so compiling every fatbin is a safe superset and needs no prediction.
 *
 * Usage: dlss_cache <libnvJitLink.so> <sm, e.g. 120> <out dir> <assertfail_trap.ptx> <runtime.so>...
 * Exit 0 only if every selected PTX object compiled or was knowingly skipped (zstd-packed, which macOS cannot unpack
 * here; reported). */
#include "elfhost.h"
#include "blake3/blake3.h"

#include <compression.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef void *jl_handle;
typedef int (*jl_create_t)(jl_handle *, unsigned, const char **);
typedef int (*jl_add_t)(jl_handle, int, const void *, size_t, const char *);
typedef int (*jl_complete_t)(jl_handle);
typedef int (*jl_size_t)(jl_handle, size_t *);
typedef int (*jl_get_t)(jl_handle, void *);
typedef int (*jl_destroy_t)(jl_handle *);
enum { JL_INPUT_PTX = 2 };
static jl_create_t jl_create; static jl_add_t jl_add; static jl_complete_t jl_complete; static jl_destroy_t jl_destroy;
static jl_size_t jl_cubin_size, jl_err_size; static jl_get_t jl_cubin, jl_err;

static void *sym(elfhost_module *m, const char *base)
{
    char n[128]; snprintf(n, sizeof n, "__%s_12_9", base);
    void *p = elfhost_sym(m, n);
    if (!p) { fprintf(stderr, "dlss_cache: nvJitLink lacks %s (12.9 API expected)\n", n); exit(2); }
    return p;
}
static uint8_t *slurp(const char *p, size_t *n)
{
    FILE *f = fopen(p, "rb"); if (!f) return NULL;
    fseek(f, 0, SEEK_END); long l = ftell(f); rewind(f);
    if (l < 0) { fclose(f); return NULL; }
    uint8_t *b = malloc((size_t)l + 1);
    if (!b || fread(b, 1, (size_t)l, f) != (size_t)l) { fclose(f); free(b); return NULL; }
    b[l] = 0; fclose(f); *n = (size_t)l; return b;
}
static void hexkey(const uint8_t *data, size_t n, char out[65])
{
    blake3_hasher h; uint8_t d[32];
    blake3_hasher_init(&h); blake3_hasher_update(&h, data, n); blake3_hasher_finalize(&h, d, 32);
    for (int i = 0; i < 32; i++) sprintf(out + 2 * i, "%02x", d[i]);
}
/* atomic: a reader never sees a half-written cubin */
static int write_file(const char *dir, const char *name, const void *data, size_t n)
{
    char tmp[1024], dst[1024];
    snprintf(dst, sizeof dst, "%s/%s", dir, name); snprintf(tmp, sizeof tmp, "%s/.%s.tmp", dir, name);
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0644);
    if (fd < 0) return -1;
    const uint8_t *p = data; size_t left = n;
    while (left) { ssize_t w = write(fd, p, left); if (w <= 0) { close(fd); unlink(tmp); return -1; } p += w; left -= (size_t)w; }
    if (fchmod(fd, 0644) || close(fd)) { unlink(tmp); return -1; }
    return rename(tmp, dst);
}

/* fatbin layout as NVK reads it (nv_fatbin_defs.h): header {magic, version, header_size, size}; objects
 * {kind u16, ?, header_size u32, size u64, ..., sm u32 @28}; compressed payloads carry the unpacked size at +0x38 and
 * the packed length at +0x10. */
struct obj { uint16_t kind; uint32_t sm; const uint8_t *hdr; const uint8_t *payload; uint64_t size; };

static int compile_one(const struct obj *o, unsigned sm, const char *outdir, const uint8_t *trap, size_t trap_n,
                       int *zstd_skipped)
{
    uint64_t raw = *(const uint64_t *)(o->hdr + 0x38);
    uint32_t clen = *(const uint32_t *)(o->hdr + 0x10);
    char key[65], name[128];
    hexkey(o->payload, (size_t)o->size, key);
    snprintf(name, sizeof name, "%s.sm%u.cubin", key, sm);
    char *ptx = NULL; size_t ptx_n = 0;
    if (raw) {
        if (clen > o->size) return -1;
        if (clen >= 4 && !memcmp(o->payload, "\x28\xb5\x2f\xfd", 4)) { (*zstd_skipped)++; return 0; }
        ptx = malloc(raw + 1);
        size_t got = compression_decode_buffer((uint8_t *)ptx, raw, o->payload, clen, NULL, COMPRESSION_LZ4_RAW);
        if (got == 0) { free(ptx); fprintf(stderr, "  %s: LZ4 unpack failed\n", key); return -1; }
        ptx_n = got;
    } else {
        ptx = malloc(o->size + 1); memcpy(ptx, o->payload, o->size); ptx_n = o->size;
    }
    while (ptx_n && ptx[ptx_n - 1] == 0) ptx_n--;
    ptx[ptx_n] = 0;

    char arch[32]; snprintf(arch, sizeof arch, "-arch=sm_%u", sm);
    const char *opts[] = { arch };
    jl_handle h = NULL;
    int r = jl_create(&h, 1, opts);
    if (!r) r = jl_add(h, JL_INPUT_PTX, ptx, ptx_n + 1, key);
    if (!r) r = jl_add(h, JL_INPUT_PTX, trap, trap_n + 1, "assertfail_trap");
    if (!r) r = jl_complete(h);
    int rc = 0;
    if (r) {
        size_t es = 0; char *log = NULL;
        if (h && !jl_err_size(h, &es) && es) { log = calloc(1, es + 1); jl_err(h, log); }
        fprintf(stderr, "  %s: nvJitLink rc=%d: %.200s\n", key, r, log ? log : "(no log)");
        free(log); rc = -1;
    } else {
        size_t cs = 0; jl_cubin_size(h, &cs);
        void *c = malloc(cs); jl_cubin(h, c);
        if (write_file(outdir, name, c, cs)) { fprintf(stderr, "  %s: write failed: %s\n", name, strerror(errno)); rc = -1; }
        free(c);
    }
    if (h) jl_destroy(&h);
    free(ptx);
    return rc;
}

int main(int argc, char **argv)
{
    if (argc < 6) { fprintf(stderr, "usage: dlss_cache <libnvJitLink.so> <sm> <outdir> <assertfail_trap.ptx> <runtime.so>...\n"); return 2; }
    char err[256];
    elfhost_module *m = elfhost_load(argv[1], NULL, NULL, err, sizeof err);
    if (!m || elfhost_unresolved(m)) { fprintf(stderr, "dlss_cache: cannot load nvJitLink: %s\n", err); return 2; }
    jl_create = sym(m, "nvJitLinkCreate"); jl_add = sym(m, "nvJitLinkAddData"); jl_complete = sym(m, "nvJitLinkComplete");
    jl_destroy = sym(m, "nvJitLinkDestroy"); jl_cubin_size = sym(m, "nvJitLinkGetLinkedCubinSize");
    jl_cubin = sym(m, "nvJitLinkGetLinkedCubin"); jl_err_size = sym(m, "nvJitLinkGetErrorLogSize"); jl_err = sym(m, "nvJitLinkGetErrorLog");
    unsigned sm = (unsigned)strtoul(argv[2], NULL, 10);
    const char *outdir = argv[3];
    size_t trap_n; uint8_t *trap = slurp(argv[4], &trap_n);
    if (!sm || !trap) { fprintf(stderr, "dlss_cache: bad SM or trap module\n"); return 2; }

    int fatbins = 0, built = 0, failed = 0, zstd = 0, no_ptx = 0, native_only = 0;
    for (int a = 5; a < argc; a++) {
        size_t n; uint8_t *d = slurp(argv[a], &n);
        if (!d) { fprintf(stderr, "dlss_cache: cannot read %s\n", argv[a]); failed++; continue; }
        for (size_t i = 0; i + 16 <= n; i++) {
            if (*(const uint32_t *)(d + i) != 0xBA55ED50u) continue;
            uint16_t hs = *(const uint16_t *)(d + i + 6); uint64_t size = *(const uint64_t *)(d + i + 8);
            if (hs != 16 || size > n - i - hs) continue;
            const uint8_t *p = d + i + hs, *end = p + size;
            struct obj best = {0}, low = {0}; low.sm = UINT32_MAX; int have_best = 0, have_low = 0;
            while ((size_t)(end - p) >= 64) {
                uint16_t kind = *(const uint16_t *)p; uint32_t ohs = *(const uint32_t *)(p + 4);
                uint64_t osz = *(const uint64_t *)(p + 8); uint32_t osm = *(const uint32_t *)(p + 28);
                if (ohs < 64 || ohs > (size_t)(end - p) || osz > (size_t)(end - p - ohs)) break;
                if (kind != 1 && kind != 2) break;
                struct obj o = { kind, osm, p, p + ohs, osz };
                if (kind == 1) {
                    if (osm <= sm && (!have_best || osm >= best.sm)) { best = o; have_best = 1; }
                    if (osm < low.sm) { low = o; have_low = 1; }
                }
                p += ohs + osz;
            }
            fatbins++;
            if (!have_best && !have_low) { no_ptx++; continue; }
            /* PTX only compiles forward: if every PTX target is above this SM (DLSS's lowest is sm_80, so Turing), the
             * fatbin's own SASS is the only code for this card and NVK loads it directly. */
            if (!have_best) { native_only++; i += hs + size - 1; continue; }
            if (compile_one(have_best ? &best : &low, sm, outdir, trap, trap_n, &zstd)) failed++; else built++;
            i += hs + size - 1;
        }
        free(d);
    }
    printf("dlss_cache sm_%u: fatbins %d, compiled %d, failed %d, zstd-packed skipped %d, without PTX %d, "
           "native SASS only %d\n", sm, fatbins, built - zstd, failed, zstd, no_ptx, native_only);
    return failed ? 1 : 0;
}

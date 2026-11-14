// xlate_harness <libnvmtl_translate.dylib> <mode> ...: drives the translator through the entry points the plugin uses.
//   translate <in.ll> <vertex|fragment|kernel> <out.spv>
//   mesh      <in.ll> <entry> <out-prefix>            nvmtl_lower_mesh, then translate the kernel and vertex halves
//   mesh2     <mesh.ll> <mesh-entry> <out-prefix> <object.ll> <object-entry>   nvmtl_lower_mesh2 (object stage linked, cap 64)
//   fc        <in.ll> <stage> <out.spv> <index>=<hex payload>...   nvmtl_translate_fc with function constant values
//   link      <in.ll> <stage> <out.spv> <symbol=module.ll>...   nvmtl_visible_refs + nvmtl_link_visible, then translate
//   table     <in.ll> <stage> <out.spv> <candidate>...          nvmtl_link_runtime (candidates live in <in.ll>), then translate
// Exit 0 = translated, 2 = refused (reason on stdout), 3 = usage or load error.
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int (*translate_fn)(const char *, const char *, uint8_t **, size_t *, char *, size_t);
typedef int (*refs_fn)(const char *, uint8_t **, size_t *, char *, size_t);
typedef int (*link_fn)(const char *, const char *, const char **, const char **, size_t, uint8_t **, size_t *, char *, size_t);
typedef int (*rt_fn)(const char *, const char *, const char **, const char **, size_t, const char **, const char **, size_t,
                     uint8_t **, size_t *, char *, size_t);
typedef int (*fc_fn)(const char *, const char *, const uint32_t *, const uint32_t *, const uint8_t **, size_t, uint8_t **, size_t *, char *, size_t);
typedef int (*mesh2_fn)(const char *, const char *, const char *, const char *, uint32_t, uint32_t, uint8_t **, size_t *, uint8_t **, size_t *,
                        uint8_t **, size_t *, uint32_t *, char *, size_t);
typedef int (*mesh_fn)(const char *, const char *, uint8_t **, size_t *, uint8_t **, size_t *, uint32_t *, char *, size_t);

static char *slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) { printf("cannot open %s\n", path); exit(3); }
    fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
    char *s = malloc(n + 1);
    if (fread(s, 1, n, f) != (size_t)n) exit(3);
    s[n] = 0; fclose(f); return s;
}

static int translate_to(void *h, const char *ll, const char *stage, const char *out_path) {
    translate_fn t = (translate_fn)dlsym(h, "nvmtl_translate");
    uint8_t *out = 0; size_t len = 0; char err[4096] = {0};
    if (t(ll, stage, &out, &len, err, sizeof err) != 0) { printf("%s\n", err); return 2; }
    FILE *o = fopen(out_path, "wb"); fwrite(out, 1, len, o); fclose(o);
    printf("spv %zu bytes\n", len);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 5) return 3;
    void *h = dlopen(argv[1], RTLD_NOW);
    if (!h) { printf("dlopen: %s\n", dlerror()); return 3; }
    const char *mode = argv[2];
    char *ll = slurp(argv[3]);
    char err[4096] = {0};
    if (!strcmp(mode, "translate")) return translate_to(h, ll, argv[4], argv[5]);
    if (!strcmp(mode, "mesh")) {
        mesh_fn m = (mesh_fn)dlsym(h, "nvmtl_lower_mesh");
        if (!m) { printf("no nvmtl_lower_mesh\n"); return 3; }
        uint8_t *k = 0, *v = 0; size_t kl = 0, vl = 0; uint32_t layout[7];
        if (m(ll, argv[4], &k, &kl, &v, &vl, layout, err, sizeof err) != 0) { printf("lower_mesh: %s\n", err); return 2; }
        char p[1024]; int rc;
        snprintf(p, sizeof p, "%s.kernel.spv", argv[5]);
        if ((rc = translate_to(h, (char *)k, "kernel", p))) { printf("mesh kernel half refused\n"); return rc; }
        snprintf(p, sizeof p, "%s.vertex.spv", argv[5]);
        if ((rc = translate_to(h, (char *)v, "vertex", p))) { printf("mesh vertex half refused\n"); return rc; }
        return 0;
    }
    if (!strcmp(mode, "mesh2")) {
        mesh2_fn m = (mesh2_fn)dlsym(h, "nvmtl_lower_mesh2");
        if (!m) { printf("no nvmtl_lower_mesh2\n"); return 3; }
        char *obj = slurp(argv[6]);
        uint8_t *o = 0, *k = 0, *v = 0; size_t ol = 0, kl = 0, vl = 0; uint32_t layout[9];
        if (m(obj, argv[7], ll, argv[4], 1, 64, &o, &ol, &k, &kl, &v, &vl, layout, err, sizeof err) != 0) { printf("lower_mesh2: %s\n", err); return 2; }
        char p[1024]; int rc;
        snprintf(p, sizeof p, "%s.object.spv", argv[5]);
        if ((rc = translate_to(h, (char *)o, "kernel", p))) { printf("object stage refused\n"); return rc; }
        snprintf(p, sizeof p, "%s.kernel.spv", argv[5]);
        if ((rc = translate_to(h, (char *)k, "kernel", p))) { printf("mesh kernel half refused\n"); return rc; }
        snprintf(p, sizeof p, "%s.vertex.spv", argv[5]);
        if ((rc = translate_to(h, (char *)v, "vertex", p))) { printf("mesh vertex half refused\n"); return rc; }
        return 0;
    }
    if (!strcmp(mode, "fc")) {
        fc_fn f = (fc_fn)dlsym(h, "nvmtl_translate_fc");
        if (!f) { printf("no nvmtl_translate_fc\n"); return 3; }
        int n = argc - 6;
        uint32_t *idx = calloc(n + 1, 4), *sz = calloc(n + 1, 4); const uint8_t **pl = calloc(n + 1, sizeof *pl);
        for (int i = 0; i < n; i++) {
            char *eq = strchr(argv[6 + i], '='); if (!eq) return 3;
            *eq = 0; idx[i] = (uint32_t)atoi(argv[6 + i]);
            size_t hl = strlen(eq + 1) / 2; uint8_t *b = malloc(hl + 1);
            for (size_t j = 0; j < hl; j++) { unsigned x; sscanf(eq + 1 + 2 * j, "%2x", &x); b[j] = (uint8_t)x; }
            sz[i] = (uint32_t)hl; pl[i] = b;
        }
        uint8_t *out = 0; size_t len = 0;
        if (f(ll, argv[4], idx, sz, pl, n, &out, &len, err, sizeof err) != 0) { printf("%s\n", err); return 2; }
        FILE *o = fopen(argv[5], "wb"); fwrite(out, 1, len, o); fclose(o);
        printf("spv %zu bytes\n", len);
        return 0;
    }
    if (!strcmp(mode, "link")) {
        refs_fn r = (refs_fn)dlsym(h, "nvmtl_visible_refs");
        link_fn l = (link_fn)dlsym(h, "nvmtl_link_visible");
        if (!r || !l) { printf("no link entry points\n"); return 3; }
        uint8_t *refs = 0; size_t rl = 0;
        if (r(ll, &refs, &rl, err, sizeof err) != 0) { printf("visible_refs: %s\n", err); return 2; }
        int n = argc - 6;
        const char **syms = calloc(n + 1, sizeof *syms), **mods = calloc(n + 1, sizeof *mods);
        for (int i = 0; i < n; i++) {
            char *eq = strchr(argv[6 + i], '=');
            if (!eq) return 3;
            *eq = 0; syms[i] = argv[6 + i]; mods[i] = slurp(eq + 1);
        }
        uint8_t *out = 0; size_t len = 0;
        if (l(ll, argv[4], syms, mods, n, &out, &len, err, sizeof err) != 0) { printf("link_visible: %s\n", err); return 2; }
        return translate_to(h, (char *)out, argv[4], argv[5]);
    }
    if (!strcmp(mode, "table")) {
        rt_fn l = (rt_fn)dlsym(h, "nvmtl_link_runtime");
        if (!l) { printf("no nvmtl_link_runtime\n"); return 3; }
        int n = argc - 6;
        const char **cn = calloc(n + 1, sizeof *cn), **cm = calloc(n + 1, sizeof *cm);
        for (int i = 0; i < n; i++) { cn[i] = argv[6 + i]; cm[i] = ll; }
        uint8_t *out = 0; size_t len = 0;
        if (l(ll, argv[4], NULL, NULL, 0, cn, cm, n, &out, &len, err, sizeof err) != 0) { printf("link_runtime: %s\n", err); return 2; }
        return translate_to(h, (char *)out, argv[4], argv[5]);
    }
    return 3;
}

/* test_app_profile.c - the application-profile rules (plugin/nvmtl_app_profile.h), run against this process's environment.
   Build and run anywhere (no GPU, no Metal):  cc -I plugin tools/test_app_profile.c -o /tmp/test_app_profile && /tmp/test_app_profile
   Optional: /tmp/test_app_profile <original.spv> <guard.spv> also proves the shipped kernel edit turns the original
   translated module (FNV-1a d57e32ce) into the known-good guard module (bd4ccf94, 113,592 bytes), byte for byte. Exit status 0 when every arm holds. */
#include <stdio.h>
#include <unistd.h>
#include "nvmtl_app_profile.h"
#include "nvmtl_spvcache_name.h"

static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
static void clear(const nvmtl_app_profile *p) { for (unsigned i = 0; i < p->nknobs; i++) unsetenv(p->knobs[i].key); }
static int none_set(const nvmtl_app_profile *p) { for (unsigned i = 0; i < p->nknobs; i++) if (getenv(p->knobs[i].key)) return 0; return 1; }
static int all_as_profile(const nvmtl_app_profile *p, const char *except)
{
    for (unsigned i = 0; i < p->nknobs; i++) {
        if (except && !strcmp(p->knobs[i].key, except)) continue;
        const char *v = getenv(p->knobs[i].key); if (!v || strcmp(v, p->knobs[i].value)) return 0;
    }
    return 1;
}
static int user_says(const char *key, void *ctx) { return !strcmp(key, (const char *)ctx); }
static uint32_t *read_words(const char *path, size_t *n)
{
    FILE *f = fopen(path, "rb"); if (!f) return NULL;
    fseek(f, 0, SEEK_END); long len = ftell(f); fseek(f, 0, SEEK_SET);
    uint32_t *w = malloc((size_t)len); if (!w || fread(w, 1, (size_t)len, f) != (size_t)len) { fclose(f); free(w); return NULL; }
    fclose(f); *n = (size_t)len / 4; return w;
}

int main(int argc, char **argv)
{
    const nvmtl_app_profile *cp = nvmtl_app_profile_for("Cyberpunk2077", "com.cdprojektred.cyberpunk.steam", NULL);
    arm("the profile applies to the game: executable Cyberpunk2077, bundle com.cdprojektred.cyberpunk.steam", cp != NULL);
    arm("and to the GOG and App Store builds (any com.cdprojektred.cyberpunk identifier)",
        nvmtl_app_profile_for("Cyberpunk2077", "com.cdprojektred.cyberpunk.gog", NULL) == cp && nvmtl_app_profile_for("Cyberpunk2077", "com.cdprojektred.cyberpunk2077", NULL) == cp);
    arm("the right name in another bundle, or in no bundle at all, is not the game",
        !nvmtl_app_profile_for("Cyberpunk2077", "com.example.cyberpunk", NULL) && !nvmtl_app_profile_for("Cyberpunk2077", "", NULL)
        && !nvmtl_app_profile_for("Cyberpunk2077", NULL, NULL));
    arm("a helper inside the game's bundle (same identifier, other executable) is not the game",
        !nvmtl_app_profile_for("Cyberpunk2077Helper", "com.cdprojektred.cyberpunk.steam", NULL) && !nvmtl_app_profile_for("REDprelauncher", "com.cdprojektred.cyberpunk.steam", NULL));
    if (!cp) { printf("FAIL: %d failed\n", fails); return 1; }

    const char *others[] = { "WindowServer", "Steam", "Cyberpunk2077Helper", "cyberpunk2077", "Cyberpunk2077.exe",
                             "Cyberpunk 2077", "REDprelauncher", "Geekbench 6", "" };
    int any = 0;
    for (size_t i = 0; i < sizeof others / sizeof others[0]; i++) any |= nvmtl_app_profile_for(others[i], "com.apple.WindowServer", NULL) != NULL;
    arm("no other process gets it: WindowServer, Steam, a helper, another case, the Windows name, an empty name", !any);
    arm("a process with no name gets nothing", nvmtl_app_profile_for(NULL, "com.cdprojektred.cyberpunk.steam", NULL) == NULL);
    arm("NVMTL_APP_PROFILE=0 turns it off for the game itself", nvmtl_app_profile_for("Cyberpunk2077", "com.cdprojektred.cyberpunk.steam", "0") == NULL);
    arm("any other NVMTL_APP_PROFILE value leaves it on", nvmtl_app_profile_for("Cyberpunk2077", "com.cdprojektred.cyberpunk.steam", "1") == cp);

    int sysmem = 0, diag = 0;
    for (unsigned i = 0; i < cp->nknobs; i++) {
        if (!strcmp(cp->knobs[i].key, "NVK_LOCAL_MAP_SYSMEM") && !strcmp(cp->knobs[i].value, "0")) sysmem = 1;
        if (strstr(cp->knobs[i].key, "SNAPSHOT") || strstr(cp->knobs[i].key, "PROBE") || strstr(cp->knobs[i].key, "DUMP")
            || strstr(cp->knobs[i].key, "LOG") || strstr(cp->knobs[i].key, "WITNESS") || strstr(cp->knobs[i].key, "TRACE")
            || !strcmp(cp->knobs[i].key, "NVMTL_PROCESS_ENV") || strstr(cp->knobs[i].key, "CACHE")) diag = 1;
    }
    arm("the game's profile carries VRAM placement (NVK_LOCAL_MAP_SYSMEM=0)", sysmem);
    arm("and no diagnostic switch: no snapshot, probe, dump, log, witness, trace, cache path or NVMTL_PROCESS_ENV", !diag);

    /* the environment: a fresh process, then the plugin's call */
    clear(cp);
    char rec[3072] = "NVMTL_FROM_CONF=1"; unsigned kept = 9;
    unsigned set = nvmtl_app_profile_apply(cp, NULL, NULL, rec, sizeof rec, &kept);
    arm("applied to the game, every setting lands with the profile's value", all_as_profile(cp, NULL) && set == cp->nknobs && kept == 0);
    arm("and joins the conf record after what was already there, so a child process forgets them",
        !strncmp(rec, "NVMTL_FROM_CONF=1;", 18) && strstr(rec, "NVK_LOCAL_MAP_SYSMEM=0") != NULL);

    clear(cp);
    setenv("NVK_LOCAL_MAP_SYSMEM", "1", 1);
    rec[0] = 0;
    set = nvmtl_app_profile_apply(cp, user_says, "NVK_LOCAL_MAP_SYSMEM", rec, sizeof rec, &kept);
    arm("a value the user set wins: NVK_LOCAL_MAP_SYSMEM=1 stays 1", !strcmp(getenv("NVK_LOCAL_MAP_SYSMEM"), "1"));
    arm("the rest of the profile still applies, and the count says one was left to the user",
        all_as_profile(cp, "NVK_LOCAL_MAP_SYSMEM") && set == cp->nknobs - 1 && kept == 1 && !strstr(rec, "NVK_LOCAL_MAP_SYSMEM"));

    clear(cp);
    setenv("NVMTL_HAZARD", "1", 1);
    nvmtl_app_profile_apply(cp, NULL, NULL, NULL, 0, NULL);
    arm("a knob outside the profile is not touched", !strcmp(getenv("NVMTL_HAZARD"), "1"));

    /* what a non-matching process does: the plugin applies nothing, because the match returns NULL */
    clear(cp);
    set = nvmtl_app_profile_apply(nvmtl_app_profile_for("WindowServer", "com.apple.WindowServer", NULL), NULL, NULL, NULL, 0, &kept);
    arm("WindowServer's path applies nothing and NVK_LOCAL_MAP_SYSMEM stays unset", set == 0 && none_set(cp) && !getenv("NVK_LOCAL_MAP_SYSMEM"));

    /* the shader edit, on a synthetic module (the shipped edit needs the game's own module: see argv below).
       Original: header, then an instruction using IDs 3 and 4 (words 6, 7), then a literal 4 (word 8, not an ID).
       Edited: IDs renumbered 3->1, 4->2, bound 5->6, version 1.6, and one word (99) added before word 8. */
    uint32_t mod[9] = { 0x07230203u, 0x00010400u, 0x000f0000u, 5, 0, 0x00030000u, 3, 4, 4 };
    uint32_t want[10] = { 0x07230203u, 0x00010600u, 0x00070000u, 6, 0, 0x00030000u, 1, 2, 99, 4 };
    uint32_t edit[] = { NVMTL_SPV_EDIT_MAGIC, 0x00010600u, 0x00070000u, 6, 8, 1, 99, 5, 0, 0, 0, 1, 2, 1, (1u << 6) | (1u << 7) };
    const size_t ne = sizeof edit / sizeof edit[0];
    uint32_t from = nvmtl_spv_fnv1a(mod, sizeof mod), to = nvmtl_spv_fnv1a(want, sizeof want);
    size_t n = 0; uint32_t *out = nvmtl_spv_apply_edit(mod, 9, edit, ne, from, to, &n);
    arm("an edit on the module it was made from gives exactly the edited module: IDs renumbered, a literal equal to an ID left alone",
        out && n == 10 && !memcmp(out, want, sizeof want));
    free(out);
    uint32_t other[9]; memcpy(other, mod, sizeof mod); other[8] = 5; uint32_t copy[9]; memcpy(copy, other, sizeof other);
    out = nvmtl_spv_apply_edit(other, 9, edit, ne, from, to, &n);
    arm("a hash miss changes nothing: another module gets no edit and is left as it was", !out && !memcmp(other, copy, sizeof copy));
    out = nvmtl_spv_apply_edit(mod, 9, edit, ne, from, to ^ 1, &n);
    arm("a result that does not hash to the recorded value is thrown away", !out);
    out = nvmtl_spv_apply_edit(mod, 9, edit, ne - 1, from, to, &n);
    arm("an edit shorter than it claims is refused, not read past", !out);
    { uint32_t e2[sizeof edit / sizeof edit[0]]; memcpy(e2, edit, sizeof edit); e2[4] = 30;
      arm("an edit that inserts past the end of the module is refused", !nvmtl_spv_apply_edit(mod, 9, e2, ne, from, to, &n)); }
    { uint32_t e3[sizeof edit / sizeof edit[0]]; memcpy(e3, edit, sizeof edit); e3[11] = 0;   /* map[3] = 0: no mapping */
      arm("an ID the map does not cover is refused, not passed through", !nvmtl_spv_apply_edit(mod, 9, e3, ne, from, to, &n)); }
    { uint32_t e4[sizeof edit / sizeof edit[0]]; memcpy(e4, edit, sizeof edit); e4[0] = 0;
      arm("an edit without its magic word is refused", !nvmtl_spv_apply_edit(mod, 9, e4, ne, from, to, &n)); }
    mod[0] = 0x03022307u;
    arm("a module without the SPIR-V magic is refused", !nvmtl_spv_apply_edit(mod, 9, edit, ne, from, to, &n));

    /* the shader cache's names: every mode must be in the tag, or a lookup can be served another mode's module */
    const char *build = "sha256-0446740c8cfcb42d5322c6dd852a4891bb1c3005b00d409f33ee1e72c5b3f828";
    char prof[NVMTL_SPVCACHE_TAG_MAX], plain[NVMTL_SPVCACHE_TAG_MAX], noas[NVMTL_SPVCACHE_TAG_MAX], small[80];
    int fit = nvmtl_spvcache_mode_tag(prof, sizeof prof, build, 1, 1, 1, 1, 1);
    nvmtl_spvcache_mode_tag(plain, sizeof plain, build, 1, 1, 1, 0, 0);
    nvmtl_spvcache_mode_tag(noas, sizeof noas, build, 1, 1, 0, 1, 1);
    arm("the game's cache tag names every mode: <build>-u2-p5-as1-np-nf", fit && strlen(prof) > 16 && !strcmp(prof + strlen(prof) - 16, "-u2-p5-as1-np-nf"));
    arm("the old 80-byte tag buffer could not hold it (it kept \"-u2-p5-a\" and lost the rest): refused, not cut",
        !nvmtl_spvcache_mode_tag(small, sizeof small, build, 1, 1, 1, 1, 1));
    arm("with and without the profile's translator switches the tags differ", strcmp(prof, plain) != 0);
    arm("with and without argument-buffer samplers the tags differ", strcmp(prof, noas) != 0);
    {
        char dir[] = "/tmp/test_app_profile_spv_XXXXXX"; int okdir = mkdtemp(dir) != NULL;
        char a1[512], a2[512];
        snprintf(a1, sizeof a1, "%s/00ff.kernel.%s.c3.spv", dir, prof);
        snprintf(a2, sizeof a2, "%s/00ff.kernel.%s.c3.spv", dir, plain);
        const uint32_t fresh[6] = { 0x07230203u, 0x00010400u, 0, 7, 0, 42 };
        FILE *w = okdir ? fopen(a1, "wb") : NULL; if (w) { fwrite(fresh, 4, 6, w); fclose(w); }
        size_t rn = 0; uint32_t *back = read_words(a1, &rn); FILE *other = fopen(a2, "rb");
        arm("store then read back under the profile's tag: the bytes are the fresh ones", back && rn == 6 && !memcmp(back, fresh, sizeof fresh));
        arm("a process without the profile looks under its own name and does not find the profile's entry", other == NULL);
        if (other) fclose(other);
        arm("the sweep keeps an entry of the same translator build in another mode",
            nvmtl_spvcache_same_build(strrchr(a2, '/') + 1, build) && nvmtl_spvcache_same_build(strrchr(a1, '/') + 1, build));
        arm("the sweep drops an entry of another translator build, and a file that is not an entry",
            !nvmtl_spvcache_same_build("00ff.kernel.sha256-1175b70008995e356ace5ec36b20ee689a88e8e8dcebb1a40102d9020b8714c3-u2-p5-a.c3.spv", build)
            && !nvmtl_spvcache_same_build("README-not-a-cache-entry.txt", build)
            && !nvmtl_spvcache_same_build("00ff.kernel.sha256-0446740c8cfcb42d5322c6dd852a4891bb1c3005b00d409f33ee1e72c5b3f828x-u2.c3.spv", build));
        free(back); unlink(a1); rmdir(dir);
    }
    {
        /* two modes in one cache folder: run the sweep's keep rule over a folder holding both, from each side */
        const char *ea = "00ff.kernel.sha256-0446740c8cfcb42d5322c6dd852a4891bb1c3005b00d409f33ee1e72c5b3f828-u2-p5-as1-np-nf.c3.spv";
        const char *eb = "00ff.kernel.sha256-0446740c8cfcb42d5322c6dd852a4891bb1c3005b00d409f33ee1e72c5b3f828-u2.c3.spv";
        arm("two modes in one cache folder do not evict each other: each side's sweep keeps the other's entry",
            nvmtl_spvcache_same_build(ea, build) && nvmtl_spvcache_same_build(eb, build));
        char ia[512], ib[512];
        nvmtl_idx_record_name(ia, sizeof ia, "idx2-abcd", prof); nvmtl_idx_record_name(ib, sizeof ib, "idx2-abcd", plain);
        arm("a library-index record written in mode A is a different file from mode B's: B misses it", strcmp(ia, ib) != 0);
        arm("the record's name carries the library and the whole tag", strstr(ia, "idx2-abcd.") == ia && strstr(ia, prof) != NULL);
        char tiny[16];
        arm("a record name that does not fit is refused (no record), not cut", !nvmtl_idx_record_name(tiny, sizeof tiny, "idx2-abcd", prof));
        char root[256];
        arm("the profile's application gets its own cache root under <caches>/nvmtl/apps/<bundle id>",
            nvmtl_app_cache_root(root, sizeof root, "/var/caches", "com.cdprojektred.cyberpunk.steam")
            && !strcmp(root, "/var/caches/nvmtl/apps/com.cdprojektred.cyberpunk.steam"));
        arm("a bundle identifier that could leave that folder (/, .., empty, leading dot) gets no root",
            !nvmtl_app_cache_root(root, sizeof root, "/c", "a/../b") && !nvmtl_app_cache_root(root, sizeof root, "/c", "..")
            && !nvmtl_app_cache_root(root, sizeof root, "/c", "") && !nvmtl_app_cache_root(root, sizeof root, "/c", ".hidden"));
    }

    if (cp->shader_from) {
        arm("the shipped edit is armed with two hashes and a patch", cp->shader_to && cp->npatch > 7 && cp->patch[0] == NVMTL_SPV_EDIT_MAGIC);
        if (argc == 3) {
            size_t on = 0, en = 0; uint32_t *orig = read_words(argv[1], &on), *ed = read_words(argv[2], &en);
            out = orig ? nvmtl_spv_apply_edit(orig, on, cp->patch, cp->npatch, cp->shader_from, cp->shader_to, &n) : NULL;
            arm("the shipped edit turns the game's translated light-cluster kernel (d57e32ce) into the known-good guard module, byte for byte",
                out && ed && n == en && !memcmp(out, ed, n * 4));
            arm("and that module is the one the correct run dispatched: FNV-1a bd4ccf94, 113,592 bytes",
                out && n * 4 == 113592 && nvmtl_spv_fnv1a(out, n * 4) == 0xbd4ccf94u && cp->shader_to == 0xbd4ccf94u);
            free(out);
            /* Every compute pipeline goes through one call, nvmtl_app_profile_edit_module, at pipeline creation. These arms
               feed it the module as each path delivers it. */
            size_t m = 0; uint32_t *e1 = nvmtl_app_profile_edit_module(cp, orig, on * 4, &m);
            arm("path: a fresh translation (the original module in memory) is edited", e1 && m == en && !memcmp(e1, ed, m * 4));
            char tmp[] = "/tmp/test_app_profile_cache_XXXXXX"; int fd = mkstemp(tmp); size_t cn = 0; uint32_t *hit = NULL;
            if (fd >= 0) { FILE *cf = fdopen(fd, "wb"); fwrite(orig, 4, on, cf); fclose(cf); hit = read_words(tmp, &cn); unlink(tmp); }
            size_t m2 = 0; uint32_t *e2 = hit ? nvmtl_app_profile_edit_module(cp, hit, cn * 4, &m2) : NULL;
            arm("path: a shader-cache hit (the same module written to disk and read back) is edited the same way",
                e2 && m2 == en && !memcmp(e2, ed, m2 * 4));
            size_t m3 = 0; uint32_t *e3 = nvmtl_app_profile_edit_module(cp, ed, en * 4, &m3);
            arm("path: the align guard's SAFE rebuild starts from the already edited module and is not edited twice (it stays edited)",
                !e3);
            uint32_t *var = malloc(on * 4); memcpy(var, orig, on * 4); var[on - 1] ^= 1u;   /* a re-translated variant differs */
            size_t m4 = 0; uint32_t *e4 = nvmtl_app_profile_edit_module(cp, var, on * 4, &m4);
            arm("path: a sampler or threadgroup variant re-translated from AIR is a different module and gets NO edit (the known limit; the plugin logs it)",
                !e4 && var[on - 1] == (orig[on - 1] ^ 1u));
            size_t m5 = 0;
            arm("another process (no profile) never edits, even the exact original", !nvmtl_app_profile_edit_module(NULL, orig, on * 4, &m5));
            arm("a module whose length is not whole words is left alone", !nvmtl_app_profile_edit_module(cp, orig, on * 4 - 1, &m5));
            free(e1); free(e2); free(e3); free(e4); free(var); free(hit); free(orig); free(ed);
        } else printf("skip  the shipped edit against the game's module (pass original.spv edited.spv)\n");
    } else printf("note  this build carries no shader edit\n");

    /* the RT working-set floor and the operating-system answer */
    { const char *rt = NULL;
      for (unsigned i = 0; i < cp->nknobs; i++) if (!strcmp(cp->knobs[i].key, "NVMTL_WS_RT_GATE_MB")) rt = cp->knobs[i].value;
      arm("the game's profile raises the RT working-set floor to 6200 MB (above the game's 6145 MB gate)", rt && atoi(rt) == 6200 && 6200 > 6145); }
    { long v[3], r26[3] = { 26, 1, 0 }, r15[3] = { 15, 7, 3 }, r27[3] = { 27, 0, 0 }, r28[3] = { 28, 0, 0 };
      const char *G = "Cyberpunk2077", *B = "com.cdprojektred.cyberpunk.steam";
      arm("os: the game on macOS 26.1 is told 27.0.0 by its profile",
          nvmtl_os_version_answer(G, B, NULL, NULL, r26, v) == 1 && v[0] == 27 && v[1] == 0 && v[2] == 0);
      arm("os: and on macOS 15.7.3", nvmtl_os_version_answer(G, B, NULL, NULL, r15, v) == 1 && v[0] == 27);
      arm("os: on macOS 27.0 and newer it gets the real answer (never lowered, nothing to do)",
          !nvmtl_os_version_answer(G, B, NULL, NULL, r27, v) && !nvmtl_os_version_answer(G, B, NULL, NULL, r28, v));
      static const char *others[][2] = { { "WindowServer", "" }, { "steam_osx", "com.valvesoftware.steam" }, { "Cyberpunk2077", "" },
          { "Cyberpunk2077", "com.example.other" }, { "REDprelauncher", "com.cdprojektred.cyberpunk.steam" }, { "cyberpunk2077", "com.cdprojektred.cyberpunk.steam" } };
      int any = 0; for (unsigned i = 0; i < sizeof others / sizeof others[0]; i++) any |= nvmtl_os_version_answer(others[i][0], others[i][1], NULL, NULL, r26, v);
      arm("os: no other process is told anything: WindowServer, Steam, the name outside the bundle, a helper in the bundle, another case", !any);
      arm("os: NVMTL_APP_PROFILE=0 turns the game's answer off", !nvmtl_os_version_answer(G, B, "0", NULL, r26, v));
      arm("os: NVMTL_OS_VERSION=0 turns it off too", !nvmtl_os_version_answer(G, B, NULL, "0", r26, v));
      arm("os: an explicit NVMTL_OS_VERSION wins over the profile, for any process (the per-process knob as before)",
          nvmtl_os_version_answer(G, B, NULL, "28.2", r26, v) == 1 && v[0] == 28 && v[1] == 2 &&
          nvmtl_os_version_answer("SomeApp", "com.example", NULL, "27.1.4", r26, v) == 1 && v[2] == 4);
      arm("os: a malformed NVMTL_OS_VERSION is ignored and the profile decides (27.0.0 for the game, nothing elsewhere)",
          nvmtl_os_version_answer(G, B, NULL, "27.x", r26, v) == 1 && v[0] == 27 && !nvmtl_os_version_answer("SomeApp", "com.example", NULL, "banana", r26, v));
      long w[3];
      arm("os: version parsing takes 27, 27.1, 27.1.2 and refuses 13, 100, 27., 27.1.2.3, -27, empty",
          nvmtl_os_version_parse("27", w) && nvmtl_os_version_parse("27.1", w) && nvmtl_os_version_parse("27.1.2", w) && w[2] == 2 &&
          !nvmtl_os_version_parse("13", w) && !nvmtl_os_version_parse("100", w) && !nvmtl_os_version_parse("27.", w) &&
          !nvmtl_os_version_parse("27.1.2.3", w) && !nvmtl_os_version_parse("-27", w) && !nvmtl_os_version_parse("", w)); }
    { char out[64]; int n = 0, ok = 1;
      const char *lines[] = { "# NVMTL_OS_VERSION=26.0\n", "NVMTL_OS_VERSION=27.0\n", "  NVMTL_OS_VERSION = 0 \r\n", "NVMTL_OS_VERSIONX=9\n", "\n" };
      char last[64] = "";
      for (unsigned i = 0; i < 5; i++) if (nvmtl_conf_line_value(lines[i], "NVMTL_OS_VERSION", out, sizeof out)) { n++; snprintf(last, sizeof last, "%s", out); }
      ok = n == 2 && !strcmp(last, "0");
      arm("conf: comments and other keys are skipped, blanks trimmed, the last line wins (the user's block)", ok);
      char tiny[3];
      arm("conf: a value that does not fit is refused, not cut", !nvmtl_conf_line_value("NVMTL_OS_VERSION=27.0.0\n", "NVMTL_OS_VERSION", tiny, sizeof tiny)); }

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}

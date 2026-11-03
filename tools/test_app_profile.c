/* test_app_profile.c - the application-profile rules (plugin/nvmtl_app_profile.h), run against this process's environment.
   Build and run anywhere (no GPU, no Metal):  cc -I plugin tools/test_app_profile.c -o /tmp/test_app_profile && /tmp/test_app_profile
   Optional: /tmp/test_app_profile <original.spv> <edited.spv> also proves the shipped kernel edit turns the original
   translated module into the edited one, byte for byte. Exit status 0 when every arm holds. */
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

    /* the shader edit, on a synthetic module (the shipped edit needs the game's own module: see argv below) */
    uint32_t mod[8] = { 0x07230203u, 0x00010500u, 0, 40, 0, 11, 22, 33 };
    uint32_t want[9] = { 0x07230203u, 0x00010500u, 0, 41, 0, 11, 99, 22, 33 };
    uint32_t edit[] = { 3, 1, 1, 41,   2, 0, 1, 99 };   /* keep 3, replace the bound; keep 2, insert one word */
    uint32_t from = nvmtl_spv_fnv1a(mod, sizeof mod), to = nvmtl_spv_fnv1a(want, sizeof want);
    size_t n = 0; uint32_t *out = nvmtl_spv_apply_edit(mod, 8, edit, 8, from, to, &n);
    arm("an edit on the module it was made from gives exactly the edited module", out && n == 9 && !memcmp(out, want, sizeof want));
    free(out);
    uint32_t other[8]; memcpy(other, mod, sizeof mod); other[7] = 34; uint32_t copy[8]; memcpy(copy, other, sizeof other);
    out = nvmtl_spv_apply_edit(other, 8, edit, 8, from, to, &n);
    arm("a hash miss changes nothing: another module gets no edit and is left as it was", !out && !memcmp(other, copy, sizeof copy));
    out = nvmtl_spv_apply_edit(mod, 8, edit, 8, from, to ^ 1, &n);
    arm("a result that does not hash to the recorded value is thrown away", !out);
    uint32_t bad[] = { 3, 1, 9, 41 };
    out = nvmtl_spv_apply_edit(mod, 8, bad, 4, from, to, &n);
    arm("a patch that claims more words than it has is refused, not read past", !out);
    uint32_t far[] = { 30, 0, 0 };
    out = nvmtl_spv_apply_edit(mod, 8, far, 3, from, to, &n);
    arm("a patch that keeps past the end of the module is refused", !out);
    mod[0] = 0x03022307u;
    arm("a module without the SPIR-V magic is refused", !nvmtl_spv_apply_edit(mod, 8, edit, 8, from, to, &n));

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

    if (cp->shader_from) {
        arm("the shipped edit is armed with two hashes and a patch", cp->shader_to && cp->npatch > 3);
        if (argc == 3) {
            size_t on = 0, en = 0; uint32_t *orig = read_words(argv[1], &on), *ed = read_words(argv[2], &en);
            out = orig ? nvmtl_spv_apply_edit(orig, on, cp->patch, cp->npatch, cp->shader_from, cp->shader_to, &n) : NULL;
            arm("the shipped edit turns the game's translated light-cluster kernel into the bounds-checked one, byte for byte",
                out && ed && n == en && !memcmp(out, ed, n * 4));
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

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}

/* nvmtl_env_knobs.h - the driver knobs a release plugin forgets when it loads.
   A release plugin drops the NVMTL_/NVK_/NAK_/NVRM_ variables it inherits from the environment: a variable left behind
   in a shell by an earlier experiment must not change how every Metal app launched from that shell draws, and the
   supported route for a setting is the conf file read right after this. Three per-process switches were always kept.
   NVMTL_PROCESS_ENV=1 keeps every knob for this one process: a launcher that sets knobs for one application on purpose
   opts in with that single line, and a process that does not set it behaves exactly as before. The rule is pure over
   the vector it is given, so tools/test_env_knobs.c proves it without loading the plugin. */
#ifndef NVMTL_ENV_KNOBS_H
#define NVMTL_ENV_KNOBS_H
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>

/* a driver knob: one of our prefixes, including the translator's (METAL2VULKAN_) and the Mesa / RADV / ACO / NIR
   switches the bundled Vulkan drivers read. Several of them name files that get written or libraries that get
   loaded, so none may reach a process that did not set them for itself. */
static inline int nvmtl_env_knob_prefixed(const char *kv)
{
    return !strncmp(kv, "NVMTL_", 6) || !strncmp(kv, "NVK_", 4) || !strncmp(kv, "NAK_", 4) || !strncmp(kv, "NVRM_", 5)
        || !strncmp(kv, "METAL2VULKAN_", 13) || !strncmp(kv, "MESA_", 5) || !strncmp(kv, "NIR_", 4)
        || !strncmp(kv, "RADV_", 5) || !strncmp(kv, "ACO_", 4);
}

/* A process with more privilege than whoever set its environment: setuid/setgid (issetugid), or running as root.
   dyld drops DYLD_* for the first case; the knobs include library paths (NVMTL_ICD_LIB, NVMTL_NVK_DYLIB, the vendor
   plugin path) and file paths that get written, so the plugin must not reopen that door. Such a process keeps no
   knob, opt-in or not. */
static inline int nvmtl_env_privileged(void)
{
    return issetugid() || geteuid() == 0;
}

/* 1 when this process asked to keep its own knobs (the exact value 1; anything else is an ordinary knob and is forgotten) */
static inline int nvmtl_env_knobs_kept(char **envp)
{
    if (nvmtl_env_privileged()) return 0;
    for (char **e = envp; e && *e; e++) if (!strcmp(*e, "NVMTL_PROCESS_ENV=1")) return 1;
    return 0;
}

/* 1 for a variable the release plugin keeps even without the opt-in */
static inline int nvmtl_env_knob_always_kept(const char *kv)
{
    if (nvmtl_env_privileged()) return 0;
    return !strncmp(kv, "NVMTL_UNIFORM_CBUF=", 19)          /* the GL uniform path's per-process override */
        || !strncmp(kv, "NVMTL_UNIFORM_CBUF_STATS=", 25)    /* and its diagnostics */
        || !strncmp(kv, "NVMTL_PRESENT_INLINE=", 21);       /* the switch that restores inline presents */
}

/* The cached answer for this process: 1 when it set NVMTL_PROCESS_ENV=1. Besides keeping the knobs, a release plugin then
   writes its log (nvmtl_log_path) and its fps census for this process, which it never does otherwise. */
static inline int nvmtl_process_env_on(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_PROCESS_ENV"); on = e && !strcmp(e, "1") && !nvmtl_env_privileged(); }
    return on;
}

/* A library path that came from a knob or the conf file. A release plugin loads it only when it is a root-owned regular
   file, not a link, in a root-owned folder nobody else can write: the plugin runs inside every Metal app, so a path to
   a user-writable file would hand each of them, WindowServer included, to whoever can write that file. */
static inline int nvmtl_code_path_allowed(const char *path)
{
#ifdef NVMTL_RELEASE
    struct stat st, ds; char dir[1024];
    if (!path || strlen(path) >= sizeof dir || path[0] != '/') return 0;
    if (lstat(path, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != 0 || (st.st_mode & (S_IWGRP | S_IWOTH))) return 0;
    strcpy(dir, path); *strrchr(dir, '/') = 0;
    if (lstat(dir[0] ? dir : "/", &ds) != 0 || !S_ISDIR(ds.st_mode)) return 0;
    return ds.st_uid == 0 && !(ds.st_mode & (S_IWGRP | S_IWOTH));
#else
    (void)path; return 1;   /* development builds load from build folders */
#endif
}

/* Unsets every driver knob in envp that is not kept and returns how many it forgot. envp is the process environment,
   which unsetenv edits, so the names are collected first and removed after. At most 256 are forgotten, as before. */
static inline unsigned nvmtl_release_forget_env(char **envp)
{
    if (nvmtl_env_knobs_kept(envp)) return 0;
    char *names[256]; unsigned n = 0;
    for (char **e = envp; e && *e && n < 256; e++) {
        if (!nvmtl_env_knob_prefixed(*e) || nvmtl_env_knob_always_kept(*e)) continue;
        const char *eq = strchr(*e, '='); if (!eq) continue;
        names[n] = strndup(*e, (size_t)(eq - *e)); if (names[n]) n++;
    }
    for (unsigned i = 0; i < n; i++) { unsetenv(names[i]); free(names[i]); }
    return n;
}
#endif

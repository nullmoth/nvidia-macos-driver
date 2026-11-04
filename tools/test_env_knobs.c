/* test_env_knobs.c - the release plugin's rule for inherited driver knobs, run against this process's own environment.
   Build and run anywhere (no GPU, no Metal):  cc -I plugin tools/test_env_knobs.c -o /tmp/test_env_knobs && /tmp/test_env_knobs
   Exit status 0 when every arm holds. Each arm is a sentence about a failure the rule must produce or refuse. */
#define NVMTL_RELEASE 1   /* the release rule for library paths */
#include <stdio.h>
#include <unistd.h>
/* privilege is faked so the test runs as anyone: the header asks issetugid() and geteuid() */
static int fake_ugid, fake_root;
#define issetugid() (fake_ugid)
#define geteuid() (fake_root ? 0 : 501)
#include "nvmtl_env_knobs.h"

extern char **environ;
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
static void fresh(void)
{
    unsetenv("NVMTL_PROCESS_ENV");
    setenv("NVMTL_SHARED_VRAM", "1", 1); setenv("NVMTL_GSUB_INFLIGHT", "2", 1); setenv("NVK_LOCAL_MAP_SYSMEM", "0", 1);
    setenv("NAK_DEBUG", "print", 1); setenv("NVRM_EVENT_WAIT", "1", 1);
    setenv("NVMTL_PRESENT_INLINE", "1", 1); setenv("NVMTL_UNIFORM_CBUF", "0", 1); setenv("NVMTL_UNIFORM_CBUF_STATS", "1", 1);
    setenv("NVMTLX_NOT_OURS", "1", 1); setenv("MY_PATH_LIKE", "kept", 1);
}

int main(void)
{
    fresh();
    unsigned n = nvmtl_release_forget_env(environ);
    arm("without the opt-in, a release plugin forgets every inherited NVMTL_/NVK_/NAK_/NVRM_ knob",
        !getenv("NVMTL_SHARED_VRAM") && !getenv("NVMTL_GSUB_INFLIGHT") && !getenv("NVK_LOCAL_MAP_SYSMEM") && !getenv("NAK_DEBUG") && !getenv("NVRM_EVENT_WAIT"));
    arm("it reports how many it forgot (5)", n == 5);
    arm("the three per-process switches survive as they always did",
        getenv("NVMTL_PRESENT_INLINE") && getenv("NVMTL_UNIFORM_CBUF") && getenv("NVMTL_UNIFORM_CBUF_STATS"));
    arm("a variable that only starts like ours, and an unrelated one, are untouched", getenv("NVMTLX_NOT_OURS") && getenv("MY_PATH_LIKE"));

    fresh(); setenv("NVMTL_PROCESS_ENV", "1", 1);
    n = nvmtl_release_forget_env(environ);
    arm("NVMTL_PROCESS_ENV=1 keeps every knob for this process",
        getenv("NVMTL_SHARED_VRAM") && getenv("NVMTL_GSUB_INFLIGHT") && getenv("NVK_LOCAL_MAP_SYSMEM") && getenv("NAK_DEBUG") && getenv("NVRM_EVENT_WAIT"));
    arm("and forgets nothing (0)", n == 0);

    fresh(); setenv("NVMTL_PROCESS_ENV", "0", 1);
    n = nvmtl_release_forget_env(environ);
    arm("NVMTL_PROCESS_ENV=0 is not an opt-in: the knobs are forgotten, the gate itself among them",
        !getenv("NVMTL_SHARED_VRAM") && !getenv("NVMTL_PROCESS_ENV"));
    arm("that run forgot 6", n == 6);

    fresh(); setenv("NVMTL_PROCESS_ENV", "yes", 1);
    n = nvmtl_release_forget_env(environ);
    arm("only the exact value 1 opts in: NVMTL_PROCESS_ENV=yes is forgotten with the rest", !getenv("NVMTL_SHARED_VRAM") && n == 6);

    fresh(); setenv("NVMTL_PROCESS_ENV", "1", 1);
    arm("the cached per-process answer agrees with the vector rule (opted in)", nvmtl_process_env_on() == 1 && nvmtl_env_knobs_kept(environ) == 1);
    unsetenv("NVMTL_PROCESS_ENV");
    arm("the vector rule sees the opt-in go; the cached answer is decided once per process (a process cannot opt in late)",
        nvmtl_env_knobs_kept(environ) == 0 && nvmtl_process_env_on() == 1);

    /* privileged processes: the opt-in is ignored and nothing is kept, not even the three per-process switches */
    fresh(); setenv("NVMTL_PROCESS_ENV", "1", 1); setenv("NVMTL_ICD_LIB", "/tmp/evil.dylib", 1);
    fake_ugid = 1; n = nvmtl_release_forget_env(environ);
    arm("a setuid/setgid process keeps no knob even with NVMTL_PROCESS_ENV=1 (no library path, no switch)",
        !getenv("NVMTL_ICD_LIB") && !getenv("NVMTL_SHARED_VRAM") && !getenv("NVMTL_PRESENT_INLINE") && !getenv("NVMTL_PROCESS_ENV"));
    fake_ugid = 0;
    fresh(); setenv("NVMTL_PROCESS_ENV", "1", 1); setenv("NVMTL_ICD_LIB", "/tmp/evil.dylib", 1);
    fake_root = 1; n = nvmtl_release_forget_env(environ);
    arm("a root process keeps no knob even with NVMTL_PROCESS_ENV=1", !getenv("NVMTL_ICD_LIB") && !getenv("NVMTL_SHARED_VRAM"));
    fake_root = 0;
    fresh(); setenv("NVMTL_PROCESS_ENV", "1", 1); setenv("NVMTL_ICD_LIB", "/tmp/dev.dylib", 1);
    n = nvmtl_release_forget_env(environ);
    arm("negative control: an ordinary process that opts in still keeps its own knobs", getenv("NVMTL_ICD_LIB") && n == 0);
    unsetenv("NVMTL_ICD_LIB");
    /* the translator's and the bundled Vulkan drivers' switches, several of which name files to write */
    fresh();
    setenv("METAL2VULKAN_OWNED_DUMP", "/etc/y", 1); setenv("MESA_LOG_FILE", "/etc/z", 1); setenv("NIR_DEBUG", "1", 1);
    setenv("RADV_DEBUG", "x", 1); setenv("ACO_DEBUG", "x", 1); setenv("MESAX_NOT_OURS", "1", 1);
    n = nvmtl_release_forget_env(environ);
    arm("METAL2VULKAN_/MESA_/NIR_/RADV_/ACO_ variables are forgotten like ours",
        !getenv("METAL2VULKAN_OWNED_DUMP") && !getenv("MESA_LOG_FILE") && !getenv("NIR_DEBUG") && !getenv("RADV_DEBUG") && !getenv("ACO_DEBUG"));
    arm("a name that only starts like one (MESAX_) is untouched", getenv("MESAX_NOT_OURS") != NULL);

    /* library paths: a release plugin loads only root-owned files in root-only folders */
    arm("a root-owned library path in a root-only folder is allowed", nvmtl_code_path_allowed("/usr/lib/libSystem.B.dylib") || nvmtl_code_path_allowed("/bin/sh"));
    char tmpl[] = "/tmp/nvmtl-knob-XXXXXX"; int tfd = mkstemp(tmpl); if (tfd >= 0) close(tfd);
    arm("negative control: a user-owned file is refused", getuid() == 0 || !nvmtl_code_path_allowed(tmpl));
    unlink(tmpl);
    arm("a relative path, a missing file and a null are refused",
        !nvmtl_code_path_allowed("libfoo.dylib") && !nvmtl_code_path_allowed("/no/such/lib.dylib") && !nvmtl_code_path_allowed(NULL));

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}

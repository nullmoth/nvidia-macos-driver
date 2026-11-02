/* test_env_knobs.c - the release plugin's rule for inherited driver knobs, run against this process's own environment.
   Build and run anywhere (no GPU, no Metal):  cc -I plugin tools/test_env_knobs.c -o /tmp/test_env_knobs && /tmp/test_env_knobs
   Exit status 0 when every arm holds. Each arm is a sentence about a failure the rule must produce or refuse. */
#include <stdio.h>
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

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}

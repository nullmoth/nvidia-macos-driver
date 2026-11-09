#!/usr/bin/env python3
"""The GPU is boosted while work is submitted and left alone when it is not.

NVK used to ask for BOOST_TO_MAX with an infinite duration when the device was created, so every GPU stayed at full
clocks for as long as any Vulkan process lived (WindowServer included): laptops idled hot with the fans up. The boost
is now a short lease that a submission renews at most once a second. This compiles the patch's own function with a
simulated clock and a recording stand-in for the resource manager, then checks the policy and the text around it.

usage: test_nvk_boost_burst.py [patch]   (default nvk/nvk-perf-boost-burst.patch)"""
import pathlib, re, shutil, subprocess, sys, tempfile

root = pathlib.Path(__file__).resolve().parents[1]
patch = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / "nvk/nvk-perf-boost-burst.patch"
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += 0 if ok else 1


# the text as it reads after the patch: context lines and added lines, not the removed ones
added = "\n".join(l[1:] for l in patch.read_text().split("\n") if l[:1] in (" ", "+") and not l.startswith("+++"))

m = re.search(r"#define NVKMD_NVRM_BOOST_LEASE_S.*?\n}\n", added, re.S)
check("the patch defines nvkmd_nvrm_boost_burst", m is not None)
if not m:
    print(f"{fails} failed")
    sys.exit(1)
func = m.group(0)

# --- text arms -----------------------------------------------------------------------------------------------------
hold = re.search(r"if \(pdev->boostMode == NVKMD_NVRM_BOOST_HOLD\) \{.*?\n   \}", added, re.S)
check("the infinite boost at device creation happens only in hold mode",
      hold is not None and "DURATION_INFINITE" in hold.group(0) and "DURATION_INFINITE" not in func)
check("a lease never carries the compute-context flag",
      "CUDA_YES" not in func and "CMD_BOOST_TO_MAX," in func)
check("unset or 1 selects the per-burst lease, 0 selects off, hold selects the held boost",
      "bv == '0' ? NVKMD_NVRM_BOOST_OFF" in added and "bv == 'h'" in added and ": NVKMD_NVRM_BOOST_BURST" in added)
check("every command submission renews the lease",
      re.search(r"nvkmd_nvrm_dev_api_ctl\(pdev, &rm\);\n\+?\s*\n?\+?\s*nvkmd_nvrm_boost_burst\(pdev\);", patch.read_text()) is not None
      or "nvkmd_nvrm_boost_burst(pdev);" in added)
check("teardown clears any boost that is still leased or held", "pdev->boostMode != NVKMD_NVRM_BOOST_OFF" in added)

# --- behaviour arms: the function against a simulated clock --------------------------------------------------------
harness = r'''
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <stdbool.h>
typedef uint32_t NV_STATUS; typedef uint32_t NvU32; typedef uint32_t NvHandle;
#define NV_OK 0
#define NV2080_CTRL_PERF_BOOST_FLAGS_CMD_BOOST_TO_MAX 2
#define NV2080_CTRL_PERF_BOOST_FLAGS_CUDA_YES 1
#define NV2080_CTRL_CMD_PERF_BOOST 0x2080200a
typedef struct { NvU32 flags; NvU32 duration; } NV2080_CTRL_PERF_BOOST_PARAMS;
struct NvRmApi { int fd; NvHandle hClient; const char *nodeName; };
enum { NVKMD_NVRM_BOOST_OFF, NVKMD_NVRM_BOOST_BURST, NVKMD_NVRM_BOOST_HOLD };
struct nvkmd_nvrm_pdev { int boostMode; uint64_t boostRenewNs; NvHandle hSubdevice, hClient; int ctlFd; char *devName; };
static uint64_t g_now;
static uint64_t os_time_get_nano(void) { return g_now; }
static bool nvkmd_nvrm_diag_on(void) { return false; }
static void nvkmd_nvrm_dev_api_ctl(struct nvkmd_nvrm_pdev *p, struct NvRmApi *rm) { rm->fd = p->ctlFd; rm->hClient = p->hClient; }
static int g_calls, g_bad; static unsigned g_flags, g_dur; static NV_STATUS g_rc;
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static NV_STATUS nvRmApiControl(struct NvRmApi *rm, NvHandle h, NvU32 cmd, void *p, NvU32 n)
{
    NV2080_CTRL_PERF_BOOST_PARAMS *bp = p;
    pthread_mutex_lock(&g_mu);
    g_calls++; g_flags = bp->flags; g_dur = bp->duration;
    if (cmd != NV2080_CTRL_CMD_PERF_BOOST || n != sizeof *bp) g_bad++;
    pthread_mutex_unlock(&g_mu);
    return g_rc;
}
''' + func + r'''
static struct nvkmd_nvrm_pdev P;
static void reset(int mode) { P.boostMode = mode; P.boostRenewNs = 0; g_calls = 0; g_rc = NV_OK; g_now = 1000000000ull; }
static void *hammer(void *a) { for (int i = 0; i < 20000; i++) nvkmd_nvrm_boost_burst(&P); return a; }
int main(void)
{
    const uint64_t S = 1000000000ull, MS = 1000000ull;
    /* idle: no submissions, no calls */
    reset(NVKMD_NVRM_BOOST_BURST); g_now += 60 * S;
    printf("idle %d\n", g_calls);
    /* steady work: a submission every millisecond for 10 s */
    reset(NVKMD_NVRM_BOOST_BURST); uint64_t last = 0, worst = 0; int lastcalls = 0;
    for (int i = 0; i < 10000; i++) { g_now += MS; nvkmd_nvrm_boost_burst(&P);
        if (g_calls != lastcalls) { if (last) { uint64_t gap = g_now - last; if (gap > worst) worst = gap; } last = g_now; lastcalls = g_calls; } }
    printf("steady %d flags %u dur %u bad %d worstgap_ms %llu\n", g_calls, g_flags, g_dur, g_bad, (unsigned long long)(worst / MS));
    /* a burst, silence, another burst */
    reset(NVKMD_NVRM_BOOST_BURST); nvkmd_nvrm_boost_burst(&P); g_now += 30 * S; nvkmd_nvrm_boost_burst(&P);
    printf("bursts %d\n", g_calls);
    /* two submissions inside one second */
    reset(NVKMD_NVRM_BOOST_BURST); nvkmd_nvrm_boost_burst(&P); g_now += 400 * MS; nvkmd_nvrm_boost_burst(&P);
    printf("quick %d\n", g_calls);
    /* off and hold never ask per submission */
    reset(NVKMD_NVRM_BOOST_OFF);  for (int i = 0; i < 100; i++) { g_now += S; nvkmd_nvrm_boost_burst(&P); } printf("off %d\n", g_calls);
    reset(NVKMD_NVRM_BOOST_HOLD); for (int i = 0; i < 100; i++) { g_now += S; nvkmd_nvrm_boost_burst(&P); } printf("hold %d\n", g_calls);
    /* the resource manager refuses: ask once, then stop */
    reset(NVKMD_NVRM_BOOST_BURST); g_rc = 0x56; for (int i = 0; i < 20; i++) { g_now += 2 * S; nvkmd_nvrm_boost_burst(&P); }
    printf("refused %d mode %d\n", g_calls, P.boostMode);
    /* eight threads submitting at the same instant: one renewal */
    reset(NVKMD_NVRM_BOOST_BURST); pthread_t t[8];
    for (int i = 0; i < 8; i++) pthread_create(&t[i], 0, hammer, 0);
    for (int i = 0; i < 8; i++) pthread_join(t[i], 0);
    printf("threads %d\n", g_calls);
    return 0;
}
'''

cc = shutil.which("clang") or shutil.which("cc")
if not cc:
    print("skip: no C compiler")
    sys.exit(0 if not fails else 1)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / "h.c").write_text(harness)
    r = subprocess.run([cc, "-O1", "-Wall", "-Wno-unused-function", "-o", str(d / "h"), str(d / "h.c"), "-lpthread"],
                       capture_output=True, text=True)
    check("the patch's function compiles", r.returncode == 0, r.stderr.strip()[-300:])
    if r.returncode == 0:
        out = subprocess.run([str(d / "h")], capture_output=True, text=True, timeout=60).stdout
        v = {l.split()[0]: l.split()[1:] for l in out.strip().split("\n")}
        check("an idle process asks for nothing", v["idle"] == ["0"], out)
        s = v["steady"]
        check("ten seconds of steady submission renew about once a second (10 or 11 calls)", 10 <= int(s[0]) <= 11, s[0])
        check("each renewal is BOOST_TO_MAX with a 3 s lease and no compute flag", s[2] == "2" and s[4] == "3" and s[6] == "0",
              f"flags {s[2]} duration {s[4]}")
        check("a busy process renews well inside the lease (worst gap under 1.1 s)", int(s[8]) < 1100, f"{s[8]} ms")
        check("work after 30 s of silence asks again (2 calls for 2 bursts)", v["bursts"] == ["2"], v["bursts"][0])
        check("two submissions within one second ask once", v["quick"] == ["1"], v["quick"][0])
        check("off asks for nothing", v["off"] == ["0"], v["off"][0])
        check("hold asks for nothing per submission", v["hold"] == ["0"], v["hold"][0])
        check("a refusal is asked once and then the policy switches itself off", v["refused"] == ["1", "mode", "0"], " ".join(v["refused"]))
        check("eight threads submitting at once renew once", v["threads"] == ["1"], v["threads"][0])

print(f"{fails} failed" if fails else "all passed")
sys.exit(1 if fails else 0)

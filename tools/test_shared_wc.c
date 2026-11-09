/* test_shared_wc.c - the coherency rules of a Shared buffer's VRAM working copy, proven on the host without a GPU.
   cc -I plugin tools/test_shared_wc.c -o /tmp/test_shared_wc && /tmp/test_shared_wc
   The "GPU" is a second byte array (the VRAM side); a copy is a memcpy of the ranges the logic returns. */
#include "nvmtl_shared_wc.h"
#include <stdio.h>

static int fails;
#define ARM(name, cond) do { const int arm_ok = !!(cond); printf("%s  %s\n", arm_ok ? "ok  " : "FAIL", name); if (!arm_ok) fails++; } while (0)

#define N (10 * NVMTL_SWC_PAGE + 123)   /* ten pages and a partial one */
static unsigned char host[N], vram[N];

static void upload(const nvmtl_swc_range *r, int n) { for (int i = 0; i < n; i++) memcpy(vram + r[i].lo, host + r[i].lo, r[i].hi - r[i].lo); }
/* one submit: upload what changed, the "kernel" runs on VRAM, then complete + copy back */
static int submit(nvmtl_swc *s, void (*kernel)(void))
{
    nvmtl_swc_range r[8]; int n = nvmtl_swc_before_submit(s, host, r, 8);
    upload(r, n); if (kernel) kernel();
    memcpy(host, vram, N); nvmtl_swc_after_complete(s, host);
    return n;
}
static void k_inc(void) { for (size_t i = 0; i < N; i++) vram[i]++; }
static void k_nop(void) { }

int main(void)
{
    nvmtl_swc s; nvmtl_swc_range r[8];
    ARM("a zero-length buffer is refused", nvmtl_swc_init(&s, 0) != 0);
    ARM("a buffer of ten and a bit pages has eleven page hashes", nvmtl_swc_init(&s, N) == 0 && s.pages == 11);

    /* first submit */
    for (size_t i = 0; i < N; i++) host[i] = (unsigned char)(i * 7 + 1);
    ARM("the first submit uploads the whole buffer as one range", nvmtl_swc_before_submit(&s, host, r, 8) == 1 && r[0].lo == 0 && r[0].hi == N);
    upload(r, 1);
    ARM("  and the VRAM side then equals the host side", memcmp(host, vram, N) == 0);

    /* the GPU's writes reach the CPU */
    k_inc(); memcpy(host, vram, N); nvmtl_swc_after_complete(&s, host);
    ARM("a kernel's writes are visible through contents after completion", host[0] == 2 && host[5] == (unsigned char)(5 * 7 + 1 + 1));
    ARM("  and the copy-back is not mistaken for a CPU write at the next submit", nvmtl_swc_before_submit(&s, host, r, 8) == 0 && s.clean_submits == 1);

    /* the CPU's writes reach the GPU */
    host[3 * NVMTL_SWC_PAGE + 10] ^= 0x5A;
    ARM("one changed byte uploads exactly its page", nvmtl_swc_before_submit(&s, host, r, 8) == 1 && r[0].lo == 3 * NVMTL_SWC_PAGE && r[0].hi == 4 * NVMTL_SWC_PAGE);
    upload(r, 1);
    ARM("  and the VRAM side then equals the host side", memcmp(host, vram, N) == 0);

    /* two separate changes and one adjacent pair */
    host[0] ^= 1; host[5 * NVMTL_SWC_PAGE] ^= 1; host[6 * NVMTL_SWC_PAGE + 4095] ^= 1;
    { int n = nvmtl_swc_before_submit(&s, host, r, 8);
      ARM("changes in pages 0, 5 and 6 give two ranges: page 0, and pages 5-6 merged",
          n == 2 && r[0].lo == 0 && r[0].hi == NVMTL_SWC_PAGE && r[1].lo == 5 * NVMTL_SWC_PAGE && r[1].hi == 7 * NVMTL_SWC_PAGE);
      upload(r, n); ARM("  VRAM equals host", memcmp(host, vram, N) == 0); }

    /* the partial last page */
    host[N - 1] ^= 0xFF;
    ARM("a change in the partial last page uploads exactly the bytes that exist", nvmtl_swc_before_submit(&s, host, r, 8) == 1 && r[0].hi == N && r[0].lo == 10 * NVMTL_SWC_PAGE);
    upload(r, 1);

    /* more ranges than the caller can take: the extras are folded, never dropped */
    for (int p = 0; p < 11; p += 2) host[p * NVMTL_SWC_PAGE] ^= 1;
    { nvmtl_swc_range few[2]; int n = nvmtl_swc_before_submit(&s, host, few, 2);
      memcpy(vram, vram, 1); /* keep the compiler honest about vram */
      for (int i = 0; i < n; i++) memcpy(vram + few[i].lo, host + few[i].lo, few[i].hi - few[i].lo);
      ARM("six scattered pages with room for two ranges: the last range absorbs the tail and VRAM still equals host", n == 2 && memcmp(host, vram, N) == 0); }

    /* a full cycle of alternating CPU and GPU work stays coherent */
    { int ok = 1;
      for (int it = 0; it < 50; it++) {
          host[(it * 997) % N] ^= (unsigned char)(it + 1);          /* CPU edit */
          unsigned char want[N]; memcpy(want, host, N); for (size_t i = 0; i < N; i++) want[i]++;
          submit(&s, k_inc);                                        /* GPU +1 on everything */
          if (memcmp(host, want, N) != 0) ok = 0;
      }
      ARM("50 alternations of a CPU edit and a GPU pass: contents always equals the CPU's data plus the GPU's pass", ok); }

    /* a read-only submit costs no upload and no stale data */
    { const uint64_t up = s.uploads; submit(&s, k_nop);
      ARM("a submit with no CPU edit and a kernel that writes nothing uploads nothing", s.uploads == up); }

    /* NEGATIVE CONTROL: a plain copy that uploads only the first time (no per-submit diff) must fail the same cycle */
    { int first = 1, stale = 0;
      memset(host, 3, N); memset(vram, 0, N);
      for (int it = 0; it < 5; it++) {
          host[(it * 997) % N] ^= (unsigned char)(it + 1);
          if (first) { memcpy(vram, host, N); first = 0; }          /* old rule: only the very first submit copies */
          if (memcmp(host, vram, N) != 0) stale++;
          memcpy(host, vram, N);
      }
      ARM("the control (upload only once) leaves the GPU with stale bytes in the same cycle", stale > 0); }

    /* the caller-pages variant (an unimportable managed no-copy range): the host side is the caller's memory and an owned
       staging buffer carries both transfers */
    { static unsigned char caller[N], staging[N], gpu[N]; nvmtl_swc c; int ok = 1; nvmtl_swc_init(&c, N);
      for (size_t i = 0; i < N; i++) caller[i] = (unsigned char)(i ^ 0x33);
      for (int it = 0; it < 20; it++) {
          caller[(it * 1013) % N] ^= (unsigned char)(it + 1);                       /* the app writes its own pages */
          unsigned char want[N]; memcpy(want, caller, N); for (size_t i = 0; i < N; i++) want[i] = (unsigned char)(want[i] * 3);
          nvmtl_swc_range r[16]; int n = nvmtl_swc_before_submit(&c, caller, r, 16);
          for (int i = 0; i < n; i++) { memcpy(staging + r[i].lo, caller + r[i].lo, r[i].hi - r[i].lo); memcpy(gpu + r[i].lo, staging + r[i].lo, r[i].hi - r[i].lo); }
          for (size_t i = 0; i < N; i++) gpu[i] = (unsigned char)(gpu[i] * 3);       /* the kernel */
          memcpy(staging, gpu, N); memcpy(caller, staging, N); nvmtl_swc_after_complete(&c, caller);
          if (memcmp(caller, want, N) != 0) ok = 0;
          memcpy(caller, want, N); memcpy(gpu, want, N);                             /* the app keeps what it read */
          nvmtl_swc_after_complete(&c, caller);
      }
      ARM("caller pages with owned staging: 20 rounds of an app edit and a kernel pass return exactly the kernel's result", ok);
      nvmtl_swc_free(&c); }

    /* eligibility */
    ARM("a Shared buffer inside the window qualifies when enabled", nvmtl_swc_wants(1, 1, 1 << 20, 4096, 64 << 20));
    ARM("  not when disabled (the default)", !nvmtl_swc_wants(0, 1, 1 << 20, 4096, 64 << 20));
    ARM("  not a Managed or Private buffer", !nvmtl_swc_wants(1, 0, 1 << 20, 4096, 64 << 20));
    ARM("  not below the window", !nvmtl_swc_wants(1, 1, 100, 4096, 64 << 20));
    ARM("  not above the window", !nvmtl_swc_wants(1, 1, (size_t)1 << 30, 4096, 64 << 20));

    /* hash sensitivity: every single-bit change in a page is seen */
    { unsigned char pg[NVMTL_SWC_PAGE] = {0}; const uint64_t h0 = nvmtl_swc_hash(pg, sizeof pg); int miss = 0;
      for (int b = 0; b < (int)sizeof pg * 8; b++) { pg[b / 8] ^= (unsigned char)(1 << (b % 8)); if (nvmtl_swc_hash(pg, sizeof pg) == h0) miss++; pg[b / 8] ^= (unsigned char)(1 << (b % 8)); }
      ARM("all 32768 single-bit changes of a zero page change its hash", miss == 0); }

    nvmtl_swc_free(&s);
    printf("%s\n", fails ? "FAILED" : "all arms passed");
    return fails != 0;
}

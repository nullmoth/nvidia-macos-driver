/* test_fault_context.c - the fault-context rings name a kernel that was in flight while its image died.
   Build and run anywhere (no GPU):  cc -I plugin tools/test_fault_context.c -o /tmp/test_fault_context && /tmp/test_fault_context */
#include <stdio.h>
#include <stdlib.h>
#include "nvmtl_fault_context.h"

static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
static char out[64][512]; static unsigned nout;
static void emit(void *ctx, const char *line) { (void)ctx; if (nout < 64) snprintf(out[nout++], sizeof out[0], "%s", line); }
static int has(const char *needle) { for (unsigned i = 0; i < nout; i++) if (strstr(out[i], needle)) return 1; return 0; }

int main(void)
{
    static nvmtl_fx_state s; static nvmtl_fx_cb cb;
    const void *vA = (const void *)0x1000, *vB = (const void *)0x2000;

    /* a command buffer binds storage view A at 3 and B at 5, dispatches k1, unbinds 5, dispatches k2 */
    nvmtl_fx_bind_storage(&cb, 3, vA); nvmtl_fx_bind_storage(&cb, 5, vB);
    nvmtl_fx_dispatch(&cb, "k1", (const void *)0xa1);
    nvmtl_fx_bind_storage(&cb, 5, NULL);
    nvmtl_fx_dispatch(&cb, "k2", (const void *)0xa2);
    arm("an unbind removes the slot from the binds in force", cb.nstor == 1 && cb.stor_idx[0] == 3);
    arm("the first dispatch kept both binds, the second one", cb.d[0].nstor == 2 && cb.d[1].nstor == 1);

    /* image A dies at seq 10, the command buffer is submitted at seq 11 */
    nvmtl_fx_image_gone(&s, 10, vA, 1920, 1080, 44, 1, NVMTL_FX_PLACED, "gbuffer");
    nvmtl_fx_submit(&s, 11, (const void *)0xcb, &cb);
    nout = 0; unsigned dead = nvmtl_fx_report(&s, 11, 8, 200, emit, NULL);
    arm("a dispatch bound to a view whose image died before its submit is marked DEAD", dead == 2 && has("k1") && has("DEAD: image 1920x1080 fmt 44 storage \"gbuffer\" died at seq 10"));
    arm("the dead image's death is listed with its sequence number and where its memory lived", has("died at seq 10: image view 0x1000") && has(" placed \"gbuffer\""));
    arm("a dispatch line names its pipeline object", has("k1 pipe 0xa1"));
    arm("the live view B is listed without a DEAD mark", has("storage 5 view 0x2000") && !has("view 0x2000 DEAD"));

    /* negative control: the death comes AFTER the submit - a kernel cannot have read a dead image */
    static nvmtl_fx_state s2; static nvmtl_fx_cb cb2;
    nvmtl_fx_bind_storage(&cb2, 0, vB); nvmtl_fx_dispatch(&cb2, "k3", NULL);
    nvmtl_fx_submit(&s2, 20, (const void *)0xcb, &cb2);
    nvmtl_fx_image_gone(&s2, 21, vB, 64, 64, 9, 0, 0, "later");
    nout = 0; dead = nvmtl_fx_report(&s2, 21, 8, 200, emit, NULL);
    arm("a death after the submit is not a DEAD mark (0)", dead == 0 && !has("DEAD"));
    arm("but it is still listed as a death in the window", has("died at seq 21: image view 0x2000 64x64 fmt 9 own memory \"later\""));

    /* heap slot deaths and the window */
    nvmtl_fx_slot_gone(&s2, 22, 14);
    nvmtl_fx_slot_gone(&s2, 2, 15);
    nout = 0; nvmtl_fx_report(&s2, 210, 8, 200, emit, NULL);   /* window = seq 10..210 */
    arm("a heap slot death inside the window is named by slot", has("died at seq 22: heap slot 14"));
    arm("a death outside the window is left out", !has("heap slot 15"));

    /* the rings wrap without losing the newest */
    static nvmtl_fx_state s3; static nvmtl_fx_cb cb3;
    for (int i = 0; i < NVMTL_FX_DISP + 3; i++) { char n[16]; snprintf(n, sizeof n, "d%d", i); nvmtl_fx_dispatch(&cb3, n, NULL); }
    for (int i = 0; i < NVMTL_FX_SUB + 5; i++) nvmtl_fx_submit(&s3, 100 + (uint64_t)i, NULL, &cb3);
    nout = 0; nvmtl_fx_report(&s3, 200, 2, 10, emit, NULL);
    arm("after wrapping, the newest submit and newest dispatch are reported", has("submit seq 152") && has("d10") && !has("d0 "));
    arm("the dispatch count survives the ring (11 in all)", has("11 dispatches in all"));

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}

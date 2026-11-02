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
    nvmtl_fx_dispatch(&cb, "k1", (const void *)0xa1, 0xdeadbeefu, 0x0123456789abcdefull);
    nvmtl_fx_bind_storage(&cb, 5, NULL);
    nvmtl_fx_dispatch(&cb, "k2", (const void *)0xa2, 0x2u, 0);
    arm("an unbind removes the slot from the binds in force", cb.nstor == 1 && cb.stor_idx[0] == 3);
    arm("the first dispatch kept both binds, the second one", cb.d[0].nstor == 2 && cb.d[1].nstor == 1);

    /* image A dies at seq 10, the command buffer is submitted at seq 11 */
    nvmtl_fx_image_gone(&s, 10, vA, 1920, 1080, 44, 1, NVMTL_FX_PLACED, "gbuffer");
    nvmtl_fx_submit(&s, 11, (const void *)0xcb, &cb);
    nout = 0; unsigned dead = nvmtl_fx_report(&s, 11, 8, 200, emit, NULL);
    arm("a dispatch bound to a view whose image died before its submit is marked DEAD", dead == 2 && has("k1") && has("DEAD: image 1920x1080 fmt 44 storage \"gbuffer\" died at seq 10"));
    arm("the dead image's death is listed with its sequence number and where its memory lived", has("died at seq 10: image view 0x1000") && has(" placed \"gbuffer\""));
    arm("a dispatch line names its kernel hash, its AIR key and pipeline object", has("k1 spv-fnv deadbeef air 0123456789abcdef pipe 0xa1"));
    arm("the live view B is listed without a DEAD mark", has("storage 5 view 0x2000") && !has("view 0x2000 DEAD"));

    /* negative control: the death comes AFTER the submit - a kernel cannot have read a dead image */
    static nvmtl_fx_state s2; static nvmtl_fx_cb cb2;
    nvmtl_fx_bind_storage(&cb2, 0, vB); nvmtl_fx_dispatch(&cb2, "k3", NULL, 0, 0);
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
    for (int i = 0; i < NVMTL_FX_DISP + 3; i++) { char n[16]; snprintf(n, sizeof n, "d%d", i); nvmtl_fx_dispatch(&cb3, n, NULL, 0, 0); }
    for (int i = 0; i < NVMTL_FX_SUB + 5; i++) nvmtl_fx_submit(&s3, 100 + (uint64_t)i, NULL, &cb3);
    nout = 0; nvmtl_fx_report(&s3, 200, 2, 10, emit, NULL);
    arm("after wrapping, the newest submit and newest dispatch are reported", has("submit seq 152") && has("d10") && !has("d0 "));
    arm("the dispatch count survives the ring (11 in all)", has("11 dispatches in all"));

    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    /* buffer deaths: a FAULT_PTE address is matched against the ranges freed, newest first; a live address names nothing */
    {   nvmtl_fx_state s4; memset(&s4, 0, sizeof s4);
        nvmtl_fx_buffer_gone(&s4, 300, 0x36f100000ull, 0x40000, 0);                 /* 0x36f100000 .. 0x36f140000 */
        nvmtl_fx_buffer_gone(&s4, 301, 0x4b0000000ull, 0x1000, NVMTL_FX_PARKED);
        nvmtl_fx_buffer_gone(&s4, 302, 0, 0x1000, 0);                               /* no address: never recorded */
        const nvmtl_fx_buf *b = nvmtl_fx_buffer_at(&s4, 0x36f122000ull);
        arm("a fault address inside a freed buffer names that buffer and the sequence it died at", b && b->seq == 300 && b->lo == 0x36f100000ull);
        arm("an address no freed buffer held names nothing (negative control)", nvmtl_fx_buffer_at(&s4, 0x36f140000ull) == NULL && nvmtl_fx_buffer_at(&s4, 0x1000ull) == NULL);
        arm("a buffer without a device address is not recorded", s4.nbuf == 2);
        nout = 0; nvmtl_fx_report(&s4, 302, 2, 10, emit, NULL);
        arm("the report lists the freed ranges with their placement", has("301 0x4b0000000..0x4b0001000 4 parked |") && has("300 0x36f100000..0x36f140000 256 |"));
        arm("  and counts them", has("2 of 2 buffer deaths within the last 10 submits shown (2 recorded in all)"));
        for (unsigned k = 0; k < NVMTL_FX_BUFS + 5; k++) nvmtl_fx_buffer_gone(&s4, 400 + k, 0x1000000ull + k * 0x1000, 0x1000, 0);
        arm("the ring wraps and the newest death still wins the match", nvmtl_fx_buffer_at(&s4, 0x1000000ull + (NVMTL_FX_BUFS + 4) * 0x1000) != NULL && nvmtl_fx_buffer_at(&s4, 0x36f122000ull) == NULL);
    }
    /* barrier record, copy ring and writer census */
    {   nvmtl_fx_state s5; memset(&s5, 0, sizeof s5); nvmtl_fx_cb me; memset(&me, 0, sizeof me);
        const void *cb = (const void *)0x1000, *other = (const void *)0x2000;
        /* dispatch 1 (writer) binds b2 = [0x100000, 0x140000); dispatch 2 (reader) binds the same b2 */
        me.nroot = 1; me.root[0] = (nvmtl_fx_root){ 0x100000, 0x140000, 2 }; nvmtl_fx_dispatch(&me, "writer", NULL, 0x844d7eb8, 1);
        nvmtl_fx_barrier(&s5, cb, 1, 0x800, 0x800, 0x40, 0x20 | 0x40, 1, 0, 0);          /* COMPUTE->COMPUTE after dispatch 1 */
        nvmtl_fx_barrier(&s5, other, 1, 0x10000, 0x10000, 0x10000, 0x8000, 1, 0, 0);  /* another cb's barrier: not ours */
        nvmtl_fx_dispatch(&me, "reader", NULL, 0x3d8cd246, 2);
        arm("a dispatch carries its ordinal and roots", me.d[1].ordinal == 2 && me.d[1].nroot == 1 && me.d[1].root[0].idx == 2 && me.d[0].ordinal == 1);
        nout = 0; unsigned nb1 = nvmtl_fx_barriers_after(&s5, cb, 1, emit, NULL);
        arm("the barrier after dispatch 1 is reported with its stages and access masks", nb1 == 1 && has("barrier after dispatch 1: COMPUTE -> COMPUTE, 1 memory [SHADER_WRITE -> SHADER_READ|SHADER_WRITE], 0 buffer, 0 image"));
        nout = 0; unsigned nb2 = nvmtl_fx_barriers_after(&s5, cb, 2, emit, NULL);
        arm("no barrier after dispatch 2 reads NONE (negative control; the other cb's barrier is not counted)", nb2 == 0 && has("barrier after dispatch 2: NONE"));
        /* a copy into b2 one submit earlier, from a host-visible staging source */
        static uint64_t staging[16] = { 0x36f100000ull, 7, 0x2ef8a1ff8ull };
        nvmtl_fx_copy_note(&s5, 0x100000, 0x200, 0x9000000, staging, 99, other, 3);
        nvmtl_fx_copy_note(&s5, 0x300000, 0x100, 0x9100000, NULL, 100, other, 1);   /* elsewhere */
        const nvmtl_fx_copy *cv = nvmtl_fx_copy_covering(&s5, 0x100000, 0x100040);
        arm("the newest copy covering a 64-byte window is found and carries the host view", cv && cv->src_map == staging && cv->seq == 99);
        arm("a window the copy does not cover whole names nothing", nvmtl_fx_copy_covering(&s5, 0x1001f0, 0x100230) == NULL && nvmtl_fx_copy_covering(&s5, 0x200000, 0x200040) == NULL);
        /* writers of b2 before dispatch 2 of this cb at seq 100: dispatch 1 here, plus the copy at seq 99 */
        nout = 0; unsigned w = nvmtl_fx_writers(&s5, &me, cb, 100, 2, 2, 0x100000, 0x140000, emit, NULL);
        arm("the writer census names the earlier dispatch of this cb and the copy one submit before", w == 2 && has("bound earlier by: writer 844d7eb8 (dispatch 1, this cb, b2) | 1 copies into it, newest 0x100000..0x100200 at seq 99 after dispatch 3"));
        nout = 0; w = nvmtl_fx_writers(&s5, &me, cb, 100, 1, 2, 0x100000, 0x140000, emit, NULL);
        arm("before dispatch 1 nothing in this cb precedes it; the copy still counts", w == 1 && has("bound earlier by: | 1 copies"));
        nout = 0; w = nvmtl_fx_writers(&s5, &me, cb, 100, 2, 5, 0x500000, 0x500100, emit, NULL);
        arm("a range nobody bound or copied reads 'nothing recorded' (negative control)", w == 0 && has("nothing recorded | 0 copies"));
        /* the previous submits' dispatches count too */
        nvmtl_fx_cb prev; memset(&prev, 0, sizeof prev); prev.nroot = 1; prev.root[0] = (nvmtl_fx_root){ 0x120000, 0x130000, 7 }; nvmtl_fx_dispatch(&prev, "froxel", NULL, 0x0d2845ba, 9);
        nvmtl_fx_submit(&s5, 98, other, &prev);
        nvmtl_fx_submit(&s5, 97, other, &prev);   /* too old: three submits back */
        nout = 0; w = nvmtl_fx_writers(&s5, &me, cb, 100, 2, 2, 0x100000, 0x140000, emit, NULL);
        arm("a dispatch of the submit two back that overlaps the root is named; three back is not", w == 3 && has("froxel 0d2845ba (dispatch 1, seq 98, b7)") && !has("seq 97"));
    }
    /* the device-loss report can neither hang on a lock nor crash on a read */
    {   pthread_mutex_t held = PTHREAD_MUTEX_INITIALIZER, freem = PTHREAD_MUTEX_INITIALIZER;
        pthread_mutex_lock(&held);
        struct timespec t0, t1; clock_gettime(CLOCK_MONOTONIC, &t0);
        const int got = nvmtl_fx_lock_bounded(&held, 50);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        const double ms = (t1.tv_sec - t0.tv_sec) * 1e3 + (t1.tv_nsec - t0.tv_nsec) / 1e6;
        arm("a lock another thread holds is given up within its bound, not waited for forever (the b74e536 dump stopped after the pages section)", got == 0 && ms >= 40 && ms < 400);
        arm("  a free lock is taken at once (negative control)", nvmtl_fx_lock_bounded(&freem, 50) == 1);
        pthread_mutex_unlock(&freem); pthread_mutex_unlock(&held);
        uint64_t word = 0x1234567887654321ull, out = 0;
        arm("a host read of mapped memory through the kernel returns the bytes", nvmtl_fx_read_safe(&word, &out, 8) && out == word);
        void *bad = (void *)0x10;
        arm("  a read of an unmapped address is refused, not a crash (negative control)", nvmtl_fx_read_safe(bad, &out, 8) == 0);
    }
    return fails ? 1 : 0;
}

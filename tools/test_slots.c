/* test_slots.c - the heap slot / image report, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_slots.c -o /tmp/test_slots && /tmp/test_slots */
#include "nvmtl_slots.h"

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)
static char out[32][512]; static unsigned nout;
static void emit(void *ctx, const char *line) { (void)ctx; if (nout < 32) snprintf(out[nout++], sizeof out[0], "%s", line); }
static int has(const char *needle) { for (unsigned i = 0; i < nout; i++) if (strstr(out[i], needle)) return 1; return 0; }

int main(void)
{
    nvmtl_sl_state sl; nvmtl_im_state im; nvmtl_vw_state vw; memset(&sl, 0, sizeof sl); memset(&im, 0, sizeof im); memset(&vw, 0, sizeof vw);
    ARM("the slot table allocates", nvmtl_sl_init(&sl, 4096) == 0 && sl.n == 4096);
    const void *dummy0 = (void *)0xd0, *dummy2 = (void *)0xd2, *dummyT = (void *)0xd3;
    nvmtl_sl_dummies d = { { dummy0, NULL, dummy2, dummyT, dummyT } };
    /* image A with sampled view vA and storage view sA; image B with vB/sB */
    const void *imgA = (void *)0xa0, *vA = (void *)0xa1, *sA = (void *)0xa2, *memA = (void *)0xaa;
    const void *imgB = (void *)0xb0, *vB = (void *)0xb1, *sB = (void *)0xb2;
    nvmtl_im_add(&im, imgA, memA, 4096, 1 << 20, vA, 512, 512, 37, 1, 1, 0, 100); nvmtl_vw_add(&vw, vA, imgA); nvmtl_vw_add(&vw, sA, imgA);
    nvmtl_im_add(&im, imgB, memA, 1 << 20, 1 << 20, vB, 256, 256, 37, 1, 1, 0, 120); nvmtl_vw_add(&vw, vB, imgB); nvmtl_vw_add(&vw, sB, imgB);
    /* slot 7: texture A takes it, bindings 0 and 2 written consistently */
    nvmtl_sl_alloc(&sl, 7, 101); nvmtl_sl_write(&sl, 7, 0, vA, 101); nvmtl_sl_write(&sl, 7, 2, sA, 101);
    nout = 0; unsigned f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "NVMTLTexture 0x1 512x512", emit, NULL);
    ARM("a consistent live slot reports OK", f == 0 && has("heap slot 7: LIVE, allocated 1 time (last at seq 101); owner: NVMTLTexture 0x1 512x512") && has("verdict: OK"));
    ARM("  each binding names its image and that it is alive", has("binding 2: view 0xa2 written at seq 101 -> image 0xa0 512x512 fmt 37 storage placed mem 0xaa+4096 1048576 bytes, created seq 100, ALIVE"));
    /* the negative control: A dies, the slot is dropped (dummies at 0, 2-4), then reused by B which rewrites binding 0 ONLY */
    nvmtl_im_died(&im, imgA, 150);
    nvmtl_sl_drop(&sl, 7, 150); nvmtl_sl_write(&sl, 7, 0, dummy0, 150); nvmtl_sl_write(&sl, 7, 2, dummy2, 150); nvmtl_sl_write(&sl, 7, 3, dummyT, 150); nvmtl_sl_write(&sl, 7, 4, dummyT, 150);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", emit, NULL);
    ARM("a dropped slot with dummies everywhere reports DROPPED and nothing stale", f == NVMTL_SL_DROPPED && has("heap slot 7: DROPPED, allocated 1 time (last at seq 101), dropped at seq 150") && has("binding 2: the dummy (written at seq 150)"));
    nvmtl_sl_alloc(&sl, 7, 200); nvmtl_sl_write(&sl, 7, 0, vB, 200);
    nvmtl_sl_write(&sl, 7, 2, sA, 200);   /* the stale case: binding 2 left (or rewritten) with the DEAD image's storage view */
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", emit, NULL);
    ARM("a reused slot whose storage descriptor names the dead image reports MISMATCH and DEAD (negative control)",
        (f & NVMTL_SL_MISMATCH) && (f & NVMTL_SL_DEAD) && !(f & NVMTL_SL_DROPPED) && has("allocated 2 times (last at seq 200)") && has("-> image 0xa0 512x512") && has("DIED at seq 150") && has("verdict: MISMATCH"));
    nvmtl_sl_write(&sl, 7, 2, sB, 201);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", emit, NULL);
    ARM("after the storage descriptor is rewritten for the new image the slot is OK again", f == 0 && has("verdict: OK"));
    /* a slot nobody ever allocated, and a live slot whose binding 2 was never written */
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 9, &d, "", emit, NULL);
    ARM("a never-allocated slot says so, with binding 2 never written", (f & NVMTL_SL_NEVER) && (f & NVMTL_SL_UNWRITTEN) && has("heap slot 9: NEVER ALLOCATED") && has("binding 2: never written (a storage write through it reads an undefined descriptor)"));
    nvmtl_sl_alloc(&sl, 11, 300); nvmtl_sl_write(&sl, 11, 0, vB, 300);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 11, &d, "", emit, NULL);
    ARM("a live slot with binding 2 never written is flagged UNWRITTEN only", f == NVMTL_SL_UNWRITTEN);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 5000, &d, "", emit, NULL);
    ARM("a slot index outside the heap is refused", f == NVMTL_SL_NEVER && has("outside the heap (4096 slots)"));
    /* a view of no known image */
    nvmtl_sl_write(&sl, 11, 2, (void *)0xee, 301);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 11, &d, "", emit, NULL);
    ARM("a view the table never paired with an image is said so, and is neither dead nor a mismatch", f == 0 && has("of no image this table knows"));
    /* census */
    nvmtl_sl_write(&sl, 7, 2, sA, 400);   /* make 7 stale again */
    nvmtl_sl_alloc(&sl, 12, 401); nvmtl_sl_write(&sl, 12, 0, vB, 401); nvmtl_sl_drop(&sl, 12, 402); nvmtl_sl_write(&sl, 12, 0, dummy0, 402);   /* dropped, but binding 2 unwritten: fine */
    nvmtl_sl_alloc(&sl, 13, 403); nvmtl_sl_write(&sl, 13, 0, vA, 403); nvmtl_sl_drop(&sl, 13, 404);   /* dropped without the dummies: still holds a view */
    nout = 0; nvmtl_sl_census(&sl, &im, &vw, &d, emit, NULL);
    ARM("the census counts live, dropped, disagreeing, dead-naming, storage-less and dropped-but-non-dummy slots",
        has("heap slot census: 2 live, 2 dropped; 1 with bindings 0/2 naming different images, 2 naming a dead image, 0 live without a storage descriptor, 1 dropped slots still holding a non-dummy descriptor; 2 images recorded (1 alive)"));
    /* a reused image handle resolves to its newest image */
    const void *vC = (void *)0xc1; nvmtl_im_add(&im, imgA, memA, 0, 4096, vC, 64, 64, 37, 0, 0, 1, 500); nvmtl_vw_add(&vw, vC, imgA);
    const nvmtl_im_entry *e = nvmtl_im_by_view(&im, &vw, vC);
    ARM("a recycled VkImage handle resolves to its newest image, alive", e && e->w == 64 && !e->died && e->seq == 500);
    /* a park is alive but recyclable; a destroy after a park is a death the census must show */
    nvmtl_im_parked(&im, imgB, 600);
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", emit, NULL);
    ARM("a parked image is reported alive but parked, with the seq", has("ALIVE but PARKED for reuse since seq 600"));
    nvmtl_im_recycled(&im, imgB);
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", emit, NULL);
    ARM("a recycled image is plain ALIVE again", !has("PARKED") && has("created seq 120, ALIVE"));
    {   nvmtl_im_state im2; memset(&im2, 0, sizeof im2); nvmtl_im_add(&im2, (void *)0xf0, NULL, 0, 4096, (void *)0xf1, 8, 8, 37, 0, 0, 0, 700);
        ARM("one image made: one alive", im2.live == 1);
        nvmtl_im_died(&im2, (void *)0xf0, 701);
        FILE *f2 = tmpfile(); nvmtl_im_dump(&im2, f2); rewind(f2); char l2[256] = ""; fgets(l2, sizeof l2, f2); fclose(f2);
        ARM("a destroyed image is a death: zero alive, and the dump says when (an instrument that counts 0 deaths after a destroy is broken)", im2.live == 0 && strstr(l2, "seq 700 died 701"));
        free(im2.e); }
    FILE *fp = tmpfile(); const uint32_t n = nvmtl_im_dump(&im, fp);
    rewind(fp); char first[256]; fgets(first, sizeof first, fp);
    ARM("the image dump writes every image with its memory, shape and lifetime", n == 3 && strstr(first, "image 0xa0 view 0xa1 mem 0xaa off 4096 size 1048576 512x512 fmt 37 storage placed seq 100 died 150"));
    fclose(fp); free(sl.s); free(im.e); free(vw.e);
    printf("test_slots: %d failed\n", fails);
    return fails ? 1 : 0;
}

/* test_slots.c - the heap slot / image report, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_slots.c -o /tmp/test_slots && /tmp/test_slots */
#include "nvmtl_slots.h"
#include "nvmtl_nvk_layout.h"

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)
static char out[32][512]; static unsigned nout;
static void emit(void *ctx, const char *line) { (void)ctx; if (nout < 32) snprintf(out[nout++], sizeof out[0], "%s", line); }
static int has(const char *needle) { for (unsigned i = 0; i < nout; i++) if (strstr(out[i], needle)) return 1; return 0; }

int main(void)
{
    nvmtl_sl_state sl; nvmtl_im_state im; nvmtl_vw_state vw; memset(&sl, 0, sizeof sl); memset(&im, 0, sizeof im); memset(&vw, 0, sizeof vw);
    nvmtl_mm_state mm; memset(&mm, 0, sizeof mm); nvmtl_mb_state mb; memset(&mb, 0, sizeof mb);
    const void *memA = (void *)0xaa; nvmtl_mm_add(&mm, memA, 0x2ef000000ull, 64 << 20, 0, "nvmtl_vk_heap_create_body", 90);
    ARM("the slot table allocates", nvmtl_sl_init(&sl, 4096) == 0 && sl.n == 4096);
    const void *dummy0 = (void *)0xd0, *dummy2 = (void *)0xd2, *dummyT = (void *)0xd3;
    nvmtl_sl_dummies d = { { dummy0, NULL, dummy2, dummyT, dummyT } };
    /* image A with sampled view vA and storage view sA; image B with vB/sB */
    const void *imgA = (void *)0xa0, *vA = (void *)0xa1, *sA = (void *)0xa2;
    const void *imgB = (void *)0xb0, *vB = (void *)0xb1, *sB = (void *)0xb2;
    nvmtl_im_add(&im, imgA, memA, 4096, 1 << 20, vA, 512, 512, 37, 1, 1, 0, 100); nvmtl_vw_add(&vw, vA, imgA); nvmtl_vw_add(&vw, sA, imgA);
    nvmtl_im_add(&im, imgB, memA, 1 << 20, 1 << 20, vB, 256, 256, 37, 1, 1, 0, 120); nvmtl_vw_add(&vw, vB, imgB); nvmtl_vw_add(&vw, sB, imgB);
    /* slot 7: texture A takes it, bindings 0 and 2 written consistently */
    nvmtl_sl_alloc(&sl, 7, 101); nvmtl_sl_write(&sl, 7, 0, vA, 101); nvmtl_sl_write(&sl, 7, 2, sA, 101);
    nout = 0; unsigned f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "NVMTLTexture 0x1 512x512", &mm, emit, NULL);
    ARM("a consistent live slot reports OK", f == 0 && has("heap slot 7: LIVE, allocated 1 time (last at seq 101); owner: NVMTLTexture 0x1 512x512") && has("verdict: OK"));
    ARM("  each binding names its image and that it is alive", has("binding 2: view 0xa2 written at seq 101 -> image 0xa0 512x512 fmt 37 storage placed mem 0xaa+4096 1048576 bytes, created seq 100, ALIVE"));
    nvmtl_im_set_params(&im, 1, 1, 12, 0, 0x7, 1, 1 << 20); nvmtl_vw_add_ex(&vw, sA, imgA, 5 + 1, 0, 1, 0, 1);
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("the image line says what it is, needs and got, and what the view covers", has("image: 1 layer, 12 mips, 1 sample, flags 0x0, usage 0x7; needs 1048576 bytes, given 1048576 from its bind offset; view: VkImageViewType 5, layers 0+1, mips 0+1"));
    nvmtl_im_set_params(&im, 1, 2, 12, 0, 0x7, 1, 1 << 19);
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("an image needing more than its bind offers is flagged UNDERSIZED BIND", has("needs 1048576 bytes, given 524288 from its bind offset - UNDERSIZED BIND") && nvmtl_im_undersized(nvmtl_im_by_img(&im, imgA)));
    nvmtl_im_set_params(&im, 1, 1, 12, 0, 0x7, 1, 1 << 20);
    ARM("  and where its bytes are: the memory, its base, liveness and the image's start", has("its memory 0xaa: nvmtl_vk_heap_create_body, base 0x2ef000000, size 67108864, created seq 90, ALIVE -> the image's bytes start at 0x2ef001000"));
    /* the memory table resolves an address to a memory and its offset, live or dead, and binds remember an image's offset */
    { char ml[300]; nvmtl_mm_describe(&mm, 0x2ef822000ull, ml, sizeof ml);
      ARM("an address inside a live memory resolves to it with the offset", strstr(ml, "0x2ef822000 = nvmtl_vk_heap_create_body memory 0xaa (base 0x2ef000000, size 67108864, created seq 90, ALIVE) + offset 8527872") != NULL);
      nvmtl_mm_describe(&mm, 0x1ull, ml, sizeof ml);
      ARM("an address in no memory says so (negative control)", strstr(ml, "is in no memory this process ever mapped (1 memories, 0 without a base address)") != NULL);
      nvmtl_mm_died(&mm, memA, 777); nvmtl_mm_describe(&mm, 0x2ef822000ull, ml, sizeof ml);
      ARM("after the memory is freed the same address resolves to it as DIED", strstr(ml, "DIED at seq 777) + offset 8527872") != NULL && mm.live == 0);
      nvmtl_mm_add(&mm, memA, 0x300000000ull, 4096, 1, "reuse", 800);
      ARM("a reused memory handle resolves to its newest record", nvmtl_mm_by_mem(&mm, memA)->base == 0x300000000ull && nvmtl_mm_at(&mm, 0x2ef822000ull)->died == 777);
      const void *bm = NULL; uint64_t bo = 0; nvmtl_mb_note(&mb, imgA, memA, 4096); nvmtl_mb_note(&mb, imgB, memA, 1 << 20);
      ARM("an image's newest bind gives its memory and offset", nvmtl_mb_find(&mb, imgB, &bm, &bo) && bm == memA && bo == (1 << 20) && nvmtl_mb_find(&mb, imgA, &bm, &bo) && bo == 4096);
      ARM("an image never bound is not found (negative control)", nvmtl_mb_find(&mb, (void *)0x99, &bm, &bo) == 0);
      nvmtl_mm_add(&mm, memA, 0x2ef000000ull, 64 << 20, 0, "nvmtl_vk_heap_create_body", 90);   /* restore a live record for the later arms */ }
    /* the negative control: A dies, the slot is dropped (dummies at 0, 2-4), then reused by B which rewrites binding 0 ONLY */
    nvmtl_im_died(&im, imgA, 150);
    nvmtl_sl_drop(&sl, 7, 150); nvmtl_sl_write(&sl, 7, 0, dummy0, 150); nvmtl_sl_write(&sl, 7, 2, dummy2, 150); nvmtl_sl_write(&sl, 7, 3, dummyT, 150); nvmtl_sl_write(&sl, 7, 4, dummyT, 150);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("a dropped slot with dummies everywhere reports DROPPED and nothing stale", f == NVMTL_SL_DROPPED && has("heap slot 7: DROPPED, allocated 1 time (last at seq 101), dropped at seq 150") && has("binding 2: the dummy (written at seq 150)"));
    nvmtl_sl_alloc(&sl, 7, 200); nvmtl_sl_write(&sl, 7, 0, vB, 200);
    nvmtl_sl_write(&sl, 7, 2, sA, 200);   /* the stale case: binding 2 left (or rewritten) with the DEAD image's storage view */
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("a reused slot whose storage descriptor names the dead image reports MISMATCH and DEAD (negative control)",
        (f & NVMTL_SL_MISMATCH) && (f & NVMTL_SL_DEAD) && !(f & NVMTL_SL_DROPPED) && has("allocated 2 times (last at seq 200)") && has("-> image 0xa0 512x512") && has("DIED at seq 150") && has("verdict: MISMATCH"));
    nvmtl_sl_write(&sl, 7, 2, sB, 201);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("after the storage descriptor is rewritten for the new image the slot is OK again", f == 0 && has("verdict: OK"));
    /* a slot nobody ever allocated, and a live slot whose binding 2 was never written */
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 9, &d, "", &mm, emit, NULL);
    ARM("a never-allocated slot says so, with binding 2 never written", (f & NVMTL_SL_NEVER) && (f & NVMTL_SL_UNWRITTEN) && has("heap slot 9: NEVER ALLOCATED") && has("binding 2: never written (a storage write through it reads an undefined descriptor)"));
    nvmtl_sl_alloc(&sl, 11, 300); nvmtl_sl_write(&sl, 11, 0, vB, 300);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 11, &d, "", &mm, emit, NULL);
    ARM("a live slot with binding 2 never written is flagged UNWRITTEN only", f == NVMTL_SL_UNWRITTEN);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 5000, &d, "", &mm, emit, NULL);
    ARM("a slot index outside the heap is refused", f == NVMTL_SL_NEVER && has("outside the heap (4096 slots)"));
    /* a view of no known image */
    nvmtl_sl_write(&sl, 11, 2, (void *)0xee, 301);
    nout = 0; f = nvmtl_sl_report(&sl, &im, &vw, 11, &d, "", &mm, emit, NULL);
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
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("a parked image is reported alive but parked, with the seq", has("ALIVE but PARKED for reuse since seq 600"));
    nvmtl_im_recycled(&im, imgB);
    nout = 0; nvmtl_sl_report(&sl, &im, &vw, 7, &d, "", &mm, emit, NULL);
    ARM("a recycled image is plain ALIVE again", !has("PARKED") && has("created seq 120, ALIVE"));
    {   nvmtl_im_state im2; memset(&im2, 0, sizeof im2); nvmtl_im_add(&im2, (void *)0xf0, NULL, 0, 4096, (void *)0xf1, 8, 8, 37, 0, 0, 0, 700);
        ARM("one image made: one alive", im2.live == 1);
        nvmtl_im_died(&im2, (void *)0xf0, 701);
        FILE *f2 = tmpfile(); nvmtl_im_dump(&im2, f2); rewind(f2); char l2[256] = ""; fgets(l2, sizeof l2, f2); fclose(f2);
        ARM("a destroyed image is a death: zero alive, and the dump says when (an instrument that counts 0 deaths after a destroy is broken)", im2.live == 0 && strstr(l2, "seq 700 died 701"));
        free(im2.e); }
    /* the texture header decode against hand-built words: address 0x4c3200000 (bits 31:9 in w0, 56:32 in w1), 2048x2048x1, 2D array, 11 mips, gob height 4 */
    {   uint32_t w[8] = {0}; const uint64_t addr = 0x4c3200000ull;
        w[0] = (uint32_t)(addr & 0xFFFFFE00u) | 0x1A3; w[1] = (uint32_t)(addr >> 32) | (0x5u << 25); w[2] = (2u << 3) | (1u << 6) | (10u << 28); w[3] = (0x7u << 16) | (1u << 28);
        w[4] = 2047u | (5u << 22); w[5] = 2047u | (0u << 17);
        nvmtl_tic t; nvmtl_tic_decode(w, &t);
        ARM("the texture header decode recovers the address, size, type, mips and tiling from the bit fields", t.addr == addr && t.width == 2048 && t.height == 2048 && t.depth == 1 && t.type == 5 && t.max_mip == 10 && t.gob_h == 2 && t.gob_d == 1 && t.components == 7 && t.version == 1
            && strcmp(nvmtl_tic_type_name(t.type), "2D_ARRAY") == 0);
        ARM("  the low 9 bits of word 0 and bits above 56 are not address (negative control)", t.addr != ((uint64_t)w[1] << 32 | w[0]));
        ARM("the arena index of a table offset follows NVK's doubling memories", nvmtl_nvk_arena_mem_index(0) == 0 && nvmtl_nvk_arena_mem_index(65535) == 0 && nvmtl_nvk_arena_mem_index(65536) == 1 && nvmtl_nvk_arena_mem_index(131072) == 2 && nvmtl_nvk_arena_mem_index(262143) == 2
            && nvmtl_nvk_arena_mem_offset(0) == 0 && nvmtl_nvk_arena_mem_offset(1) == 65536 && nvmtl_nvk_arena_mem_offset(2) == 131072 && nvmtl_nvk_arena_mem_offset(3) == 262144);
    }
    /* descriptor index history: index 10263 held first by a view of A (died), then by a view of B */
    {   nvmtl_vw_set_tic(&vw, sA, 11, 0x2ef400000ull, 10263, 0x2ef400000ull, 4000); nvmtl_vw_died(&vw, sA, 4100);
        nvmtl_vw_set_tic(&vw, sB, 12, 0x4d6a00000ull, 10263, 0x4d6a00000ull, 4200);
        nout = 0; unsigned hn = nvmtl_vw_index_history(&vw, &im, 10263, sB, emit, NULL);
        ARM("the index history lists every holder in order with addresses and lifetimes", hn == 2 && has("table[10263] held by view 0xa2 (storage) of image 0xa0: header address 0x2ef400000, seq 4000..4100") && has("held by view 0xb2 (storage) of image 0xb0: header address 0x4d6a00000, seq 4200..alive <- the current holder"));
        ARM("  and names the previous holder's bytes as the stale-header candidate", has("index 10263 was REISSUED 1 time; the previous holder's bytes began at 0x2ef400000 - a fault there is a STALE HEADER"));
        nout = 0; hn = nvmtl_vw_index_history(&vw, &im, 12, sB, emit, NULL);
        ARM("a first-use index says so (negative control)", hn == 1 && has("index 12 is on its first use"));
        nout = 0; hn = nvmtl_vw_index_history(&vw, &im, 777, sB, emit, NULL);
        ARM("an index nobody held has no holder", hn == 0 && has("index 777 has no recorded holder"));
        ARM("a dead view is not found as live, its record keeps its death", nvmtl_vw_find_live(&vw, sA) == NULL && nvmtl_vw_find(&vw, sA)->died == 4100);
        FILE *vf = tmpfile(); const uint32_t vn = nvmtl_vw_dump(&vw, vf); rewind(vf); char vl[256] = ""; int hit = 0; while (fgets(vl, sizeof vl, vf)) if (strstr(vl, "tic view 0xa2 image 0xa0 sidx 11 saddr 0x2ef400000 tidx 10263 taddr 0x2ef400000 type 5 layers 0+1 mips 0+1 seq 4000 died 4100")) hit = 1; fclose(vf);
        ARM("the view dump writes the resolver's tic lines", vn >= 2 && hit); }
    FILE *fp = tmpfile(); const uint32_t n = nvmtl_im_dump(&im, fp);
    rewind(fp); char first[256]; fgets(first, sizeof first, fp);
    ARM("the image dump writes every image with its memory, shape and lifetime", n == 3 && strstr(first, "image 0xa0 view 0xa1 mem 0xaa off 4096 size 1048576 512x512 fmt 37 storage placed seq 100 died 150"));
    { FILE *mf = tmpfile(); const uint32_t mn = nvmtl_mm_dump(&mm, mf); rewind(mf); char l1[256] = ""; fgets(l1, sizeof l1, mf); fclose(mf);
      ARM("the memory dump writes one line per memory in the resolver's format", mn == 3 && strstr(l1, "memory nvmtl_vk_heap_create_body mem 0xaa base 0x2ef000000 size 67108864 type 0 seq 90 died 777")); }
    fclose(fp); free(sl.s); free(im.e); free(vw.e); free(mm.e);
    printf("test_slots: %d failed\n", fails);
    return fails ? 1 : 0;
}

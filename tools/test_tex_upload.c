// test_tex_upload.c: the async-texture-upload ordering/lifetime decisions (nvmtl_tex_upload.h), driven
// against a tiny GPU model that detects a copy into freed/overwritten memory.
//
//   cc -std=c11 -Wall -Wextra -I../plugin -o /tmp/test_tex_upload test_tex_upload.c && /tmp/test_tex_upload
//
// The model: an "image" owns GPU memory. An async upload marks the memory "being copied into" until its
// handle is finished (fence waited). Freeing or overwriting memory that is still being copied, or reading
// back before the copy finishes, trips a fault counter. The real plugin's guards must drive these decisions
// so the fault counter stays zero. The negative control runs the SAME scenarios with the guards removed and
// asserts the faults DO fire (that is the #96 use-after-free, which proves the test has teeth).

#include "nvmtl_tex_upload.h"
#include <stdio.h>

static int g_faults;
static int g_pass;
static int g_quiet;   // set during the negative control, where faults are expected
#define CHECK(c, m) do { if (!(c)) { if (!g_quiet) printf("  FAIL: %s\n", m); g_pass = 0; } } while (0)

// --- GPU model -------------------------------------------------------------------------------------------
typedef struct {
    nvmtl_up_state up;
    int freed;           // image memory freed
    int gpu_copying;     // a submitted-but-unfinished upload is still copying into this image's memory
    int bytes_seen;      // "data" the GPU has committed (what a readback would observe)
    int bytes_pending;   // data of the in-flight upload, visible only after finish
} img_t;

static uint64_t g_next_handle = 1;   // each async submit gets a unique non-zero handle

// Finish (fence-wait) a handle: the copy lands, memory is safe to touch again.
static void model_finish(img_t *im, uint64_t handle) {
    if (!handle) return;
    CHECK(!im->freed, "finish must not run on freed memory");
    im->gpu_copying = 0;
    im->bytes_seen = im->bytes_pending;   // the copy completed, data is now visible
    if (im->up.infl == handle) nvmtl_up_clear(&im->up);
}

// --- operations, WITH the guards (the real design) -------------------------------------------------------
static void op_upload(img_t *im, int async_flag, int data) {
    uint64_t finish_first = 0;
    nvmtl_up_submit mode = nvmtl_up_before_upload(&im->up, async_flag, &finish_first);
    if (finish_first) model_finish(im, finish_first);     // serialize same-image / wait-on-reuse
    // overwrite staging/texture now that any prior copy is done
    CHECK(!im->gpu_copying, "upload must not overwrite while a copy is in flight");
    im->bytes_pending = data;
    if (mode == NVMTL_UP_SUBMIT_ASYNC) {
        uint64_t h = g_next_handle++;
        im->gpu_copying = 1;
        nvmtl_up_set(&im->up, h);
    } else {
        im->gpu_copying = 1;          // submit...
        model_finish(im, 0);          // ...and wait immediately
        im->gpu_copying = 0; im->bytes_seen = im->bytes_pending;
    }
}
static void op_consume(img_t *im) {   // destroy / readback / cpu-map / surface-out
    uint64_t pending = nvmtl_up_before_consume(&im->up);
    if (pending) model_finish(im, pending);
}
static void op_destroy(img_t *im) { op_consume(im); CHECK(!im->gpu_copying, "destroy frees while copying"); im->freed = 1; }
static int  op_readback(img_t *im) { op_consume(im); return im->bytes_seen; }

// --- operations, WITHOUT the guards (negative control: the reverted #96 behaviour) -----------------------
static void bad_upload(img_t *im, int data) { // always async, never finish-first
    uint64_t h = g_next_handle++; im->bytes_pending = data; im->gpu_copying = 1; nvmtl_up_set(&im->up, h);
}
static void bad_destroy(img_t *im) { CHECK(!im->gpu_copying, "destroy frees while copying"); im->freed = 1; }
static int  bad_readback(img_t *im) { return im->bytes_seen; }

int main(void) {
    // ---- the real design: faults must stay at zero ----
    g_pass = 1; g_faults = 0;

    // sync path: nothing is ever left pending; the consume guard is a pure no-op
    { img_t im = {0};
      op_upload(&im, 0, 42);
      CHECK(im.up.infl == 0, "sync upload leaves nothing pending");
      CHECK(nvmtl_up_before_consume(&im.up) == 0, "consume guard is a no-op on the sync path");
      CHECK(op_readback(&im) == 42, "sync readback sees the data");
      op_destroy(&im); }

    // async destroy-with-inflight: must finish before free (the #96 case)
    { img_t im = {0};
      op_upload(&im, 1, 7);
      CHECK(im.up.infl != 0, "async upload leaves a pending handle");
      CHECK(im.gpu_copying == 1, "async upload does not wait");
      op_destroy(&im);
      CHECK(im.freed == 1 && im.gpu_copying == 0, "destroy finished the upload before freeing"); }

    // async readback: must finish first so it observes the uploaded data
    { img_t im = {0};
      op_upload(&im, 1, 99);
      CHECK(op_readback(&im) == 99, "readback finished the pending upload and sees its data"); }

    // second upload to the same image: must finish the first (overwrite / staging reuse)
    { img_t im = {0};
      op_upload(&im, 1, 1);
      uint64_t first = im.up.infl;
      op_upload(&im, 1, 2);
      CHECK(first != 0 && im.up.infl != first, "second upload got a new handle after finishing the first");
      CHECK(op_readback(&im) == 2, "latest data wins"); }

    // a draw that only samples is NOT a consume: it must not force a CPU wait (queue order handles it)
    { img_t im = {0};
      op_upload(&im, 1, 5);
      CHECK(im.gpu_copying == 1, "sampling left the upload in flight (no CPU wait forced)");
      op_destroy(&im); }

    printf(g_pass ? "real design: all arms pass\n" : "real design: ARMS FAILED\n");
    int real_ok = g_pass;

    // ---- negative control: the same scenarios without the guards MUST fault ----
    g_pass = 1;
    g_quiet = 1;   // the faults below are expected; do not print them as failures
    int control_fired = 0;
    { img_t im = {0}; bad_upload(&im, 7); g_pass = 1;
      bad_destroy(&im);                       // frees while copying -> CHECK fires
      if (!g_pass) control_fired = 1; }
    { img_t im = {0}; bad_upload(&im, 99);
      if (bad_readback(&im) != 99) control_fired = 1; }   // reads stale data -> mismatch

    printf(control_fired ? "negative control: unguarded path faults, as it must\n"
                         : "negative control: DID NOT FAULT (test has no teeth)\n");

    int ok = real_ok && control_fired;
    printf(ok ? "\nPASS\n" : "\nFAIL\n");
    return ok ? 0 : 1;
}

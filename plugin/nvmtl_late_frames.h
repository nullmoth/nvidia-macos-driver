/* nvmtl_late_frames.h - which presents were late, reported at a bounded rate.
   An integrated GPU that is saving power can sit at a low clock while a frame is already late. The plugin watches the
   interval between an application's presents; the display's frame time is not known to it, so the baseline is the shortest
   recent interval (the vsync pace the application has been able to hold). A present more than half a frame later than that,
   and at least 3 ms later, is a late frame. Late frames are handed to the GPU service in batches, at most one batch per
   100 ms and at most 16 frames per batch, so a stalled application cannot flood it. A gap over a quarter of a second is a
   pause, not lateness, and is not counted. Pure over the clock it is given: tools/test_late_frames.c proves it.
   The mechanism (raise the GPU's minimum clock when frames come late) follows Mountainatic/ArrowWhatever's description for
   Arrow Lake; the detection and the rate limit here are our own. */
#ifndef NVMTL_LATE_FRAMES_H
#define NVMTL_LATE_FRAMES_H
#include <stdint.h>
#include <string.h>

#define NVMTL_LATE_RING 16
#define NVMTL_LATE_MIN_SAMPLES 8
#define NVMTL_LATE_PAUSE_NS 250000000ull
#define NVMTL_LATE_BATCH_NS 100000000ull
#define NVMTL_LATE_MAX_BATCH 16u

struct nvmtl_late {
    uint64_t last_ns, last_report_ns;
    uint64_t ring[NVMTL_LATE_RING];
    unsigned filled, pos;
    uint32_t pending;
};

/* a present at now_ns; returns the number of late frames to report now (0 when none, or when the batch is not yet due) */
static inline uint32_t nvmtl_late_note(struct nvmtl_late *s, uint64_t now_ns)
{
    uint64_t gap;
    if (s->last_ns == 0 || now_ns <= s->last_ns) { s->last_ns = now_ns; return 0; }
    gap = now_ns - s->last_ns;
    s->last_ns = now_ns;
    if (gap > NVMTL_LATE_PAUSE_NS) return 0;
    if (s->filled >= NVMTL_LATE_MIN_SAMPLES) {
        uint64_t base = ~0ull;
        unsigned i;
        for (i = 0; i < s->filled; i++) if (s->ring[i] < base) base = s->ring[i];
        if (base < 4000000ull) base = 4000000ull;                 /* nothing under 4 ms is a frame time */
        if (gap > base + base / 2 && gap >= base + 3000000ull && s->pending < 1000u) s->pending++;
    }
    s->ring[s->pos] = gap;                                         /* a late interval joins the ring too; the minimum ignores it */
    s->pos = (s->pos + 1) % NVMTL_LATE_RING;
    if (s->filled < NVMTL_LATE_RING) s->filled++;
    if (s->pending && now_ns - s->last_report_ns >= NVMTL_LATE_BATCH_NS) {
        uint32_t n = s->pending > NVMTL_LATE_MAX_BATCH ? NVMTL_LATE_MAX_BATCH : s->pending;
        s->pending = 0;
        s->last_report_ns = now_ns;
        return n;
    }
    return 0;
}
#endif

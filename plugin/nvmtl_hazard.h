// Hazard tracking between encoders (NVMTL_HAZARD). Included by nvmtl_vk.c right after the binding table and before
// the first recorded command, so the command wrappers below see every vkCmd* call in nvmtl_vk.c and in the files
// included after it.
//
// Why: on NVK every vkCmdPipelineBarrier whose source holds shader or attachment writes is a WAIT_FOR_IDLE plus a cache
// flush, and every VkRenderPass pays two of them whatever its dependencies say: the begin dependency, and the implicit
// end dependency the common render-pass code always adds (src COLOR/DEPTH_ATTACHMENT_WRITE, vk_render_pass.c). Metal
// orders encoders only where one touches what another wrote. vkCmdBeginRendering emits no barrier on NVK, so a pass
// begun that way costs a drain only where this model asks for one.
//
// The model, per VkCommandBuffer: P = what every tracked command touched since the last full barrier, as
// (VkDeviceMemory, byte range, written) entries, split at k into earlier encoders [0,k) and the current one [k,n).
// "all" = something unknown touched memory since that barrier (untracked work, an entry that did not fit), so the next
// tracked command barriers. A tracked command (a pass begin, a draw in a pass begun without a barrier, a compute
// dispatch, a clear-only pass) needs a barrier only when it reads what an earlier encoder wrote, or writes what an
// earlier encoder read or wrote. Untracked work (blits, copies, internal kernels, anything not checked here) first pays
// any skipped barrier, so it keeps today's ordering; so does the end of a command buffer, and a new command buffer
// starts with "all" set.
//
// NVMTL_HAZARD=1 acts on the model. NVMTL_HAZARD=census runs the same model beside today's barriers and only counts
// (the "hazard census" line, with the commonest conflict reasons).
//
// Untracked resources follow Metal: a resource whose hazardTrackingMode is untracked (every resource of an untracked
// heap, and heaps default to untracked) adds nothing to the model, and useHeap: of an untracked heap adds nothing.
// Metal does not order them between encoders either; an app that needs them ordered encodes MTLFence waits, event waits
// or memory barriers, and each of those stays a full barrier here. useHeap: of a tracked heap is a read of the heap's
// own memory (anything, if some of its members fell back outside it). NVMTL_HAZARD_HEAPS=all is the conservative
// opt-out: every resource is tracked and every used heap reads all memory.
//
// Skipping a barrier that the old code emitted leaves a debt, even when only untracked resources were touched: the
// next untracked work or the end of the command buffer pays it, as the old render-pass end dependency did.

enum { HZ_CAP = 8192, HZ_DECL = 4096, HZ_PEND = 2048, HZ_TAB = 1024, HZ_RC = 1024, HZ_HB = 8192, HZ_HM = 2048, HZ_POOL = HZ_TAB };
#include <mach/mach_time.h>
enum { HZ_BIN_SHIFT = 21 };   // 2 MiB bins: an entry no longer than a bin sits in one bin chain and can overlap only two
enum { HZC_PASS, HZC_PASS_FREE, HZC_PASS_BAR, HZC_PASS_RP, HZC_RESTART, HZC_DISP_FIRST, HZC_DISP_FREE, HZC_DISP_BAR,
       HZC_UNTRACKED, HZC_END, HZC_CLEAN, HZC_CLEAR, HZC_WILD, HZC_HEAP_ALL, HZC_NOSTATE, HZC_HEAP_UNTRACKED,
       HZC_HEAP_TRACKED, HZC_UNTRACKED_BIND, HZC_STORM, HZC_FORCED, HZC_BLIT, HZC_BLIT_FREE, HZC_BLIT_BAR, HZC_BLIT_PREDROP,
       HZC_DISP_C2C, HZC_DISP_C2C_FREE, HZC_N };   /* later dispatches decided: a compute-to-compute barrier kept / skipped */
static _Atomic uint64_t g_hzc[HZC_N];
#define HZC(k) atomic_fetch_add_explicit(&g_hzc[k], 1, memory_order_relaxed)
void nvmtl_vk_hz_counts(uint64_t out[32]) { for (int i = 0; i < 32; i++) out[i] = i < HZC_N ? atomic_load(&g_hzc[i]) : 0; }
// NVMTL_HAZARD_DISPATCH=1: every later dispatch of a compute encoder is decided too. Today the first dispatch is checked
// against earlier encoders and each later one takes a compute-to-compute barrier unconditionally, which on NVK waits for
// every compute dispatch before it. With this on, a later dispatch is checked against this encoder's own entries since
// the last such barrier and takes the narrow barrier only on a real conflict. Off by default; nothing without the model.
int nvmtl_hz_dispatch_decides(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_HAZARD_DISPATCH"); on = e && e[0] == '1'; }
    return on;
}
// The model's own CPU time, per entry point (the census prints it per frame): it runs on the encoding threads, and a
// CPU that submits late starves the GPU, so its cost is fps. Thread-local sums, folded into the totals every 256 events.
enum { HZT_DRAW, HZT_PASS, HZT_DISPATCH, HZT_BIND, HZT_DECLARE, HZT_WORK, HZT_BLIT, HZT_OTHER, HZT_N };
static _Atomic uint64_t g_hzt[HZT_N];
// Every thread-local the model uses, in one block: a dylib reaches each __thread variable through a _tlv_get_addr
// call, and one block lets a function pay that once (measured: separate variables were 8% of the model's time).
typedef struct { uint64_t t[HZT_N]; uint32_t tn, sn; int shadow, blit_pre; VkCommandBuffer ccb, blit_cb; void *cs; } hz_tls;
static __thread hz_tls t_hz;
void nvmtl_vk_hz_cpu(uint64_t out_ns[8])
{
    static double scale; if (!scale) { mach_timebase_info_data_t tb; mach_timebase_info(&tb); scale = (double)tb.numer / (double)tb.denom; }
    for (int i = 0; i < 8; i++) out_ns[i] = (uint64_t)((double)atomic_load(&g_hzt[i]) * scale);
}
static inline uint64_t hzt_now(void) { return mach_absolute_time(); }
// The cost of reading the clock twice, subtracted from every timed interval: the frequent entry points take a few
// nanoseconds, the clock pair more, and a sample scaled by 32 or 128 multiplies that error (the first census
// printed 774 us of "bindings" that were mostly the clock).
static uint64_t g_hzt_bias = UINT64_MAX;
static uint64_t hzt_bias(void)
{
    uint64_t b = g_hzt_bias;
    if (b == UINT64_MAX) {
        b = UINT64_MAX;
        for (int i = 0; i < 64; i++) { const uint64_t a = mach_absolute_time(), z = mach_absolute_time(); if (z - a < b) b = z - a; }
        g_hzt_bias = b;
    }
    return b;
}
static inline uint64_t hzt_since(uint64_t t0) { const uint64_t d = mach_absolute_time() - t0, b = hzt_bias(); return d > b ? d - b : 0; }
static inline void hzt_add(int k, uint64_t t0)
{
    hz_tls *T = &t_hz;
    T->t[k] += hzt_since(t0);
    if (++T->tn >= 256) { T->tn = 0; for (int i = 0; i < HZT_N; i++) if (T->t[i]) { atomic_fetch_add_explicit(&g_hzt[i], T->t[i], memory_order_relaxed); T->t[i] = 0; } }
}
// The frequent entry points are timed on one call in 32 (bindings: one in 128) and scaled: measured on the M4,
// mach_absolute_time at one in 8 was a quarter of the model's own time. The counter lives in the command buffer where
// there is one (a thread-local is a _tlv_get_addr call per use in a dylib).
enum { HZT_EVERY = 32, HZT_EVERY_BIND = 128 };
static inline int hzt_sample_n(uint32_t *n, uint32_t every) { return (++*n & (every - 1)) == 0; }
static inline int hzt_sample(void) { return hzt_sample_n(&t_hz.sn, HZT_EVERY); }
static inline void hzt_add_scaled(int k, uint64_t t0, uint32_t every) { t_hz.t[k] += (every - 1) * hzt_since(t0); hzt_add(k, t0); }
// What an access came from, for the conflict-reason census.
enum { HZS_ATT, HZS_BUF, HZS_TEX, HZS_STEX, HZS_KEYLESS, HZS_USE, HZS_HEAP, HZS_INDEX, HZS_CLEAR, HZS_BLIT, HZS_N };
static const char *const kHzSrc[HZS_N] = { "attachment", "buffer slot", "texture slot", "storage texture", "keyless binding",
                                           "useResource", "tracked heap", "index buffer", "clear-only pass", "blit" };
// Why "all" (unknown) was set; a conflict on it is reported by this cause.
enum { HZW_NONE, HZW_NEWCB, HZW_UNTRACKED, HZW_OVERFLOW };
// Conflict reasons: [context: restart, pass begin, dispatch, blit][reason]. Reasons 0-3: unknown state by cause; 4: a
// pass writing the very attachment an earlier pass wrote (consecutive passes on one target); then (new access source,
// earlier access source, read-after-write / write-after-read / write-after-write).
enum { HZR_NEWCB, HZR_UNTRACKED, HZR_OVERFLOW, HZR_PENDALL, HZR_SAMEATT, HZR_PAIR0, HZR_N = HZR_PAIR0 + HZS_N * HZS_N * 3 };
enum { HZ_NCTX = 4 };
static _Atomic uint64_t g_hzr[HZ_NCTX][HZR_N];

typedef struct {
    VkCommandBuffer cb;
    int all, in_pass, nodep, arm_disp, arm_any, full_scan, pend_all, decl_over;
    int debt;        // a barrier the old code emitted was skipped since the last full barrier
    int all_why;     // HZW_*: why "all" was last set
    int enc_restarts;   // restarts of the current encoder's pass (a storm turns into a render pass)
    uint32_t region;    // unique id of the current region (g_hz_region): a slot committed in it is not collected again
    uint32_t n, k, nw_k, nw_n, npend, ndecl, decl_done;
    uint32_t c2c_k, nw_c2c;   // entries [c2c_k, n) are this encoder's dispatches since its last compute-to-compute barrier
    uint32_t hits_from;       // a check in progress ignores entries below this index (0 = none)
    uint32_t nidx;      // entries [0,nidx) are in the lookup chains: the rest are indexed when a check first needs them
    uint32_t rc[HZ_RC];                       // entry index + 1 of a key already in the current region (dedupe)
    uint32_t head_b[HZ_HB], head_m[HZ_HM], head_w;   // chains: by (memory, bin) or (memory, wide); by memory; wildcards
    // A head or cache slot counts only when its generation is the state's: a reset bumps a counter instead of clearing
    // 44 KB (measured: the memsets were 9% of the model's time at ~100 resets a frame).
    uint32_t gen, rcgen, gb[HZ_HB], gm[HZ_HM], grc[HZ_RC];
    uint32_t next_b[HZ_CAP], next_m[HZ_CAP];
    hz_ent e[HZ_CAP], pend[HZ_PEND], decl[HZ_DECL];
} hz_state;

static struct { VkCommandBuffer cb; hz_state *s; } g_hz_tab[HZ_TAB];
static hz_state *g_hz_pool[HZ_POOL]; static int g_hz_npool;
static pthread_mutex_t g_hz_lock = PTHREAD_MUTEX_INITIALIZER;
static _Atomic uint32_t g_hz_region = 1;   // region ids, unique across command buffers (a binding table outlives a part)
static inline uint32_t hz_new_region(void) { uint32_t r = atomic_fetch_add_explicit(&g_hz_region, 1, memory_order_relaxed) + 1; return r ? r : 1; }
#define HZ_TOMB ((VkCommandBuffer)(uintptr_t)1)

static int g_hz_m = -1;
int nvmtl_hz_mode(void)
{
    int m = g_hz_m;
    if (m < 0) {
        const char *e = getenv("NVMTL_HAZARD");
        int v = !e ? 0 : !strcmp(e, "1") ? 1 : !strcmp(e, "census") ? 2 : 0;
        nvlog("vk: hazard tracking %s (NVMTL_HAZARD=%s)", v == 1 ? "ON - encoders barrier only on a read/write overlap" :
              v == 2 ? "CENSUS - the model counts what it would skip; every barrier is still emitted" : "off", e ? e : "unset");
        m = g_hz_m = v;
    }
    return m;
}
static inline int hzm(void) { const int m = g_hz_m; return m >= 0 ? m : nvmtl_hz_mode(); }
int nvmtl_hz_heaps_all(void)
{
    static int f = -1;
    if (f < 0) { const char *e = getenv("NVMTL_HAZARD_HEAPS"); f = e && !strcmp(e, "all");
        if (hzm() > 0) nvlog("vk: hazard model %s (NVMTL_HAZARD_HEAPS=%s)", f ? "tracks every resource and reads all memory for every used heap"
                                                                           : "follows Metal: untracked resources and heaps add nothing; a tracked heap is its own memory", e ? e : "unset"); }
    return f;
}
void nvmtl_hz_shadow(int on) { t_hz.shadow = on; }

static void nvmtl_clear_colour(VkClearColorValue *cv, uint32_t fmt, const float c[4]);
static PFN_vkCmdBeginRendering pvkCmdBeginRendering;
static PFN_vkCmdEndRendering pvkCmdEndRendering;
static int nvmtl_hz_dyn_ready(void)
{
    static int r = -1;
    if (r < 0) {
        if (g_hz_dynr && g_dev && hzm() == 1) {
            pvkCmdBeginRendering = (PFN_vkCmdBeginRendering)pvkGetDeviceProcAddr(g_dev, "vkCmdBeginRendering");
            pvkCmdEndRendering = (PFN_vkCmdEndRendering)pvkGetDeviceProcAddr(g_dev, "vkCmdEndRendering");
        }
        r = hzm() == 1 ? pvkCmdBeginRendering && pvkCmdEndRendering : g_hz_dynr;   // census: what real mode would find
        nvlog("vk: hazard passes %s", r ? "begin with vkCmdBeginRendering when eligible (no render-pass dependencies)"
                                          : "keep their render passes - dynamicRendering is unavailable");
    }
    return r;
}

static inline uint32_t hz_hb(const void *mem, uint64_t bin)
{ return (uint32_t)(((((uint64_t)(uintptr_t)mem >> 4) * 0x9E3779B97F4A7C15ull) ^ (bin * 0xC2B2AE3D27D4EB4Full)) >> 52) & (HZ_HB - 1); }
static inline uint32_t hz_hm(const void *mem)
{ return (uint32_t)((((uint64_t)(uintptr_t)mem >> 4) * 0x9E3779B97F4A7C15ull) >> 54) & (HZ_HM - 1); }
static inline uint32_t hz_rch(const hz_ent *e)
{ return (uint32_t)(((((uint64_t)(uintptr_t)e->mem >> 4) ^ (e->lo >> 6) ^ (e->hi << 7)) * 0x9E3779B97F4A7C15ull) >> 56) & (HZ_RC - 1); }
static inline int hz_narrow(const hz_ent *e) { return e->hi != UINT64_MAX && e->hi - e->lo <= (1ull << HZ_BIN_SHIFT); }
static inline int hz_overlap(const hz_ent *a, const hz_ent *b)
{ return !a->mem || !b->mem || (a->mem == b->mem && a->lo < b->hi && b->lo < a->hi); }

static hz_ent hz_wild(uint32_t w) { hz_ent e = { NULL, 0, UINT64_MAX, (uint8_t)w, HZS_KEYLESS, 0 }; return e; }
// A buffer: its range of its memory when it was placed (heap, pool), else its whole memory. Sparse: anything.
static hz_ent hz_key_buffer(const nvk_buffer *b, uint32_t w)
{
    hz_ent e = hz_wild(w);
    if (!b || !b->mem || b->sparse) return e;
    e.mem = b->mem; e.src = HZS_BUF;
    if (b->placed) { const size_t len = b->alloc > b->size ? b->alloc : b->size; e.lo = b->moff; e.hi = b->moff + (len ? len : 1); }
    return e;
}
// An image: its recorded range when it was placed (hz_len), else its whole memory. No memory (imported): anything.
static hz_ent hz_key_image(const nvk_image *im, uint32_t w)
{
    hz_ent e = hz_wild(w);
    if (!im || !im->mem) return e;
    e.mem = im->mem; e.src = HZS_TEX;
    if (im->hz_len) { e.lo = im->hz_off; e.hi = im->hz_off + im->hz_len; }
    return e;
}

static void hz_index_clear(hz_state *s)
{
    s->head_w = 0; s->nidx = 0;
    if (!++s->gen) { memset(s->gb, 0, sizeof s->gb); memset(s->gm, 0, sizeof s->gm); s->gen = 1; }
    if (!++s->rcgen) { memset(s->grc, 0, sizeof s->grc); s->rcgen = 1; }
}
#define HZ_HEADB(s, h) ((s)->gb[h] == (s)->gen ? (s)->head_b[h] : 0u)
#define HZ_HEADM(s, h) ((s)->gm[h] == (s)->gen ? (s)->head_m[h] : 0u)
#define HZ_RC(s, h) ((s)->grc[h] == (s)->rcgen ? (s)->rc[h] : 0u)
static void hz_init(hz_state *s, VkCommandBuffer cb)
{
    s->region = hz_new_region();
    s->cb = cb; s->all = 1; s->all_why = HZW_NEWCB; s->debt = 0; s->enc_restarts = 0; s->in_pass = s->nodep = s->arm_disp = s->arm_any = 0; s->full_scan = 1; s->pend_all = s->decl_over = 0;
    s->n = s->k = s->nw_k = s->nw_n = s->npend = s->ndecl = s->decl_done = 0;
    s->c2c_k = s->nw_c2c = s->hits_from = 0;
    hz_index_clear(s);
}
static void hz_reset(hz_state *s)
{
    s->n = s->k = s->nw_k = s->nw_n = 0; s->all = 0; s->debt = 0; s->decl_done = 0; s->region = hz_new_region(); s->full_scan = 1; s->arm_disp = s->arm_any = 0;
    s->c2c_k = s->nw_c2c = s->hits_from = 0;
    hz_index_clear(s);
}

static hz_state *hz_find(VkCommandBuffer cb, int make)
{
    if (!cb || cb == HZ_TOMB) return NULL;
    const uint32_t h = (uint32_t)((((uint64_t)(uintptr_t)cb >> 4) * 2654435761u)) & (HZ_TAB - 1);
    hz_state *s = NULL; int spot = -1;
    pthread_mutex_lock(&g_hz_lock);
    for (uint32_t i = 0; i < HZ_TAB; i++) {
        const uint32_t j = (h + i) & (HZ_TAB - 1);
        if (g_hz_tab[j].cb == cb) { s = g_hz_tab[j].s; break; }
        if (g_hz_tab[j].cb == HZ_TOMB) { if (spot < 0) spot = (int)j; continue; }
        if (!g_hz_tab[j].cb) { if (spot < 0) spot = (int)j; break; }
    }
    if (!s && make && spot >= 0) {
        s = g_hz_npool ? g_hz_pool[--g_hz_npool] : (hz_state *)calloc(1, sizeof *s);   // zeroed once: generations start valid
        if (s) { hz_init(s, cb); g_hz_tab[spot].cb = cb; g_hz_tab[spot].s = s; }
    }
    pthread_mutex_unlock(&g_hz_lock);
    if (!s && make) HZC(HZC_NOSTATE);
    return s;
}
static inline hz_state *hz_get(VkCommandBuffer cb, int make);
static hz_state *hz_take(VkCommandBuffer cb)
{
    if (!cb || cb == HZ_TOMB) return NULL;
    const uint32_t h = (uint32_t)((((uint64_t)(uintptr_t)cb >> 4) * 2654435761u)) & (HZ_TAB - 1);
    hz_state *s = NULL;
    pthread_mutex_lock(&g_hz_lock);
    for (uint32_t i = 0; i < HZ_TAB; i++) {
        const uint32_t j = (h + i) & (HZ_TAB - 1);
        if (g_hz_tab[j].cb == cb) { s = g_hz_tab[j].s; g_hz_tab[j].cb = HZ_TOMB; g_hz_tab[j].s = NULL; break; }
        if (!g_hz_tab[j].cb) break;
    }
    pthread_mutex_unlock(&g_hz_lock);
    return s;
}
static void hz_free(hz_state *s)
{
    if (!s) return;
    s->cb = NULL;   // a thread's cached pointer to it stops matching (states are pooled, never freed: see hz_get)
    pthread_mutex_lock(&g_hz_lock);
    if (g_hz_npool < HZ_POOL) { g_hz_pool[g_hz_npool++] = s; s = NULL; }
    pthread_mutex_unlock(&g_hz_lock);
    free(s);
}

// The lookup every hot path uses: the calling thread's last command buffer without the table lock. A state is never
// freed (the pool holds as many as the table), so a cached pointer stays readable; hz_free clears its cb, so it no
// longer matches once that command buffer ended.
static inline hz_state *hz_get(VkCommandBuffer cb, int make)
{
    hz_tls *T = &t_hz; hz_state *c = (hz_state *)T->cs;
    if (cb && cb == T->ccb && c && c->cb == cb) return c;
    hz_state *s = hz_find(cb, make);
    if (s) { T->ccb = cb; T->cs = s; }
    return s;
}
// Does a touch anything in [0,upto) that it conflicts with (a read against a write, a write against anything)?
static int hz_hits(hz_state *s, const hz_ent *a, uint32_t upto, uint32_t *who)
{
    *who = UINT32_MAX;   // the earlier entry hit; UINT32_MAX = any earlier entry (the new access is keyless)
    const uint32_t from = s->hits_from;
    if (upto <= from) return 0;
    if (!a->mem) { if (a->w) return from < upto; return (from ? s->nw_n - s->nw_c2c : upto == s->k ? s->nw_k : s->nw_n) > 0; }
    for (uint32_t i = s->head_w; i; i = s->next_b[i - 1])
        if (i - 1 >= from && i - 1 < upto && (a->w || s->e[i - 1].w)) { *who = i - 1; return 1; }
    if (hz_narrow(a)) {
        const uint64_t b0 = a->lo >> HZ_BIN_SHIFT, b1 = (a->hi - 1) >> HZ_BIN_SHIFT;
        for (uint64_t b = b0 ? b0 - 1 : 0; b <= b1; b++)
            for (uint32_t i = HZ_HEADB(s, hz_hb(a->mem, b)); i; i = s->next_b[i - 1]) {
                const hz_ent *e = &s->e[i - 1];
                if (i - 1 >= from && i - 1 < upto && (a->w || e->w) && hz_overlap(a, e)) { *who = i - 1; return 1; }
            }
        for (uint32_t i = HZ_HEADB(s, hz_hb(a->mem, UINT64_MAX)); i; i = s->next_b[i - 1]) {
            const hz_ent *e = &s->e[i - 1];
            if (i - 1 >= from && i - 1 < upto && (a->w || e->w) && hz_overlap(a, e)) { *who = i - 1; return 1; }
        }
        return 0;
    }
    for (uint32_t i = HZ_HEADM(s, hz_hm(a->mem)); i; i = s->next_m[i - 1]) {
        const hz_ent *e = &s->e[i - 1];
        if (i - 1 >= from && i - 1 < upto && (a->w || e->w) && hz_overlap(a, e)) { *who = i - 1; return 1; }
    }
    return 0;
}
// Appending only records the entry (and the current-region dedupe cache). The lookup chains are built by hz_index_to
// when a check first reads past nidx: draws and dispatches check against earlier encoders only, so an encoder's own
// entries are chained once, when the next encoder first checks, not on every draw that adds one.
static void hz_append(hz_state *s, const hz_ent *a)
{
    if (s->n == HZ_CAP) { s->all = 1; s->all_why = HZW_OVERFLOW; return; }
    const uint32_t i = s->n++;
    s->e[i] = *a;
    if (a->w) s->nw_n++;
    if (a->mem) { const uint32_t rh = hz_rch(a); s->rc[rh] = i + 1; s->grc[rh] = s->rcgen; }
}
static void hz_index_to(hz_state *s, uint32_t upto)
{
    for (uint32_t i = s->nidx; i < upto; i++) {
        const hz_ent *a = &s->e[i];
        if (!a->mem) { s->next_b[i] = s->head_w; s->head_w = i + 1; s->next_m[i] = 0; continue; }
        const uint32_t hb = hz_hb(a->mem, hz_narrow(a) ? a->lo >> HZ_BIN_SHIFT : UINT64_MAX), hm = hz_hm(a->mem);
        s->next_b[i] = HZ_HEADB(s, hb); s->head_b[hb] = i + 1; s->gb[hb] = s->gen;
        s->next_m[i] = HZ_HEADM(s, hm); s->head_m[hm] = i + 1; s->gm[hm] = s->gen;
    }
    if (upto > s->nidx) s->nidx = upto;
}
static void hz_pend(hz_state *s, hz_ent e)
{
    if (e.mem) {
        const uint32_t i = HZ_RC(s, hz_rch(&e));
        if (i && i - 1 >= s->k && i - 1 < s->n) {
            const hz_ent *o = &s->e[i - 1];
            if (o->mem == e.mem && o->lo == e.lo && o->hi == e.hi && o->w >= e.w) return;   // already in this encoder
        }
    }
    if (s->npend == HZ_PEND) { s->pend_all = 1; return; }
    s->pend[s->npend++] = e;
}
// ctx: 0 a restart, 1 a pass begin, 2 a dispatch, 3 a blit; the reason of a conflict is counted there. Checked
// against entries [0,upto): earlier encoders for draws and dispatches, everything for a blit.
static int hz_conflict_upto(hz_state *s, int ctx, uint32_t upto)
{
    int r = -1;
    if (s->npend && upto > s->nidx) hz_index_to(s, upto);
    if (s->all) r = s->all_why == HZW_NEWCB ? HZR_NEWCB : s->all_why == HZW_UNTRACKED ? HZR_UNTRACKED : HZR_OVERFLOW;
    else if (s->pend_all && upto) r = HZR_PENDALL;
    else for (uint32_t j = 0; j < s->npend && r < 0; j++) {
        uint32_t who;
        if (!hz_hits(s, &s->pend[j], upto, &who)) continue;
        const hz_ent *a = &s->pend[j];
        const uint32_t es = who == UINT32_MAX ? HZS_KEYLESS : s->e[who].src, ew = who == UINT32_MAX ? 1u : s->e[who].w;
        const int type = !a->w ? 0 : !ew ? 1 : 2;   // read-after-write, write-after-read, write-after-write
        if (who != UINT32_MAX && a->src == HZS_ATT && es == HZS_ATT && a->mem == s->e[who].mem && a->lo == s->e[who].lo && a->hi == s->e[who].hi)
            r = HZR_SAMEATT;
        else r = HZR_PAIR0 + ((int)(a->src % HZS_N) * HZS_N + (int)(es % HZS_N)) * 3 + type;
    }
    if (r < 0) return 0;
    atomic_fetch_add_explicit(&g_hzr[ctx][r], 1, memory_order_relaxed);
    return 1;
}
static int hz_conflict(hz_state *s, int ctx) { return hz_conflict_upto(s, ctx, s->k); }
// The pending dispatch against this encoder's own entries since its last compute-to-compute barrier: a read of what an
// earlier dispatch of the encoder wrote, or a write over what one read or wrote, needs that barrier; anything else does not.
static int hz_conflict_intra(hz_state *s)
{
    s->hits_from = s->c2c_k;
    const int r = hz_conflict_upto(s, 2, s->n);
    s->hits_from = 0;
    return r;
}
// A compute-to-compute barrier was recorded: the encoder's entries so far are ordered for its later dispatches.
static void hz_c2c_done(hz_state *s) { s->c2c_k = s->n; s->nw_c2c = s->nw_n; }
static void hz_commit(hz_state *s)
{
    for (uint32_t j = 0; j < s->npend; j++) hz_append(s, &s->pend[j]);
    if (s->pend_all) { s->all = 1; s->all_why = HZW_OVERFLOW; }
    s->npend = 0; s->pend_all = 0; s->decl_done = s->ndecl;
}
static void hz_close_region(hz_state *s)
{
    s->k = s->n; s->nw_k = s->nw_n; s->c2c_k = s->n; s->nw_c2c = s->nw_n; s->region = hz_new_region();
    if (!++s->rcgen) { memset(s->grc, 0, sizeof s->grc); s->rcgen = 1; }
}

// The resource the next binding-table write belongs to, carried by the command buffer (recorded by one thread at a
// time): a pointer and a kind, set only around one bind call, so another path's write gets none. Its key is built only
// when nvmtl_bind_slot sees the handle change (a rebind of the same handle is the same resource, the same key).
static inline void nvmtl_hz_pend_res(nvk_cmdbuf *c, const void *res, uint8_t kind, uint8_t skip)
{ c->hzp_res = res; c->hzp_kind = kind; c->hzp_skip = skip; c->hzp_on = 1; }
static inline void nvmtl_hz_pend_clear(nvk_cmdbuf *c) { c->hzp_on = 0; c->hzp_skip = 0; }
// nvmtl_bind_slot calls this only when the slot's handle or kind changed.
static inline void hz_slot_bound_impl(nvk_cmdbuf *c, nvmtl_bindtable *t, uint32_t set, uint32_t binding, uint32_t kind, const void *h)
{
    if (set > 1 || binding >= NVMTL_NBIND) return;
    hz_ent e = hz_wild(0); const void *owner = NULL;
    if (kind != NVMTL_SLOT_NONE && kind != NVMTL_SLOT_SAMP && h) {
        owner = h;
        if (c->hzp_on) {
            e = c->hzp_kind == 1 ? hz_key_buffer((const nvk_buffer *)c->hzp_res, 0) : c->hzp_kind == 2 ? hz_key_image((const nvk_image *)c->hzp_res, 0) : e;
            e.skip = c->hzp_skip;
        }
    }
    hz_ent *k = &t->hk[set][binding];
    if (t->hkh[set][binding] == owner && k->mem == e.mem && k->lo == e.lo && k->hi == e.hi && k->skip == e.skip) return;
    *k = e; t->hkh[set][binding] = owner; t->hzseen[set][binding] = 0;
    if (e.skip) HZC(HZC_UNTRACKED_BIND);
    const uint64_t bit = 1ull << (binding & 63);
    if (owner && !e.skip) t->hzbound[set][binding >> 6] |= bit; else t->hzbound[set][binding >> 6] &= ~bit;   // untracked: never collected
    t->hzdirty[set][binding >> 6] |= bit; t->hzany = 1;
    if (owner && !e.skip) c->hz_draw_due = 1;   // a tracked resource arrived: the next draw's check has something to look at
}
// Called by nvmtl_bind_slot when a slot's handle changes: timed on one call in 128, scaled.
static void nvmtl_hz_slot_bound(nvk_cmdbuf *c, nvmtl_bindtable *t, uint32_t set, uint32_t binding, uint32_t kind, const void *h)
{
    if (hzm() <= 0) return;
    if (!hzt_sample_n(&c->hz_sn, HZT_EVERY_BIND)) { hz_slot_bound_impl(c, t, set, binding, kind, h); return; }
    const uint64_t t0 = hzt_now(); hz_slot_bound_impl(c, t, set, binding, kind, h); hzt_add_scaled(HZT_BIND, t0, HZT_EVERY_BIND);
}
// untracked: Metal does not order this resource between encoders (its hazardTrackingMode, or CPU-written constants
// such as the setBytes ring that no GPU command writes); the slot adds nothing to the model.
int nvmtl_hz_bind_texture(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view)
{ nvmtl_hz_pend_res(c, im, 2, untracked != 0); int r = nvmtl_vk_bind_texture_view(c, set, index, view); nvmtl_hz_pend_clear(c); return r; }
int nvmtl_hz_bind_storage(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view)
{ nvmtl_hz_pend_res(c, im, 2, untracked != 0); int r = nvmtl_vk_bind_storage_view(c, set, index, view); nvmtl_hz_pend_clear(c); return r; }
int nvmtl_hz_bind_texel(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view, int storage)
{ nvmtl_hz_pend_res(c, im, 2, untracked != 0); int r = nvmtl_vk_bind_texel_view(c, set, index, view, storage); nvmtl_hz_pend_clear(c); return r; }
// nvmtl_vk_bind_buffer itself names the buffer (any caller); this only says whether it is untracked.
int nvmtl_hz_bind_buffer(nvk_cmdbuf *c, int untracked, uint32_t set, uint32_t index, nvk_buffer *b, size_t offset)
{ c->hzp_bskip = untracked != 0; int r = nvmtl_vk_bind_buffer(c, set, index, b, offset); c->hzp_bskip = 0; return r; }

// Every bound buffer, texture and storage texture of the sets in setmask (dirty ones only, unless full). Buffers are
// written where the stage's reflection does not prove them read-only (any buffer can also be reached through the
// address table, so none is skipped); sampled textures are reads, skipped where the pipeline's SPIR-V never names the
// binding; storage textures are written where the reflection says the function writes that texture.
static void hz_collect_slots(nvk_cmdbuf *c, hz_state *s, uint32_t setmask, int full)
{
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    if (!t) return;
    if (setmask == 3u) t->hzany = 0;   // both sets scanned: no dirty bit is left
    for (uint32_t set = 0; set < 2; set++) {
        if (!(setmask & (1u << set))) { if (full) memset(t->hzdirty[set], 0, sizeof t->hzdirty[set]); continue; }
        for (uint32_t w = 0; w < (NVMTL_NBIND + 63) / 64; w++) {
            uint64_t bits = (full ? ~0ull : t->hzdirty[set][w]) & t->hzbound[set][w];   // only slots with a tracked resource
            t->hzdirty[set][w] = 0;
            while (bits) {
                const uint32_t i = w * 64 + (uint32_t)__builtin_ctzll(bits); bits &= bits - 1;
                if (i >= NVMTL_NBIND) break;
                const nvmtl_slot *sl = &t->s[set][i];
                if (sl->kind == NVMTL_SLOT_NONE || sl->kind == NVMTL_SLOT_SAMP || !sl->h) continue;
                if (i >= NVMTL_NBUF + NVMTL_NTEX && i < NVMTL_STEX_BASE) continue;   // samplers, the address table
                hz_ent e;
                e = t->hkh[set][i] == sl->h ? t->hk[set][i] : hz_wild(0);   // bound by a path that gave no key: anything
                if (e.skip) continue;   // hzbound already excludes these; a stale bit is harmless
                if (!e.mem) HZC(HZC_WILD);
                if (i < NVMTL_NBUF) { e.w = (c->hz_bufw[set] >> i) & 1u; if (e.mem) e.src = HZS_BUF; }
                else if (i < NVMTL_NBUF + NVMTL_NTEX) {
                    const uint32_t b = i;
                    if (c->hz_tmask_ok[set] && b < 256 && !(c->hz_tmask[set][b >> 6] & (1ull << (b & 63)))) continue;
                    e.w = 0; if (e.mem) e.src = HZS_TEX;
                } else { const uint32_t ti = i - NVMTL_STEX_BASE; e.w = ti < 128 ? (uint8_t)((c->hz_texw[set][ti >> 6] >> (ti & 63)) & 1u) : 1u; if (e.mem) e.src = HZS_STEX; }
                // seen in this region with at least this access: nothing to check or add (no hashing on rebinding)
                const uint32_t seen = t->hzseen[set][i], mark = (s->region << 1) | e.w;
                if (seen == mark || seen == ((s->region << 1) | 1u)) continue;
                if (e.mem) t->hzseen[set][i] = mark;   // a conflict resets the region, so an uncommitted mark expires with it
                hz_pend(s, e);
            }
        }
    }
}
static void hz_collect_decls(hz_state *s)
{
    for (uint32_t i = s->decl_done; i < s->ndecl; i++) hz_pend(s, s->decl[i]);
    if (s->decl_over) s->pend_all = 1;
}
static void hz_declare_impl(nvk_cmdbuf *c, hz_ent e, uint8_t src)
{
    if (hzm() <= 0 || !c || !c->cb) return;
    hz_state *s = hz_get(c->cb, 1);
    if (!s) return;
    e.src = src;
    if (e.mem) {   // already in this encoder (apps repeat useResource for every draw): nothing new
        const uint32_t i = HZ_RC(s, hz_rch(&e));
        if (i && i - 1 >= s->k && i - 1 < s->n && s->e[i - 1].mem == e.mem && s->e[i - 1].lo == e.lo && s->e[i - 1].hi == e.hi && s->e[i - 1].w >= e.w) return;
        if (s->ndecl && s->decl[s->ndecl - 1].mem == e.mem && s->decl[s->ndecl - 1].lo == e.lo && s->decl[s->ndecl - 1].hi == e.hi && s->decl[s->ndecl - 1].w >= e.w) return;
    }
    if (s->ndecl == HZ_DECL) { s->decl_over = 1; c->hz_draw_due = 1; return; }
    s->decl[s->ndecl++] = e;
    c->hz_draw_due = 1;
}
static void hz_declare(nvk_cmdbuf *c, hz_ent e, uint8_t src)
{ if (hzm() <= 0) return; if (!hzt_sample_n(&c->hz_sn, HZT_EVERY)) { hz_declare_impl(c, e, src); return; } const uint64_t t0 = hzt_now(); hz_declare_impl(c, e, src); hzt_add_scaled(HZT_DECLARE, t0, HZT_EVERY); }
void nvmtl_hz_declare_buffer(nvk_cmdbuf *c, const nvk_buffer *b, int w, int index)
{ hz_declare(c, hz_key_buffer(b, w ? 1u : 0u), index ? HZS_INDEX : HZS_USE); }
void nvmtl_hz_declare_image(nvk_cmdbuf *c, const nvk_image *im, int w) { hz_declare(c, hz_key_image(im, w ? 1u : 0u), HZS_USE); }
void nvmtl_hz_declare_any(nvk_cmdbuf *c, int w) { hz_declare(c, hz_wild(w ? 1u : 0u), HZS_KEYLESS); }
// useHeap: of a heap. untracked: nothing (Metal semantics, header comment). Tracked: a read of the heap's memory, or of
// anything when a member fell back outside it (legacy) or the heap has no memory of its own. HEAPS=all: anything.
void nvmtl_hz_use_heap(nvk_cmdbuf *c, int tracked, void *mem, size_t size, int legacy)
{
    if (hzm() <= 0) return;
    const int all = nvmtl_hz_heaps_all();
    if (!tracked && !all) { HZC(HZC_HEAP_UNTRACKED); return; }
    if (tracked) HZC(HZC_HEAP_TRACKED);
    if (all || legacy || !mem) { HZC(HZC_HEAP_ALL); hz_declare(c, hz_wild(0), HZS_HEAP); return; }
    hz_ent e = { mem, 0, size ? size : UINT64_MAX, 0, HZS_HEAP, 0 };
    hz_declare(c, e, HZS_HEAP);
}

// A full barrier: real mode emits it; census only records that the model would have.
static void hz_barrier(nvk_cmdbuf *c, hz_state *s)
{
    if (hzm() == 1) nvmtl_vk_cmd_barrier(c);   // nvmtl_hz_full_barrier resets the model
    else hz_reset(s);
}
// Untracked work, or the end of the command buffer, with skipped barriers outstanding: pay one.
static void hz_pay(VkCommandBuffer cb, hz_state *s)
{
    if (hzm() == 1) {
        VkMemoryBarrier mb = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT, nvmtl_dst_reads() | VK_ACCESS_MEMORY_WRITE_BIT };
        pvkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    }
    hz_reset(s);
}

// Called by nvmtl_vk_cmd_barrier: everything before a full memory barrier outside a pass is ordered.
// t_hz.blit_cb: a tracked blit is being recorded into this command buffer; t_hz.blit_pre: it has not recorded its first
// command yet (real mode).
static void nvmtl_hz_full_barrier(nvk_cmdbuf *c)
{
    if (hzm() <= 0 || t_hz.shadow || !c) return;
    if (t_hz.blit_pre && c->cb == t_hz.blit_cb) return;   // a blit's pre-barrier, emitted without its source access
    hz_state *s = hz_get(c->cb, 0);
    if (s && !s->in_pass) hz_reset(s);
}
// The command wrappers: any recorded work that no tracked command armed is untracked.
static void hz_work_impl(VkCommandBuffer cb, int dispatch)
{
    if (hzm() <= 0) return;
    if (t_hz.blit_cb && t_hz.blit_cb == cb) { t_hz.blit_pre = 0; return; }   // a tracked blit's own commands
    hz_state *s = hz_get(cb, 0);
    if (!s || s->in_pass) return;   // in a pass: draws are checked by nvmtl_hz_draw; a render pass orders itself
    if (s->arm_any) { s->arm_any = 0; return; }
    if (dispatch && s->arm_disp) { s->arm_disp = 0; return; }
    s->arm_disp = 0;
    if (s->n || s->debt) { HZC(HZC_UNTRACKED); hz_pay(cb, s); }
    s->all = 1; s->all_why = HZW_UNTRACKED;
}
static void nvmtl_hz_work(VkCommandBuffer cb, int dispatch)
{ if (hzm() <= 0) return; if (!hzt_sample()) { hz_work_impl(cb, dispatch); return; } const uint64_t t0 = hzt_now(); hz_work_impl(cb, dispatch); hzt_add_scaled(HZT_WORK, t0, HZT_EVERY); }
static void nvmtl_hz_end(VkCommandBuffer cb)
{
    if (hzm() <= 0) return;
    hz_state *s = hz_take(cb);
    if (!s) return;
    if ((s->n || s->debt) && !s->in_pass) { HZC(HZC_END); hz_pay(cb, s); }
    hz_free(s);
}
static void nvmtl_hz_forget(VkCommandBuffer cb) { if (hzm() > 0) hz_free(hz_take(cb)); }

// Draws are not wrapped: Vulkan records a draw only inside a pass, where the wrapper returned at once (an old render
// pass orders itself; a pass begun without a barrier checks its draws in nvmtl_hz_draw). That was ~2700 calls a frame.
#define pvkCmdDispatch(cb, ...) (nvmtl_hz_work((cb), 1), pvkCmdDispatch((cb), __VA_ARGS__))
#define pvkCmdDispatchIndirect(cb, ...) (nvmtl_hz_work((cb), 1), pvkCmdDispatchIndirect((cb), __VA_ARGS__))
#define pvkCmdCopyBuffer(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyBuffer((cb), __VA_ARGS__))
#define pvkCmdCopyImage(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyImage((cb), __VA_ARGS__))
#define pvkCmdCopyBufferToImage(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyBufferToImage((cb), __VA_ARGS__))
#define pvkCmdCopyImageToBuffer(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyImageToBuffer((cb), __VA_ARGS__))
#define pvkCmdBlitImage(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdBlitImage((cb), __VA_ARGS__))
#define pvkCmdClearColorImage(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdClearColorImage((cb), __VA_ARGS__))
#define pvkCmdClearDepthStencilImage(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdClearDepthStencilImage((cb), __VA_ARGS__))
#define pvkCmdFillBuffer(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdFillBuffer((cb), __VA_ARGS__))
#define pvkCmdCopyQueryPoolResults(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyQueryPoolResults((cb), __VA_ARGS__))
#define pvkCmdBuildAccelerationStructuresKHR(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdBuildAccelerationStructuresKHR((cb), __VA_ARGS__))
#define pvkCmdCopyAccelerationStructureKHR(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdCopyAccelerationStructureKHR((cb), __VA_ARGS__))
#define pvkCmdBeginRenderPass(cb, ...) (nvmtl_hz_work((cb), 0), pvkCmdBeginRenderPass((cb), __VA_ARGS__))
#define pvkEndCommandBuffer(cb) (nvmtl_hz_end(cb), pvkEndCommandBuffer(cb))
#define pvkBeginCommandBuffer(cb, ...) (nvmtl_hz_forget(cb), pvkBeginCommandBuffer((cb), __VA_ARGS__))

// The image has no access recorded since the last full barrier and nothing unknown happened: a layout change on it
// orders nothing, so its barrier needs no source access (no wait on NVK; layouts are not acted on there).
static VkAccessFlags nvmtl_hz_clean_src(nvk_cmdbuf *c, const nvk_image *img, VkAccessFlags srcA)
{
    const int m = hzm();
    if (m <= 0 || !srcA || !c || !c->cb) return srcA;
    hz_state *s = hz_get(c->cb, 0);
    if (!s || s->all || s->in_pass) return srcA;
    const hz_ent e = hz_key_image(img, 1);
    uint32_t who;
    if (e.mem) hz_index_to(s, s->n);
    if (!e.mem || hz_hits(s, &e, s->n, &who)) return srcA;
    HZC(HZC_CLEAN);
    return m == 1 ? 0 : srcA;
}

// Pass begin. Eligible passes (one sample, one layer, no framebuffer-fetch pipeline, no forced render pass) begin with
// vkCmdBeginRendering in real mode, behind a barrier only when the model finds a conflict; the rest keep the render
// pass, whose begin dependency orders everything before it (the model resets there and again at its end).
static void hz_collect_pass(nvk_cmdbuf *c, hz_state *s, const nvk_pass *ps, const nvk_pipeline *p, uint32_t ncol, const uint32_t *cf)
{
    s->npend = 0; s->pend_all = 0;
    // an untracked attachment (c->hz_att_skip: bit i colour, bit 8 depth) adds nothing, as in Metal
    for (uint32_t i = 0; i < ncol; i++) if (cf[i] && ps->col[i] && !(c->hz_att_skip & (1u << i))) { hz_ent e = hz_key_image(ps->col[i], 1); e.src = HZS_ATT; hz_pend(s, e); }
    if (p->has_depth && ps->ds && !(c->hz_att_skip & (1u << 8))) { hz_ent e = hz_key_image(ps->ds, 1); e.src = HZS_ATT; hz_pend(s, e); }
    const int full = s->full_scan || c->hz_wm_dirty;
    hz_collect_slots(c, s, 3u, full); s->full_scan = 0; c->hz_wm_dirty = 0;
    hz_collect_decls(s);
}
static int hz_pass_decide_impl(nvk_cmdbuf *c, hz_state *s, const nvk_pass *ps, const nvk_pipeline *p, uint32_t ncol, const uint32_t *cf, int elig)
{
    HZC(HZC_PASS);
    if (!elig) { HZC(HZC_PASS_RP); s->npend = 0; return 0; }
    hz_collect_pass(c, s, ps, p, ncol, cf);
    if (hz_conflict(s, 1)) { HZC(HZC_PASS_BAR); hz_barrier(c, s); hz_collect_pass(c, s, ps, p, ncol, cf); }
    else HZC(HZC_PASS_FREE);
    return 1;
}
static void hz_pass_enter_impl(hz_state *s, int nodep)
{
    if (!nodep) { hz_reset(s); s->in_pass = 1; s->nodep = 0; return; }
    hz_commit(s); s->in_pass = 1; s->nodep = 1; s->debt = 1;   // the render pass's two dependencies were skipped
}
static int hz_pass_decide(nvk_cmdbuf *c, hz_state *s, const nvk_pass *ps, const nvk_pipeline *p, uint32_t ncol, const uint32_t *cf, int elig)
{ const uint64_t t0 = hzt_now(); const int r = hz_pass_decide_impl(c, s, ps, p, ncol, cf, elig); hzt_add(HZT_PASS, t0); return r; }
static void hz_pass_enter(nvk_cmdbuf *c, hz_state *s, int nodep)
{ const uint64_t t0 = hzt_now(); hz_pass_enter_impl(s, nodep); c->hz_draw_due = s->all; hzt_add(HZT_PASS, t0); }
static void nvmtl_hz_pass_exit(nvk_cmdbuf *c)
{
    if (hzm() <= 0 || !c->cb) return;
    hz_state *s = hz_get(c->cb, 0);
    if (!s || !s->in_pass) return;
    s->in_pass = 0;
    if (!s->nodep) hz_reset(s);   // the render pass's end dependency ordered everything in it
    s->nodep = 0;
}
static void nvmtl_hz_begin_rendering(nvk_cmdbuf *c, const nvk_pass *ps, const nvk_pipeline *p, uint32_t ncol, const uint32_t *cf)
{
    VkRenderingAttachmentInfo col[NVMTL_NCOL]; memset(col, 0, sizeof col);
    const VkImageLayout cl = nvmtl_fbfetch_on() && ncol ? VK_IMAGE_LAYOUT_GENERAL : nvmtl_image_access_layout(VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL);
    for (uint32_t i = 0; i < ncol; i++) {
        col[i].sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO;
        if (!cf[i]) continue;   // no view: the slot is unused, as VK_ATTACHMENT_UNUSED was in the render pass
        col[i].imageView = (VkImageView)ps->colview[i]; col[i].imageLayout = cl;
        col[i].loadOp = (ps->load & NVMTL_LOAD_COLOUR(i)) ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR;
        col[i].storeOp = VK_ATTACHMENT_STORE_OP_STORE;
        nvmtl_clear_colour(&col[i].clearValue.color, cf[i], ps->clear[i]);
    }
    VkRenderingAttachmentInfo dep = { VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO }, sten = dep;
    VkRenderingInfo ri = { VK_STRUCTURE_TYPE_RENDERING_INFO, NULL, 0, { { 0, 0 }, { ps->w, ps->h } }, 1, 0, ncol, ncol ? col : NULL, NULL, NULL };
    if (p->has_depth) {
        const uint32_t df = p->dfmt ? p->dfmt : VK_FORMAT_D32_SFLOAT;
        const int has_s = df == VK_FORMAT_D16_UNORM_S8_UINT || df == VK_FORMAT_D24_UNORM_S8_UINT || df == VK_FORMAT_D32_SFLOAT_S8_UINT || df == VK_FORMAT_S8_UINT;
        const VkImageLayout dl = nvmtl_image_access_layout(VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        dep.imageView = (VkImageView)ps->dsview; dep.imageLayout = dl; dep.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
        dep.loadOp = (ps->load & NVMTL_LOAD_DEPTH) ? VK_ATTACHMENT_LOAD_OP_LOAD : VK_ATTACHMENT_LOAD_OP_CLEAR;
        dep.clearValue.depthStencil.depth = ps->clear_depth; dep.clearValue.depthStencil.stencil = ps->clear_stencil;
        if (df != VK_FORMAT_S8_UINT) ri.pDepthAttachment = &dep;
        if (has_s) {
            // a pipeline without stencil leaves it as it was (the render pass did not store it; loading is the superset)
            sten = dep;
            sten.loadOp = p->has_stencil && !(ps->load & NVMTL_LOAD_STENCIL) ? VK_ATTACHMENT_LOAD_OP_CLEAR : VK_ATTACHMENT_LOAD_OP_LOAD;
            ri.pStencilAttachment = &sten;
        }
    }
    pvkCmdBeginRendering(c->cb, &ri);
    c->hz_dyn = 1;
}

// A draw in a pass begun without a barrier: its new bindings against earlier encoders. Real mode returns 1 when the
// encoder must restart the pass behind a full barrier; census counts that, resets the model and returns 0.
static int hz_draw_impl(nvk_cmdbuf *c)
{
    const int m = hzm();
    if (m <= 0 || !c || !c->cb) return 0;
    hz_state *s = hz_get(c->cb, 0);
    if (!s || !s->in_pass || !s->nodep) return 0;
    c->hz_draw_due = 0;
    const nvmtl_bindtable *tb = (const nvmtl_bindtable *)c->table;
    // the common draw: no binding changed, no pipeline-mask change, no new useResource, nothing unknown - nothing to do
    if (!s->full_scan && !c->hz_wm_dirty && (!tb || !tb->hzany) && s->decl_done == s->ndecl && !s->decl_over && !s->all) return 0;
    const int full = s->full_scan || c->hz_wm_dirty; s->full_scan = 0; c->hz_wm_dirty = 0;
    s->npend = 0; s->pend_all = 0;
    hz_collect_slots(c, s, 3u, full); hz_collect_decls(s);
    if (!s->npend && !s->pend_all && !s->all) return 0;
    if (hz_conflict(s, 0)) {
        HZC(HZC_RESTART);
        if (++s->enc_restarts > 2) {   // a storm: this encoder keeps a render pass from here on (real: the caller forces it)
            HZC(HZC_STORM);
            if (m == 1) { s->npend = 0; s->pend_all = 0; s->full_scan = 1; return 2; }
            hz_reset(s); s->nodep = 0; return 0;
        }
        if (m == 1) { s->npend = 0; s->pend_all = 0; s->full_scan = 1; return 1; }
        hz_reset(s); s->npend = 0; s->pend_all = 0; hz_collect_slots(c, s, 3u, 1); s->full_scan = 0; hz_collect_decls(s);
        s->debt = 1;
    }
    hz_commit(s);
    if (s->all) c->hz_draw_due = 1;   // an overflow: the next draw must see it
    return 0;
}
// Census-mode twin of a forced restart (an indirect, patch or ICB draw, a framebuffer-fetch pipeline, a fence wait in
// a pass begun without a barrier): the model takes the barrier real mode would emit, and the pass orders itself after.
void nvmtl_hz_force_full(nvk_cmdbuf *c, int keep_nodep)
{
    if (hzm() <= 0 || !c || !c->cb) return;
    hz_state *s = hz_get(c->cb, 0);
    if (!s || !s->in_pass || !s->nodep) return;
    HZC(HZC_FORCED); hz_reset(s); s->nodep = keep_nodep ? 1 : 0;   // keep: real mode restarts without a render pass
    if (keep_nodep) s->debt = 1;
}
int nvmtl_hz_nodep(nvk_cmdbuf *c)
{
    if (hzm() <= 0 || !c || !c->cb) return 0;
    hz_state *s = hz_get(c->cb, 0);
    return s && s->in_pass && s->nodep;
}
// A compute dispatch. Real mode emits the full barrier itself when the model finds a conflict; returns that verdict
// (-1 when the model is off). The caller emits nothing for a first dispatch without a conflict.
static int hz_dispatch_impl(nvk_cmdbuf *c, int first)
{
    const int m = hzm();
    if (m <= 0 || !c || !c->cb) return -1;
    hz_state *s = hz_get(c->cb, 1);
    if (!s) { if (m == 1) nvmtl_vk_cmd_barrier(c); return 1; }
    if (s->in_pass) return -1;
    const int full = s->full_scan || c->hz_wm_dirty; s->full_scan = 0; c->hz_wm_dirty = 0;
    s->npend = 0; s->pend_all = 0;
    hz_collect_slots(c, s, 1u, full); hz_collect_decls(s);
    const int v = hz_conflict(s, 2);
    if (first) { HZC(HZC_DISP_FIRST); if (!v) { HZC(HZC_DISP_FREE); s->debt = 1; } }
    else if (v) HZC(HZC_DISP_BAR);
    if (v) { hz_barrier(c, s); s->npend = 0; s->pend_all = 0; hz_collect_slots(c, s, 1u, 1); s->full_scan = 0; hz_collect_decls(s); }
    else if (!first && nvmtl_hz_dispatch_decides()) {
        // NVMTL_HAZARD_DISPATCH: the caller left the compute-to-compute barrier to the model
        if (hz_conflict_intra(s)) { HZC(HZC_DISP_C2C); if (m == 1) nvmtl_vk_cmd_barrier_c2c(c); hz_c2c_done(s); }
        else { HZC(HZC_DISP_C2C_FREE); s->debt = 1; }
    }
    hz_commit(s);
    s->arm_disp = 1;
    return v;
}
void nvmtl_hz_encoder_end(nvk_cmdbuf *c)
{
    if (hzm() <= 0 || !c || !c->cb) return;
    hz_state *s = hz_get(c->cb, 0);
    if (!s) return;
    hz_close_region(s);
    s->ndecl = s->decl_done = 0; s->decl_over = 0; s->full_scan = 1; s->arm_disp = s->arm_any = 0; s->enc_restarts = 0;
}
int nvmtl_hz_draw(nvk_cmdbuf *c)
{ if (hzm() <= 0) return 0; if (!hzt_sample_n(&c->hz_sn, HZT_EVERY)) return hz_draw_impl(c); const uint64_t t0 = hzt_now(); const int r = hz_draw_impl(c); hzt_add_scaled(HZT_DRAW, t0, HZT_EVERY); return r; }
int nvmtl_hz_dispatch(nvk_cmdbuf *c, int first)
{ if (hzm() <= 0) return -1; const uint64_t t0 = hzt_now(); const int r = hz_dispatch_impl(c, first); hzt_add(HZT_DISPATCH, t0); return r; }
// The commonest conflict reasons since the last call, per frame, for the census line.
static const char *const kHzCtx[HZ_NCTX] = { "restart", "pass barrier", "dispatch barrier", "blit barrier" };
static void hz_reason_text(int r, char *what, size_t cap)
{
    static const char *const alln[5] = { "unknown: new command buffer", "unknown: untracked work", "unknown: table overflow",
                                         "unknown: too many accesses", "attachment written again by the next pass (same target)" };
    if (r < HZR_PAIR0) { snprintf(what, cap, "%s", alln[r]); return; }
    const int q = r - HZR_PAIR0, type = q % 3, es = (q / 3) % HZS_N, as = q / 3 / HZS_N;
    snprintf(what, cap, "%s %s after %s %s", kHzSrc[as], type ? "write" : "read", kHzSrc[es], type == 1 ? "read" : "write");
}
// Every context's top ten reasons since the last call, one log line each (the census line has no room for them).
void nvmtl_vk_hz_reasons_log(double frames)
{
    static uint64_t last[HZ_NCTX][HZR_N];
    extern const char *nvmtl_log_path(void);
    FILE *f = fopen(nvmtl_log_path(), "a");
    for (int ctx = 0; ctx < HZ_NCTX; ctx++) {
        uint64_t d[HZR_N], total = 0;
        for (int r = 0; r < HZR_N; r++) { const uint64_t v = atomic_load(&g_hzr[ctx][r]); d[r] = v - last[ctx][r]; last[ctx][r] = v; total += d[r]; }
        if (!f || !total) continue;
        fprintf(f, "pid %d NVMTL: hazard reasons, %s: %.1f per frame in all\n", getpid(), kHzCtx[ctx], frames > 0 ? (double)total / frames : 0.0);
        for (int top = 0; top < 10; top++) {
            int best = -1;
            for (int r = 0; r < HZR_N; r++) if (d[r] && (best < 0 || d[r] > d[best])) best = r;
            if (best < 0) break;
            char what[160]; hz_reason_text(best, what, sizeof what);
            fprintf(f, "pid %d NVMTL: hazard reason, %s #%d: %.2f per frame - %s\n", getpid(), kHzCtx[ctx], top + 1, frames > 0 ? (double)d[best] / frames : 0.0, what);
            d[best] = 0;
        }
    }
    if (f) fclose(f);
}
// The same, as one string (selftest).
void nvmtl_vk_hz_reasons(double frames, char *out, size_t cap)
{
    static uint64_t last[HZ_NCTX][HZR_N];
    size_t n = 0; out[0] = 0;
    for (int ctx = 0; ctx < HZ_NCTX; ctx++) {
        uint64_t d[HZR_N];
        for (int r = 0; r < HZR_N; r++) { const uint64_t v = atomic_load(&g_hzr[ctx][r]); d[r] = v - last[ctx][r]; last[ctx][r] = v; }
        for (int top = 0; top < 5; top++) {
            int best = -1;
            for (int r = 0; r < HZR_N; r++) if (d[r] && (best < 0 || d[r] > d[best])) best = r;
            if (best < 0 || n + 200 > cap) break;
            char what[160];
            if (best < HZR_PAIR0) hz_reason_text(best, what, sizeof what);
            else hz_reason_text(best, what, sizeof what);
            const int k = snprintf(out + n, cap - n, "%s%s: %s %.1f", n ? "; " : "", kHzCtx[ctx], what, frames > 0 ? (double)d[best] / frames : 0.0);
            if (k > 0) n += (size_t)k;
            d[best] = 0;
        }
    }
}

// Tracked blits (copies, fills, texture copies, mipmaps from the blit encoder): the blit's source and destination are
// keyed, checked against everything since the last full barrier (Metal orders blit commands by hazard too), and a full
// barrier is taken only on a conflict. In real mode the barriers the copy routine records before its first command
// (its own "everything before me" pre-barriers) then carry no source access, so NVK does not drain for them; the ones
// between and after its commands (mip chains, the result made visible) stay as they are. The blit's entries close a
// region, so the next encoder is checked against them. MetalFX/MPS kernels and acceleration-structure builds are not
// blits and stay untracked work.
static int hz_blit_begin_impl(nvk_cmdbuf *c)
{
    if (hzm() <= 0 || !c || !c->cb || t_hz.blit_cb || c->in_rp) return 0;   // nested: the outer blit keyed it
    hz_state *s = hz_get(c->cb, 1);
    if (!s) return 0;
    s->npend = 0; s->pend_all = 0;
    t_hz.blit_cb = c->cb;
    return 1;
}
void nvmtl_hz_blit_key(nvk_cmdbuf *c, const nvk_buffer *b, const nvk_image *im, int w)
{
    hz_state *s = t_hz.blit_cb == c->cb ? hz_get(c->cb, 0) : NULL;
    if (!s) return;
    hz_ent e = b ? hz_key_buffer(b, w ? 1u : 0u) : im ? hz_key_image(im, w ? 1u : 0u) : hz_wild(w ? 1u : 0u);
    e.src = HZS_BLIT;
    if (s->npend == HZ_PEND) { s->pend_all = 1; return; }
    s->pend[s->npend++] = e;
}
static void hz_blit_go_impl(nvk_cmdbuf *c)
{
    hz_state *s = t_hz.blit_cb == c->cb ? hz_get(c->cb, 0) : NULL;
    if (!s) return;
    HZC(HZC_BLIT);
    if (hz_conflict_upto(s, 3, s->n)) { HZC(HZC_BLIT_BAR); if (hzm() == 1) nvmtl_vk_cmd_barrier(c); else hz_reset(s); }
    else HZC(HZC_BLIT_FREE);
    t_hz.blit_pre = hzm() == 1;
}
static void hz_blit_end_impl(nvk_cmdbuf *c)
{
    (void)c;
    hz_state *s = t_hz.blit_cb ? hz_get(t_hz.blit_cb, 0) : NULL;   // the command buffer may have ended inside the blit
    if (s) { hz_commit(s); hz_close_region(s); }
    t_hz.blit_cb = NULL; t_hz.blit_pre = 0;
}
int nvmtl_hz_blit_begin(nvk_cmdbuf *c)
{ if (hzm() <= 0) return 0; const uint64_t t0 = hzt_now(); const int r = hz_blit_begin_impl(c); hzt_add(HZT_BLIT, t0); return r; }
void nvmtl_hz_blit_go(nvk_cmdbuf *c) { const uint64_t t0 = hzt_now(); hz_blit_go_impl(c); hzt_add(HZT_BLIT, t0); }
void nvmtl_hz_blit_end(nvk_cmdbuf *c) { const uint64_t t0 = hzt_now(); hz_blit_end_impl(c); hzt_add(HZT_BLIT, t0); }
// Every vkCmdPipelineBarrier recorded after this point goes through here: a tracked blit's pre-barriers lose their
// source access (no drain on NVK); image barriers keep their layouts and destination access.
static void nvmtl_hz_barrier_call(VkCommandBuffer cb, VkPipelineStageFlags ss, VkPipelineStageFlags ds, VkDependencyFlags f,
                                  uint32_t nm, const VkMemoryBarrier *m, uint32_t nb, const VkBufferMemoryBarrier *b,
                                  uint32_t ni, const VkImageMemoryBarrier *im)
{
    nvmtl_fx_note_barrier(cb, ss, ds, nm, m, nb, b, ni, im);   // the fault context's barrier record, both hazard modes
    if (t_hz.blit_pre && cb == t_hz.blit_cb && ni <= 16) {
        HZC(HZC_BLIT_PREDROP);
        if (!ni) return;
        VkImageMemoryBarrier tmp[16];
        for (uint32_t i = 0; i < ni; i++) { tmp[i] = im[i]; tmp[i].srcAccessMask = 0; }
        pvkCmdPipelineBarrier(cb, ss, ds, f, 0, NULL, 0, NULL, ni, tmp);
        return;
    }
    pvkCmdPipelineBarrier(cb, ss, ds, f, nm, m, nb, b, ni, im);
}
#define pvkCmdPipelineBarrier(cb, ...) nvmtl_hz_barrier_call((cb), __VA_ARGS__)

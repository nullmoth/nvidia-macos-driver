/* Hybrid laptop present path (kexts/NVRM/accel/nm_hybrid.h), on the host.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_present.c -o /tmp/t && /tmp/t     -> 0 failed
 *   cc -O1 -Wall -DLEGACY_COPY ...   (the loop before nm_hybrid.h: every word, every frame)  -> the "only what changed" arms fail */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nm_hybrid.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

#define W 64
#define H 24

#ifdef LEGACY_COPY
/* the display pipe's copy loop as it was: no shadow, every word of every row */
static int present(struct nmh_shadow *sh, const volatile uint32_t *src, uint32_t w, uint32_t h, uint32_t spitch, int from10,
                   volatile uint32_t *dst, uint32_t dpitch, struct nmh_stats *st)
{
    uint32_t words = (spitch < dpitch ? spitch : dpitch) / 4, spr = spitch / 4, dpr = dpitch / 4;
    (void)sh;
    memset(st, 0, sizeof *st);
    for (uint32_t y = 0; y < h; y++)
        for (uint32_t x = 0; x < words; x++)
            dst[(uint64_t)y * dpr + x] = from10 ? nv_iop_l10r_to_x8r8g8b8(src[(uint64_t)y * spr + x]) : src[(uint64_t)y * spr + x];
    st->rows = h; st->words = h * words; st->full = 1;
    return 0;
}
#else
#define present nmh_present
#endif

static uint32_t src[W * H], dst[W * H], shadow[W * H];
static struct nmh_shadow sh = { shadow, W, H, 0 };
static struct nmh_stats st;

static void fill(uint32_t seed) { for (unsigned i = 0; i < W * H; i++) src[i] = 0xff000000u | (seed * 2654435761u + i * 40503u); }
static int same(void) { return memcmp(src, dst, sizeof src) == 0; }
static uint64_t rd_tab(void *c, uint64_t i) { return ((uint64_t *)c)[i]; }


/* fakes for the level-4 order test: they record the order of the calls and use the real ownership model */
static struct nmh_own own; static char order[16]; static int on, ce_result, render_result, present_result, acquired_idx;
static int lock_depth, lock_at_present, lock_at_copy, lock_at_render, lock_leaks, lookup_result;
static void rec(char c) { if (on < 15) order[on++] = c; order[on] = 0; }
static int f_lookup(void *c) { (void)c; rec('L'); if (lookup_result) return -1; lock_depth++; return 0; }
static void f_unlock(void *c) { (void)c; rec('U'); lock_depth--; if (lock_depth < 0) lock_leaks++; }
static int f_render(void *c) { (void)c; rec('W'); lock_at_render = lock_depth; return render_result; }
static int f_acquire(void *c, int *i) { (void)c; rec('A'); *i = nmh_own_acquire(&own); acquired_idx = *i; return *i < 0; }
static int f_copy(void *c, int i) { (void)c; (void)i; rec('C'); lock_at_copy = lock_depth; return ce_result; }
static int f_present(void *c, int i) { (void)c; rec('P'); lock_at_present = lock_depth; if (present_result) return -1; nmh_own_present(&own, i); nmh_own_landed(&own); return 0; }
static void f_abort(void *c, int i) { (void)c; rec('X'); nmh_own_abort(&own, i); }
static void f_quar(void *c, int i) { (void)c; rec('Q'); nmh_own_quarantine(&own, i); }

int main(void)
{
    /* ---- the switch */
    ARM("no boot-arg: off", nmh_level_parse(0, 1) == NMH_OFF);
    ARM("nmhybrid=1: copy", nmh_level_parse(1, 1) == NMH_COPY);
    ARM("nmhybrid=2: import", nmh_level_parse(1, 2) == NMH_IMPORT);
    ARM("nmhybrid=3: owned buffers", nmh_level_parse(1, 3) == NMH_OWNED);
    ARM("nmhybrid=4: copy engine into owned buffers", nmh_level_parse(1, 4) == NMH_DMA);
    ARM("nmhybrid=0, 5 and -1 are off", nmh_level_parse(1, 0) == NMH_OFF && nmh_level_parse(1, 5) == NMH_OFF && nmh_level_parse(1, -1) == NMH_OFF);

    /* ---- level 1: the copy */
    memset(&shadow, 0, sizeof shadow); sh.valid = 0; memset(dst, 0, sizeof dst);
    fill(1);
    ARM("first frame: present succeeds", present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) == 0);
    ARM("first frame: the screen equals the frame", same());
    ARM("first frame: every word stored", st.words == W * H && st.full);
    ARM("an identical frame stores nothing", present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) == 0 && st.words == 0 && st.rows == 0);
    src[5 * W + 7] ^= 0x00ffffffu;
    ARM("one changed pixel: one row, one word", present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) == 0 && st.rows == 1 && st.words == 1 && st.first == 5 && st.last == 5);
    ARM("one changed pixel: the screen equals the frame", same());
    src[9 * W + 3] ^= 0x1u; src[9 * W + 40] ^= 0x1u; src[12 * W + 0] ^= 0x1u;
    ARM("two changes in a row: the stored span runs from the first to the last", present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) == 0 && st.rows == 2 && st.words == (40 - 3 + 1) + 1 && st.first == 9 && st.last == 12);
    ARM("two changes in a row: the screen equals the frame", same());

    /* many frames: correctness of the dirty tracking itself */
    srand(7); int bad = 0;
    for (int f = 0; f < 300; f++) {
        int n = rand() % 6;
        for (int k = 0; k < n; k++) src[rand() % (W * H)] = (uint32_t)rand() * 2654435761u;
        if (present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) || !same()) bad++;
    }
    ARM("300 random frames: the screen always equals the frame", bad == 0);

    /* something else wrote to the scan-out */
    for (unsigned i = 0; i < W * H; i += 5) dst[i] = 0xdeadbeefu;
    present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st);
    ARM("without invalidate, a scribble by another writer stays (why invalidate exists)",
#ifdef LEGACY_COPY
        1
#else
        !same()
#endif
    );
    nmh_shadow_invalidate(&sh);
    ARM("after invalidate the next frame repairs the whole screen", present(&sh, src, W, H, W * 4, 0, dst, W * 4, &st) == 0 && same());

    /* geometry */
    { static uint32_t s2[W * H * 2], d2[W * H * 2];
      struct nmh_shadow other = { shadow, W, H, 1 };   /* shadow sized for another mode */
      for (unsigned i = 0; i < W * H * 2; i++) s2[i] = 0xff000000u + i;
      ARM("a frame of another size stores everything", present(&other, s2, W, H * 2, W * 4, 0, d2, W * 4, &st) == 0 && st.full && st.words == W * H * 2 && !memcmp(s2, d2, sizeof s2));
      ARM("and leaves no stale shadow behind", other.valid == 0); }

    /* pitches: padding must survive, a narrower destination must not be overrun */
    { enum { SP = W * 4 + 64, DP = W * 4 + 128 };
      static uint32_t sp[(SP / 4) * H], dp[(DP / 4) * H + 16];
      uint32_t shd[W * H]; struct nmh_shadow s3 = { shd, W, H, 0 };
      for (unsigned i = 0; i < (SP / 4) * H; i++) sp[i] = 0xff000000u + i;
      for (unsigned i = 0; i < (DP / 4) * H + 16; i++) dp[i] = 0xa5a5a5a5u;
      ARM("wider pitches: present succeeds", present(&s3, sp, W, H, SP, 0, dp, DP, &st) == 0);
      int ok = 1;
      for (unsigned y = 0; y < H; y++) {
          for (unsigned x = 0; x < W; x++) if (dp[y * (DP / 4) + x] != sp[y * (SP / 4) + x]) ok = 0;
          for (unsigned x = W; x < DP / 4; x++) if (dp[y * (DP / 4) + x] != 0xa5a5a5a5u) ok = 0;
      }
      ARM("wider pitches: every pixel lands at its own row, row padding is untouched", ok);
      static uint32_t dn[(W / 2) * H + 8];
      for (unsigned i = 0; i < (W / 2) * H + 8; i++) dn[i] = 0x5a5a5a5au;
      struct nmh_shadow s4 = { shd, W, H, 0 };
      ARM("narrower destination: present succeeds", present(&s4, sp, W, H, SP, 0, dn, (W / 2) * 4, &st) == 0);
      ok = 1;
      for (unsigned y = 0; y < H; y++) for (unsigned x = 0; x < W / 2; x++) if (dn[y * (W / 2) + x] != sp[y * (SP / 4) + x]) ok = 0;
      for (unsigned i = (W / 2) * H; i < (W / 2) * H + 8; i++) if (dn[i] != 0x5a5a5a5au) ok = 0;
      ARM("narrower destination: the left part is shown and nothing past the buffer is written", ok); }

    /* l10r (login window) through the dirty path */
    { static uint32_t sl[W * H], dl[W * H], shl[W * H]; struct nmh_shadow s5 = { shl, W, H, 0 };
      for (unsigned i = 0; i < W * H; i++) sl[i] = (3u << 30) | ((i & 1023u) << 20) | (((i * 7) & 1023u) << 10) | ((i * 13) & 1023u);
      present(&s5, sl, W, H, W * 4, 1, dl, W * 4, &st);
      int ok = 1; for (unsigned i = 0; i < W * H; i++) if (dl[i] != nv_iop_l10r_to_x8r8g8b8(sl[i])) ok = 0;
      ARM("l10r frame: every pixel narrowed to 8 bits", ok);
      ARM("l10r frame: an unchanged login window stores nothing", present(&s5, sl, W, H, W * 4, 1, dl, W * 4, &st) == 0 && st.words == 0); }

    /* refusals write nothing */
    { uint32_t d[4] = { 1, 2, 3, 4 };
      ARM("zero width, tiny pitch and NULL are refused without a store",
          present(&sh, src, 0, H, W * 4, 0, d, 16, &st) == -1 && present(&sh, src, W, H, 2, 0, d, 16, &st) == -1 &&
          present(&sh, NULL, W, H, W * 4, 0, d, 16, &st) == -1 && d[0] == 1 && d[3] == 4); }

    /* ---- level 2: import */
    ARM("import at level 1 is refused as not enabled", nmh_import_check(NMH_COPY, 12, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080) == NMH_E_LEVEL);
    ARM("display versions 12 and 13 may import", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080) == NMH_OK && nmh_import_check(NMH_IMPORT, 13, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080) == NMH_OK);
    ARM("Meteor Lake and Lunar Lake (14, 20) are left to the copy", nmh_import_check(NMH_IMPORT, 14, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080) == NMH_E_GEN && nmh_import_check(NMH_IMPORT, 20, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080) == NMH_E_GEN);
    ARM("an l10r login window frame is left to the narrowing copy", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_L10R, 1920, 1080, 7680, 1920, 1080) == NMH_E_FMT);
    ARM("a surface of another size is refused", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 2560, 1440, 10240, 1920, 1080) == NMH_E_SIZE);
    ARM("a pitch that is not a multiple of 64 is refused", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 1000, 100, 4004, 1000, 100) == NMH_E_PITCH);
    ARM("a pitch under 4 bytes a pixel is refused", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 1024, 100, 2048, 1024, 100) == NMH_E_PITCH);
    ARM("a pitch past the stride field (131008 bytes) is refused", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 32770, 10, 131072, 32770, 10) == NMH_E_PITCH);
    ARM("3840x2160 at pitch 15360 is within the field", nmh_import_check(NMH_IMPORT, 12, NMH_FMT_BGRA, 3840, 2160, 15360, 3840, 2160) == NMH_OK);
    ARM("1920x1080 at 7680 bytes is 2025 pages", nmh_pages_for(7680, 1080) == 2025);

    ARM("a page entry is the address with the present bit", nmh_ggtt_pte(0x12345000ull) == 0x12345001ull);
    ARM("an unaligned address has no entry", nmh_ggtt_pte(0x12345800ull) == 0);
    ARM("an address past bit 45 has no entry", nmh_ggtt_pte(1ull << 46) == 0 && nmh_ggtt_pte((1ull << 46) - 4096) != 0);

    { uint64_t tab[1000]; const uint64_t scratch = 0x7777000ull | 1;
      for (int i = 0; i < 1000; i++) tab[i] = scratch;
      for (int i = 0; i < 100; i++) tab[i] = 0x1000ull * (i + 1) | 1;      /* firmware surface and friends at the bottom */
      tab[990] = 0x9000ull | 1;                                           /* a stray mapping near the top */
      int64_t r = nmh_find_free_run(rd_tab, tab, 1000, 1000, 200, 2000, 2001);
      ARM("a free range is the highest run of equal entries below the stray mapping", r == 790);
      ARM("a free range never includes a stray mapping", r >= 0 && !(r <= 990 && 990 < r + 200));
      ARM("a range longer than any run is not found", nmh_find_free_run(rd_tab, tab, 1000, 1000, 950, 2000, 2001) == -1);
      ARM("the scan is bounded", nmh_find_free_run(rd_tab, tab, 1000, 5, 6, 2000, 2001) == -1);
      int64_t r2 = nmh_find_free_run(rd_tab, tab, 1000, 1000, 100, 800, 899);
      ARM("the firmware surface range is never part of the answer", r2 == 700); }

    { struct nmh_ring r; int nm; uint32_t ev; nmh_ring_init(&r, 0x10000000ull, 0x800000ull);
      int a = nmh_ring_acquire(&r, 11, &nm, &ev), b = nmh_ring_acquire(&r, 12, &nm, &ev);
      ARM("new surfaces take free slots and need their pages written", a == 0 && b == 1 && nm == 1 && ev == 0);
      nmh_ring_mapped(&r, a); nmh_ring_mapped(&r, b);
      ARM("a surface seen again keeps its slot and needs no write", nmh_ring_acquire(&r, 11, &nm, &ev) == 0 && nm == 0);
      ARM("slot ranges are one slot apart", nmh_slot_surf(&r, 1) - nmh_slot_surf(&r, 0) == 0x800000ull);
      int c = nmh_ring_acquire(&r, 13, &nm, &ev), d = nmh_ring_acquire(&r, 14, &nm, &ev);
      nmh_ring_mapped(&r, c); nmh_ring_mapped(&r, d);
      ARM("four slots fill", c == 2 && d == 3);
      /* 11 was touched at tick 3, 13 at 4, 14 at 5: slot 0 is the least recently used one that is not on screen */
      nmh_flip_begin(&r, 1); nmh_flip_poll(&r, (uint32_t)nmh_slot_surf(&r, 1), 0x100000u, 5);
      ARM("the slot on screen is live", r.live == 1);
      int e = nmh_ring_acquire(&r, 15, &nm, &ev);
      ARM("a fifth surface evicts the least recently used slot that is not on screen", e == 0 && ev == 11 && nm == 1);
      nmh_ring_mapped(&r, e);
      nmh_flip_begin(&r, 0);
      for (int i = 0; i < NMH_SLOTS; i++) if (i != 0 && i != 1) { r.s[i].used_at = r.tick + 10; }
      int f = nmh_ring_acquire(&r, 16, &nm, &ev);
      ARM("neither the live slot nor the one flipping is ever evicted", f != 0 && f != 1 && f >= 0);
      nmh_ring_forget(&r, 3);
      ARM("a forgotten slot needs its pages again", nmh_ring_acquire(&r, 14, &nm, &ev) >= 0 && nm == 1); }

    { struct nmh_ring r; nmh_ring_init(&r, 0x10000000ull, 0x800000ull);
      const uint32_t home = 0x100000u;
      ARM("no flip in flight polls as done", nmh_flip_poll(&r, home, home, 4) == 1);
      ARM("a flip is accepted", nmh_flip_begin(&r, 2) == NMH_OK);
      ARM("a second flip while one is in flight is refused", nmh_flip_begin(&r, 1) == NMH_E_BUSY);
      ARM("the flip waits while the plane still shows the old surface", nmh_flip_poll(&r, home, home, 4) == 0 && r.live == -1);
      ARM("the flip lands when the live register shows the slot", nmh_flip_poll(&r, (uint32_t)nmh_slot_surf(&r, 2) | 0x3u, home, 4) == 1 && r.live == 2 && r.pending == NMH_NONE);
      ARM("flipping to what is already live is a no-op", nmh_flip_begin(&r, 2) == NMH_OK && r.pending == NMH_NONE);
      ARM("the copy path needs the plane home first when a slot is live", nmh_need_home(&r) == 1);
      nmh_flip_begin(&r, -1);
      ARM("going home lands on the boot surface", nmh_flip_poll(&r, home, home, 4) == 1 && r.live == -1 && !nmh_need_home(&r));
      nmh_flip_begin(&r, 3);
      int rc = 0; for (int i = 0; i < 4 && rc == 0; i++) rc = nmh_flip_poll(&r, home, home, 4);
      ARM("a flip that never lands is reported after the poll limit", rc == -1);
      nmh_flip_abandon(&r);
      ARM("an abandoned flip leaves nothing assumed about the plane", r.pending == NMH_NONE && r.live == -3 && nmh_need_home(&r)); }

    /* ---- the state machine */
    { struct nmh_sm m; nmh_sm_init(&m, NMH_OFF);
      ARM("level 0: every event leaves it off", nmh_sm_step(&m, NMH_EV_PROVIDER, 0) == NMH_S_OFF && nmh_sm_step(&m, NMH_EV_IMPORT_OK, 0) == NMH_S_OFF);
      nmh_sm_init(&m, NMH_IMPORT);
      ARM("level 2 starts armed", m.state == NMH_S_ARMED);
      ARM("a provider moves it to copy", nmh_sm_step(&m, NMH_EV_PROVIDER, 0) == NMH_S_COPY);
      ARM("an imported flip moves it to import", nmh_sm_step(&m, NMH_EV_IMPORT_OK, 0) == NMH_S_IMPORT);
      ARM("one refused surface drops back to copy, import stays enabled", nmh_sm_step(&m, NMH_EV_IMPORT_FAIL, NMH_E_FMT) == NMH_S_COPY && m.level == NMH_IMPORT && m.why == NMH_E_FMT);
      ARM("and the next good surface imports again", nmh_sm_step(&m, NMH_EV_IMPORT_OK, 0) == NMH_S_IMPORT);
      ARM("a stuck flip ends import for the boot", nmh_sm_step(&m, NMH_EV_IMPORT_STUCK, NMH_E_STUCK) == NMH_S_FALLBACK && m.level == NMH_COPY);
      ARM("after that an import event cannot bring it back", nmh_sm_step(&m, NMH_EV_IMPORT_OK, 0) == NMH_S_FALLBACK);
      ARM("losing the provider re-arms", nmh_sm_step(&m, NMH_EV_PROVIDER_GONE, 0) == NMH_S_ARMED);
      ARM("every refusal has a name", strcmp(nmh_why_name(NMH_E_STUCK), "a flip never reached the screen") == 0 && strcmp(nmh_why_name(NMH_E_FMT), "surface is not 8-bit BGRA") == 0); }



    /* ---- level 3: buffers the panel driver owns */
    { struct nmh_own o; nmh_own_init(&o);
      int a = nmh_own_acquire(&o);
      ARM("a writer gets a free buffer", a == 0 && o.st[0] == NMH_OB_WRITER);
      ARM("only one writer at a time", nmh_own_acquire(&o) == -1);
      ARM("a buffer another writer holds cannot be presented", nmh_own_present(&o, 1) == NMH_E_BUSY);
      ARM("presenting the writer's own buffer queues the flip", nmh_own_present(&o, a) == NMH_OK && o.queued == 0 && o.st[0] == NMH_OB_QUEUED);
      int b = nmh_own_acquire(&o);
      ARM("the next writer gets another buffer while the flip is queued", b == 1);
      ARM("a second flip cannot be queued before the first lands", nmh_own_present(&o, b) == NMH_E_BUSY);
      nmh_own_landed(&o);
      ARM("a landed flip makes its buffer live", o.live == 0 && o.st[0] == NMH_OB_LIVE && o.queued == -1);
      ARM("now the second buffer can be presented", nmh_own_present(&o, b) == NMH_OK);
      nmh_own_landed(&o);
      ARM("the buffer that was on screen is free again, the new one is live", o.st[0] == NMH_OB_FREE && o.st[1] == NMH_OB_LIVE && o.live == 1);
      int c = nmh_own_acquire(&o), d;
      ARM("the live buffer is never handed to a writer", c == 0);
      nmh_own_abort(&o, c);
      ARM("a writer that gives up returns the buffer without showing it", o.st[0] == NMH_OB_FREE && o.live == 1);
      c = nmh_own_acquire(&o); nmh_own_present(&o, c); d = -9;
      ARM("buffers cannot be freed while a flip is queued", !nmh_own_may_free(&o));
      nmh_own_landed(&o); (void)d;
      ARM("nor while one is on screen", !nmh_own_may_free(&o));
      nmh_own_home(&o);
      ARM("going home frees every buffer and the buffers may then be released", o.live == -1 && o.st[0] == NMH_OB_FREE && o.st[1] == NMH_OB_FREE && nmh_own_may_free(&o));
      c = nmh_own_acquire(&o); nmh_own_present(&o, c); nmh_own_flip_failed(&o);
      ARM("a flip that never landed leaves nothing queued and nothing assumed live", o.queued == -1 && o.live == -1 && o.st[c] == NMH_OB_FREE);
      ARM("a writer in progress keeps its buffer through home", (c = nmh_own_acquire(&o), nmh_own_home(&o), o.st[c] == NMH_OB_WRITER));
      struct nmh_own full; nmh_own_init(&full); full.st[0] = full.st[1] = full.st[2] = NMH_OB_LIVE; full.live = 0;
      ARM("with every buffer taken the caller is told to copy the old way", nmh_own_acquire(&full) == -1); }

    /* ---- level 4: the copy engine writes the frame into an owned buffer */
    ARM("level 3 does not use the copy engine path", nmh_dma_eligible(NMH_OWNED, 1, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_E_LEVEL);
    ARM("a surface already paged to system memory is left to level 3", nmh_dma_eligible(NMH_DMA, 0, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_E_NOTVRAM);
    ARM("a 10-bit login frame is left to the narrowing copy", nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_L10R, 1920, 1080, 7680, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_E_FMT);
    ARM("a linear byte copy needs the scan-out's size and pitch exactly", nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_BGRA, 1920, 1080, 7744, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_E_SIZE && nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_BGRA, 1920, 1088, 7680, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_E_SIZE);
    ARM("a surface longer than the GPU memory behind it or the buffer is refused", nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080, 7680, 7u << 20, 9u << 20) == NMH_E_DMALEN && nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080, 7680, 9u << 20, 7u << 20) == NMH_E_DMALEN);
    ARM("an eligible frame is accepted", nmh_dma_eligible(NMH_DMA, 1, NMH_FMT_BGRA, 1920, 1080, 7680, 1920, 1080, 7680, 9u << 20, 9u << 20) == NMH_OK);
    {   /* fakes that record the order of the calls and use the real ownership model */
        struct nmh_dma_ops ops = { &own, f_render, f_lookup, f_unlock, f_acquire, f_copy, f_present, f_abort, f_quar };
        nmh_own_init(&own); on = 0; order[0] = 0; ce_result = 0; render_result = 0; present_result = 0; lookup_result = 0; lock_depth = 0; lock_leaks = 0;
        ARM("a good frame: render wait, acquire, copy, then present, in that order", nmh_dma_frame(&ops) == NMH_DMA_DONE && !strcmp(order, "WLACUP") && own.live == acquired_idx);
        ARM("the accelerator lock is not held while the GPU is waited for", lock_at_render == 0);
        ARM("the accelerator lock is held across the lookup and the copy", lock_at_copy == 1);
        ARM("the accelerator lock is released before the flip wait", lock_at_present == 0 && lock_depth == 0);
        nmh_own_init(&own); on = 0; render_result = 1;
        ARM("a surface the GPU is still drawing is not copied: nothing is acquired", nmh_dma_frame(&ops) == NMH_DMA_RENDER && !strcmp(order, "W") && lock_depth == 0);
        nmh_own_init(&own); on = 0; render_result = 0; ce_result = 1;
        ARM("a copy that failed cleanly gives the buffer back and presents nothing", nmh_dma_frame(&ops) == NMH_DMA_COPY_FAILED && !strcmp(order, "WLACUX") && own.st[0] == NMH_OB_FREE && lock_depth == 0);
        nmh_own_init(&own); on = 0; ce_result = 2;
        ARM("a copy that timed out is quarantined and never presented", nmh_dma_frame(&ops) == NMH_DMA_COPY_TIMEOUT && !strcmp(order, "WLACUQ") && own.st[0] == NMH_OB_QUARANTINE && lock_depth == 0);
        int a = nmh_own_acquire(&own); nmh_own_abort(&own, a); int b = nmh_own_acquire(&own); nmh_own_present(&own, b); nmh_own_landed(&own);
        int c = nmh_own_acquire(&own); nmh_own_abort(&own, c); int d = nmh_own_acquire(&own);
        ARM("a quarantined buffer is never handed out again", a == 1 && b == 1 && c == 2 && d == 2);
        ARM("and the buffers cannot be released while one may still be written", !nmh_own_may_free(&own) && (nmh_own_home(&own), own.st[0] == NMH_OB_QUARANTINE));
        nmh_own_init(&own); on = 0; ce_result = 0; present_result = 1;
        ARM("a flip that fails after a good copy gives the buffer back", nmh_dma_frame(&ops) == NMH_DMA_PRESENT_FAILED && !strcmp(order, "WLACUPX") && own.st[0] == NMH_OB_FREE && lock_depth == 0);
        nmh_own_init(&own); own.st[0] = own.st[1] = own.st[2] = NMH_OB_LIVE; on = 0; present_result = 0;
        ARM("with every buffer taken nothing is copied and the lock is given back", nmh_dma_frame(&ops) == NMH_DMA_NO_BUFFER && !strcmp(order, "WLAU") && lock_depth == 0);
        nmh_own_init(&own); on = 0; lookup_result = 1;
        ARM("a frame that is not eligible takes no buffer and leaves no lock", nmh_dma_frame(&ops) == NMH_DMA_NOT_ELIGIBLE && !strcmp(order, "WL") && lock_depth == 0 && lock_leaks == 0);
    }
    /* ---- start-up controls */
    ARM("nmhybriddelay absent is 0", nmh_delay_parse(0, 45) == 0);
    ARM("nmhybriddelay=45 waits 45 s", nmh_delay_parse(1, 45) == 45);
    ARM("nmhybriddelay is capped: 121, 0 and -3 mean no wait", nmh_delay_parse(1, 121) == 0 && nmh_delay_parse(1, 0) == 0 && nmh_delay_parse(1, -3) == 0);
    ARM("nmhybriddelay=120 is the longest", nmh_delay_parse(1, 120) == 120);
    ARM("a healthy NVIDIA driver: the panel driver goes ahead", nmh_stand_down_why(0, 0, 0, 2) == NMH_STAY_NONE && nmh_stand_down_why(0, 0, 1, 2) == NMH_STAY_NONE);
    ARM("-nvoff: stands down", nmh_stand_down_why(1, 0, 0, 2) == NMH_STAY_NVOFF);
    ARM("the picker's remove flag: stands down", nmh_stand_down_why(0, 1, 0, 2) == NMH_STAY_REMOVE);
    ARM("two starts that never lit a display: stands down", nmh_stand_down_why(0, 0, 2, 2) == NMH_STAY_GUARD && nmh_stand_down_why(0, 0, 5, 2) == NMH_STAY_GUARD);
    ARM("every stand-down reason has a name for the log", nmh_stay_name(NMH_STAY_NVOFF)[0] && nmh_stay_name(NMH_STAY_REMOVE)[0] && nmh_stay_name(NMH_STAY_GUARD)[0] && !nmh_stay_name(NMH_STAY_NONE)[0]);
    { int ok = 1; for (int i = 1; i < NMH_STEP__COUNT; i++) if (!nmh_step_name(i)[0] || !strcmp(nmh_step_name(i), "?") || !strcmp(nmh_step_name(i), nmh_step_name(i - 1))) ok = 0;
      ARM("every step has its own name", ok && !strcmp(nmh_step_name(NMH_STEP__COUNT), "?")); }

    /* ---- flat frames */
    { struct nmh_flat f = {0}; int held = 0, shown_after = -1;
      ARM("nmflatskip off: a flat frame is never held", nmh_flat_hold(&f, 1, 0) == 0);
      for (int i = 0; i < 10; i++) { if (nmh_flat_hold(&f, 1, 3)) held++; else { shown_after = i; break; } }
      ARM("three flat frames in a row are held, the fourth is shown", held == 3 && shown_after == 3);
      ARM("a frame with content is never held and resets the count", nmh_flat_hold(&f, 0, 3) == 0 && f.held == 0);
      ARM("a one-frame black blink is hidden", nmh_flat_hold(&f, 1, 3) == 1 && nmh_flat_hold(&f, 0, 3) == 0);
      ARM("a steady flat screen still reaches the panel (one frame in four)", (nmh_flat_hold(&f, 1, 3), nmh_flat_hold(&f, 1, 3), nmh_flat_hold(&f, 1, 3), nmh_flat_hold(&f, 1, 3)) == 0);
      ARM("nmflatskip is 0 to 8", nmh_flat_parse(1, 3) == 3 && nmh_flat_parse(1, 9) == 0 && nmh_flat_parse(0, 3) == 0 && nmh_flat_parse(1, 8) == 8);
      uint32_t a[64], b[64]; for (int i = 0; i < 64; i++) { a[i] = 0xff102030u; b[i] = a[i]; } b[40] = 0xff102031u;
      ARM("64 equal samples are flat, alpha ignored", nmh_samples_flat(a, 64) && (a[3] = 0x00102030u, nmh_samples_flat(a, 64)));
      ARM("one different sample is not flat", !nmh_samples_flat(b, 64));
    }
    printf("%d failed\n", fails);
    return fails != 0;
}

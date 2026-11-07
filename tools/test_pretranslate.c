/* test_pretranslate.c - the background pre-translation rules (plugin/nvmtl_pretranslate.h) with a stub translator.
   Build and run anywhere (no GPU, no Metal):  cc -O1 -pthread -I plugin tools/test_pretranslate.c -o /tmp/test_pretranslate && /tmp/test_pretranslate
   The stub counts how many times each function is translated while a two-thread background pool and four application
   threads ask for the same functions at once. Exit status 0 when every arm holds. */
#include <stdio.h>
#include <stdatomic.h>
#include <unistd.h>
#include "nvmtl_pretranslate.h"

#define NFN 400
#define FAILING 7                       /* the function whose translation is refused */
static atomic_int translated[NFN];      /* how many times the stub translated function i */
static atomic_int stored[NFN];          /* 1 once the result is in the "cache" */
static int naive;                       /* control: skip single-flight */
static nvmtl_sf sf;
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }

static atomic_int failed[NFN];          /* 1 once a refusal is remembered, as the plugin does, so it is not retried */
static int *results[NFN];               /* the "cache" payload: a heap int holding i + 1000 */
static void *keep(void *p) { return p; }

/* what the plugin's library entry does: cache lookup, then single-flight, with the cache checked again by the owner */
static int *get(int i)
{
    if (atomic_load(&stored[i])) return results[i];
    if (atomic_load(&failed[i])) return NULL;
    char key[16]; snprintf(key, sizeof key, "fn%d", i);
    void *r = NULL;
    if (!naive && !nvmtl_sf_begin(&sf, key, 1, &r)) return r;
    if (atomic_load(&failed[i])) { if (!naive) { nvmtl_sf_publish(&sf, key, NULL); nvmtl_sf_retire(&sf, key); } return NULL; }
    if (atomic_load(&stored[i])) { if (!naive) { nvmtl_sf_publish(&sf, key, results[i]); nvmtl_sf_retire(&sf, key); } return results[i]; }
    atomic_fetch_add(&translated[i], 1);
    usleep(200 + (unsigned)(i * 37 % 400));            /* translation takes time */
    int *spv = NULL;
    if (i != FAILING) { spv = malloc(sizeof *spv); *spv = i + 1000; }
    if (!naive) nvmtl_sf_publish(&sf, key, spv);
    usleep(300);                                        /* the cache file is written some time after the translation */
    results[i] = spv; atomic_store(&stored[i], spv != NULL); atomic_store(&failed[i], spv == NULL);
    if (!naive) nvmtl_sf_retire(&sf, key);
    return spv;
}
static void bg_job(void *ctx) { (void)get((int)(intptr_t)ctx); }
static atomic_int app_bad;
static void *app_thread(void *arg)
{
    int dir = (int)(intptr_t)arg;                       /* 0 forward, 1 backward, 2 stride 3, 3 stride 7 */
    for (int n = 0; n < NFN; n++) {
        int i = dir == 0 ? n : dir == 1 ? NFN - 1 - n : (n * (dir == 2 ? 3 : 7)) % NFN;
        int *p = get(i);
        if (i == FAILING ? p != NULL : (!p || *p != i + 1000)) atomic_fetch_add(&app_bad, 1);
    }
    return NULL;
}
static int run(void)
{
    memset(results, 0, sizeof results);
    for (int i = 0; i < NFN; i++) { atomic_store(&translated[i], 0); atomic_store(&stored[i], 0); atomic_store(&failed[i], 0); }
    atomic_store(&app_bad, 0);
    static nvmtl_pool pool; nvmtl_pool_init(&pool, 2, 1);
    for (int i = 0; i < NFN; i++) nvmtl_pool_submit(&pool, bg_job, (void *)(intptr_t)i);
    pthread_t t[4];
    for (int k = 0; k < 4; k++) pthread_create(&t[k], NULL, app_thread, (void *)(intptr_t)k);
    for (int k = 0; k < 4; k++) pthread_join(t[k], NULL);
    nvmtl_pool_drain(&pool);
    int twice = 0, never = 0;
    for (int i = 0; i < NFN; i++) { int c = atomic_load(&translated[i]); if (c > 1) twice++; if (c == 0) never++; }
    printf("     translated: %d functions, %d more than once, %d never\n", NFN, twice, never);
    return twice;
}
int main(void)
{
    nvmtl_sf_init(&sf, keep, NULL);
    naive = 1; int control = run();
    arm("control: without single-flight the racing callers DO translate some function twice", control > 0);
    naive = 0; int twice = run();
    int never = 0; for (int i = 0; i < NFN; i++) if (!atomic_load(&translated[i])) never++;
    arm("every function is translated exactly once with a pool and four application threads racing it", twice == 0 && never == 0);
    arm("every application request got the right module (or the refusal for the one that fails)", atomic_load(&app_bad) == 0);
    arm("the refused function was translated once, not once per caller", atomic_load(&translated[FAILING]) == 1);
    arm("no entry is left behind once every result is stored", sf.head == NULL);
    /* the pool never runs more than its bound */
    static nvmtl_pool p2; nvmtl_pool_init(&p2, 2, 1);
    for (int i = 0; i < 50; i++) nvmtl_pool_submit(&p2, bg_job, (void *)(intptr_t)(i % 5));
    nvmtl_pool_drain(&p2);
    arm("the pool starts at most two threads for fifty jobs", p2.started == 2);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}

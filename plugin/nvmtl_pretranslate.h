/* nvmtl_pretranslate.h - translate each shader of a new library once, in the background, before the application asks.
   A cold shader cache makes the first run translate every shader the application builds a pipeline for, on the thread
   that asked, which shows as long frames. The shaders of a library are known the moment it is created, so a small pool of
   low-priority threads can translate them ahead of the application.

   Two parts, both pure C over callbacks so tools/test_pretranslate.c proves them without Metal or the translator:
   - single-flight: one translation per key at a time. The caller that finds no entry owns the job; every other caller,
     background or application, waits for the owner's result instead of translating again. A published result stays
     available until its owner retires it, which the owner does after the result is stored where the next lookup will
     find it (the cache file is written later than the translation ends).
   - a bounded worker pool that runs jobs at background quality of service. */
#ifndef NVMTL_PRETRANSLATE_H
#define NVMTL_PRETRANSLATE_H
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#ifdef __APPLE__
#include <pthread/qos.h>
#endif

typedef struct nvmtl_sf_entry {
    char *key; int done, retired, waiters; void *result;
    struct nvmtl_sf_entry *next;
} nvmtl_sf_entry;

typedef struct {
    pthread_mutex_t mu; pthread_cond_t cv; nvmtl_sf_entry *head;
    void *(*retain)(void *); void (*release)(void *);   /* either may be NULL for a result that needs neither */
} nvmtl_sf;

static inline void nvmtl_sf_init(nvmtl_sf *s, void *(*retain)(void *), void (*release)(void *))
{
    pthread_mutex_init(&s->mu, NULL); pthread_cond_init(&s->cv, NULL); s->head = NULL; s->retain = retain; s->release = release;
}

static inline nvmtl_sf_entry *nvmtl_sf_find(nvmtl_sf *s, const char *key)
{
    for (nvmtl_sf_entry *e = s->head; e; e = e->next) if (!e->retired && !strcmp(e->key, key)) return e;
    return NULL;
}

/* Returns 1 when the caller now owns the job for `key` (it must call publish, then retire). Returns 0 when somebody else
   owns or owned it: with wait != 0 this blocks until the result is published and *result receives a retained copy
   (NULL when the owner failed); with wait == 0 it returns 0 at once with *result NULL and does not wait. */
static inline int nvmtl_sf_begin(nvmtl_sf *s, const char *key, int wait, void **result)
{
    if (result) *result = NULL;
    pthread_mutex_lock(&s->mu);
    nvmtl_sf_entry *e = nvmtl_sf_find(s, key);
    if (!e) {
        e = calloc(1, sizeof *e); e->key = strdup(key); e->next = s->head; s->head = e;
        pthread_mutex_unlock(&s->mu);
        return 1;
    }
    if (wait) {
        e->waiters++;
        while (!e->done) pthread_cond_wait(&s->cv, &s->mu);
        if (result) *result = (e->result && s->retain) ? s->retain(e->result) : e->result;
        e->waiters--;
        if (e->retired && !e->waiters) {   /* the owner retired while this caller was still reading */
            if (e->result && s->release) s->release(e->result);
            free(e->key); free(e);
        }
    }
    pthread_mutex_unlock(&s->mu);
    return 0;
}

/* The owner's result (NULL for a failure). The entry keeps its own retained copy for callers that arrive later. */
static inline void nvmtl_sf_publish(nvmtl_sf *s, const char *key, void *result)
{
    pthread_mutex_lock(&s->mu);
    nvmtl_sf_entry *e = nvmtl_sf_find(s, key);
    if (e) { e->result = (result && s->retain) ? s->retain(result) : result; e->done = 1; }
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

/* The result is now stored where a fresh lookup finds it: later callers no longer need the entry. */
static inline void nvmtl_sf_retire(nvmtl_sf *s, const char *key)
{
    pthread_mutex_lock(&s->mu);
    nvmtl_sf_entry **pp = &s->head;
    while (*pp && (strcmp((*pp)->key, key) || (*pp)->retired)) pp = &(*pp)->next;
    nvmtl_sf_entry *e = *pp;
    if (e) {
        *pp = e->next; e->retired = 1;
        if (!e->waiters) { if (e->result && s->release) s->release(e->result); free(e->key); free(e); }
    }
    pthread_mutex_unlock(&s->mu);
}

/* ---- the bounded pool ---- */
typedef struct nvmtl_job { void (*fn)(void *); void *ctx; struct nvmtl_job *next; } nvmtl_job;
typedef struct {
    pthread_mutex_t mu; pthread_cond_t work, idle;
    nvmtl_job *head, *tail; int max, started, running, queued; int background_qos;
} nvmtl_pool;

static inline void nvmtl_pool_init(nvmtl_pool *p, int max_threads, int background_qos)
{
    memset(p, 0, sizeof *p);
    pthread_mutex_init(&p->mu, NULL); pthread_cond_init(&p->work, NULL); pthread_cond_init(&p->idle, NULL);
    p->max = max_threads < 1 ? 1 : max_threads; p->background_qos = background_qos;
}

static inline void *nvmtl_pool_main(void *arg)
{
    nvmtl_pool *p = arg;
#ifdef __APPLE__
    if (p->background_qos) pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);
#endif
    pthread_mutex_lock(&p->mu);
    for (;;) {
        while (!p->head) pthread_cond_wait(&p->work, &p->mu);
        nvmtl_job *j = p->head; p->head = j->next; if (!p->head) p->tail = NULL;
        p->queued--; p->running++;
        pthread_mutex_unlock(&p->mu);
        j->fn(j->ctx); free(j);
        pthread_mutex_lock(&p->mu);
        p->running--;
        if (!p->head && !p->running) pthread_cond_broadcast(&p->idle);
    }
    return NULL;
}

/* Threads start on demand, never more than max. Jobs run in the order submitted. */
static inline int nvmtl_pool_submit(nvmtl_pool *p, void (*fn)(void *), void *ctx)
{
    nvmtl_job *j = calloc(1, sizeof *j); if (!j) return -1;
    j->fn = fn; j->ctx = ctx;
    pthread_mutex_lock(&p->mu);
    if (p->tail) p->tail->next = j; else p->head = j;
    p->tail = j; p->queued++;
    if (p->started < p->max) {
        pthread_t t; pthread_attr_t a; pthread_attr_init(&a); pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
        pthread_attr_setstacksize(&a, 8u << 20);   /* the translator recurses */
        if (!pthread_create(&t, &a, nvmtl_pool_main, p)) p->started++;
        pthread_attr_destroy(&a);
    }
    pthread_cond_signal(&p->work);
    pthread_mutex_unlock(&p->mu);
    return 0;
}

/* Blocks until every submitted job has finished (tests; the plugin never waits for the pool). */
static inline void nvmtl_pool_drain(nvmtl_pool *p)
{
    pthread_mutex_lock(&p->mu);
    while (p->head || p->running) pthread_cond_wait(&p->idle, &p->mu);
    pthread_mutex_unlock(&p->mu);
}
#endif

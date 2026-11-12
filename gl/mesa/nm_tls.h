/*
 * nm_tls.h: per-thread variables without compiler thread-local storage.
 *
 * The compiler provides thread-local storage only from deployment target 10.7 (the dynamic loader of 10.6 has no
 * support for it). When NM_NO_COMPILER_TLS is defined, each thread-local variable in the tree is replaced by a
 * block of memory kept under a per-thread key: allocated zeroed on a thread's first use, freed when the thread
 * exits. The variable's name becomes a macro for "the calling thread's block", so the code that reads and writes
 * it is unchanged. Used only for the 10.6 build; every other build keeps the compiler's storage.
 */
#ifndef NM_TLS_H
#define NM_TLS_H
#ifdef NM_NO_COMPILER_TLS

#include <pthread.h>
#include <stdlib.h>

struct nm_tls_key { pthread_key_t key; volatile int ready; };

/* One lock per translation unit is enough: every key is declared in exactly one. */
static pthread_mutex_t nm_tls_init_lock __attribute__((unused)) = PTHREAD_MUTEX_INITIALIZER;

static inline void *nm_tls_block(struct nm_tls_key *k, size_t size) __attribute__((unused));
static inline void *nm_tls_block(struct nm_tls_key *k, size_t size)
{
    if (!k->ready) {
        pthread_mutex_lock(&nm_tls_init_lock);
        if (!k->ready && pthread_key_create(&k->key, free) == 0) k->ready = 1;
        pthread_mutex_unlock(&nm_tls_init_lock);
        if (!k->ready) abort();              /* no key means no per-thread state at all: nothing can run correctly */
    }
    void *p = pthread_getspecific(k->key);
    if (!p) {
        p = calloc(1, size);
        if (!p || pthread_setspecific(k->key, p) != 0) abort();
    }
    return p;
}

/* Declares the key for a variable of the given type; the caller then defines the variable's name as NM_TLS_REF. */
#define NM_TLS_KEY(name) static struct nm_tls_key name##_nm_key
#define NM_TLS_REF(type, name) (*(type *)nm_tls_block(&name##_nm_key, sizeof(type)))

#endif
#endif

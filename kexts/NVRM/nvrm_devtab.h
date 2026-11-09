/* nvrm_devtab.h - which GPU objects exist, which one is primary, and when a card may start.
   NVRM and NVAccel each used to keep one global pointer, set by the newest start() and never cleared. This table replaces
   the pointer without changing what one card (or an un-opted several) does:

     legacy (nvmultigpu absent): add() replaces the whole table with the newest object and remove() does nothing, which is
     exactly "the global is set by every start() and never cleared". primary() is that global.

     multi (boot argument nvmultigpu=1): the table holds every started card, the primary is the card the primary selector
     marked (else the first to start), and remove() takes a stopped card out.

   The start gate is the second half. The four-card report showed what unrestricted matching does: several cards began
   start-up at once, one reached RmInitAdapter and display set-up failed on another. In multi mode a card may begin only when
   no other card is mid-start, and a card the selector did not mark waits until the marked one is running. In legacy mode the
   gate is not there at all.

   Pure over its inputs (no kernel types), so tools/test_nvrm_devtab.cpp proves every rule on the host. */
#ifndef NVRM_DEVTAB_H
#define NVRM_DEVTAB_H

#define NVRM_DEVTAB_MAX 8

template <class T> struct NvrmDevTab {          /* zero-initialised static storage: no constructor, no static init order */
    T *dev[NVRM_DEVTAB_MAX];
    unsigned n;
    T *prim;

    void add(T *d, bool multi, bool marked_primary) {
        if (!multi) { dev[0] = d; n = 1; prim = d; return; }
        for (unsigned i = 0; i < n; i++) if (dev[i] == d) { if (marked_primary) prim = d; return; }
        if (n >= NVRM_DEVTAB_MAX) return;                 /* a ninth card is not accelerated */
        dev[n++] = d;
        if (!prim || marked_primary) prim = d;
    }
    void remove(T *d, bool multi) {
        if (!multi) return;                               /* legacy: the pointer was never cleared */
        for (unsigned i = 0; i < n; i++) if (dev[i] == d) {
            for (unsigned j = i + 1; j < n; j++) dev[j - 1] = dev[j];
            dev[--n] = 0;
            break;
        }
        if (prim == d) prim = n ? dev[0] : 0;
    }
    T *primary() const { return prim; }
    unsigned count() const { return n; }
    T *at(unsigned i) const { return i < n ? dev[i] : 0; }
};

/* The object an operation runs against: the owner an object holds (the accelerator that made it), or the primary when the
   caller has none. With one card the owner is the primary, so this equals the single global it replaced. */
template <class T> static inline T *nvrm_resolve(T *owner, T *primary) { return owner ? owner : primary; }

struct NvrmStartGate { unsigned starting; unsigned running; };   /* zero-initialised static storage */

/* May this card begin start-up now?  marked: the selector marked this card. primary_known: some card is marked (the selector
   is active); without it the first card to ask is the primary. */
static inline bool nvrm_gate_enter(NvrmStartGate *g, bool multi, bool marked, bool primary_known) {
    if (!multi) return true;
    if (primary_known && !marked && __atomic_load_n(&g->running, __ATOMIC_ACQUIRE) == 0) return false;
    /* matching runs start() for different cards on different threads: the claim is one compare-and-swap */
    unsigned expect = 0;
    return __atomic_compare_exchange_n(&g->starting, &expect, 1u, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
}
static inline void nvrm_gate_leave(NvrmStartGate *g, bool multi, bool started) {
    if (!multi) return;
    if (started) __atomic_add_fetch(&g->running, 1u, __ATOMIC_ACQ_REL);
    __atomic_store_n(&g->starting, 0u, __ATOMIC_RELEASE);
}

/* Cards refused by the gate wait here to be offered again. IOKit does not retry a start() that returned false, so when a
   start finishes the next waiting card's provider is registered again (IOService::registerService re-runs matching on it,
   a public mechanism). One card is re-offered per finished start, in the order they were refused, and a card is re-offered at
   most kReofferTries times so a card that can never start does not loop. The queue holds opaque provider pointers; the caller
   retains them. */
#define NVRM_REOFFER_MAX 8
#define NVRM_REOFFER_TRIES 3
template <class P> struct NvrmWaitQueue {
    P *prov[NVRM_REOFFER_MAX]; unsigned n;                       /* waiting, oldest first */
    P *seen[NVRM_REOFFER_MAX]; unsigned times[NVRM_REOFFER_MAX]; unsigned nseen;   /* how many times each provider was queued */
    unsigned lock;
    void acquire() { while (__sync_lock_test_and_set(&lock, 1u)) {} }
    void release() { __sync_lock_release(&lock); }
    /* true if the provider was queued (the caller must retain it); false if it is already waiting, was queued
       NVRM_REOFFER_TRIES times already, or the queue is full */
    bool add(P *p) {
        acquire(); bool added = false;
        for (unsigned i = 0; i < n; i++) if (prov[i] == p) { release(); return false; }
        unsigned k = nseen; for (unsigned i = 0; i < nseen; i++) if (seen[i] == p) k = i;
        if (k == nseen) { if (nseen >= NVRM_REOFFER_MAX) { release(); return false; } seen[nseen] = p; times[nseen] = 0; nseen++; }
        if (times[k] < NVRM_REOFFER_TRIES && n < NVRM_REOFFER_MAX) { times[k]++; prov[n++] = p; added = true; }
        release(); return added;
    }
    /* the oldest waiting provider, removed; 0 when none */
    P *next() {
        acquire(); P *p = 0;
        if (n) { p = prov[0]; for (unsigned i = 1; i < n; i++) prov[i-1] = prov[i]; n--; }
        release(); return p;
    }
};
#endif

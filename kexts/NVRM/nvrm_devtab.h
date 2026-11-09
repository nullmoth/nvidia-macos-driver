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

struct NvrmStartGate { unsigned starting; unsigned running; };   /* zero-initialised static storage */

/* May this card begin start-up now?  marked: the selector marked this card. primary_known: some card is marked (the selector
   is active); without it the first card to ask is the primary. */
static inline bool nvrm_gate_enter(NvrmStartGate *g, bool multi, bool marked, bool primary_known) {
    if (!multi) return true;
    if (g->starting) return false;
    if (primary_known && !marked && g->running == 0) return false;
    g->starting = 1;
    return true;
}
static inline void nvrm_gate_leave(NvrmStartGate *g, bool multi, bool started) {
    if (!multi) return;
    g->starting = 0;
    if (started) g->running++;
}
#endif

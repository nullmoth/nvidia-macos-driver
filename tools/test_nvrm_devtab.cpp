/* Host test for kexts/NVRM/nvrm_devtab.h. c++ -std=c++11 -I kexts/NVRM tools/test_nvrm_devtab.cpp -o /tmp/t && /tmp/t
   Every arm is a sentence. The legacy arms compare against a model of the old code (one global, set by each start, never
   cleared), so they fail if the table changes what a single card, or an un-opted several, does. */
#include <cstdio>
#include "nvrm_devtab.h"
static int fails;
static void arm(const char *w, bool ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
struct Dev { int id; };
static NvrmDevTab<Dev> tab; static NvrmStartGate gate;
static void reset() { tab = NvrmDevTab<Dev>(); gate = NvrmStartGate(); for (unsigned i = 0; i < NVRM_DEVTAB_MAX; i++) tab.dev[i] = 0; }

int main() {
    Dev a{0}, b{1}, c{2}, d{3};
    /* the old behaviour, as a model */
    Dev *old = 0;
    reset();
    arm("one card: the table's primary is the object that started, as the old global was", (tab.add(&a, false, false), old = &a, tab.primary() == old));
    arm("one card: the table holds exactly one entry and at(0) is it", tab.count() == 1 && tab.at(0) == &a && tab.at(1) == 0);
    tab.remove(&a, false);
    arm("one card: stop does not clear it (the old global never was)", tab.primary() == &a);
    reset(); Dev *oldg = 0;
    Dev *seq[] = { &a, &b, &c, &d };
    bool same = true;
    for (Dev *x : seq) { tab.add(x, false, x == &a); oldg = x; if (tab.primary() != oldg || tab.count() != 1) same = false; }
    arm("several cards without nvmultigpu: the newest start is primary every time and the table never grows (old behaviour)", same);
    arm("without nvmultigpu the start gate never refuses, however many cards ask at once",
        nvrm_gate_enter(&gate, false, false, true) && nvrm_gate_enter(&gate, false, false, true) && gate.starting == 0);

    reset();
    tab.add(&a, true, false);
    arm("multi: the first card to start is primary when no card is marked", tab.primary() == &a);
    tab.add(&b, true, false);
    arm("multi: a later card does not take over and is listed second", tab.primary() == &a && tab.count() == 2 && tab.at(1) == &b);
    tab.add(&c, true, true);
    arm("multi: a card the selector marked becomes primary", tab.primary() == &c && tab.count() == 3);
    tab.add(&b, true, false);
    arm("multi: the same card registering twice is one entry", tab.count() == 3);
    tab.remove(&c, true);
    arm("multi: a stopped primary is taken out and the first remaining card takes over", tab.primary() == &a && tab.count() == 2);
    tab.remove(&a, true); tab.remove(&b, true);
    arm("multi: when the last card stops there is no primary", tab.primary() == 0 && tab.count() == 0);
    reset();
    Dev devs[NVRM_DEVTAB_MAX + 2]; for (auto &x : devs) tab.add(&x, true, false);
    arm("multi: the table stops at its capacity and keeps the first card primary", tab.count() == NVRM_DEVTAB_MAX && tab.primary() == &devs[0]);

    /* the four-card failure: overlapping start-up */
    reset();
    bool g1 = nvrm_gate_enter(&gate, true, true, true);
    bool g2 = nvrm_gate_enter(&gate, true, false, true);
    bool g3 = nvrm_gate_enter(&gate, true, false, true);
    bool g4 = nvrm_gate_enter(&gate, true, false, true);
    arm("multi, four cards asking at once: exactly one starts, the other three are refused (overlapping start-up is the failure)", g1 && !g2 && !g3 && !g4);
    nvrm_gate_leave(&gate, true, true);
    arm("multi: after the primary is running, the next card may start, one at a time",
        nvrm_gate_enter(&gate, true, false, true) && !nvrm_gate_enter(&gate, true, false, true));
    nvrm_gate_leave(&gate, true, false);
    arm("multi: a card whose start-up failed does not count as running, and the gate opens again", gate.running == 1 && nvrm_gate_enter(&gate, true, false, true));
    reset();
    arm("multi: an unmarked card waits for the marked one when a selector is active", !nvrm_gate_enter(&gate, true, false, true) && gate.starting == 0);
    arm("multi: without a selector the first card to ask goes ahead", nvrm_gate_enter(&gate, true, false, false));

    printf(fails ? "FAILED %d\n" : "PASS all arms\n", fails);
    return fails != 0;
}

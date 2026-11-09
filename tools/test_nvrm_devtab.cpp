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

    /* slice 2: which object an operation runs against */
    reset();
    tab.add(&a, false, false);
    arm("one card: an operation on an object made by the card, and one with no owner, both resolve to the card the old global named",
        nvrm_resolve(&a, tab.primary()) == &a && nvrm_resolve((Dev *)0, tab.primary()) == &a);
    reset(); tab.add(&a, true, false); tab.add(&b, true, false);
    arm("two cards: an object made by the second card runs against the second card, not the primary",
        nvrm_resolve(&b, tab.primary()) == &b && nvrm_resolve(&a, tab.primary()) == &a);
    arm("two cards: code with no owner (display, scanout) runs against the primary", nvrm_resolve((Dev *)0, tab.primary()) == &a);
    reset();
    arm("no card at all: nothing resolves", nvrm_resolve((Dev *)0, tab.primary()) == 0);
    /* slice 4b: re-offering the cards the gate refused (IOKit does not retry a refused start) */
    {
        struct Card { int id; bool canStart; int starts; };
        Card cards[4] = { {0, true, 0}, {1, true, 0}, {2, true, 0}, {3, true, 0} };
        NvrmStartGate g = NvrmStartGate(); NvrmWaitQueue<Card> q = NvrmWaitQueue<Card>();
        int order[16], norder = 0, inflight = 0, maxInflight = 0;
        /* one start attempt by IOKit's matching: refused -> queued; otherwise runs to its end, then the next waiting card is re-offered */
        auto offer = [&](Card *c, bool marked) {
            Card *cur = c; bool mk = marked;
            for (;;) {
                if (!nvrm_gate_enter(&g, true, mk, true)) { q.add(cur); return; }
                inflight++; if (inflight > maxInflight) maxInflight = inflight;
                bool ok = cur->canStart; if (ok) { cur->starts++; order[norder++] = cur->id; }
                inflight--; nvrm_gate_leave(&g, true, ok);
                cur = q.next(); if (!cur) return;
                mk = false;
            }
        };
        offer(&cards[3], false); offer(&cards[2], false); offer(&cards[1], false);
        arm("four cards, primary last to be offered: the three unmarked ones are refused and queued, nothing has started", norder == 0 && q.n == 3);
        offer(&cards[0], true);
        arm("once the primary has started, the waiting cards are re-offered in the order they were refused: 0, 3, 2, 1",
            norder == 4 && order[0] == 0 && order[1] == 3 && order[2] == 2 && order[3] == 1);
        arm("exactly one start was ever in flight, and every card started once", maxInflight == 1 && cards[0].starts + cards[1].starts + cards[2].starts + cards[3].starts == 4);
        arm("the queue is empty at the end and a card already waiting is not queued twice", q.n == 0 && q.add(&cards[1]) && !q.add(&cards[1]));
        q.next();
        Card dead = { 9, false, 0 }; NvrmStartGate g2 = NvrmStartGate(); NvrmWaitQueue<Card> q2 = NvrmWaitQueue<Card>();
        int offers = 0; Card *cur = &dead;
        while (cur) { offers++; nvrm_gate_enter(&g2, true, true, true); nvrm_gate_leave(&g2, true, false); cur = q2.add(&dead) ? q2.next() : 0; }
        arm("a card that can never start is offered once and re-offered NVRM_REOFFER_TRIES times, then left alone (no loop)", offers == NVRM_REOFFER_TRIES + 1);
    }
    printf(fails ? "FAILED %d\n" : "PASS all arms\n", fails);
    return fails != 0;
}

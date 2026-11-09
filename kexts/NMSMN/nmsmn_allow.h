// NMSMN: which system-management-network (SMN) registers a client may touch.
//
// The only registers a power-limit tool needs are the SMU mailboxes: a message register, a response register and six
// argument registers for each SMU. Everything else on the SMN is refused, so the user client cannot be used to poke the
// rest of the SoC. The addresses are the mailbox assignments of the Zen APU/CPU families RyzenAdj supports (MP1 and PSMU
// sets; per-family AMD documentation and the Linux amdgpu/ryzen_smu drivers list the same offsets). Plain C: the check
// is tested on the host (tools/test_nmsmn_allow.py).
#ifndef NMSMN_ALLOW_H
#define NMSMN_ALLOW_H
#include <stdint.h>

struct nmsmn_mailbox {
    uint32_t msg;       // command register
    uint32_t rsp;       // response register
    uint32_t arg;       // first of six consecutive 32-bit argument registers
};

#define NMSMN_ARGS 6

static const struct nmsmn_mailbox kNmsmnMailboxes[] = {
    { 0x3B10528, 0x3B10564, 0x3B10998 },    // MP1, most families
    { 0x3B10528, 0x3B10578, 0x3B10998 },    // MP1, Van Gogh, Rembrandt, Mendocino, Phoenix, Hawk Point
    { 0x3B10928, 0x3B10978, 0x3B10998 },    // MP1, Krackan Point, Strix Point, Strix Halo
    { 0x3B10530, 0x3B1057C, 0x3B109C4 },    // MP1, Dragon Range, Fire Range
    { 0x3B10A20, 0x3B10A80, 0x3B10A88 },    // PSMU
    { 0x3B10524, 0x3B10570, 0x3B10A40 },    // PSMU, Dragon Range, Fire Range
};

static inline int nmsmn_allowed(uint32_t addr)
{
    if (addr & 3u)
        return 0;
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        if (addr == m->msg || addr == m->rsp)
            return 1;
        if (addr >= m->arg && addr < m->arg + 4u * NMSMN_ARGS)
            return 1;
    }
    return 0;
}
#endif

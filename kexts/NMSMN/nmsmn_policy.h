// NMSMN: what a client may do on the system management network (SMN): which registers, and which memory.
//
// The only registers a power-limit tool needs are the SMU mailboxes: a message register, a response register and six
// argument registers for each SMU. Everything else on the SMN is refused, so the user client cannot be used to poke the
// rest of the SoC. The addresses are the mailbox assignments of the Zen APU/CPU families RyzenAdj supports (MP1 and PSMU
// sets; per-family AMD documentation and the Linux amdgpu/ryzen_smu drivers list the same offsets). Plain C: the check
// is tested on the host (tools/test_nmsmn_policy.py).
// The mailbox assignments are those of RyzenAdj's nb_smu_ops.c (FlyGoat/RyzenAdj, LGPL-3.0, tag v0.19.0).
#ifndef NMSMN_POLICY_H
#define NMSMN_POLICY_H
#include <stdint.h>

struct nmsmn_mailbox {
    uint32_t msg;       // command register
    uint32_t rsp;       // response register
    uint32_t arg;       // first of six consecutive 32-bit argument registers
    int psmu;           // the platform SMU, whose table-address message is the only one that yields the power table
};

#define NMSMN_ARGS 6

static const struct nmsmn_mailbox kNmsmnMailboxes[] = {
    { 0x3B10528, 0x3B10564, 0x3B10998, 0 },    // MP1, most families
    { 0x3B10528, 0x3B10578, 0x3B10998, 0 },    // MP1, Van Gogh, Rembrandt, Mendocino, Phoenix, Hawk Point
    { 0x3B10928, 0x3B10978, 0x3B10998, 0 },    // MP1, Krackan Point, Strix Point, Strix Halo
    { 0x3B10530, 0x3B1057C, 0x3B109C4, 0 },    // MP1, Dragon Range, Fire Range
    { 0x3B10A20, 0x3B10A80, 0x3B10A88, 1 },    // PSMU
    { 0x3B10524, 0x3B10570, 0x3B10A40, 1 },    // PSMU, Dragon Range, Fire Range
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

// ---- the power table ----------------------------------------------------------------------------------------------
// The SMU's power table lives in DRAM at an address the SMU itself reports. A client is never allowed to name a physical
// address: the policy watches the platform SMU's exchange and remembers the address the SMU answers with, and only that
// address (and at most NMSMN_PM_MAX bytes of it) can be read. Message 0xB answers with a 32-bit address in argument 0;
// message 0x66 with a 64-bit one in arguments 0 and 1 (RyzenAdj's request_table_addr). The answer counts only after the
// response register read 1 (success). A client cannot forge that: the response register accepts only the value 0 from a
// client, and the argument registers it writes are overwritten by the SMU's answer before they are read back.
#define NMSMN_MSG_TABLE_ADDR32 0xBu
#define NMSMN_MSG_TABLE_ADDR64 0x66u
#define NMSMN_RESP_OK 1u
#define NMSMN_PM_MAX 0x10000u

struct nmsmn_state {
    int pending;            // index of the mailbox a table-address request is in flight on, or -1
    int wide;               // that request answers with a 64-bit address
    int resp_ok;            // the response register has read OK since the request
    int have_lo;
    uint32_t lo;
    int table_valid;
    uint64_t table;         // the SMU's own answer
};

static inline void nmsmn_state_init(struct nmsmn_state *s)
{
    s->pending = -1; s->wide = 0; s->resp_ok = 0; s->have_lo = 0; s->lo = 0; s->table_valid = 0; s->table = 0;
}

// which = 0 message, 1 response, 2.. argument (2 + index); the mailbox index, or -1
static inline int nmsmn_mailbox_of(uint32_t addr, int *which)
{
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        if (addr & 3u)
            return -1;
        if (addr == m->msg) { *which = 0; return (int)i; }
        if (addr == m->rsp) { *which = 1; return (int)i; }
        if (addr >= m->arg && addr < m->arg + 4u * NMSMN_ARGS) { *which = 2 + (int)((addr - m->arg) / 4u); return (int)i; }
    }
    return -1;
}

// 0 = the write may happen. Refused: any register outside the mailboxes, and a response register set to anything but 0.
static inline int nmsmn_check_write(struct nmsmn_state *s, uint32_t addr, uint32_t value)
{
    int which = 0;
    int i = nmsmn_mailbox_of(addr, &which);
    if (i < 0)
        return -1;
    if (which == 1 && value != 0)
        return -1;
    if (which == 0) {                           // a new message ends whatever request was in flight
        s->pending = -1; s->resp_ok = 0; s->have_lo = 0;
        if (kNmsmnMailboxes[i].psmu && (value == NMSMN_MSG_TABLE_ADDR32 || value == NMSMN_MSG_TABLE_ADDR64)) {
            s->pending = i;
            s->wide = value == NMSMN_MSG_TABLE_ADDR64;
            s->table_valid = 0;                 // the old address stays unreadable until the SMU answers again
        }
    }
    return 0;
}

// Called with the value a permitted read returned.
static inline void nmsmn_note_read(struct nmsmn_state *s, uint32_t addr, uint32_t value)
{
    int which = 0;
    int i = nmsmn_mailbox_of(addr, &which);
    if (i < 0 || i != s->pending)
        return;
    if (which == 1) {
        s->resp_ok = value == NMSMN_RESP_OK;
    } else if (which == 2 && s->resp_ok) {      // argument 0
        s->lo = value; s->have_lo = 1;
        if (!s->wide && value != 0) { s->table = value; s->table_valid = 1; s->pending = -1; }
    } else if (which == 3 && s->resp_ok && s->wide && s->have_lo) {   // argument 1
        uint64_t a = ((uint64_t)value << 32) | s->lo;
        if (a != 0 && a < (1ull << 48)) { s->table = a; s->table_valid = 1; }
        s->pending = -1;
    }
}

// Whether 'len' bytes at 'off' of the remembered table may be read: inside NMSMN_PM_MAX, and only after the SMU gave one.
static inline int nmsmn_table_range_ok(const struct nmsmn_state *s, uint32_t off, uint32_t len)
{
    return s->table_valid && len > 0 && len <= NMSMN_PM_MAX && off <= NMSMN_PM_MAX - len;
}
#endif

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
// The SMU's power table lives in DRAM at an address the SMU itself reports. A client NEVER takes part in that exchange:
//  * a client may not send the table-address message to the platform SMU (nmsmn_check_write refuses it), so it cannot
//    steer the reply, and it cannot have the kext latch values it wrote itself into the argument registers;
//  * the kext sends that message itself (NMSMN::tableInit), under its lock, with the argument registers it chose, polls the
//    response register with a bound, reads the answer, and keeps only that result;
//  * the answer is used only if nmsmn_table_accept approves it: page aligned, under 2^48, and entirely inside a region the
//    firmware's own memory map marks reserved (not RAM the kernel manages). The readable window is that region's remainder,
//    capped at 64 KiB.
// Message 0xB answers a 32-bit address in argument 0 (argument 0 is set to 3 first); message 0x66 a 64-bit address in
// arguments 0 and 1 (RyzenAdj request_table_addr, FlyGoat/RyzenAdj v0.19.0, LGPL-3.0).
#define NMSMN_MSG_TABLE_ADDR32 0xBu
#define NMSMN_MSG_TABLE_ADDR64 0x66u
#define NMSMN_RESP_OK 1u
#define NMSMN_PM_MAX 0x10000u
#define NMSMN_PAGE 0x1000u

struct nmsmn_state {
    int table_valid;
    uint64_t table;         // the SMU's own answer, approved
    uint32_t limit;         // readable bytes from it
};

static inline void nmsmn_state_init(struct nmsmn_state *s)
{
    s->table_valid = 0; s->table = 0; s->limit = 0;
}

// which = 0 message, 1 response, 2.. argument (2 + index); the mailbox index, or -1
static inline int nmsmn_mailbox_of(uint32_t addr, int *which)
{
    if (addr & 3u)
        return -1;
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        if (addr == m->msg) { *which = 0; return (int)i; }
        if (addr == m->rsp) { *which = 1; return (int)i; }
        if (addr >= m->arg && addr < m->arg + 4u * NMSMN_ARGS) { *which = 2 + (int)((addr - m->arg) / 4u); return (int)i; }
    }
    return -1;
}

// 0 = the write may happen. Refused: any register outside the mailboxes, a response register set to anything but 0, and the
// table-address message to a platform-SMU message register.
static inline int nmsmn_check_write(uint32_t addr, uint32_t value)
{
    int which = 0;
    int i = nmsmn_mailbox_of(addr, &which);
    if (i < 0)
        return -1;
    if (which == 1 && value != 0)
        return -1;
    if (which == 0 && kNmsmnMailboxes[i].psmu && (value == NMSMN_MSG_TABLE_ADDR32 || value == NMSMN_MSG_TABLE_ADDR64))
        return -1;
    return 0;
}

// The message the kext itself sends to learn the table address, by CPU family and model (RyzenAdj cpuid.c / api.c).
// Returns 0 and fills *msg (0xB or 0x66), *arg0 (the value argument 0 starts with) and *wide, or -1 when the CPU has none.
// Van Gogh, Mendocino, Dragon Range and Fire Range have none because RyzenAdj v0.19.0 has none: its request_table_ver_and_size
// and request_table_addr (lib/api.c) switch on the family and fall to "not supported on this family" for exactly those four,
// while its limit setters (stapm, fast, slow, ...) do handle them. So on those CPUs limits work through the mailboxes and only
// the power-table readout (--info) is unavailable, as upstream.
static inline int nmsmn_table_message(unsigned family, unsigned model, uint32_t *msg, uint32_t *arg0, int *wide)
{
    *arg0 = 0;
    if (family == 0x17 && (model == 17 || model == 24 || model == 32)) {               // Raven, Picasso, Dali
        *msg = NMSMN_MSG_TABLE_ADDR32; *arg0 = 3; *wide = 0; return 0;
    }
    if ((family == 0x17 && (model == 96 || model == 104)) ||                           // Renoir, Lucienne
        (family == 0x19 && (model == 80 || model == 64 || model == 68 || model == 116 || model == 117 || model == 120)) ||
        (family == 0x1A && (model == 32 || model == 36 || model == 96 || model == 112))) {   // Cezanne .. Strix Halo
        *msg = NMSMN_MSG_TABLE_ADDR64; *wide = 1; return 0;
    }
    return -1;
}

// ---- the firmware's memory map ---------------------------------------------------------------------------------
// UEFI memory descriptors as boot.efi hands them to the kernel: type (u32), padding, physical start (u64), virtual start
// (u64), page count (u64), attributes (u64); 'stride' is the descriptor size the firmware reported.
enum { kEfiReserved = 0, kEfiRuntimeCode = 5, kEfiRuntimeData = 6, kEfiAcpiNvs = 10 };

static inline uint32_t nmsmn_rd32(const unsigned char *p) { return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24); }
static inline uint64_t nmsmn_rd64(const unsigned char *p) { return nmsmn_rd32(p) | ((uint64_t)nmsmn_rd32(p + 4) << 32); }

// 1 and *limit set when [addr, addr + 1) lies in a descriptor whose type the SMU's table can be in (reserved, runtime
// services, ACPI NVS). Everything else - conventional memory, loader and boot services memory, ACPI reclaim, unknown - is
// RAM the kernel may manage, and is refused. A descriptor list that is malformed (stride too small, truncated) refuses all.
static inline int nmsmn_reserved_region(const unsigned char *map, uint32_t size, uint32_t stride, uint64_t addr, uint64_t *end)
{
    if (map == 0 || stride < 40 || size < stride)
        return 0;
    for (uint32_t off = 0; off + stride <= size; off += stride) {
        uint32_t type = nmsmn_rd32(map + off);
        uint64_t start = nmsmn_rd64(map + off + 8), pages = nmsmn_rd64(map + off + 24);
        if (pages == 0 || pages > (1ull << 36) || start > (1ull << 48))
            continue;
        uint64_t stop = start + pages * NMSMN_PAGE;
        if (addr >= start && addr < stop) {
            if (type == kEfiReserved || type == kEfiRuntimeCode || type == kEfiRuntimeData || type == kEfiAcpiNvs) {
                *end = stop;
                return 1;
            }
            return 0;           // the first descriptor that holds the address decides; managed RAM refuses
        }
    }
    return 0;
}

// Approves the SMU's answer 'addr' and records it. 0 = accepted.
static inline int nmsmn_table_accept(struct nmsmn_state *s, uint64_t addr, const unsigned char *map, uint32_t size, uint32_t stride)
{
    uint64_t end = 0;
    nmsmn_state_init(s);
    if (addr == 0 || (addr & (NMSMN_PAGE - 1)) || addr >= (1ull << 48))
        return -1;
    if (!nmsmn_reserved_region(map, size, stride, addr, &end))
        return -1;
    uint64_t room = end - addr;
    s->table = addr;
    s->limit = room > NMSMN_PM_MAX ? NMSMN_PM_MAX : (uint32_t)room;
    s->table_valid = 1;
    return 0;
}

// Whether 'len' bytes at 'off' of the approved table may be read.
static inline int nmsmn_table_range_ok(const struct nmsmn_state *s, uint32_t off, uint32_t len)
{
    return s->table_valid && len > 0 && len <= s->limit && off <= s->limit - len;
}
#endif

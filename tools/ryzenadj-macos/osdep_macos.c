// SPDX-License-Identifier: LGPL-3.0-or-later
/*
 * RyzenAdj operating-system layer for macOS, written by NullMoth Systems. It is added to an unmodified RyzenAdj
 * checkout (see build.sh) and, with it, forms a combined work under RyzenAdj's licence (LGPL-3.0); the complete source
 * is this file plus the pinned upstream tag.
 *
 * RyzenAdj reaches the SMU through the host bridge's SMN index/data registers. On macOS that needs the NMSMN kext
 * (boot argument nmsmn=1), whose user client reads and writes only the SMU mailbox registers and reads the power table at
 * the address the SMU reported. The kext lets only a process named ryzenadj that carries the entitlement
 * com.nullmoth.ryzenadj, run as root, connect (build.sh signs the binary that way).
 */
#include <IOKit/IOKitLib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "nb_smu_ops.h"

enum { kRead = 0, kWrite = 1, kTableInit = 3 };

/*
 * The power table's address. RyzenAdj learns it by sending message 0xB or 0x66 to the platform SMU and reading the answer.
 * NMSMN does not let a client take part in that exchange (a client could steer it into reading any physical memory): the
 * kext sends the message itself, keeps the address, and only ever reads the table at it. So when RyzenAdj writes one of
 * those messages, this layer asks the kext to do it and then plays the exchange back to RyzenAdj: response OK, and a
 * placeholder non-zero address that RyzenAdj passes on and nothing uses.
 */
static const uint32_t kPsmuMsg[2] = { 0x3B10A20, 0x3B10524 };
static const uint32_t kPsmuRsp[2] = { 0x3B10A80, 0x3B10570 };
static const uint32_t kPsmuArg[2] = { 0x3B10A88, 0x3B10A40 };
static struct { int set; int active; int ok; } g_shadow = { 0, 0, 0 };

os_access_obj_t *init_os_access_obj(void)
{
    io_service_t service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NMSMN"));
    if (service == IO_OBJECT_NULL) {
        fprintf(stderr, "NMSMN is not loaded: install the NMSMN kext and boot with nmsmn=1 (Ryzen only)\n");
        return NULL;
    }
    io_connect_t conn = IO_OBJECT_NULL;
    kern_return_t kr = IOServiceOpen(service, mach_task_self(), 0, &conn);
    IOObjectRelease(service);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "cannot open NMSMN (0x%x): run as root\n", kr);
        return NULL;
    }
    os_access_obj_t *obj = calloc(1, sizeof *obj);
    if (obj == NULL) {
        IOServiceClose(conn);
        return NULL;
    }
    obj->access.kmod.smn_fd = (int)conn;    // the connection is kept in the slot the Linux layer uses for a descriptor
    return obj;
}

void free_os_access_obj(os_access_obj_t *obj)
{
    if (obj == NULL)
        return;
    IOServiceClose((io_connect_t)obj->access.kmod.smn_fd);
    free(obj);
}

uint32_t smn_reg_read(const os_access_obj_t *obj, uint32_t addr)
{
    if (g_shadow.active) {
        if (addr == kPsmuRsp[g_shadow.set])
            return g_shadow.ok ? 1u : 0xFFu;                 // success, or "failed"
        if (addr == kPsmuArg[g_shadow.set])
            return g_shadow.ok ? 0x1000u : 0u;               // a placeholder address, not the real one
        if (addr == kPsmuArg[g_shadow.set] + 4u)
            return 0u;
    }
    uint64_t in = addr & ~3u, out = 0;
    uint32_t outCount = 1;
    kern_return_t kr = IOConnectCallScalarMethod((io_connect_t)obj->access.kmod.smn_fd, kRead, &in, 1, &out, &outCount);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "SMN read of 0x%x refused (0x%x)\n", addr, kr);
        return 0xFFFFFFFFu;
    }
    return (uint32_t)out;
}

void smn_reg_write(const os_access_obj_t *obj, uint32_t addr, uint32_t data)
{
    for (int i = 0; i < 2; i++) {
        if (addr != kPsmuMsg[i])
            continue;
        g_shadow.active = 0;                                 // any new message ends a played-back exchange
        if (data == 0xB || data == 0x66) {
            uint64_t limit = 0;
            uint32_t n = 1;
            kern_return_t kr = IOConnectCallScalarMethod((io_connect_t)obj->access.kmod.smn_fd, kTableInit, NULL, 0, &limit, &n);
            g_shadow.set = i;
            g_shadow.active = 1;
            g_shadow.ok = kr == KERN_SUCCESS;
            if (kr != KERN_SUCCESS)
                fprintf(stderr, "the power table is not available (0x%x)\n", kr);
            return;
        }
    }
    uint64_t in[2] = { addr, data };
    kern_return_t kr = IOConnectCallScalarMethod((io_connect_t)obj->access.kmod.smn_fd, kWrite, in, 2, NULL, NULL);
    if (kr != KERN_SUCCESS)
        fprintf(stderr, "SMN write of 0x%x refused (0x%x)\n", addr, kr);
}

// The power table (--info): NMSMN reads it at the address the SMU itself reported, never at one we name, so the address
// RyzenAdj passes is not used. A refused read fails the call and nothing else.
enum { kTable = 2, kChunk = 4096 };

static int read_table(const os_access_obj_t *obj, size_t off, void *buffer, size_t len)
{
    uint64_t in[2] = { off, len };
    size_t outSize = len;
    kern_return_t kr = IOConnectCallMethod((io_connect_t)obj->access.kmod.smn_fd, kTable, in, 2, NULL, 0, NULL, NULL,
                                           buffer, &outSize);
    return kr == KERN_SUCCESS && outSize == len ? 0 : -1;
}

static const os_access_obj_t *g_table_obj;

int init_mem_obj(os_access_obj_t *os_access, uintptr_t physAddr)
{
    (void)physAddr;
    char probe[8];
    if (read_table(os_access, 0, probe, sizeof probe) != 0)
        return -1;
    g_table_obj = os_access;
    return 0;
}

int copy_pm_table(const os_access_obj_t *obj, void *buffer, size_t size)
{
    if (obj == NULL || buffer == NULL || size == 0 || size > 0x10000)
        return -1;
    for (size_t off = 0; off < size; off += kChunk) {
        size_t n = size - off < kChunk ? size - off : kChunk;
        if (read_table(obj, off, (char *)buffer + off, n) != 0)
            return -1;
    }
    return 0;
}

int compare_pm_table(const void *buffer, size_t size)
{
    char probe[24];
    if (g_table_obj == NULL || size > sizeof probe || read_table(g_table_obj, 0, probe, size) != 0)
        return -1;
    return memcmp(buffer, probe, size);
}

bool is_using_smu_driver(void)
{
    return false;
}

// SPDX-License-Identifier: LGPL-3.0-or-later
/*
 * RyzenAdj operating-system layer for macOS, written by NullMoth Systems. It is added to an unmodified RyzenAdj
 * checkout (see build.sh) and, with it, forms a combined work under RyzenAdj's licence (LGPL-3.0); the complete source
 * is this file plus the pinned upstream tag.
 *
 * RyzenAdj reaches the SMU through the host bridge's SMN index/data registers. On macOS that needs the NMSMN kext
 * (boot argument nmsmn=1), whose user client reads and writes only the SMU mailbox registers. The SMU power table
 * (--info) needs physical memory, which NMSMN does not give: those calls fail and setting limits is unaffected.
 */
#include <IOKit/IOKitLib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "nb_smu_ops.h"

enum { kRead = 0, kWrite = 1 };

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
    uint64_t in[2] = { addr, data };
    kern_return_t kr = IOConnectCallScalarMethod((io_connect_t)obj->access.kmod.smn_fd, kWrite, in, 2, NULL, NULL);
    if (kr != KERN_SUCCESS)
        fprintf(stderr, "SMN write of 0x%x refused (0x%x)\n", addr, kr);
}

// No physical-memory access on purpose: the power table (--info) is not available.
int init_mem_obj(os_access_obj_t *os_access, uintptr_t physAddr)
{
    (void)os_access; (void)physAddr;
    return -1;
}

int copy_pm_table(const os_access_obj_t *obj, void *buffer, size_t size)
{
    (void)obj; (void)buffer; (void)size;
    return -1;
}

int compare_pm_table(const void *buffer, size_t size)
{
    (void)buffer; (void)size;
    return -1;
}

bool is_using_smu_driver(void)
{
    return false;
}

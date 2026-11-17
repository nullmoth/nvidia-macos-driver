/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include <stdint.h>

/* Optional, statistics-only connection on the selected NVAccel. No RM commands,
 * mappings, process IDs or rendering state cross this interface. */
#define NV_GPU_ACCOUNTING_VERSION 1u
#define NV_GPU_ACCOUNTING_PROPERTY "NVAccelGPUAccounting"
#define NV_GPU_ACCOUNTING_CLIENT 0x4e560002u
#define NV_GPU_ACCOUNTING_REPORT 0u

typedef struct {
    uint32_t version, reserved;
    uint64_t sequence;
    uint64_t busy_ns;             /* cumulative union of completed GPU intervals */
    uint64_t last_submit_absolute; /* mach_absolute_time, not continuous time */
} nv_gpu_accounting_report;

typedef struct {
    uint64_t sequence, busy_ns, last_submit_absolute;
} nv_gpu_accounting_counter;

/* Caller serializes access and supplies its connection's creation time. A
 * report can only charge its own connection; even a bad reporter cannot claim
 * more than that connection's lifetime. Failed validation is transactional. */
static inline int nv_gpu_accounting_accept(nv_gpu_accounting_counter *counter,
    const nv_gpu_accounting_report *report, uint64_t opened_absolute,
    uint64_t now_absolute, uint64_t elapsed_ns)
{
    if (report->version != NV_GPU_ACCOUNTING_VERSION || report->reserved ||
        report->sequence <= counter->sequence || report->busy_ns < counter->busy_ns ||
        report->busy_ns > elapsed_ns || !report->busy_ns ||
        report->last_submit_absolute < opened_absolute ||
        report->last_submit_absolute > now_absolute ||
        report->last_submit_absolute < counter->last_submit_absolute)
        return 0;
    counter->sequence = report->sequence;
    counter->busy_ns = report->busy_ns;
    counter->last_submit_absolute = report->last_submit_absolute;
    return 1;
}

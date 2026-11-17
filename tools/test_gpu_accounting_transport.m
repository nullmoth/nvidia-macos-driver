/* Exercise the production background reporter against a controlled sink.
 * No driver connection, GPU work or installation; run each mode in a new process.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#import <Foundation/Foundation.h>
#import <IOKit/IOKitLib.h>
#include <dispatch/dispatch.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <math.h>
#include <time.h>
#include <mach/mach_time.h>
#include <vulkan/vulkan_core.h>
#include "../plugin/nvmtl_vk.h"
#include "../kexts/NVRM/nvrm_gpu_accounting_abi.h"

static _Atomic unsigned properties, opens, reports, closes;
static int capability = 1, refuse_report;
static uint64_t opened_absolute;
static nv_gpu_accounting_counter sink;
static CFTypeRef fake_property(io_registry_entry_t service, CFStringRef key, CFAllocatorRef allocator, IOOptionBits options) {
    (void)service; (void)key; (void)allocator; (void)options;
    atomic_fetch_add(&properties, 1);
    return capability ? CFNumberCreate(kCFAllocatorDefault, kCFNumberIntType, &capability) : NULL;
}
static kern_return_t fake_open(io_service_t service, task_port_t task, uint32_t type, io_connect_t *connection) {
    assert(service == 123 && task == mach_task_self() && type == NV_GPU_ACCOUNTING_CLIENT);
    atomic_fetch_add(&opens, 1); *connection = 456; opened_absolute = mach_absolute_time(); return KERN_SUCCESS;
}
static kern_return_t fake_close(io_connect_t connection) {
    assert(connection == 456); atomic_fetch_add(&closes, 1); return KERN_SUCCESS;
}
static kern_return_t fake_report(mach_port_t connection, uint32_t selector, const void *input, size_t size,
    void *output, size_t *output_size) {
    assert(connection == 456 && selector == NV_GPU_ACCOUNTING_REPORT);
    assert(size == sizeof(nv_gpu_accounting_report) && !output && !output_size);
    if (refuse_report) { atomic_fetch_add(&reports, 1); return kIOReturnNotAttached; }
    const uint64_t now = mach_absolute_time(); mach_timebase_info_data_t scale; mach_timebase_info(&scale);
    const uint64_t elapsed = (now - opened_absolute) * scale.numer / scale.denom;
    assert(nv_gpu_accounting_accept(&sink, input, opened_absolute, now, elapsed));
    atomic_fetch_add_explicit(&reports, 1, memory_order_release); return KERN_SUCCESS;
}
static void nvlog(const char *format, ...) { (void)format; }
int nvmtl_vk_interval_seconds(const nvmtl_gpu_interval *timing, double *start, double *end) {
    if (!timing->valid) return -1;
    *start = timing->start_tick / 1e9; *end = timing->end_tick / 1e9; return 0;
}
#define IORegistryEntryCreateCFProperty fake_property
#define IOServiceOpen fake_open
#define IOServiceClose fake_close
#define IOConnectCallStructMethod fake_report
#include "../plugin/nvmtl_gpu_accounting.c"
#undef IORegistryEntryCreateCFProperty
#undef IOServiceOpen
#undef IOServiceClose
#undef IOConnectCallStructMethod

static uint64_t base_ns, submitted;
static void *complete_many(void *argument) {
    uintptr_t index = (uintptr_t)argument;
    nvmtl_gpu_interval interval = {0}; interval.valid = 1;
    interval.start_tick = base_ns + 1000 * index;
    interval.end_tick = interval.start_tick + 2000;
    interval.host_after_ns = base_ns + 5000;
    for (unsigned i = 0; i < 20000; i++) nvmtl_gpu_accounting_complete(&interval, submitted);
    return NULL;
}
static void await_report(unsigned wanted) {
    for (unsigned i = 0; i < 100 && atomic_load_explicit(&reports, memory_order_acquire) < wanted; i++) usleep(10000);
    assert(atomic_load(&reports) == wanted);
}
int main(int argc, char **argv) { @autoreleasepool {
    if (argc != 2) return 2;
    unsetenv("NVMTL_GPU_ACCOUNTING");
    const int opted_in = !strcmp(argv[1], "missing") || !strcmp(argv[1], "unknown") ||
        !strcmp(argv[1], "working") || !strcmp(argv[1], "failure");
    if (opted_in) setenv("NVMTL_GPU_ACCOUNTING", "1", 1);
    if (!strcmp(argv[1], "missing")) capability = 0;
    else if (!strcmp(argv[1], "unknown")) capability = 2;
    else if (!strcmp(argv[1], "disabled")) setenv("NVMTL_GPU_ACCOUNTING", "0", 1);
    else if (!strcmp(argv[1], "empty")) setenv("NVMTL_GPU_ACCOUNTING", "", 1);
    else if (!strcmp(argv[1], "invalid")) setenv("NVMTL_GPU_ACCOUNTING", "1junk", 1);
    else if (!strcmp(argv[1], "default")) { /* unset means disabled */ }
    else if (!strcmp(argv[1], "failure")) refuse_report = 1;
    else if (strcmp(argv[1], "working")) return 2;
    nvmtl_gpu_accounting_open(123); nvmtl_gpu_accounting_open(123);
    const int enabled = capability == 1 && opted_in;
    assert(atomic_load(&properties) == (unsigned)opted_in);
    assert(atomic_load(&opens) == (unsigned)enabled);
    if (!enabled) {
        complete_many(NULL);
        assert(!atomic_load(&reports) && !atomic_load(&g_accounting_on) &&
            !g_accounting_queue && !g_accounting_pending);
        printf("PASS: %s endpoint leaves reporting inactive\n", argv[1]); return 0;
    }
    base_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW); submitted = mach_absolute_time();
    usleep(1000); // synthetic timestamps are completed, never future work
    nvmtl_gpu_interval invalid = {0}; nvmtl_gpu_accounting_complete(&invalid, submitted);
    assert(!g_accounting_pending);
    pthread_t threads[4];
    const uint64_t started = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    for (uintptr_t i = 0; i < 4; i++) assert(!pthread_create(&threads[i], NULL, complete_many, (void *)i));
    for (unsigned i = 0; i < 4; i++) assert(!pthread_join(threads[i], NULL));
    double elapsed = (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - started) / 1e9;
    await_report(1);
    if (refuse_report) {
        for (unsigned i = 0; i < 100 && !atomic_load(&closes); i++) usleep(10000);
        assert(atomic_load(&closes) == 1 && !atomic_load(&g_accounting_on));
        complete_many(NULL); usleep(300000);
        assert(atomic_load(&reports) == 1);
        puts("PASS: failed telemetry closes only its connection and disables retries"); return 0;
    }
    assert(sink.busy_ns >= 4998 && sink.busy_ns <= 5002 && sink.last_submit_absolute == submitted);
    usleep(350000); assert(atomic_load(&reports) == 1); // no idle timer/reports
    nvmtl_gpu_interval tail = {0}; tail.valid = 1;
    tail.start_tick = base_ns + 10000; tail.end_tick = base_ns + 12000; tail.host_after_ns = base_ns + 12000;
    nvmtl_gpu_accounting_complete(&tail, submitted); await_report(2);
    assert(sink.busy_ns >= 6997 && sink.busy_ns <= 7003 && !atomic_load(&closes));
    printf("PASS: concurrent completion union, batching, idle tail; 80000 callbacks in %.6f s (controlled transport)\n", elapsed);
    return 0;
} }

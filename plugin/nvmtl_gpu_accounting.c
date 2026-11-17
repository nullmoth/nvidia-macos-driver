/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
/* Included by nvmtl_vk.c. All IOKit reporting runs off the rendering/completion
 * threads. The connection and counters are optional for rendering. */
#include "nvmtl_gpu_accounting.h"
#include "../kexts/NVRM/nvrm_gpu_accounting_abi.h"

static pthread_mutex_t g_accounting_lock = PTHREAD_MUTEX_INITIALIZER;
static nvmtl_gpu_accounting_state g_accounting;
static io_connect_t g_accounting_connection;
static dispatch_queue_t g_accounting_queue;
static _Atomic int g_accounting_on;
static int g_accounting_tried, g_accounting_pending;
static pid_t g_accounting_pid;
static uint64_t g_accounting_sequence, g_accounting_open_ns;

static void nvmtl_gpu_accounting_open(io_service_t accelerator)
{
    pthread_mutex_lock(&g_accounting_lock);
    if (g_accounting_tried) { pthread_mutex_unlock(&g_accounting_lock); return; }
    g_accounting_tried = 1;
    const char *setting = getenv("NVMTL_GPU_ACCOUNTING");
    // Disabled by default: opening a Metal device must not create a reporting
    // client or background queue unless the user explicitly opts in.
    if (setting && !strcmp(setting, "1")) {
        CFTypeRef property = IORegistryEntryCreateCFProperty(accelerator,
            CFSTR(NV_GPU_ACCOUNTING_PROPERTY), kCFAllocatorDefault, 0);
        int version = 0;
        if (property && CFGetTypeID(property) == CFNumberGetTypeID())
            CFNumberGetValue((CFNumberRef)property, kCFNumberIntType, &version);
        if (property) CFRelease(property);
        if (version == NV_GPU_ACCOUNTING_VERSION &&
            IOServiceOpen(accelerator, mach_task_self(), NV_GPU_ACCOUNTING_CLIENT,
                &g_accounting_connection) == KERN_SUCCESS) {
            g_accounting_queue = dispatch_queue_create("com.nullmoth.nvmtl.gpu-accounting",
                dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_UTILITY, 0));
            g_accounting_pid = getpid();
            g_accounting_open_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
            atomic_store_explicit(&g_accounting_on, 1, memory_order_release);
            nvlog("vk: per-process GPU accounting enabled (completed timestamps, batched reports)");
        }
    }
    pthread_mutex_unlock(&g_accounting_lock);
}

static void nvmtl_gpu_accounting_flush(void)
{
    nv_gpu_accounting_report report = { .version = NV_GPU_ACCOUNTING_VERSION };
    pthread_mutex_lock(&g_accounting_lock);
    g_accounting_pending = 0;
    report.sequence = ++g_accounting_sequence;
    report.busy_ns = g_accounting.busy_ns;
    report.last_submit_absolute = g_accounting.last_submit_absolute;
    const io_connect_t connection = g_accounting_connection;
    pthread_mutex_unlock(&g_accounting_lock);
    if (!connection) return;
    const kern_return_t result = IOConnectCallStructMethod(connection, NV_GPU_ACCOUNTING_REPORT,
        &report, sizeof report, NULL, NULL);
    if (result != KERN_SUCCESS) {
        atomic_store_explicit(&g_accounting_on, 0, memory_order_release);
        pthread_mutex_lock(&g_accounting_lock);
        g_accounting_connection = IO_OBJECT_NULL;
        pthread_mutex_unlock(&g_accounting_lock);
        IOServiceClose(connection);
        nvlog("vk: GPU accounting unavailable (0x%x); rendering continues", result);
    }
}

static void nvmtl_gpu_accounting_complete(const nvmtl_gpu_interval *timing, uint64_t submitted_absolute)
{
    if (!atomic_load_explicit(&g_accounting_on, memory_order_acquire) || getpid() != g_accounting_pid)
        return;
    double start, end;
    if (nvmtl_vk_interval_seconds(timing, &start, &end)) return;
    long double a = (long double)start * 1000000000.0L;
    long double b = (long double)end * 1000000000.0L;
    if (!isfinite(a) || !isfinite(b) || a < 1 || b >= (long double)UINT64_MAX || b <= a)
        return;
    // Calibration uncertainty must not charge time before the connection was
    // opened or after this completion was observed by the host.
    if (a < g_accounting_open_ns) a = g_accounting_open_ns;
    if (b > timing->host_after_ns) b = timing->host_after_ns;
    if (b <= a) return;
    pthread_mutex_lock(&g_accounting_lock);
    const int added = nvmtl_gpu_accounting_add(&g_accounting, (uint64_t)a, (uint64_t)b,
        submitted_absolute);
    if (added && !g_accounting_pending && g_accounting_connection) {
        g_accounting_pending = 1;
        // A delayed tail report also delivers the last completed buffer when
        // the application goes idle. There is no idle polling timer.
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 250 * NSEC_PER_MSEC),
            g_accounting_queue, ^{ nvmtl_gpu_accounting_flush(); });
    }
    pthread_mutex_unlock(&g_accounting_lock);
}

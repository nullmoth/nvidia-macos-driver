/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
#include <kern/thread_call.h>
#include <sys/sysctl.h>
#include <mach/semaphore.h>
#include <mach/task.h>
#include <sys/proc.h>
extern "C" {
#include "nvidia-modeset-os-interface.h"
#include "nv-gpu-info.h"
#include "nvUnixVersion.h"
}

static inline void nvu_memset(void *p, int c, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(p), "+c"(n) : "a"(c) : "memory"); }
static inline void nvu_memcpy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }
extern "C" nv_state_t *nv_xnu_gpu_state(void);
extern "C" void *nv_xnu_gpu_os_device(void);

static IOLock *gKmsLock;
static bool    gKmsLoaded;
struct nvkms_timer_t;
static IOSimpleLock *gKmsTimerQueueLock;
static thread_call_t gKmsTimerWorker;
static nvkms_timer_t *gKmsTimerHead, *gKmsTimerTail;
static SInt64 gKmsTimerDepth, gKmsTimerMaxDepth, gKmsTimerExecuted;
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_kms_timer_depth, CTLFLAG_RD, &gKmsTimerDepth, "queued NVKMS timers");
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_kms_timer_max_depth, CTLFLAG_RD, &gKmsTimerMaxDepth, "peak queued NVKMS timers");
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_kms_timer_executed, CTLFLAG_RD, &gKmsTimerExecuted, "completed NVKMS timer callbacks");
static void kms_timer_drain(thread_call_param_t, thread_call_param_t);

struct nvkms_per_open {
    void *data;
    enum NvKmsClientType type;
    struct NvKmsKapiDevice *device;
    thread_call_t eventCall;
    bool eventBusy, eventWanted, closing;
    volatile int eventsAvailable;
};

static void kms_kapi_event_fire(thread_call_param_t p0, thread_call_param_t)
{
    struct nvkms_per_open *popen = (struct nvkms_per_open *)p0;
    for (;;) {
        IOLockLock(gKmsLock);
        bool run = !popen->closing;
        popen->eventWanted = false;
        struct NvKmsKapiDevice *device = popen->device;
        IOLockUnlock(gKmsLock);
        if (run) nvKmsKapiHandleEventQueueChange(device);
        IOLockLock(gKmsLock);
        if (!popen->closing && popen->eventWanted) {
            IOLockUnlock(gKmsLock);
            continue;
        }
        popen->eventBusy = false;
        IOLockWakeup(gKmsLock, popen, true);
        IOLockUnlock(gKmsLock);
        return;
    }
}

static struct nvkms_per_open *kms_open_common(enum NvKmsClientType type, struct NvKmsKapiDevice *device, int pid)
{
    struct nvkms_per_open *popen = (struct nvkms_per_open *)nvkms_alloc(sizeof *popen, NV_TRUE);
    if (!popen) return NULL;
    popen->type = type; popen->device = device;
    IOLockLock(gKmsLock);
    popen->data = nvKmsOpen((NvU32)pid, type, popen);
    IOLockUnlock(gKmsLock);
    if (!popen->data) { nvkms_free(popen, sizeof *popen); return NULL; }
    if (type == NVKMS_CLIENT_KERNEL_SPACE) popen->eventCall = thread_call_allocate_with_options(kms_kapi_event_fire, popen, THREAD_CALL_PRIORITY_KERNEL, THREAD_CALL_OPTIONS_ONCE);
    return popen;
}

static void kms_close_common(struct nvkms_per_open *popen)
{
    IOLockLock(gKmsLock);
    nvKmsClose(popen->data);
    popen->data = NULL;
    popen->closing = true;
    if (popen->eventCall && popen->eventBusy) {
        if (thread_call_cancel(popen->eventCall)) popen->eventBusy = false;
        while (popen->eventBusy) IOLockSleep(gKmsLock, popen, THREAD_UNINT);
    }
    IOLockUnlock(gKmsLock);
    if (popen->eventCall) thread_call_free(popen->eventCall);
    nvkms_free(popen, sizeof *popen);
}

static NvBool kms_ioctl_common(struct nvkms_per_open *popen, NvU32 cmd, NvU64 address, size_t size)
{
    NvBool ret = NV_FALSE;
    IOLockLock(gKmsLock);
    if (popen->data) ret = nvKmsIoctl(popen->data, cmd, address, size);
    IOLockUnlock(gKmsLock);
    return ret;
}

extern "C" {
NvBool nvkms_xnu_load(void)
{
    if (gKmsLoaded) return NV_TRUE;
    if (!gKmsLock) gKmsLock = IOLockAlloc();
    if (!gKmsLock) return NV_FALSE;
    if (!gKmsTimerQueueLock) gKmsTimerQueueLock = IOSimpleLockAlloc();
    if (!gKmsTimerQueueLock) return NV_FALSE;
    if (!gKmsTimerWorker) gKmsTimerWorker = thread_call_allocate_with_options(
        kms_timer_drain, NULL, THREAD_CALL_PRIORITY_KERNEL, THREAD_CALL_OPTIONS_ONCE);
    if (!gKmsTimerWorker) return NV_FALSE;
    IOLockLock(gKmsLock);
    NvBool ok = nvKmsModuleLoad();
    IOLockUnlock(gKmsLock);
    kprintf("NVRM-xnu: nvKmsModuleLoad -> %u\n", ok);
    if (!ok) return NV_FALSE;
    gKmsLoaded = true;
    sysctl_register_oid(&sysctl__debug_nvrm_kms_timer_depth);
    sysctl_register_oid(&sysctl__debug_nvrm_kms_timer_max_depth);
    sysctl_register_oid(&sysctl__debug_nvrm_kms_timer_executed);
    kprintf("NVRM-xnu: NVKMS timers use serial ONCE worker; event callbacks ONCE at kernel priority\n");
    nv_gpu_info_t info[1];
    if (nvkms_enumerate_gpus(info) == 1) { nvKmsKapiProbe(&info[0]); kprintf("NVRM-xnu: nvKmsKapiProbe gpu_id 0x%x\n", info[0].gpu_id); }
    return NV_TRUE;
}
struct nvkms_per_open *nvkms_xnu_open(int pid) { return gKmsLoaded ? kms_open_common(NVKMS_CLIENT_KERNEL_SPACE, NULL, pid) : NULL; }
void nvkms_xnu_close(struct nvkms_per_open *popen) { if (popen) kms_close_common(popen); }
int nvkms_xnu_ioctl(struct nvkms_per_open *popen, NvU32 cmd, void *params, size_t size)
{
    return kms_ioctl_common(popen, cmd, (NvU64)(uintptr_t)params, size) ? 0 : -1;
}
int nvkms_xnu_events_available(struct nvkms_per_open *popen) { return popen ? popen->eventsAvailable : 0; }
static struct NvKmsKapiFunctionsTable gKapiTable;
void *nvkms_xnu_kapi_table(void)
{
    static bool filled;
    if (!filled) { gKapiTable.versionString = NV_VERSION_STRING; if (!nvKmsKapiGetFunctionsTableInternal(&gKapiTable)) return NULL; filled = true; }
    return &gKapiTable;
}
}

extern "C" {

NvBool nvkms_test_fail_alloc_core_channel(enum NvKmsFailAllocCoreChannelMethod) { return NV_FALSE; }
enum NvKmsFrlRateForce nvkms_force_frl_rate(void) { return NVKMS_FRL_RATE_FORCE_NONE; }
NvBool nvkms_conceal_vrr_caps(void)             { return NV_FALSE; }
NvBool nvkms_enhanced_pcon_support(void)        { return NV_FALSE; }
NvBool nvkms_output_rounding_fix(void)          { return NV_TRUE; }
NvBool nvkms_disable_hdmi_frl(void)             { return NV_FALSE; }
NvBool nvkms_disable_vrr_memclk_switch(void)    { return NV_FALSE; }
NvBool nvkms_hdmi_deepcolor(void)               { return NV_TRUE; }
NvBool nvkms_opportunistic_display_sync(void)   { return NV_TRUE; }
enum NvKmsDebugForceColorSpace nvkms_debug_force_color_space(void) { return NVKMS_DEBUG_FORCE_COLOR_SPACE_NONE; }
NvBool nvkms_enable_overlay_layers(void)        { return NV_TRUE; }
NvBool nvkms_debug_logging(void)                { return NV_FALSE; }

static bool gKmsTraceRm = true;
void nvkms_call_rm(void *ops)
{
    nvidia_kernel_rmapi_ops_t *o = (nvidia_kernel_rmapi_ops_t *)ops;
    rm_kernel_rmapi_op(NULL, ops);
    if (!gKmsTraceRm) return;
    switch (o->op) {
    case NV04_ALLOC:          if (o->params.alloc.status) kprintf("NVKMS-rm: ALLOC class 0x%x parent 0x%x -> status 0x%x\n", o->params.alloc.hClass, o->params.alloc.hObjectParent, o->params.alloc.status); break;
    case NV04_CONTROL:        if (o->params.control.status) kprintf("NVKMS-rm: CONTROL obj 0x%x cmd 0x%x -> status 0x%x\n", o->params.control.hObject, o->params.control.cmd, o->params.control.status); break;
    case NV04_DUP_OBJECT:     kprintf("NVKMS-rm: DUP_OBJECT src client 0x%x obj 0x%x -> status 0x%x\n", o->params.dupObject.hClientSrc, o->params.dupObject.hObjectSrc, o->params.dupObject.status); break;
    case NV04_MAP_MEMORY_DMA: if (o->params.mapMemoryDma.status) kprintf("NVKMS-rm: MAP_MEMORY_DMA -> status 0x%x\n", o->params.mapMemoryDma.status); break;
    case NV04_MAP_MEMORY:     if (o->params.mapMemory.status) kprintf("NVKMS-rm: MAP_MEMORY -> status 0x%x\n", o->params.mapMemory.status); break;
    case NV01_FREE:           if (o->params.free.status) kprintf("NVKMS-rm: FREE obj 0x%x -> status 0x%x\n", o->params.free.hObjectOld, o->params.free.status); break;
    case NV04_ALLOC_CONTEXT_DMA: kprintf("NVKMS-rm: ALLOC_CONTEXT_DMA class 0x%x flags 0x%x mem 0x%x limit 0x%llx -> status 0x%x\n", o->params.allocContextDma2.hClass, o->params.allocContextDma2.flags, o->params.allocContextDma2.hMemory, (unsigned long long)o->params.allocContextDma2.limit, o->params.allocContextDma2.status); break;
    case NV04_BIND_CONTEXT_DMA:  kprintf("NVKMS-rm: BIND_CONTEXT_DMA chan 0x%x dma 0x%x -> status 0x%x\n", o->params.bindContextDma.hChannel, o->params.bindContextDma.hCtxDma, o->params.bindContextDma.status); break;
    case NV04_VID_HEAP_CONTROL: if (o->params.pVidHeapControl && o->params.pVidHeapControl->status) kprintf("NVKMS-rm: VID_HEAP_CONTROL fn %u -> status 0x%x\n", o->params.pVidHeapControl->function, o->params.pVidHeapControl->status); break;
    default: break;
    }
}

void *nvkms_alloc(size_t size, NvBool zero)
{
    void *p = kern_os_malloc(size);
    if (p && zero) nvu_memset(p, 0, size);
    return p;
}
void   nvkms_free(void *ptr, size_t) { if (ptr) kern_os_free(ptr); }
void  *nvkms_memset(void *ptr, NvU8 c, size_t size) { nvu_memset(ptr, c, size); return ptr; }
void  *nvkms_memcpy(void *dest, const void *src, size_t n) { nvu_memcpy(dest, src, n); return dest; }
void  *nvkms_memmove(void *dest, const void *src, size_t n) { return memmove(dest, src, n); }
int    nvkms_memcmp(const void *s1, const void *s2, size_t n) { return memcmp(s1, s2, n); }
size_t nvkms_strlen(const char *s) { return strlen(s); }
int    nvkms_strcmp(const char *s1, const char *s2) { return strcmp(s1, s2); }
void   nvkms_usleep(NvU64 usec) { if (usec < 1000) IODelay((unsigned)usec); else IOSleep((unsigned)((usec + 999) / 1000)); }
NvU64  nvkms_get_usec(void) { uint64_t ns; absolutetime_to_nanoseconds(mach_absolute_time(), &ns); return ns / 1000; }
int nvkms_copyin(void *kptr, NvU64 uaddr, size_t n)
{
    if (uaddr >> 63) { nvu_memcpy(kptr, (const void *)(uintptr_t)uaddr, n); return 0; }
    return copyin((user_addr_t)uaddr, kptr, n) ? -14 : 0;
}
int nvkms_copyout(NvU64 uaddr, const void *kptr, size_t n)
{
    if (uaddr >> 63) { nvu_memcpy((void *)(uintptr_t)uaddr, kptr, n); return 0; }
    return copyout(kptr, (user_addr_t)uaddr, n) ? -14 : 0;
}
void nvkms_yield(void) { IOSleep(1); }
void nvkms_dump_stack(void) { kprintf("NVKMS: dump_stack requested\n"); }
NvBool nvkms_syncpt_op(enum NvKmsSyncPtOp, NvKmsSyncPtOpParams *) { return NV_FALSE; }
int nvkms_snprintf(char *str, size_t size, const char *format, ...)
{
    va_list ap; va_start(ap, format); int r = vsnprintf(str, size, format, ap); va_end(ap); return r;
}
int nvkms_vsnprintf(char *str, size_t size, const char *format, va_list ap) { return vsnprintf(str, size, format, ap); }
void nvkms_log(const int level, const char *gpuPrefix, const char *msg)
{
    const char *lvl = level == NVKMS_LOG_LEVEL_ERROR ? "ERROR" : level == NVKMS_LOG_LEVEL_WARN ? "WARN" : "INFO";
    kprintf("NVKMS %s: %s%s\n", lvl, gpuPrefix ? gpuPrefix : "", msg);
    IOLog("NVKMS %s: %s%s\n", lvl, gpuPrefix ? gpuPrefix : "", msg);
}

struct nvkms_ref_ptr { volatile SInt32 refcnt; void *ptr; };
struct nvkms_ref_ptr *nvkms_alloc_ref_ptr(void *ptr)
{
    struct nvkms_ref_ptr *r = (struct nvkms_ref_ptr *)nvkms_alloc(sizeof *r, NV_FALSE);
    if (r) { r->refcnt = 1; r->ptr = ptr; }
    return r;
}
void nvkms_inc_ref(struct nvkms_ref_ptr *r) { OSIncrementAtomic(&r->refcnt); }
void *nvkms_dec_ref(struct nvkms_ref_ptr *r)
{
    void *ptr = r->ptr;
    if (OSDecrementAtomic(&r->refcnt) == 1) nvkms_free(r, sizeof *r);
    return ptr;
}
void nvkms_free_ref_ptr(struct nvkms_ref_ptr *r) { if (r) { r->ptr = NULL; nvkms_dec_ref(r); } }

struct nvkms_timer_t {
    thread_call_t call;
    nvkms_timer_t *next;
    nvkms_timer_proc_t *proc;
    void *dataPtr; NvU32 dataU32;
    NvBool isRefPtr, cancel, complete;
};
static void kms_timer_release(struct nvkms_timer_t *t) { thread_call_free(t->call); kern_os_free(t); }
static void kms_timer_execute(struct nvkms_timer_t *t)
{
    void *dataPtr;
    IOLockLock(gKmsLock);
    if (t->isRefPtr) { dataPtr = nvkms_dec_ref((struct nvkms_ref_ptr *)t->dataPtr); if (!dataPtr) t->cancel = NV_TRUE; }
    else dataPtr = t->dataPtr;
    if (!t->cancel) { t->proc(dataPtr, t->dataU32); t->complete = NV_TRUE; }
    NvBool freeIt = t->isRefPtr || t->cancel;
    IOLockUnlock(gKmsLock);
    if (freeIt) kms_timer_release(t);
    OSAddAtomic64(1, &gKmsTimerExecuted);
}
static void kms_timer_drain(thread_call_param_t, thread_call_param_t)
{
    for (;;) {
        IOSimpleLockLock(gKmsTimerQueueLock);
        nvkms_timer_t *t = gKmsTimerHead;
        if (t) {
            gKmsTimerHead = t->next;
            if (!gKmsTimerHead) gKmsTimerTail = NULL;
            t->next = NULL;
            gKmsTimerDepth--;
        }
        IOSimpleLockUnlock(gKmsTimerQueueLock);
        if (!t) return;
        kms_timer_execute(t);
    }
}
static void kms_timer_fire(thread_call_param_t p0, thread_call_param_t)
{
    nvkms_timer_t *t = (nvkms_timer_t *)p0;
    IOSimpleLockLock(gKmsTimerQueueLock);
    if (gKmsTimerTail) gKmsTimerTail->next = t;
    else gKmsTimerHead = t;
    gKmsTimerTail = t;
    gKmsTimerDepth++;
    if (gKmsTimerDepth > gKmsTimerMaxDepth) gKmsTimerMaxDepth = gKmsTimerDepth;
    IOSimpleLockUnlock(gKmsTimerQueueLock);
    thread_call_enter(gKmsTimerWorker);
}
static struct nvkms_timer_t *kms_timer_start(nvkms_timer_proc_t *proc, void *dataPtr, NvU32 dataU32, NvBool isRefPtr, NvU64 usec)
{
    struct nvkms_timer_t *t = (struct nvkms_timer_t *)kern_os_malloc(sizeof *t);
    if (!t) return NULL;
    nvu_memset(t, 0, sizeof *t);
    t->proc = proc; t->dataPtr = dataPtr; t->dataU32 = dataU32; t->isRefPtr = isRefPtr;
    t->call = thread_call_allocate(kms_timer_fire, t);
    if (!t->call) { kern_os_free(t); return NULL; }
    if (isRefPtr) nvkms_inc_ref((struct nvkms_ref_ptr *)dataPtr);
    if (usec == 0) thread_call_enter(t->call);
    else { uint64_t deadline; clock_interval_to_deadline((uint32_t)(usec > 0xffffffffull ? 0xffffffffu : usec), kMicrosecondScale, &deadline); thread_call_enter_delayed(t->call, deadline); }
    return t;
}
nvkms_timer_handle_t *nvkms_alloc_timer(nvkms_timer_proc_t *proc, void *dataPtr, NvU32 dataU32, NvU64 usec)
{
    return kms_timer_start(proc, dataPtr, dataU32, NV_FALSE, usec);
}
NvBool nvkms_alloc_timer_with_ref_ptr(nvkms_timer_proc_t *proc, struct nvkms_ref_ptr *ref_ptr, NvU32 dataU32, NvU64 usec)
{
    return kms_timer_start(proc, ref_ptr, dataU32, NV_TRUE, usec) != NULL;
}
void nvkms_free_timer(nvkms_timer_handle_t *handle)
{
    struct nvkms_timer_t *t = handle;
    if (!t) return;
    if (t->complete) { kms_timer_release(t); return; }
    t->cancel = NV_TRUE;
    if (thread_call_cancel(t->call)) kms_timer_release(t);
}

void nvkms_event_queue_changed(nvkms_per_open_handle_t *pOpenKernel, NvBool eventsAvailable)
{
    struct nvkms_per_open *popen = pOpenKernel;
    if (popen->type == NVKMS_CLIENT_USER_SPACE) popen->eventsAvailable = eventsAvailable;
    else if (eventsAvailable && popen->eventCall && !popen->closing) {
        popen->eventWanted = true;
        if (!popen->eventBusy) {
            popen->eventBusy = true;
            thread_call_enter(popen->eventCall);
        }
    }
}
void *nvkms_get_per_open_data(int) { return NULL; }
NvBool nvkms_fd_is_nvidia_chardev(int) { return NV_FALSE; }
NvBool nvkms_allow_write_combining(void) { return NV_TRUE; }
NvBool nvkms_kernel_supports_syncpts(void) { return NV_FALSE; }

NvU32 nvkms_enumerate_gpus(nv_gpu_info_t *gpu_info)
{
    nv_state_t *nv = nv_xnu_gpu_state();
    if (!nv) return 0;
    nvu_memset(&gpu_info[0], 0, sizeof gpu_info[0]);
    gpu_info[0].gpu_id = nv->gpu_id;
    gpu_info[0].pci_info.domain   = nv->pci_info.domain;
    gpu_info[0].pci_info.bus      = nv->pci_info.bus;
    gpu_info[0].pci_info.slot     = nv->pci_info.slot;
    gpu_info[0].pci_info.function = nv->pci_info.function;
    gpu_info[0].os_device_ptr = nv_xnu_gpu_os_device();
    return 1;
}
NvBool nvkms_open_gpu(NvU32 gpuId, NvBool) { nv_state_t *nv = nv_xnu_gpu_state(); return nv && nv->gpu_id == gpuId; }
void nvkms_close_gpu(NvU32, NvBool) {}

// NVKMS reports a backlight-capable internal panel here (nvRmRegisterBacklight); nvidia-modeset-linux.c
// turns it into a Linux backlight device. Kept so NVRMFB can set the panel's brightness through
// NVRM::callPlatformFunction("NVRMBacklight") -> nvkms_xnu_backlight().
struct nvkms_backlight_device { NvU32 gpu_id, display_id; void *drv_priv; };
static struct nvkms_backlight_device gKmsBacklight;
static NvBool gKmsBacklightRegistered;
struct nvkms_backlight_device *nvkms_register_backlight(NvU32 gpu_id, NvU32 display_id, void *drv_priv, NvU32 current_brightness)
{
    if (gKmsBacklightRegistered || !drv_priv) return NULL;
    gKmsBacklight = { gpu_id, display_id, drv_priv };
    gKmsBacklightRegistered = NV_TRUE;
    kprintf("NVRM-xnu: backlight registered: gpu 0x%x display 0x%x at %u%%\n", gpu_id, display_id, current_brightness);
    return &gKmsBacklight;
}
void nvkms_unregister_backlight(struct nvkms_backlight_device *bd)
{
    if (bd != &gKmsBacklight) return;
    gKmsBacklightRegistered = NV_FALSE;
    kprintf("NVRM-xnu: backlight unregistered\n");
}
// Panel brightness in percent (0-100), read or set under the NVKMS lock like nvkms_get_backlight_brightness()
// and nvkms_update_backlight_status() in nvidia-modeset-linux.c. Not for callers that hold gKmsLock.
NvBool nvkms_xnu_backlight(NvBool set, NvU32 *percent)
{
    if (!percent || !gKmsLock) return NV_FALSE;
    IOLockLock(gKmsLock);
    NvBool ok = gKmsBacklightRegistered &&
                (set ? nvKmsSetBacklight(gKmsBacklight.display_id, gKmsBacklight.drv_priv, *percent)
                     : nvKmsGetBacklight(gKmsBacklight.display_id, gKmsBacklight.drv_priv, percent));
    IOLockUnlock(gKmsLock);
    return ok;
}

struct nvkms_per_open *nvkms_open_from_kapi(struct NvKmsKapiDevice *device) { return kms_open_common(NVKMS_CLIENT_KERNEL_SPACE, device, 0); }
void nvkms_close_from_kapi(struct nvkms_per_open *popen) { kms_close_common(popen); }
NvBool nvkms_ioctl_from_kapi(struct nvkms_per_open *popen, NvU32 cmd, void *params, const size_t size)
{
    return kms_ioctl_common(popen, cmd, (NvU64)(uintptr_t)params, size);
}
NvBool nvkms_ioctl_from_kapi_try_pmlock(struct nvkms_per_open *popen, NvU32 cmd, void *params, const size_t size)
{
    return kms_ioctl_common(popen, cmd, (NvU64)(uintptr_t)params, size);
}

nvkms_sema_handle_t *nvkms_sema_alloc(void)
{
    semaphore_t s = SEMAPHORE_NULL;
    if (semaphore_create(kernel_task, &s, SYNC_POLICY_FIFO, 1) != KERN_SUCCESS) return NULL;
    return (nvkms_sema_handle_t *)s;
}
void nvkms_sema_free(nvkms_sema_handle_t *sema) { if (sema) semaphore_destroy(kernel_task, (semaphore_t)sema); }
void nvkms_sema_down(nvkms_sema_handle_t *sema) { semaphore_wait((semaphore_t)sema); }
void nvkms_sema_up(nvkms_sema_handle_t *sema) { semaphore_signal((semaphore_t)sema); }

}

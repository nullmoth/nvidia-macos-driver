/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nvrm_off.h"
#include "nv-xnu.h"
#include "nvrm_utilization.h"
#include "nvrm_bar_limits.h"
#include "nvrm_off_bar.h"
#include "nvrm_startup_policy.h"
#include "nvrm_bar_recover.h"
#include "nvrm_shutdown_plan.h"
#include "nvrm_aspm.h"

extern "C" void nv_xnu_power_refresh(void);   // nv_xnu_power.cpp
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOSubMemoryDescriptor.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOFilterInterruptEventSource.h>
#include <pexpert/pexpert.h>
#include <kern/thread_call.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <mach/mach_time.h>
#include <kern/sched_prim.h>
#include <libkern/OSDebug.h>

extern "C" {
#include <mach/mach_types.h>
#include <sys/proc.h>
#include <sys/kauth.h>
#include <sys/sysctl.h>
#include <libkern/version.h>

// The 2.x driver is built for macOS 15 (Darwin 24) and macOS 26 (Darwin 25) only; on any other kernel NVRM
// claims nothing, so NVRMFB and NVRMAGDC, which attach to it, never start either.
static bool nvrm_kernel_supported(void) { return version_major == 24 || version_major == 25; }

#include "nvrm_gpuva_abi.h"
#define NVRM_SS_KERNEL_ABI 1
#include "nvrm_surfshare_abi.h"
extern "C" int nvrm_surfshare_rm(struct NVRMSurfShareRm *);
extern "C" int nvrm_surfshare_pageoff(struct NVRMSurfPageoff *, void *nv);
extern "C" task_t current_task(void);
extern "C" int nvrm_gpuva_alloc(struct NVRMGpuVaRequest *);
extern "C" int nvrm_gpuva_map(struct NVRMGpuVaRequest *);
extern "C" int nvrm_gpuva_free(struct NVRMGpuVaRequest *);
extern "C" bool nvrm_query_vram_kb(NvU32 gpuId, NvU32 *pRamKb, NvU32 *pTotalKb, NvU32 *pStatus);
extern "C" bool nvrm_query_gpu_profile(NvU32 gpuId, NvU32 *pArch, NvU32 *pImpl, NvU32 *pCls3d, NvU32 *pClsCompute,
                                       NvU32 *pClsCopy, NvU32 *pClsEnc, NvU32 *pClsDec, NvU32 *pStatus);
kern_return_t _start(kmod_info_t *ki, void *data);
kern_return_t _stop(kmod_info_t *ki, void *data);
KMOD_EXPLICIT_DECL(com.nullmoth.NVRM, "0.1", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = NULL;
__private_extern__ kmod_stop_func_t  *_antimain = NULL;
__private_extern__ int _kext_apple_cc = __APPLE_CC__;
}

#define LOG(fmt, ...) do { kprintf("NVRM-xnu: " fmt "\n", ##__VA_ARGS__); IOLog("NVRM-xnu: " fmt "\n", ##__VA_ARGS__); } while (0)

// Longest a utilization firmware read may take before it counts as hung (chosen by us: two orders above a control
// round trip, an order below the firmware's own timeout); the read's real duration is logged when it trips.
static const uint64_t kUtilHangNs = 500ull * 1000 * 1000;

extern "C" { NvBool nvkms_xnu_load(void); }
class NVRM;
#include "nvrm_devtab.h"
// The GPU objects, newest-start-wins and never cleared unless the boot argument nvmultigpu=1 is present (nvrm_devtab.h).
static NvrmDevTab<NVRM> gNVRMTab;
static NvrmStartGate gNVRMGate;
static NvrmWaitQueue<IOService> gNVRMWait;
// Offers the oldest refused card to IOKit again. registerService on its PCI device runs matching on it once more, which is
// how a start() that returned false is retried; the queue holds a reference until then.
static void nvrmReofferNext(void)
{
    if (IOService *p = gNVRMWait.next()) { p->registerService(kIOServiceAsynchronous); p->release(); }
}
static bool nvrmMultiGpu(void)
{
    static int on = -1;
    if (on < 0) { uint32_t v = 0; on = (PE_parse_boot_argn("nvmultigpu", &v, sizeof v) && v) ? 1 : 0; if (on) kprintf("NVRM-xnu: nvmultigpu=1: more than one card may be accelerated, one start-up at a time\n"); }
    return on == 1;
}
#include "nv_rtd3_policy.h"
extern "C" void nv_rtd3_arm(const struct nv_rtd3_ops *ops);
extern "C" void nvrm_power_start(nv_state_t *nv);   // nvrm_power.cpp
extern "C" void nvrm_power_stop(void);
extern "C" void nvrm_power_suspend(void);
extern "C" void nvrm_power_resume(void);
extern "C" int  nv_rtd3_before_system_resume(void);
extern "C" int  nv_rtd3_power_off(IOACPIPlatformDevice *acpi);
extern "C" int  nv_rtd3_power_on(IOACPIPlatformDevice *acpi);
extern "C" nv_state_t *nv_xnu_gpu_state(void);
extern "C" nv_state_t *nv_xnu_gpu_state_at(unsigned index);
extern "C" void *nv_xnu_gpu_os_device_at(unsigned index);
struct NVRMDdcRequest;
extern "C" int nvrm_ddc_transfer(struct NVRMDdcRequest *r);   // nvrm_ddc.cpp
struct NVRMDpAuxRequest;
extern "C" int nvrm_dpaux_transfer(struct NVRMDpAuxRequest *r);   // nvrm_ddc.cpp
extern "C" void nv_xnu_log_init(void);
extern "C" bool nv_xnu_maps_init(void);
extern "C" void *nvkms_xnu_kapi_table(void);
extern "C" NvU64 nv_xnu_phys_for_kernel_va(const void *va);
class NVRMDisplay : public IOService { OSDeclareDefaultStructors(NVRMDisplay) };
OSDefineMetaClassAndStructors(NVRMDisplay, IOService)
#define NVRM_MAX_HEADS 4
static IOService *gDisplayNub0;
static void nvrm_display_publish(IOService *provider, NvU32 gpuId)
{
    static bool gPublished;
    if (gPublished) return;
    void *kapi = nvkms_xnu_kapi_table();
    if (!kapi) { kprintf("NVRM-xnu: no NVKMS kapi table, no display nub\n"); return; }
    int want = 1;
    if (!PE_parse_boot_argn("nvfbheads", &want, sizeof want) || want < 1) want = 1;
    if (want > NVRM_MAX_HEADS) want = NVRM_MAX_HEADS;
    unsigned made = 0;
    for (int i = 0; i < want; i++) {
        NVRMDisplay *nub = OSTypeAlloc(NVRMDisplay);
        if (!nub) break;
        if (!nub->init()) { nub->release(); break; }
        nub->setProperty("gpu-id", (unsigned long long)gpuId, 32);
        nub->setProperty("fb-index", (unsigned long long)i, 32);
        if (!nub->attach(provider)) { nub->release(); break; }
        nub->registerService();
        if (i == 0) gDisplayNub0 = nub;
        if (i == 0 && want > 1) {
            IOReturn q = nub->waitQuiet(10ULL * 1000000000ULL);
            kprintf("NVRM-xnu: nub 0 published first; its framebuffer started (waitQuiet 0x%x) before nubs 1..%d\n", q, want - 1);
        }
        made++;
    }
    if (!made) { kprintf("NVRM-xnu: no NVRMDisplay nub could be published\n"); return; }
    gPublished = true;
    kprintf("NVRM-xnu: %u NVRMDisplay nub(s) published (kapi %p, nvfbheads=%d) — NVRMFB.kext matches them\n",
            made, kapi, want);
}
class NVRM : public IOService {
    OSDeclareDefaultStructors(NVRM)
    friend class NVRMUserClient;
    friend nv_state_t *nv_xnu_gpu_state(void);
    friend nv_state_t *nv_xnu_gpu_state_at(unsigned index);
    friend void *nv_xnu_gpu_os_device_at(unsigned index);
    bool startGPU(IOService *provider);
public:
    virtual IOService *probe(IOService *provider, SInt32 *score) override;
    virtual bool start(IOService *provider) override;
    virtual void stop(IOService *provider) override;
    virtual void free() override;
    IOReturn go(uint32_t pass);
    static void autoGo(void *p0, wait_result_t w);
    static void bootHoldCap(thread_call_param_t p0, thread_call_param_t p1);
    void releaseBootHold(const char *why);
    void snapshotBootScreen();
    void measureBootRaster();
    int fBootHead = -1; uint32_t fBootPclk = 0, fBootHt = 0, fBootVt = 0;
    static void headWatch(void *p0, wait_result_t w);
    IOBufferMemoryDescriptor *fBootScreen = nullptr; uint32_t fBootW = 0, fBootH = 0, fBootPitch = 0;
    uint32_t     fAutoGoSettleMs = 10000;
    bool         fBar1Placed = false;
    volatile UInt32 fBootHold = 0;
    // A fixed settle + 40 s cap lost the desktop whenever the GSP bring-up ran long (up to 48 s here); while go(2)
    // runs the cap now waits in 10 s steps, up to nvrmholdcap seconds (default 120). A hung bring-up is still released.
    volatile UInt32 fBringUp = 0;          // 1 from go(2) until the display is armed or the bring-up gave up
    uint64_t        fBringUpStartNs = 0;
    uint32_t        fHoldCapMs = 120000;   // boot-arg nvrmholdcap (seconds)
    uint32_t        fHoldWaitedMs = 0;     // how long the cap has waited past its first deadline
    thread_call_t   fHoldCapCall = nullptr;
    virtual IOReturn callPlatformFunction(const OSSymbol *fn, bool wait,
                                         void *p1, void *p2, void *p3, void *p4) override;
    virtual IOReturn setPowerState(unsigned long ordinal, IOService *device) override;
    virtual void systemWillShutdown(IOOptionBits specifier) override;
private:
    // PCIe link power (ASPM L1) and the BAR facts published for the app (nvrm-aspm, nvrm-bar1-*, nvrm-above-4g)
    UInt16 fOrigEpLnkCtl = 0xffff, fOrigUpLnkCtl = 0xffff;   // what the firmware left; 0xffff = not changed by us
    void enableAspmL1();
    void restoreAspm();
    void publishBarFacts();
    struct RebarFacts { bool cap; UInt32 supported; UInt64 firmwareSize; } fRebar = {};
    NvrmBarUndo fBarUndo = {};          // what placeLargeBar1 changed, for systemWillShutdown
    volatile SInt32 fShutdownDone = 0;  // the quiesce thread's finish flag
    bool fShutdownHandled = false;
    static void shutdownQuiesce(void *p0, wait_result_t w);
    static void shutdownFull(void *p0, wait_result_t w);
    // System sleep (S3). NVRM is a power child of the GPU's IOPCIDevice, so it is told to power off before the slot
    // loses power and to power on once IOPCIFamily has the device back in D0.
    enum { kGpuPmRunning = 0, kGpuPmSuspended = 1, kGpuPmLost = 2 };
    volatile SInt32 fGpuPm = kGpuPmRunning;
    bool         fPmJoined = false;
    unsigned     fPmCycles = 0;
    struct PciSave {
        bool valid; UInt32 rbOff[6], rbCtl[6]; unsigned rbCount;
        UInt32 bar[6]; UInt16 cmd;
        IOPCIDevice *rp; UInt32 rpWin[6]; UInt16 rpCmd;   // root port 0x18..0x2c and command
    } fPciSave = {};
    void gpuSuspend();
    void gpuResume();
    void rtd3Arm();   // opt-in D3cold (nvrtd3=1): kexts/NVRM/nv_rtd3.cpp
    void pciSaveForSleep();
    bool pciRestoreAfterWake();
    bool gpuAnswers();
    void notifyFramebuffers(uintptr_t state);
    IOPCIDevice *fPCI = NULL;
    nv_state_t   fNv = {};
    bool         fPrivateState = false;
    bool         fAdapter = false;
    IOLock      *fUtilizationLock = nullptr;
    void        *fUtilizationState = nullptr;
    bool         fUtilizationStopping = false;
    uint32_t     fPassDone = 0;
    IOWorkLoop  *fWorkLoop = NULL;
    IOFilterInterruptEventSource *fIntSrc = NULL;
    void readBARs();
    void publishGpuProfile();
    bool placeLargeBar1();
    bool interruptFilter(IOFilterInterruptEventSource *src);
    void interruptOccurred(IOInterruptEventSource *src, int count);
};
extern "C" { extern volatile int nv_xnu_in_isr; extern volatile thread_t nv_xnu_isr_thread; void nv_xnu_dump_live_allocs(void); }
static volatile unsigned long gNvIsrCount = 0, gNvIsrHandled = 0;
// Read from a shell (also over SSH while the screen is frozen): do GPU interrupts still arrive and get handled?
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_isr_count, CTLFLAG_RD, (int64_t *)&gNvIsrCount, "GPU MSIs taken");
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_isr_handled, CTLFLAG_RD, (int64_t *)&gNvIsrHandled, "GPU MSIs RM claimed");
static volatile SInt64 gNvEvtPosts = 0, gNvEvtMatched = 0, gNvEvtWaits = 0, gNvEvtSlept = 0;
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_evt_posts, CTLFLAG_RD, (int64_t *)&gNvEvtPosts, "RM events posted to user clients");
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_evt_waits, CTLFLAG_RD, (int64_t *)&gNvEvtWaits, "user event waits");

// System sleep gate, the nv_system_pm_lock of kernel-open/nvidia/nv.c. Linux freezes user space before suspend and
// still takes that lock in every ioctl; macOS freezes nothing, so on wake WindowServer and Metal apps called into RM
// while the GPU was still in standby (an auto-unmap mid-standby broke the GMMU walk, TLB invalidates
// failed and RESUME took 31 s; NVK got errors and WindowServer froze on the restored frame). The PM thread
// closes the gate before NVKMS suspend and opens it after RM and NVKMS resume; calls arriving meanwhile wait.
static IOLock  *gNvPmGate;
static int      gNvPmUsers;
static bool     gNvPmClosed;
static thread_t gNvPmOwner;
static SInt64   gNvPmWaited;
SYSCTL_QUAD(_debug, OID_AUTO, nvrm_pm_waited, CTLFLAG_RD, &gNvPmWaited, "RM calls that waited for a GPU resume");
extern "C" int nv_xnu_pm_enter(void)
{
    if (!gNvPmGate || current_thread() == gNvPmOwner) return 0;
    IOLockLock(gNvPmGate);
    if (gNvPmClosed) {
        OSIncrementAtomic64(&gNvPmWaited);
        while (gNvPmClosed) IOLockSleep(gNvPmGate, &gNvPmClosed, THREAD_UNINT);
    }
    gNvPmUsers++;
    IOLockUnlock(gNvPmGate);
    return 1;
}
extern "C" void nv_xnu_pm_exit(int counted)
{
    if (!counted || !gNvPmGate) return;
    IOLockLock(gNvPmGate);
    if (--gNvPmUsers == 0 && gNvPmClosed) IOLockWakeup(gNvPmGate, &gNvPmUsers, false);
    IOLockUnlock(gNvPmGate);
}
// Telemetry is optional: skip a suspended GPU instead of parking its polling
// thread until wake. Admission and the users count share the PM gate lock, so
// suspend cannot pass its drain while a successful admission is still in RM.
static bool nv_xnu_pm_try_enter(int *counted)
{
    *counted = 0;
    if (!gNvPmGate || current_thread() == gNvPmOwner) return true;
    IOLockLock(gNvPmGate);
    const bool ready = !gNvPmClosed;
    if (ready) { gNvPmUsers++; *counted = 1; }
    IOLockUnlock(gNvPmGate);
    return ready;
}
// Close the gate and wait up to waitMs for the calls already inside RM. Returns how many were still inside.
static int nv_xnu_pm_close(uint32_t waitMs)
{
    if (!gNvPmGate) return 0;
    uint64_t deadline; clock_interval_to_deadline(waitMs, kMillisecondScale, &deadline);
    IOLockLock(gNvPmGate);
    gNvPmClosed = true;
    gNvPmOwner = current_thread();
    while (gNvPmUsers > 0)
        if (IOLockSleepDeadline(gNvPmGate, &gNvPmUsers, deadline, THREAD_UNINT) == THREAD_TIMED_OUT) break;
    const int left = gNvPmUsers;
    IOLockUnlock(gNvPmGate);
    return left;
}
// System wake runs on another PM thread than the sleep that closed the gate: hand it the gate before RM and NVKMS
// resume (nvKmsResume's RM calls waited at the gate on the wake thread until the 180 s wake watchdog panicked).
static void nv_xnu_pm_take(void)
{
    if (!gNvPmGate) return;
    IOLockLock(gNvPmGate);
    gNvPmOwner = current_thread();
    IOLockUnlock(gNvPmGate);
}
static void nv_xnu_pm_open(void)
{
    if (!gNvPmGate) return;
    IOLockLock(gNvPmGate);
    gNvPmClosed = false;
    gNvPmOwner = NULL;
    IOLockWakeup(gNvPmGate, &gNvPmClosed, false);
    IOLockUnlock(gNvPmGate);
}

static inline void nvu_memset(void *d, int c, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory"); }
static inline void nvu_memcpy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }
#define NV_XNU_MAX_CLIENTS 32768
class NVRMUserClient;
static NVRMUserClient *gNvClients[NV_XNU_MAX_CLIENTS];
static IOLock         *gNvClientsLock = NULL;
#include <kern/clock.h>
#if defined(NM_ERA) && NM_ERA < 1010
// Before 10.10 the kernel does not export get_task_pmap to kexts, only get_task_map. The kernel's own
// get_task_pmap is "load the map, load one field of it"; the field's offset is read from each release's kernel
// (10.6: 0x38, 10.7 and 10.8: 0x48, 10.9: 0x50).
extern "C" vm_map_t get_task_map(task_t);
static inline struct pmap *get_task_pmap(task_t t)
{
    return *(struct pmap **)((unsigned char *)get_task_map(t) + (NM_ERA < 1007 ? 0x38 : NM_ERA < 1009 ? 0x48 : 0x50));
}
#else
extern "C" struct pmap *get_task_pmap(task_t);
#endif
struct nv_pm_owner { uint64_t pm; int pid; int nopen; uint64_t closed_ms; char name[20]; };
static nv_pm_owner gNvPmTab[256];
static uint64_t nv_pm_now_ms(void) { uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time(), &ns); return ns / 1000000ULL; }
static void nv_pm_open(uint64_t pm, int pid)
{
    if (!pm) return;
    int hit = -1, freeRow = -1, oldest = -1;
    for (int i = 0; i < 256; i++) {
        if (gNvPmTab[i].pm == pm) { hit = i; break; }
        if (!gNvPmTab[i].pm && freeRow < 0) freeRow = i;
        if (gNvPmTab[i].pm && !gNvPmTab[i].nopen && (oldest < 0 || gNvPmTab[i].closed_ms < gNvPmTab[oldest].closed_ms)) oldest = i;
    }
    nv_pm_owner *o;
    if (hit >= 0 && gNvPmTab[hit].pid == pid) { o = &gNvPmTab[hit]; o->nopen++; o->closed_ms = 0; return; }
    const int row = hit >= 0 ? hit : freeRow >= 0 ? freeRow : oldest;
    if (row < 0) return;
    o = &gNvPmTab[row]; o->pm = pm; o->pid = pid; o->nopen = 1; o->closed_ms = 0; o->name[0] = 0;
    proc_name(pid, o->name, (int)sizeof o->name);
}
static void nv_pm_close(uint64_t pm)
{
    for (int i = 0; i < 256; i++) if (gNvPmTab[i].pm == pm) {
        if (gNvPmTab[i].nopen > 0 && --gNvPmTab[i].nopen == 0) gNvPmTab[i].closed_ms = nv_pm_now_ms();
        return;
    }
}
extern "C" int nv_xnu_pmap_owner(uint64_t pm, char *name, size_t cap, uint64_t *closed_ms)
{
    if (name && cap) name[0] = 0;
    if (closed_ms) *closed_ms = 0;
    if (!pm || !gNvClientsLock) return -1;
    int pid = -1;
    IOLockLock(gNvClientsLock);
    for (int i = 0; i < 256; i++) if (gNvPmTab[i].pm == pm) {
        pid = gNvPmTab[i].pid;
        if (name && cap) strlcpy(name, gNvPmTab[i].name, cap);
        if (closed_ms) *closed_ms = gNvPmTab[i].closed_ms;
        break;
    }
    IOLockUnlock(gNvClientsLock);
    return pid;
}

class NVRMUserClient : public IOUserClient {
    OSDeclareDefaultStructors(NVRMUserClient)
    uint64_t fPm;
public:
    bool start(IOService *provider) override;
    IOReturn clientClose(void) override;
    void free(void) override;
    IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args,
                            IOExternalMethodDispatch *dispatch, OSObject *target, void *reference) override;
    bool initWithTask(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties) override;
    nv_file_private_t *nvfp() { return &fNvfp; }
    IOReturn clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory) override;
    nv_alloc_mapping_list_node_t **mapList() { return &fMapList; }
    NvU64 *mapLen() { return &fMapLen; }
    nv_alloc_mapping_list_node_t *mapLenNode() const { return fMapLenNode; }
    void setMapLenNode(nv_alloc_mapping_list_node_t *n) { fMapLenNode = n; }
    NvU64 *mapGen() { return &fMapGen; }
    nv_alloc_mapping_list_node_t **mapHeadAtAcquire() { return &fMapHeadAtAcquire; }
    bool isCtl() const { return fIsCtl; }
    nv_state_t *nvState();
protected:
    NVRM *fOwner = NULL;
private:
    nv_file_private_t fNvfp = {};
    // RM's pending mapping context for this fd (NVIDIA 620: RM allocates the node with os_alloc_mem and stores it here
    // through nv_acquire_file_va; at most one, a second is refused with NV_ERR_STATE_IN_USE). Freed in free().
    nv_alloc_mapping_list_node_t *fMapList = nullptr;
    // Bytes RM validated for that pending system-memory mapping, from page_index's first byte (pageOffset + Length of
    // its osMapSystemMemory call). 620's context no longer carries a size, and its alloc is the ROOT allocation, which
    // can hold other objects past this one, so the map path clamps to this and refuses when it is 0. Under fMapLock.
    NvU64 fMapLen = 0;
    nv_alloc_mapping_list_node_t *fMapLenNode = nullptr;   // the node fMapLen belongs to
    // Bumped under fMapLock whenever the list head is stored or freed: a node address can be reused after a free, so a
    // pending length is applied only while the generation it was recorded at is still current.
    NvU64 fMapGen = 0;
    nv_alloc_mapping_list_node_t *fMapHeadAtAcquire = nullptr;   // list head when RM took the write hold
    // Token mappings (selector 7 advertises them, selector 8 forgets one; see clientMemoryForType). A memory type with
    // kNvMapTokenBit set names one mapping: its first clientMemoryForType consumes the pending context and keeps the
    // descriptor it built under that token, so the unmap - which IOKit resolves by calling clientMemoryForType again
    // with the same type and looking the mapping up on the descriptor it gets back - finds it. Under fMapLock.
    struct NvMapToken { uint32_t token; IOOptionBits options; IOMemoryDescriptor *md; };
    NvMapToken *fTok = nullptr;
    unsigned fNTok = 0;
    unsigned fTokCap = 0;                // entries fTok has room for (nvrm_tok_table.h)
    bool fIsCtl = true;
    int  fFd = -1;
    int  fPid = 0;
    bool fEverBound = false;
    int  fBound = -1;
    SInt32 fCleaned = 0;
    IOLock *fGate = nullptr;      // serialises this client's methods against its own teardown
    // Guards fMapList and nothing else. It is separate from fGate because RM writes another client's context from inside
    // that client's neighbour's esc(), and because bind does not hold fGate. Nothing that takes another lock runs under it.
    IOLock *fMapLock = nullptr;
    // Held across esc() and clientMemoryForType() and across the teardown in clientClose. The RM
    // callbacks below it (nv_acquire_file_va and friends) must NOT take it: they run
    // nested inside esc() for this same client and IOLock is not recursive.
    struct Gate {
        IOLock *l;
        explicit Gate(IOLock *g) : l(g) { if (l) IOLockLock(l); }
        ~Gate() { if (l) IOLockUnlock(l); }
    };
    IOReturn esc(uint32_t cmd, void *data, uint32_t size);
    static IOReturn sGo(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sEsc(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sGetFd(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sBindFd(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sRetired(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sWaitEvent(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sCaps(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sForgetToken(OSObject *target, void *reference, IOExternalMethodArguments *args);
public:
    volatile UInt32 fEvtSeq = 0;
    UInt32 fEvtMagic = 0x4e564556u;
    static NVRMUserClient *fromNvfp(nv_file_private_t *p);
    int  pid() const { return fPid; }
    int  boundFd() const { return fBound; }
    bool everBound() const { return fEverBound; }
    int  index() const { return fFd; }
    bool cleaned() const { return fCleaned != 0; }
    IOLock *mapLock() const { return fMapLock; }
};
extern "C" nv_file_private_t *nv_xnu_lookup_fd(NvS32 fd, void **priv, NvBool *isCtl)
{
    if (!gNvClientsLock) return NULL;
    int pid = proc_selfpid();
    NVRMUserClient *c = NULL;
    IOLockLock(gNvClientsLock);
    for (int i = 0; i < NV_XNU_MAX_CLIENTS && !c; i++) { NVRMUserClient *k = gNvClients[i]; if (k && k->pid() == pid && k->boundFd() == (int)fd) c = k; }
    for (int i = 0; i < NV_XNU_MAX_CLIENTS && !c; i++) { NVRMUserClient *k = gNvClients[i]; if (k && k->pid() == pid && !k->everBound() && k->index() == (int)fd) c = k; }
    // The caller keeps this client until nv_xnu_put_client. Taken under the table lock: clientClose empties the slot
    // under the same lock, so a client still in the table cannot have been freed.
    if (c && c->cleaned()) c = NULL;
    if (c) c->retain();
    IOLockUnlock(gNvClientsLock);
    if (!c) return NULL;
    if (priv) *priv = c;
    if (isCtl) *isCtl = c->isCtl() ? NV_TRUE : NV_FALSE;
    return c->nvfp();
}
// Frees RM's mapping nodes and their ranges; both came from RM's os_alloc_mem, so they go back through os_free_mem.
static void nvMapListFree(nv_alloc_mapping_list_node_t **list)
{
    while (*list) {
        nv_alloc_mapping_list_node_t *n = *list;
        *list = n->pNext;
        if (n->context.memArea.pRanges) os_free_mem(n->context.memArea.pRanges);
        os_free_mem(n);
    }
}
extern "C" void nv_xnu_put_client(void *priv) { if (priv) ((OSObject *)priv)->release(); }
extern "C" void nv_xnu_mmap_lock(void *priv)
{
    NVRMUserClient *c = OSDynamicCast(NVRMUserClient, (OSObject *)priv);
    if (c && c->mapLock()) IOLockLock(c->mapLock());
}
extern "C" void nv_xnu_mmap_unlock(void *priv)
{
    NVRMUserClient *c = OSDynamicCast(NVRMUserClient, (OSObject *)priv);
    if (c && c->mapLock()) IOLockUnlock(c->mapLock());
}
// NVIDIA 620 OS interface: RM reaches a file's mapping list only between these two calls, so they hold that client's
// fMapLock (the same lock clientMemoryForType and a re-bind take). RM calls them nested inside another client's esc(),
// which is why this is fMapLock and never fGate.
// A file private that is not one of our clients cannot occur (every nvfp RM sees came from nv_get_file_private); if it
// ever did, RM gets a list that already holds a node, so it refuses with NV_ERR_STATE_IN_USE and frees its own node,
// instead of parking a mapping no fd can reach.
static nv_alloc_mapping_list_node_t gNvMapBusyNode;
static nv_alloc_mapping_list_node_t *gNvMapBusy = &gNvMapBusyNode;

// Which node a thread just stored on which client. RM's osMapSystemMemory stores the node (acquire/release below) and
// then, on the same thread and inside the same escape, calls nv_alloc_user_mapping with the Length it validated; that
// call reports the length through nv_xnu_note_map_len, which applies it to that exact node only.
// Lifetime: a slot lives for one escape call. NVRMUserClient::esc clears the calling thread's slot on entry and on every
// exit (NvMapPendingScope), so a slot can never outlive the escape or point at a client from an earlier one. A slot also
// holds its own reference on the client (taken when stored, dropped with the slot, always outside the simple lock), so
// it stays valid even on an RM path that does not come through esc().
// Fail closed: no free slot, an error before nv_alloc_user_mapping, or a node that changed means no length is recorded
// for the current node, and the map path refuses a node without one.
struct NvMapPending { thread_t th; NVRMUserClient *c; nv_alloc_mapping_list_node_t *node; NvU64 gen; bool wasEmpty; };
static NvMapPending gNvMapPending[64];
static IOSimpleLock *gNvMapPendingLock = nullptr;
static IOSimpleLock *mapPendingLock()
{
    if (!gNvMapPendingLock) {
        IOSimpleLock *l = IOSimpleLockAlloc();
        if (!OSCompareAndSwapPtr(nullptr, l, (void * volatile *)&gNvMapPendingLock)) IOSimpleLockFree(l);
    }
    return gNvMapPendingLock;
}
// The current thread's slot (claiming a free one if asked); pending lock held.
static NvMapPending *mapPendingSlot(bool claim)
{
    thread_t me = current_thread(); NvMapPending *freeSlot = nullptr;
    for (auto &p : gNvMapPending) { if (p.th == me) return &p; if (!p.th && !freeSlot) freeSlot = &p; }
    if (claim && freeSlot) { freeSlot->th = me; freeSlot->c = nullptr; freeSlot->node = nullptr; freeSlot->gen = 0; freeSlot->wasEmpty = false; return freeSlot; }
    return nullptr;
}
// Empties a slot and hands back its client reference; the caller releases it after dropping the simple lock.
static NVRMUserClient *mapPendingDrop(NvMapPending *p)
{ NVRMUserClient *c = p->c; p->th = nullptr; p->c = nullptr; p->node = nullptr; p->gen = 0; p->wasEmpty = false; return c; }
static void mapPendingClearThread()
{
    NVRMUserClient *drop = nullptr;
    IOSimpleLockLock(mapPendingLock());
    NvMapPending *p = mapPendingSlot(false);
    if (p) drop = mapPendingDrop(p);
    IOSimpleLockUnlock(gNvMapPendingLock);
    if (drop) drop->release();
}
struct NvMapPendingScope { NvMapPendingScope() { mapPendingClearThread(); } ~NvMapPendingScope() { mapPendingClearThread(); } };

extern "C" nv_alloc_mapping_list_node_t **NV_API_CALL nv_acquire_file_va(nv_file_private_t *nvfp, NvBool write)
{
    NVRMUserClient *c = NVRMUserClient::fromNvfp(nvfp);
    if (!c || !c->mapLock()) { gNvMapBusy = &gNvMapBusyNode; return &gNvMapBusy; }
    IOLockLock(c->mapLock());
    if (write) {
        *c->mapHeadAtAcquire() = *c->mapList();   // under c's lock, held until nv_release_file_va
        // Releasing the previous holder here is safe while c's lock is held: if it is c, RM still holds c (it got it
        // from nv_get_file_private), so this is not the last reference; any other client frees under its own lock.
        NVRMUserClient *old = nullptr;
        c->retain();
        IOSimpleLockLock(mapPendingLock());
        NvMapPending *p = mapPendingSlot(true);
        if (p) { old = mapPendingDrop(p); p->th = current_thread(); p->c = c; p->wasEmpty = (*c->mapList() == nullptr); }
        IOSimpleLockUnlock(gNvMapPendingLock);
        if (!p) c->release();   // no slot: nothing holds it, and the node gets no length (refused at map time)
        if (old) old->release();
    }
    return c->mapList();
}
extern "C" void NV_API_CALL nv_release_file_va(nv_file_private_t *nvfp, NvBool write)
{
    NVRMUserClient *c = NVRMUserClient::fromNvfp(nvfp);
    if (!c || !c->mapLock()) return;
    if (write) {
        // A node that appeared during this hold is new. Whatever length the client held belonged to an older node.
        nv_alloc_mapping_list_node_t *now = *c->mapList();
        NVRMUserClient *drop = nullptr;
        IOSimpleLockLock(mapPendingLock());
        NvMapPending *p = mapPendingSlot(false);
        // Any head change during this hold moves the generation, whether or not this thread got a slot, so no pending
        // length recorded before it can land afterwards.
        if (now != *c->mapHeadAtAcquire()) ++*c->mapGen();
        const bool fresh = p && p->c == c && p->wasEmpty && now;
        if (fresh) { p->node = now; p->gen = *c->mapGen(); } else if (p) drop = mapPendingDrop(p);
        IOSimpleLockUnlock(gNvMapPendingLock);
        if (now != c->mapLenNode()) { *c->mapLen() = 0; c->setMapLenNode(nullptr); }
        if (drop) drop->release();   // not the last reference to c (RM holds it); others free under their own lock
    }
    IOLockUnlock(c->mapLock());
}
// Called from nv_alloc_user_mapping with the bytes RM validated, counted from page_index's first byte.
extern "C" void nv_xnu_note_map_len(NvU64 bytes)
{
    NVRMUserClient *c = nullptr; nv_alloc_mapping_list_node_t *node = nullptr; NvU64 gen = 0;
    IOSimpleLockLock(mapPendingLock());
    NvMapPending *p = mapPendingSlot(false);
    if (p && p->node) { node = p->node; gen = p->gen; }
    if (p) c = mapPendingDrop(p);   // the slot's reference now belongs to this function
    IOSimpleLockUnlock(gNvMapPendingLock);
    if (!c) return;
    if (node) {
        IOLockLock(c->mapLock());
        // Only the node this thread stored, only while it is current, and only in the generation it was stored in (a
        // freed node's address can come back as a new node).
        if (*c->mapList() == node && *c->mapGen() == gen && c->mapLenNode() == nullptr) { *c->mapLen() = bytes; c->setMapLenNode(node); }
        IOLockUnlock(c->mapLock());
    }
    c->release();
}
// 620 callers check control-vs-device themselves (nv_verify_nvfp_control_or_put); this answers which state a file is on.
extern "C" nv_state_t *NV_API_CALL nv_get_state_ptr_from_nvfp(nv_file_private_t *nvfp)
{
    NVRMUserClient *c = NVRMUserClient::fromNvfp(nvfp);
    return c ? c->nvState() : NULL;
}
extern "C" IOMemoryDescriptor *nv_xnu_alloc_md(void *priv, NvU64 *bytes, NvU32 *cache);
extern "C" { IOMemoryDescriptor *nv_pm_ram_submd(NvU64, NvU64, int); bool nv_pm_is_gpu(NvU64, NvU64); bool nv_pm_is_gpu_fb(NvU64, NvU64); void nv_pm_dev_refused(NvU64, NvU64); bool nv_xnu_chat(void); }

#define super IOService
OSDefineMetaClassAndStructors(NVRM, IOService)
static bool gRmInitDone = false;

// Turing (TU102/TU104/TU106/TU117 = 0x1E00-0x1FFF, TU116 = 0x2180-0x21FF) needs about ten seconds after the BAR1 move
// before the display can be armed: its GSP firmware is still settling, and arming earlier left a GTX 1660 SUPER and an
// RTX 2070 SUPER stuck at boot. The installer used to be the only place that asked for this (nvrmsettle=10000), and it
// matched only display-class (0300) cards, so a muxless laptop card (class 0302) or a machine whose boot arguments were
// rewritten started with the 0.5 s default. The driver now knows the family itself; the boot argument still overrides.
// Moving BAR1 takes the boot console with it when the firmware put the console inside BAR1: the root port stops
// decoding the old address, but the kernel console keeps drawing there until the framebuffer takes over. On a verbose
// boot (-v) every IOLog line is drawn (and the screen scrolled) through that dead address, and each line then cost
// about half a second. A GTX 1650 moved from 64 MB at 0xfff80000000 to 256 MB at 0x3ffa0000000 spent 247 s in
// RmInitAdapter printing its register map, missed the settle + 120 s boot-hold cap, and reached the desktop with no
// Metal device. The console is silenced at the move only in that case: a verbose boot whose console was inside the
// old BAR1 and is not inside the new one. The framebuffer silences it at takeover anyway and never turns it back on.
static inline bool nvrmConsoleLostByMove(unsigned long long con, unsigned long long oldBase, unsigned long long oldSize,
                                         unsigned long long newBase, unsigned long long newSize, bool verbose)
{
    if (!verbose || !con || !oldSize) return false;
    if (con < oldBase || con - oldBase >= oldSize) return false;                 // the console was never in BAR1
    return !(newSize && con >= newBase && con - newBase < newSize);              // still decoded where BAR1 now is
}

static inline bool nvrmIsTuringDeviceId(unsigned int id)
{
    return (id >= 0x1E00 && id <= 0x1FFF) || (id >= 0x2180 && id <= 0x21FF);
}
static inline bool nvrmNameIsUnknown(const char *s)
{
    static const char u[] = "Unknown";
    for (unsigned i = 0; i < sizeof u; i++) if (s[i] != u[i]) return false;
    return true;
}

// Driver off with nvoffbar=1 and this card's BAR1 lost (nvrm_off_bar.h): NVRM claims the card only to repair BAR1.
static bool nvrmOffBarWanted(IOService *provider)
{
    UInt32 on = 0;
    if (!nvrm_driver_off() || !PE_parse_boot_argn("nvoffbar", &on, sizeof on) || !on) return false;
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci || pci->getProperty("nvrm-bar1-placed")) return false;       // not a PCI device, or already repaired this boot
    if (pci->configRead16(0x00) != 0x10de || pci->configRead8(0x0B) != 0x03) return false;
    NvrmApertureSpan ap[8]; unsigned n = 0;
    for (IOItemCount i = 0; i < pci->getDeviceMemoryCount() && n < 8; i++)
        if (IOMemoryDescriptor *m = pci->getDeviceMemoryWithIndex(i)) { ap[n].base = (UInt64)m->getPhysicalSegment(0, 0, kIOMemoryMapperNone); ap[n].length = m->getLength(); n++; }
    return nvrmBar64Lost(pci->configRead32(0x10 + 4), pci->configRead32(0x10 + 8), ap, n);    // BAR1 is the 64-bit BAR at 0x14/0x18
}

IOService *NVRM::probe(IOService *provider, SInt32 *score)
{
    if (nvrm_driver_off()) return nvrmOffBarWanted(provider) ? super::probe(provider, score) : NULL;
    if (!nvrm_kernel_supported()) { LOG("Darwin %d is not supported by this driver: not claiming the GPU", version_major); return NULL; }
    IOService *res = super::probe(provider, score);
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci) return NULL;
    const NvU8  cls      = (NvU8)pci->configRead8(0x0B);
    const NvU8  subclass = (NvU8)pci->configRead8(0x0A);
    const NvU16 vendor   = pci->configRead16(0x00), device   = pci->configRead16(0x02);
    const NvU16 ssVendor = pci->configRead16(0x2C), ssDevice = pci->configRead16(0x2E);
    LOG("probe %04x:%04x class %02x.%02x subsystem %04x:%04x", vendor, device, cls, subclass, ssVendor, ssDevice);
    if (!rm_is_supported_pci_device(cls, subclass, vendor, device, ssVendor, ssDevice, NV_TRUE)) {
        LOG("rm_is_supported_pci_device says NO — not claiming %04x:%04x", vendor, device);
        return NULL;
    }
    {
        const char *nm = rm_get_device_name(device, ssVendor, ssDevice);
        LOG("rm_is_supported_pci_device OK; RM names %04x:%04x \"%s\"", vendor, device, nm ? nm : "(null)");
    }
    return res;
}

bool NVRM::start(IOService *provider)
{
    // Without nvmultigpu=1 the gate is not there and this is startGPU() unchanged. With it, one card starts at a time and a
    // card the primary selector did not mark waits for the marked one (the overlapping start-up of the four-card report).
    const bool multi = nvrmMultiGpu();
    uint32_t selector = 0; PE_parse_boot_argn("nvselector", &selector, sizeof selector);
    const bool marked = provider && provider->getProperty("nullmoth-auto-primary") == kOSBooleanTrue;
    if (!nvrm_gate_enter(&gNVRMGate, multi, marked, selector != 0)) {
        if (provider && gNVRMWait.add(provider)) provider->retain();
        LOG("nvmultigpu: another card is starting, or the primary card is not running yet; this card waits to be offered again");
        // the start that refused us may have finished while we queued: then nobody is left to re-offer us
        if (!__atomic_load_n(&gNVRMGate.starting, __ATOMIC_ACQUIRE)) nvrmReofferNext();
        return false;
    }
    const bool ok = startGPU(provider);
    nvrm_gate_leave(&gNVRMGate, multi, ok);
    if (multi) nvrmReofferNext();   // one waiting card per finished start
    return ok;
}

bool NVRM::startGPU(IOService *provider)
{
    if (nvrm_driver_off()) {
        if (nvrmOffBarWanted(provider) && super::start(provider)) {
            fPCI = OSDynamicCast(IOPCIDevice, provider);
            if (fPCI) {
                LOG("driver off, BAR1 lost: placing it so the firmware screen can be shown (nvoffbar=1)");
                const bool placed = placeLargeBar1();
                LOG("driver off: BAR1 placement %s", placed ? "done" : "not possible; left as IOPCIFamily had it");
                fPCI->registerService(kIOServiceAsynchronous);        // matching runs again: NVRMBootFB reads nvrm-bar1-placed
                // Stay attached with nothing started: systemWillShutdown() puts BAR1, BAR3 and the root port window back as the
                // firmware left them (fBarUndo), without which some boards do not POST after a shutdown.
                if (placed) return true;
            }
        }
        return false;
    }
    if (!nvrm_kernel_supported()) return false;
    nvrm_guard_count_start();
    if (!super::start(provider)) return false;
    fPCI = OSDynamicCast(IOPCIDevice, provider);
    if (!fPCI) return false;
    // An unavailable telemetry lock disables this optional interface, not the GPU.
    fUtilizationLock = IOLockAlloc();
    gNVRMTab.add(this, nvrmMultiGpu(), provider->getProperty("nullmoth-auto-primary") == kOSBooleanTrue);
    nv_xnu_log_init();
    // The kernel mapping table is serialised, so its lock has to exist before RM maps anything.
    if (!nv_xnu_maps_init()) { LOG("IOLockAlloc FAILED for the kernel mapping table"); return false; }
    // Say which build is running, first thing. A machine reported "2.3.2" from the app while the kernel collection still
    // held a kext from before 2.1.0 (its BAR1 line used wording that version had replaced), and nothing in the log said
    // so: the installed bundle and the loaded kext can differ, and only the loaded one knows what it is. The loader
    // stamps this kext's kmod_info with the bundle version before start; the OSKextGetCurrent* helpers would say the
    // same but live in libkmod's c_start.o next to a second _start, which this kext already defines.
    LOG("NVRM kext %s (%s) starting", kmod_info.version, kmod_info.name);
    setProperty("nvrm-kext-version", kmod_info.version);
    {
        static bool sysctlsUp;
        if (!sysctlsUp) {
            sysctlsUp = true;
            sysctl_register_oid(&sysctl__debug_nvrm_isr_count); sysctl_register_oid(&sysctl__debug_nvrm_isr_handled);
            sysctl_register_oid(&sysctl__debug_nvrm_evt_posts); sysctl_register_oid(&sysctl__debug_nvrm_evt_waits);
            sysctl_register_oid(&sysctl__debug_nvrm_pm_waited);
        }
    }
    UInt8 bus = fPCI->getBusNumber(), dev = fPCI->getDeviceNumber(), fn = fPCI->getFunctionNumber();
    nv_xnu_register_pci(fPCI, 0, bus, dev, fn);
    nv_xnu_power_refresh();        // read the power source here, where no firmware lock is held
    nv_xnu_register_all_pci();   // RM's chipset init needs the whole PCI tree (host bridge, root ports)
    nv_state_t *nv = &fNv;
    nv->pci_info.domain    = 0;
    nv->pci_info.bus       = bus;
    nv->pci_info.slot      = dev;
    nv->pci_info.function  = fn;
    nv->pci_info.vendor_id = fPCI->configRead16(0x00);
    nv->pci_info.device_id = fPCI->configRead16(0x02);
    nv->subsystem_vendor   = fPCI->configRead16(0x2C);
    nv->subsystem_id       = fPCI->configRead16(0x2E);
    nv->handle = os_pci_init_handle(0, bus, dev, fn, NULL, NULL);
    nv->regs = &nv->bars[NV_GPU_BAR_INDEX_REGS];
    nv->fb   = &nv->bars[NV_GPU_BAR_INDEX_FB];
    snapshotBootScreen();
    // Firmware leaves bus mastering on for the card it used as console. Until pass 2 the RM has not touched the GPU, and
    // the first kernel panics of a fresh install were "element modified after free" in 16-byte kalloc elements (values 1
    // and 0x8040) inside rm_init_rm and rm_init_adapter, on a boot-to-boot random pattern (#529). Bus mastering stays
    // off until pass 2 turns it on, so nothing the card might still write can reach memory the kernel is reusing.
    {
        const UInt16 cmd0 = fPCI->configRead16(0x04);
        if (cmd0 & 0x4) { fPCI->configWrite16(0x04, (UInt16)(cmd0 & ~0x4u)); LOG("bus mastering was on from firmware (command 0x%04x): off until the RM starts the card", cmd0); }
    }
    const bool bar1Placed = placeLargeBar1();
    fBar1Placed = bar1Placed;
    readBARs();
    publishBarFacts();
    {
        const char *nm = rm_get_device_name(nv->pci_info.device_id, nv->subsystem_vendor, nv->subsystem_id);
        if (nm && nm[0] && !nvrmNameIsUnknown(nm)) {
            unsigned int len = 0; while (nm[len]) len++;
            fPCI->setProperty("model", (void *)nm, len + 1);
            LOG("published model=\"%s\" from RM's released-chip table", nm);
        } else {
            LOG("RM cannot name device 0x%04x (subsystem %04x:%04x) - publishing no model rather than a guess",
                nv->pci_info.device_id, nv->subsystem_vendor, nv->subsystem_id);
        }
    }
    LOG("armed on %02x:%02x.%d %04x:%04x", bus, dev, fn, nv->pci_info.vendor_id, nv->pci_info.device_id);
    fPCI->setProperty("nvrm-claimed", kOSBooleanTrue);
    if (!gNvClientsLock) gNvClientsLock = IOLockAlloc();
    if (!gNvClientsLock) { LOG("IOLockAlloc FAILED"); return false; }
    {
        char nogo[8] = { 0 };
        thread_t th = THREAD_NULL;
        { uint32_t ms = 0; const bool haveArg = PE_parse_boot_argn("nvrmsettle", &ms, sizeof(ms));
          fAutoGoSettleMs = nvrmAutoGoSettleMs(bar1Placed, nvrmIsTuringDeviceId(nv->pci_info.device_id), haveArg, ms); }
        if (PE_parse_boot_argn("-nvrmnogo", nogo, sizeof(nogo))) {
            setProperty("nvrm-autogo", "off (-nvrmnogo)"); LOG("auto-go OFF (-nvrmnogo): idle until `nvrmctl go <pass>`");
        } else {
            fBootHold = 1; adjustBusy(1); setProperty("nvrm-boot-hold", "holding");
            { uint32_t sec = 0; if (PE_parse_boot_argn("nvrmholdcap", &sec, sizeof(sec)) && sec >= 40 && sec <= 600) fHoldCapMs = sec * 1000; }
            if (thread_call_t cap = thread_call_allocate(&NVRM::bootHoldCap, this)) {
                // The BAR fallback settles for 100 s. A 40 s timer from start() expired before
                // autoGo could run, so its display-arm CAS always failed on that path.
                // Keep the 40 s bring-up budget, measured after the configured settling delay.
                uint64_t dl, settle;
                clock_interval_to_deadline(40000, kMillisecondScale, &dl);
                clock_interval_to_absolutetime_interval(fAutoGoSettleMs, kMillisecondScale, &settle);
                dl += settle;
                fHoldCapCall = cap;   // the cap re-arms itself while the bring-up is still running
                retain(); thread_call_enter_delayed(cap, dl);
            } else { releaseBootHold("no cap timer — not holding"); }
            retain();
            if (kernel_thread_start(&NVRM::autoGo, this, &th) == KERN_SUCCESS) {
                thread_deallocate(th);
                setProperty("nvrm-autogo-settle-ms", fAutoGoSettleMs, 32);
                // NVAccel reads it from the PCI device it shares with this service to place its headless deadline
                // after the settle delay instead of inside it.
                fPCI->setProperty("nvrm-autogo-settle-ms", fAutoGoSettleMs, 32);
                setProperty("nvrm-autogo", "scheduled");
                LOG("auto-go: go(2) in %u ms on its own thread (BAR1 %s); registry held busy until the display is armed (cap settle + 40 s)",
                    fAutoGoSettleMs, bar1Placed ? "placed outside the console" : "not placed");
            } else {
                release();
                setProperty("nvrm-autogo", "kernel_thread_start FAILED"); LOG("auto-go: kernel_thread_start FAILED — idle until asked");
                releaseBootHold("no auto-go thread");
            }
        }
    }
    {
        // Join the power tree under the GPU's IOPCIDevice: on system sleep NVRM puts the GPU in standby
        // (rm_power_management, as nvidia_suspend() does on Linux) before the slot loses power, and resumes it on wake.
        // WAS: NVRM was not in the power tree at all; on wake NVRMFB re-lit its head on a GPU nobody had resumed and
        // the push spun in nvEvoMakeRoom ("Darkwake Exit Failure", screen black until a hard reset).
        // -nvrmnopm keeps the old behaviour.
        static IOPMPowerState kNvrmPowerStates[2] = {
            { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
            { kIOPMPowerStateVersion1, kIOPMPowerOn | kIOPMDeviceUsable, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
        };
        char nopm[8] = { 0 };
        if (PE_parse_boot_argn("-nvrmnopm", nopm, sizeof nopm)) {
            setProperty("nvrm-pm", "off (-nvrmnopm)");
            LOG("power management OFF (-nvrmnopm): system sleep will lose the GPU");
        } else {
            if (!gNvPmGate) gNvPmGate = IOLockAlloc();
            PMinit();
            fPCI->joinPMtree(this);
            registerPowerDriver(this, kNvrmPowerStates, 2);
            changePowerStateTo(1);
            fPmJoined = true;
            setProperty("nvrm-pm", "running");
            LOG("power management: joined the power tree under %s (sleep = RM standby, wake = RM resume)", fPCI->getName());
        }
    }
    registerService();
    return true;
}

extern "C" int sysctlbyname(const char *, void *, size_t *, void *, size_t);
static int nvrmSysW(const char *n, int v) { return sysctlbyname(n, NULL, NULL, &v, sizeof v); }
static int nvrmSysR(const char *n, int *v) { size_t l = sizeof *v; *v = -99; return sysctlbyname(n, v, &l, NULL, 0); }
static unsigned nvrmPop(unsigned m) { unsigned c = 0; for (; m; m &= m - 1) c++; return c; }

// The name of another display-class PCI device whose memory windows hold [phys, phys + size), or NULL. The name points
// into a static buffer, so the caller logs it before calling again.
static const char *otherDisplayHolding(IOPCIDevice *self, uint64_t phys, uint64_t size)
{
    static char name[64];
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    OSIterator *it = match ? IOService::getMatchingServices(match) : NULL;
    OSSafeReleaseNULL(match);
    const char *found = NULL;
    while (OSObject *o = it ? it->getNextObject() : NULL) {
        IOPCIDevice *d = OSDynamicCast(IOPCIDevice, o);
        if (!d || d == self) continue;
        OSData *cc = OSDynamicCast(OSData, d->getProperty("class-code"));
        if (!cc || cc->getLength() < 3 || ((const uint8_t *)cc->getBytesNoCopy())[2] != 0x03) continue;
        for (unsigned i = 0, n = d->getDeviceMemoryCount(); i < n && !found; i++) {
            IODeviceMemory *dm = d->getDeviceMemoryWithIndex(i);
            if (!dm) continue;
            const uint64_t pa = dm->getPhysicalAddress(), len = dm->getLength();
            if (phys >= pa && size <= len && phys - pa <= len - size) {
                strlcpy(name, d->getName(), sizeof name);
                found = name;
            }
        }
        if (found) break;
    }
    OSSafeReleaseNULL(it);
    return found;
}

void NVRM::snapshotBootScreen()
{
    char nb[8] = { 0 };
    if (PE_parse_boot_argn("-nvrmnobootscreen", nb, sizeof(nb))) { LOG("boot screen: snapshot OFF (-nvrmnobootscreen)"); return; }
    PE_Video v; bzero(&v, sizeof v);
    if (!getPlatform() || getPlatform()->getConsoleInfo(&v) != kIOReturnSuccess || !v.v_baseAddr || v.v_depth != 32
        || !v.v_width || !v.v_height || v.v_rowBytes < v.v_width * 4) {
        LOG("boot screen: no 32-bit console to copy (depth %lu %lux%lu) — the takeover will be black", v.v_depth, v.v_width, v.v_height);
        return;
    }
    const uint64_t phys = (uint64_t)v.v_baseAddr & ~3ull;
    const uint64_t size = (uint64_t)v.v_rowBytes * v.v_height;
    if (size > (64ull << 20)) { LOG("boot screen: %llu B is not a console — not copying", size); return; }
    // On a laptop with an Intel GPU the firmware console lives on that GPU, not on this card. Its memory is not ours to
    // read, and the takeover then starts black, which is what that panel shows anyway. Only a console another display
    // device's window holds is skipped: this card's own window list can be out of date before BAR1 is placed, so a
    // console missing from it is still copied.
    if (const char *other = otherDisplayHolding(fPCI, phys, size)) {
        LOG("boot screen: the console at 0x%llx (+%llu) is in %s's window, not this card's — not copying", phys, size, other);
        return;
    }
    IOMemoryDescriptor *src = IOMemoryDescriptor::withAddressRange(phys, size, kIODirectionIn | kIOMemoryMapperNone, NULL);
    IOMemoryMap *map = src ? src->map(kIOMapInhibitCache) : NULL;
    IOBufferMemoryDescriptor *dst = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut, size, page_size);
    if (!map || !dst) {
        LOG("boot screen: map %p buffer %p — not copying", map, dst);
        if (map) map->release(); if (src) src->release(); if (dst) dst->release();
        return;
    }
    const uint64_t t0 = mach_absolute_time();
    const volatile uint64_t *in = (const volatile uint64_t *)map->getVirtualAddress();
    uint64_t *out = (uint64_t *)dst->getBytesNoCopy();
    for (uint64_t i = 0; i < size / 8; i++) out[i] = in[i];
    uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - t0, &ns);
    map->release(); src->release();
    fBootScreen = dst; fBootW = (uint32_t)v.v_width; fBootH = (uint32_t)v.v_height; fBootPitch = (uint32_t)v.v_rowBytes;
    LOG("boot screen: copied %ux%u pitch %u (%llu KB) from the console at 0x%llx in %llu ms", fBootW, fBootH, fBootPitch,
        size >> 10, phys, ns / 1000000);
}

static int gSmoothHead = -1; static uint32_t gSmoothPclk = 0, gSmoothHt = 0, gSmoothVt = 0;
extern "C" int nvrm_xnu_smooth_takeover(NvU32 *head, NvU32 *pclkHz, NvU32 *htotal, NvU32 *vtotal)
{
    char nb[8] = { 0 };
    if (PE_parse_boot_argn("-nvkmsnosmooth", nb, sizeof(nb))) { kprintf("NVRM-xnu: smooth takeover OFF (-nvkmsnosmooth)\n"); return 0; }
    if (gSmoothHead < 0 || !head || !pclkHz || !htotal || !vtotal) return 0;
    *head = (NvU32)gSmoothHead; *pclkHz = gSmoothPclk; *htotal = gSmoothHt; *vtotal = gSmoothVt;
    return 1;
}

void NVRM::measureBootRaster()
{
    nv_state_t *nv = &fNv;
    if (nv->bars[0].size < 0x700000) { LOG("boot raster: BAR0 too small (%llu) — unknown", (unsigned long long)nv->bars[0].size); return; }
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)nv->bars[0].cpu_address, 0x700000, kIODirectionIn);
    IOMemoryMap *map = md ? md->map(kIOMapInhibitCache) : NULL;
    if (!map) { LOG("boot raster: BAR0 map failed — unknown"); if (md) md->release(); return; }
    volatile uint8_t *b = (volatile uint8_t *)map->getVirtualAddress();
    char out[256]; int o = 0; out[0] = 0;
    for (unsigned h = 0; h < 4; h++) {
        const uint32_t rs = *(volatile uint32_t *)(b + 0x682064 + 0x8000 + h * 0x400);
        const uint32_t pc = *(volatile uint32_t *)(b + 0x68200c + 0x8000 + h * 0x400);
        uint32_t last = *(volatile uint32_t *)(b + 0x616330 + h * 0x800) & 0xffff, maxv = last, wraps = 0, moves = 0;
        const uint32_t raw0 = *(volatile uint32_t *)(b + 0x616330 + h * 0x800);
        uint64_t t0 = mach_absolute_time(), ns = 0, tFirst = 0, tLast = 0;
        if ((raw0 & 0xffff0000) == 0xbadf0000) { LOG("boot raster: head %u vline reads 0x%08x (PRI error) — unknown", h, raw0); continue; }
        do {
            const uint32_t v = *(volatile uint32_t *)(b + 0x616330 + h * 0x800) & 0xffff;
            if (v != last) moves++;
            if (v + 64 < last) { wraps++; if (!tFirst) tFirst = mach_absolute_time(); tLast = mach_absolute_time(); }
            if (v > maxv) maxv = v;
            last = v;
            absolutetime_to_nanoseconds(mach_absolute_time() - t0, &ns);
        } while (ns < 250000000ull);
        uint64_t span = 0; if (wraps >= 2) absolutetime_to_nanoseconds(tLast - tFirst, &span);
        const uint32_t mhz = (wraps >= 2 && span) ? (uint32_t)((uint64_t)(wraps - 1) * 1000000000000ull / span) : 0;
        const uint32_t vt = rs >> 16, ht = rs & 0xffff;
        const uint32_t calc = (vt && ht && pc && pc < 2000000000u) ? (uint32_t)((uint64_t)pc * 1000ull / ((uint64_t)vt * ht)) : 0;
        const bool ok = moves > 100 && mhz >= 20000 && mhz <= 250000;
        LOG("boot raster: head %u vline wraps %u moves %u max %u -> %u.%03u Hz %s | armed raster %ux%u pclk %u Hz -> %u.%03u Hz",
            h, wraps, moves, maxv, mhz / 1000, mhz % 1000, ok ? "MEASURED" : "(not running / unknown)", ht, vt, pc, calc / 1000, calc % 1000);
        if (ok && fBootHead < 0 && ht && vt && pc) { fBootHead = (int)h; fBootPclk = pc; fBootHt = ht; fBootVt = vt; }
        if (ok && gSmoothHead < 0 && ht && vt && pc) { gSmoothHead = (int)h; gSmoothPclk = pc; gSmoothHt = ht; gSmoothVt = vt; }
        if (ok && o < (int)sizeof(out) - 40) o += snprintf(out + o, sizeof(out) - (size_t)o, "%shead%u %u.%03uHz", o ? " " : "", h, mhz / 1000, mhz % 1000);
    }
    setProperty("nvrm-boot-raster", o ? out : "unknown");
    map->release(); md->release();
}

void NVRM::headWatch(void *p0, wait_result_t)
{
    NVRM *s = (NVRM *)p0;
    nv_state_t *nv = &s->fNv;
    IOMemoryDescriptor *md = nv->bars[0].size >= 0x700000
        ? IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)nv->bars[0].cpu_address, 0x700000, kIODirectionIn) : NULL;
    IOMemoryMap *map = md ? md->map(kIOMapInhibitCache) : NULL;
    if (map) {
        volatile uint8_t *b = (volatile uint8_t *)map->getVirtualAddress();
        uint32_t last[2] = { 0xffffffff, 0xffffffff }; uint64_t lastMove[2] = { 0, 0 }; int state[2] = { -1, -1 };
        static uint32_t armH[256], armW[256]; unsigned armLogs = 0;
        for (int i = 0; i < 256; i++) { armH[i] = *(volatile uint32_t *)(b + 0x688000 + 0x2000 + i * 4);
                                        armW[i] = *(volatile uint32_t *)(b + 0x688000 + 0x1000 + i * 4); }
        kprintf("NVRM-xnu: armwatch: baseline taken (firmware) head0 SET_CONTROL 0x%08x RASTER_SIZE 0x%08x\n", armH[0], armH[0x64 / 4]);
        uint64_t t0 = mach_absolute_time(), released = 0, ns = 0;
        for (;;) {
            const uint64_t now = mach_absolute_time();
            absolutetime_to_nanoseconds(now - t0, &ns);
            for (int h = 0; h < 2; h++) {
                const uint32_t v = *(volatile uint32_t *)(b + 0x616330 + h * 0x800);
                if (v != last[h]) { last[h] = v; lastMove[h] = now; }
                uint64_t idle = 0; absolutetime_to_nanoseconds(now - lastMove[h], &idle);
                const int st = idle < 20000000ull ? 1 : 0;
                if (st != state[h]) {
                    uint64_t up = 0; absolutetime_to_nanoseconds(now, &up);
                    kprintf("NVRM-xnu: headwatch: head %d %s at %llu ms since boot (vline raw 0x%08x)\n",
                            h, st ? "RUNNING" : "STOPPED", up / 1000000ull, v);
                    state[h] = st;
                }
            }
            for (int i = 0; i < 256 && armLogs < 400; i++) {
                const uint32_t vh = *(volatile uint32_t *)(b + 0x688000 + 0x2000 + i * 4);
                const uint32_t vw = *(volatile uint32_t *)(b + 0x688000 + 0x1000 + i * 4);
                uint64_t up = 0;
                if (vh != armH[i]) { absolutetime_to_nanoseconds(now, &up); kprintf("NVRM-xnu: armwatch: %llu ms HEAD0 +0x%03x 0x%08x -> 0x%08x\n", up / 1000000ull, 0x2000 + i * 4, armH[i], vh); armH[i] = vh; armLogs++; }
                if (vw != armW[i]) { absolutetime_to_nanoseconds(now, &up); kprintf("NVRM-xnu: armwatch: %llu ms WIN    +0x%03x 0x%08x -> 0x%08x\n", up / 1000000ull, 0x1000 + i * 4, armW[i], vw); armW[i] = vw; armLogs++; }
            }
            if (!released && s->fBootHold == 0) released = now;
            uint64_t sinceRel = 0; if (released) absolutetime_to_nanoseconds(now - released, &sinceRel);
            if ((released && sinceRel > 15000000000ull) || ns > 90000000000ull) break;
            IOSleep(5);
        }
        map->release();
    }
    if (md) md->release();
    kprintf("NVRM-xnu: headwatch: done\n");
    s->release();
    thread_terminate(current_thread());
}

void NVRM::releaseBootHold(const char *why)
{
    if (!OSCompareAndSwap(1, 0, &fBootHold) && !OSCompareAndSwap(2, 0, &fBootHold)) return;
    adjustBusy(-1);
    setProperty("nvrm-boot-hold", why);
    LOG("boot hold RELEASED (%s) — WindowServer's IOKitWaitQuiet may return", why);
}

void NVRM::bootHoldCap(thread_call_param_t p0, thread_call_param_t)
{
    NVRM *s = (NVRM *)p0;
    // bring-up still running and under the ceiling: look again in 10 s (the arm-time retain stays with the call)
    if (s->fBootHold == 1 && s->fBringUp == 1 && s->fHoldWaitedMs + 40000 < s->fHoldCapMs && s->fHoldCapCall) {
        const uint32_t step = 10000;
        s->fHoldWaitedMs += step;
        uint64_t dl; clock_interval_to_deadline(step, kMillisecondScale, &dl);
        char why[96]; snprintf(why, sizeof why, "holding: bring-up still running %u s past the 40 s mark (cap %u s)", s->fHoldWaitedMs / 1000, s->fHoldCapMs / 1000);
        s->setProperty("nvrm-boot-hold", why);
        LOG("boot hold: bring-up still running at settle + %u s, holding on (cap settle + %u s)", 40 + s->fHoldWaitedMs / 1000 - step / 1000, s->fHoldCapMs / 1000);
        thread_call_enter_delayed(s->fHoldCapCall, dl);
        return;
    }
    if (OSCompareAndSwap(1, 0, &s->fBootHold)) {
        s->adjustBusy(-1); s->setProperty("nvrm-boot-hold", "CAP: bring-up not finished in time, NOT arming");
        LOG("boot hold RELEASED by the settle + %u s cap — the bring-up will not arm with WindowServer up", 40 + s->fHoldWaitedMs / 1000);   // the cap is no longer fixed at 40 s
    }
    s->release();
}

// connected-displays of every display nub NVRMFB has enumerated; see nvrmNoDisplayOnAnyConnector
static bool nvrmNoDisplayAnywhere()
{
    int counts[8]; unsigned n = 0;
    if (OSIterator *it = IOService::getMatchingServices(IOService::serviceMatching("NVRMDisplay"))) {
        while (OSObject *o = it->getNextObject()) {
            IOService *sv = OSDynamicCast(IOService, o);
            if (!sv || n >= 8) continue;
            OSNumber *cd = OSDynamicCast(OSNumber, sv->getProperty("connected-displays"));
            counts[n++] = cd ? (int)cd->unsigned32BitValue() : -1;
        }
        it->release();
    }
    return nvrmNoDisplayOnAnyConnector(counts, n);
}

void NVRM::autoGo(void *p0, wait_result_t)
{
    NVRM *s = (NVRM *)p0;
    char nb[8] = { 0 };
    // A card whose BAR1 could not be placed outside the boot screen (Resizable BAR off: 256 MB) waits 100 s here with
    // the registry held busy, so the verbose screen showed only "busy timeout ... 'NVRM'" for two minutes and users
    // reset machines that were still starting (7 reports, RTX 2070 SUPER/3050/3060/3070, GTX 1660 SUPER, Quadro T2000,
    // in 1.12). Say what is happening and how to avoid it, every 15 s, on the same screen.
    if (s->fAutoGoSettleMs > 5000) {
        if (s->fBar1Placed)
            LOG("starting the NVIDIA card: waiting %u s for the card's firmware to settle before it takes over the "
                "screen. This is not a freeze.", s->fAutoGoSettleMs / 1000);
        else
            LOG("starting the NVIDIA card: waiting %u s before it takes over the screen, because its memory window (BAR1) "
                "is too small to move off the boot screen. This is not a freeze. To start in about a second instead, enable "
                "\"Above 4G Decoding\" and \"Resizable BAR\" in the BIOS.", s->fAutoGoSettleMs / 1000);
        nvrm_guard_pause_for_wait(s->fAutoGoSettleMs);
        for (uint32_t left = s->fAutoGoSettleMs; left > 0;) {
            const uint32_t step = left > 15000 ? 15000 : left;
            IOSleep(step); left -= step;
            if (left) LOG("starting the NVIDIA card: %u s left (not a freeze)", left / 1000);
        }
        // the framebuffer says whether it already cleared the guard this boot (a display lit, or a card with nothing
        // to light, after a hand-run go during the wait): counting again then would leave a strike behind a good boot
        { int cleared = 0; if (nvrmSysR("debug.nvrmfb_guard_cleared", &cleared) != 0) cleared = 0; nvrm_guard_resume_after_wait(cleared > 0); }
    } else {
        IOSleep(s->fAutoGoSettleMs);
    }
    s->measureBootRaster();
    { thread_t hw = THREAD_NULL; s->retain();
      if (kernel_thread_start(&NVRM::headWatch, s, &hw) == KERN_SUCCESS) thread_deallocate(hw); else s->release(); }
    { uint64_t t; clock_get_uptime(&t); absolutetime_to_nanoseconds(t, &s->fBringUpStartNs); }
    s->fBringUp = 1;
    IOReturn r = s->go(2);
    // how long go(2) took, which says whether the cap was wide enough
    { uint64_t t, ns = 0; clock_get_uptime(&t); absolutetime_to_nanoseconds(t, &ns);
      LOG("auto-go: go(2) took %llu ms (hold cap settle + %u s)", (unsigned long long)((ns - s->fBringUpStartNs) / 1000000ULL), s->fHoldCapMs / 1000); }
    kprintf("NVRM-xnu: auto-go: go(2) -> 0x%x (fPassDone %u)\n", r, s->fPassDone);
    if (r != kIOReturnSuccess) {
        s->fBringUp = 0;
        s->setProperty("nvrm-autogo", r == kIOReturnBusy ? "busy (a hand-run go is running)" : "go(2) failed");
        s->releaseBootHold("go(2) failed");
    } else {
        s->setProperty("nvrm-autogo", "up");
        int want = 1; if (!PE_parse_boot_argn("nvfbheads", &want, sizeof(want)) || want < 1) want = 1;
        // Hybrid laptops (boot-arg nmhybrid 1 or 2): the panel is another GPU's framebuffer, published as head 4 of this
        // accelerator. Arming before it exists leaves it without a pipe until a second write and a WindowServer restart,
        // so wait for it, bounded by the same 20 s as the NVIDIA heads.
        bool hyWait = false;
        { int lv = 0; const int present = PE_parse_boot_argn("nmhybrid", &lv, sizeof(lv)); hyWait = present && lv >= 1 && lv <= 4; }
        int mask = 0, last = -1, still = 0, i;
        for (i = 0; i < 20; i++) {
            if (nvrmSysR("debug.nvaccel_heads_published", &mask) != 0) mask = 0;
            if (hyWait) {
                if (mask & 0x10) { LOG("auto-go: hybrid: the other GPU's panel published as head 4 after %d s", i); break; }
                if (i == 0) LOG("auto-go: hybrid: waiting up to 20 s for the other GPU's panel (head 4) before arming");
                IOSleep(1000);
                continue;
            }
            if ((int)nvrmPop((unsigned)mask) >= want) break;
            still = (mask && mask == last) ? still + 1 : 0; last = mask;
            if (still >= 3) break;
            if (gDisplayNub0) {
                OSNumber *cd = OSDynamicCast(OSNumber, gDisplayNub0->getProperty("connected-displays"));
                if (cd && cd->unsigned32BitValue() > 0 && (int)nvrmPop((unsigned)mask) >= (int)cd->unsigned32BitValue()) break;
            }
            // Every connector list is enumerated and none carries a display (a panel on the Intel or AMD GPU): no head
            // will ever publish, so waiting out the full twenty seconds only holds the registry for nothing.
            if (!mask && nvrmNoDisplayAnywhere()) { LOG("auto-go: no display on any NVIDIA connector - not waiting for heads"); break; }
            IOSleep(1000);
        }
        LOG("auto-go: heads published mask 0x%x (%u of nvfbheads %d) after %d s", mask, nvrmPop((unsigned)mask), want, i);
        s->fBringUp = 0;   // the cap may fire from here on; the next line decides whether it got there first
        const bool earlyRelease = nvrmReleaseBeforeArming((unsigned)mask, nvrmNoDisplayAnywhere());
        if (!OSCompareAndSwap(1, 2, &s->fBootHold)) {
            LOG("auto-go: the hold was already released (cap) — NOT arming with WindowServer up; heads 0x%x", mask);
            s->setProperty("nvrm-display", "not armed: hold released by the cap first");
            s->release(); thread_terminate(current_thread()); return;
        }
        if (earlyRelease) s->releaseBootHold("no NVIDIA display head can arm yet; arming continues for a later hotplug");
        int armed = 0;
        for (i = 0; i < 10; i++) { nvrmSysW("debug.nvaccelfb", 1); nvrmSysR("debug.nvaccelfb", &armed); if (armed == 1) break; IOSleep(1000); }
        if (armed == 1) nvrmSysW("debug.nvaccelfb", 4);
        LOG("auto-go: accelerator level 1 %s after %d tr%s%s", armed == 1 ? "took" : "NEVER took", i + 1, i ? "ies" : "y",
            armed == 1 ? ", level 4 written" : " — not arming further");
        int k5 = -99, ag = -99, iop = -99;
        if (armed == 1) {
            nvrmSysW("debug.nvrmfb_agdc_k5", 1);
            nvrmSysW("debug.nvrmfb_agdc", 1);
            for (i = 0; i < 25; i++) { nvrmSysR("debug.nvrmfb_agdc", &ag); if (ag == 1 || ag < 0) break; IOSleep(200); }
            nvrmSysW("debug.nvaccel_iop", 1);
            nvrmSysR("debug.nvrmfb_agdc_k5", &k5); nvrmSysR("debug.nvaccel_iop", &iop);
        }
        char sum[160];
        snprintf(sum, sizeof sum, "heads 0x%x accel %d agdc %d k5 %d iop %d", mask, armed == 1 ? 4 : 0, ag, k5, iop);
        s->setProperty("nvrm-display", sum);
        LOG("auto-go: display stack: %s", sum);
        if (!earlyRelease) s->releaseBootHold(armed == 1 && ag == 1 && iop == 1 ? "display armed" : "display PARTIAL");
        if (armed == 1 && iop == 1 && !PE_parse_boot_argn("-nvrmnoflip", nb, sizeof(nb))) {
            // Zero-copy scan-out shows only what sits in this GPU's VRAM. One setup blacks the screen (cursor only) the
            // moment it starts, and every report was fixed by -nvrmnoflip: a second accelerated GPU (Intel/AMD iGPU;
            // WindowServer composites there, issues #16 #26). The black screens reported for unplaced windows (#54, #82)
            // also had an iGPU, which the check below covers, so whether BAR1 was moved doesn't decide flipping. That
            // setup keeps the copy path, decided below from what is known right now.
            const char *why = nullptr;
            // An unplaced BAR1 of 512 MB or more holds the scan-out surfaces, so it may flip. A 256 MB firmware window
            // keeps the copy path: 12 machines in the field run that way with no iGPU, and none has been tested flipping.
            const uint64_t bar1MB = s->fNv.bars[NV_GPU_BAR_INDEX_FB].size >> 20;
            if (!s->fBar1Placed && bar1MB < 512) why = "BAR1 not placed and below 512 MB (small firmware window)";
            else if (!s->fBar1Placed) LOG("auto-go: BAR1 not placed (%llu MB) - flip allowed", bar1MB);
            if (!why) if (OSIterator *it = IOService::getMatchingServices(IOService::serviceMatching("IOAccelerator"))) {
                while (OSObject *o = it->getNextObject()) {
                    const char *cn = o->getMetaClass()->getClassName();
                    if (cn && strncmp(cn, "NV", 2)) { why = "another GPU's accelerator is running"; LOG("auto-go: accelerator %s present", cn); break; }
                }
                it->release();
            }
            if (why) {
                LOG("auto-go: zero-copy flip NOT started: %s; the copy path stays (as -nvrmnoflip)", why);
                s->setProperty("nvrm-flip", "off (copy path)");
                s->release(); thread_terminate(current_thread()); return;
            }
            int f = -99; nvrmSysW("debug.nvaccel_iop_flip", 1); nvrmSysR("debug.nvaccel_iop_flip", &f);
            LOG("auto-go: flip debug.nvaccel_iop_flip=%d (right after the release)", f);
            s->setProperty("nvrm-flip", f == 1 ? "on" : "FAILED");
        }
    }
    s->release();
    thread_terminate(current_thread());
}

static void nv_xnu_dump_gsp_regs(nv_state_t *nv)
{
    static const struct { uint32_t off; const char *name; } regs[] = {
        {0x000000, "BOOT_0"}, {0x110040, "PGSP_FALCON_MAILBOX0"}, {0x110044, "PGSP_FALCON_MAILBOX1"},
        {0x1100f4, "PGSP_FALCON_HWCFG2(bit13=lockdown)"}, {0x110080, "PGSP_FALCON_OS"}, {0x110008, "PGSP_FALCON_IRQSTAT"},
        {0x1103c0, "PGSP_FALCON_ENGINE"}, {0x111388, "GSP_RISCV_CPUCTL(bit7=active,bit4=halted)"}, {0x111668, "GSP_RISCV_BCR_CTRL"},
        {0x110804, "PGSP_MAILBOX(0)"}, {0x110808, "PGSP_MAILBOX(1)"}, {0x110c00, "PGSP_QUEUE_HEAD(0)"},
        {0x1FA824, "PFB_PRI_MMU_WPR2_ADDR_LO"}, {0x1FA828, "PFB_PRI_MMU_WPR2_ADDR_HI"}, {0x118234, "GFW_BOOT_PROGRESS(scratch05_0)"},
    };
    if (nv->bars[0].size < 0x200000) { kprintf("NVRM-xnu: regdump: no BAR0\n"); return; }
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)nv->bars[0].cpu_address, 0x200000, kIODirectionInOut);
    if (!md) { kprintf("NVRM-xnu: regdump: withPhysicalAddress failed\n"); return; }
    IOMemoryMap *map = md->map(kIOMapInhibitCache);
    if (!map) { kprintf("NVRM-xnu: regdump: map failed\n"); md->release(); return; }
    volatile uint8_t *base = (volatile uint8_t *)map->getVirtualAddress();
    for (unsigned i = 0; i < sizeof regs / sizeof regs[0]; i++) {
        uint32_t v = *(volatile uint32_t *)(base + regs[i].off);
        kprintf("NVRM-xnu: regdump %-42s @0x%06x = 0x%08x\n", regs[i].name, regs[i].off, v);
    }
    map->release(); md->release();
}

// 10-07 (1401 Probe uploads, 70+ PCs): BAR1 was always moved to 1 TB (kBigBase) with the window capped at
// 0x3ffbfffffff - the PCI window of Core Ultra 200 (Arrow Lake) boards, where it was built. Every Intel 6th-14th gen probe
// reports 39 bits physical and a 64-bit PCI window ending at 0x7fffffffff (512 GB); 46-bit Alder/Raptor Lake boards and
// Ryzen B450/B550 boards end theirs at 0x7fffffffff too. At 1 TB the card is outside what the CPU or the host bridge
// decodes, so on those machines the GPU went silent. BAR1 now goes where the FIRMWARE says PCI memory lives: the host
// bridge's ACPI _CRS 64-bit window, clipped to the CPU's MAXPHYADDR, top-down, away from every other device and bridge.

// The largest 64-bit memory window in the host bridge's _CRS (QWord Address Space descriptors, ACPI 6.5 6.4.3.5.1).
static bool nvrmHostWindow(IOService *dev, UInt64 *outMin, UInt64 *outMax,
                           UInt64 addressMax, bool *advertisedWindow)
{
    IOACPIPlatformDevice *acpi = NULL;
    IOService *s = dev;
    for (int i = 0; s && i < 16 && !acpi; i++) { s = s->getProvider(); acpi = OSDynamicCast(IOACPIPlatformDevice, s); }
    if (!acpi) return false;
    OSObject *o = NULL;
    if (acpi->evaluateObject("_CRS", &o) != kIOReturnSuccess || !o) return false;
    bool found = false;
    if (OSData *d = OSDynamicCast(OSData, o)) {
        const UInt8 *p = (const UInt8 *)d->getBytesNoCopy();
        UInt32 n = d->getLength(), i = 0;
        while (p && i < n) {
            UInt8 t = p[i];
            if (t & 0x80) {                                   // large item: tag, 16-bit length, body
                if (i + 3 > n) break;
                UInt32 len = (UInt32)p[i + 1] | ((UInt32)p[i + 2] << 8);
                if (i + 3 + len > n) break;
                if (t == 0x8A && len >= 43 && p[i + 3] == 0) {   // QWord Address Space, resource type 0 = memory
                    UInt64 mn, mx, tra;
                    memcpy(&mn, p + i + 14, 8); memcpy(&mx, p + i + 22, 8); memcpy(&tra, p + i + 30, 8);
                    if (tra == 0 && mn >= (4ULL << 30) && mx > mn) {
                        *advertisedWindow = true;
                        UInt64 clippedMax = 0;
                        if (nvrmClipBarWindow(mn, mx, addressMax, &clippedMax)
                            && (!found || clippedMax - mn > *outMax - *outMin)) {
                            *outMin = mn; *outMax = clippedMax; found = true;
                        }
                    }
                }
                i += 3 + len;
            } else {                                          // small item: length in bits 0-2; 0x79 = end tag
                if ((t >> 3) == 0x0F) break;
                i += 1 + (t & 7);
            }
        }
    }
    o->release();
    return found;
}

// CPUID 0x80000008 EAX[7:0]: the physical address width the CPU can reach (MAXPHYADDR).
static unsigned nvrmPhysBits()
{
    uint32_t a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0x80000000u), "c"(0u));
    if (a < 0x80000008u) return 36;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0x80000008u), "c"(0u));
    return a & 0xff;
}

// True when [lo, hi] overlaps a memory range of any other PCI function, or the prefetchable window of a bridge other
// than our own root port (two bridges claiming one address is a decode conflict even with nothing behind it).
static bool nvrmRangeBusy(UInt64 lo, UInt64 hi, IOPCIDevice *self, IOPCIDevice *ourPort)
{
    bool busy = false;
    IORegistryIterator *it = IORegistryIterator::iterateOver(gIOServicePlane, kIORegistryIterateRecursively);
    if (!it) return true;
    while (OSObject *e = it->getNextObject()) {
        IOPCIDevice *d = OSDynamicCast(IOPCIDevice, e);
        if (!d || d == self || d == ourPort) continue;
        if (OSArray *mem = d->getDeviceMemory())
            for (unsigned i = 0; i < mem->getCount() && !busy; i++)
                if (IOMemoryDescriptor *m = OSDynamicCast(IOMemoryDescriptor, mem->getObject(i))) {
                    UInt64 a = m->getPhysicalAddress(), z = a + m->getLength() - 1;
                    if (m->getLength() && a <= hi && z >= lo) { LOG("bar1: 0x%llx-0x%llx is taken by a PCI device's 0x%llx-0x%llx", lo, hi, a, z); busy = true; }
                }
        if (!busy && (d->configRead8(0x0e) & 0x7f) == 1) {
            UInt32 w = d->configRead32(0x24);
            UInt64 a = ((UInt64)d->configRead32(0x28) << 32) | ((UInt64)(w & 0xfff0) << 16);
            UInt64 z = ((UInt64)d->configRead32(0x2c) << 32) | ((UInt64)(w & 0xfff00000) | 0xfffff);
            if ((w & 0xf) == 1 && z > a && a <= hi && z >= lo) { LOG("bar1: 0x%llx-0x%llx overlaps a bridge window 0x%llx-0x%llx", lo, hi, a, z); busy = true; }
        }
        if (busy) break;
    }
    it->release();
    return busy;
}

// 10-11 (community fix, Discord #general, i5-6500 on B150): Intel's reference _CRS leaves out the 64-bit window when the
// firmware sets M64L to 0, which this board does even with Above 4G Decoding on. The fallback above then clipped to an
// empty window at the CPU's 39-bit top ("cannot hold 8224 MB — not placing"), then the 100 s timeout and a black screen.
// An SSDT giving the same window through _CRS placed BAR1 at 0x7c00000000 and ran the desktop. Every Intel 6th-14th gen
// probe reports PCI 64-bit space ending at 0x7fffffffff, so the 256-512 GB range is used, but only when the firmware has
// already put BAR1 above 4 GB (it decodes 64-bit there), the CPU is Intel and reaches 39 bits, and RAM + 4 GB stays below
// 256 GB. Otherwise the reason is returned and the old window is kept; AMD is unchanged.
static const UInt64 kIntel39Min = 0x4000000000ULL, kIntel39Max = 0x7fffffffffULL;
static const char *nvrmIntel39Window(unsigned physBits, UInt64 fwBar1)
{
    uint32_t a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0u), "c"(0u));
    if (!(b == 0x756e6547u && d == 0x49656e69u && c == 0x6c65746eu)) return "not an Intel CPU";
    if (physBits < 39) return "CPU reaches fewer than 39 bits";
    if (fwBar1 < (4ULL << 30)) return "firmware left BAR1 below 4 GB";
    UInt64 memsize = 0; size_t l = sizeof memsize;
    if (sysctlbyname("hw.memsize", &memsize, &l, NULL, 0) != 0 || !memsize) return "hw.memsize unreadable";
    if (memsize + (4ULL << 30) >= kIntel39Min) return "RAM reaches into 256 GB";
    return NULL;
}

// True when the bus numbers advertised by a root port contain another PCI function. When BAR1 is moved,
// placeLargeBar1() replaces that root port's prefetchable window, so a sibling behind the same port must keep
// the old forwarding window and makes the move unsafe.
static bool nvrmBusRangeHasOtherDevices(UInt8 first, UInt8 last, IOPCIDevice *self)
{
    IORegistryIterator *it = IORegistryIterator::iterateOver(gIOServicePlane, kIORegistryIterateRecursively);
    if (!it) return true;  // Fail closed: do not replace a bridge window if the bus range cannot be checked.
    bool found = false;
    while (OSObject *e = it->getNextObject()) {
        IOPCIDevice *d = OSDynamicCast(IOPCIDevice, e);
        if (!d || d == self) continue;
        const UInt8 bus = d->getBusNumber();
        if (bus >= first && bus <= last) {
            LOG("bar1: root port bus range %u-%u also contains PCI device %u:%u:%u",
                first, last, bus, d->getDeviceNumber(), d->getFunctionNumber());
            found = true;
            break;
        }
    }
    it->release();
    return found;
}

// -nvrmkeepbar1: leave a BAR1 the firmware sized above 8 GB at that size (the behaviour before the 8 GB cap)
static bool nvrmKeepBar1Size()
{
    char nb[8] = { 0 };
    return PE_parse_boot_argn("-nvrmkeepbar1", nb, sizeof nb);
}

// True when something is known to be decoded above 4 GB on this machine: this card's own BAR3, or any PCI device's
// memory aperture. Firmware with Above 4G Decoding off places every BAR below 4 GB.
static bool nvrmAbove4GRouted(UInt64 ownBar3)
{
    if (ownBar3 >= (4ULL << 30)) return true;
    bool above = false;
    if (OSIterator *it = IOService::getMatchingServices(IOService::serviceMatching("IOPCIDevice"))) {
        while (!above) {
            IOService *d = OSDynamicCast(IOService, it->getNextObject());
            if (!d) break;
            for (unsigned i = 0, n = d->getDeviceMemoryCount(); i < n && !above; i++)
                if (IODeviceMemory *dm = d->getDeviceMemoryWithIndex(i)) above = dm->getPhysicalAddress() >= (4ULL << 30);
        }
        it->release();
    }
    return above;
}

bool NVRM::placeLargeBar1()
{
    static const UInt64 kBigBase = 0x10000000000ULL;
    static const UInt64 kHostWindowEnd = 0x3ffbfffffffULL;
    // -nvrmsmallbar: a 256 MB BAR1, as on a card without Resizable BAR (every GTX 16 and most Turing boards), placed by
    // the same path as a large one. Lets a ReBAR card run the small-BAR paths (surface budget, copy path, scan-out model)
    // for testing. It used to leave BAR1 to IOPCIFamily, which on a ReBAR card can leave BAR1 with no address at all
    // (measured on an RTX 5060: the nub held BAR0 and BAR3 only), so the switch never showed a small BAR1.
    bool smallBar = false;
    { char nb[8] = { 0 }; smallBar = PE_parse_boot_argn("-nvrmsmallbar", nb, sizeof nb); }
    UInt32 cap = 0;
    for (UInt32 off = 0x100, n = 0; off >= 0x100 && off < 0x1000 && n < 64; n++) {
        UInt32 h = fPCI->extendedConfigRead32(off);
        if (h == 0 || h == 0xffffffffU) break;
        if ((h & 0xffff) == 0x15) { cap = off; break; }
        off = h >> 20;
    }
    fRebar = {};
    if (!cap) { LOG("bar1: no Resizable BAR capability — BAR1 left as IOPCIFamily placed it"); return false; }
    UInt32 nbars = (fPCI->extendedConfigRead32(cap + 8) >> 5) & 7;
    UInt64 bar1Size = 0;
    UInt32 rbCtlOff = 0, rbCtlOld = 0, rbSupported = 0;
    for (UInt32 i = 0; i < nbars && i < 6; i++) {
        UInt32 ctl = fPCI->extendedConfigRead32(cap + 8 + i * 8);
        if ((ctl & 7) == 1) {
            bar1Size = 1ULL << (20 + ((ctl >> 8) & 0x3f));
            rbCtlOff = cap + 8 + i * 8; rbCtlOld = ctl; rbSupported = fPCI->extendedConfigRead32(cap + 4 + i * 8) >> 4;
        }
    }
    LOG("bar1: Resizable BAR capability @0x%x says BAR1 = %llu MB (sizes supported mask 0x%x)", cap, bar1Size >> 20, rbSupported);
    const UInt64 fwBar1Size = bar1Size;   // the size the firmware decoded, before any resize below
    fRebar.cap = true; fRebar.supported = rbSupported; fRebar.firmwareSize = bar1Size;
    // 10-07: with "Resizable BAR" off in the BIOS (or a card the firmware leaves at 256 MB) the firmware places a small
    // BAR1 over the boot screen and the driver had nothing to move. The card itself can resize: take the largest size it
    // supports between 4 GB and 8 GB, written below once the destination is proven free.
    // 10-07 (RTX 3060, RTX 5080): both cards resized to 16 GB stopped during bring-up (GSP boot,
    // NVAccel start), and the same RTX 3060 runs the desktop at 8 GB. 8 GB is the size proven on hardware (RTX 5060, 3060).
    UInt64 newSize = 0;
    if (smallBar) {
        if (!rbCtlOff || !((rbSupported >> 8) & 1)) { LOG("bar1: -nvrmsmallbar set but the card cannot be sized to 256 MB (mask 0x%x) — not placing", rbSupported); return false; }
        newSize = 256ULL << 20;
        LOG("bar1: -nvrmsmallbar set — BAR1 %llu MB will be sized to 256 MB, as on a card without Resizable BAR", bar1Size >> 20);
        bar1Size = newSize;
    } else if (const UInt64 shrunk = nvrmBar1ShrinkTarget(bar1Size, rbSupported, nvrmKeepBar1Size())) {
        if (!rbCtlOff) { LOG("bar1: BAR1 is %llu MB and larger than the proven 8 GB, but no resize control was found - left as is", bar1Size >> 20); return false; }
        LOG("bar1: BAR1 is %llu MB; will resize it to %llu MB (the size proven on hardware; -nvrmkeepbar1 keeps the firmware's)", bar1Size >> 20, shrunk >> 20);
        newSize = shrunk; bar1Size = shrunk;
    } else if (bar1Size < (4ULL << 30)) {
        for (int k = 13; k >= 12; k--) if (rbCtlOff && (rbSupported >> k) & 1) { newSize = 1ULL << (20 + k); break; }
        // Turing cards offer 64-256 MB only (sizes mask 0x1c0), and some firmware leaves them at 64 MB: a GTX 1630, a
        // GTX 1660 SUPER and an RTX 2060 in user logs, against 66 Turing boots at 256 MB. 64 MB holds four 1440p
        // surfaces, and the GTX 1630 at 64 MB parked console-overlapping VRAM until the park ceiling and then failed
        // mapMemory. Take the largest size the card offers that is bigger than the current one; the placement below is
        // the same path that moves 64 MB cards to 8 GB.
        for (int k = 11; !newSize && k >= 0; k--)
            if (rbCtlOff && ((rbSupported >> k) & 1) && (1ULL << (20 + k)) > bar1Size) newSize = 1ULL << (20 + k);
        if (!newSize) {
            // A 256 MB Turing / GTX 16 BAR1 the firmware placed over the boot screen: nothing to resize, but it still moves,
            // at its own size, by the path below. Left in place it kept the copy path (no zero-copy flip, tearing,
            // WindowServer waits; #215 #122 #196) and its VRAM shared addresses with the console. The same placement at
            // 256 MB runs zero-copy flips (an RTX 5060 with -nvrmsmallbar: 1,058 flips, none refused for VRAM).
            const UInt64 cur = ((UInt64)fPCI->configRead32(0x18) << 32) | (fPCI->configRead32(0x14) & ~0xFULL);
            PE_Video cv; bzero(&cv, sizeof cv);
            const UInt64 con = (getPlatform() && getPlatform()->getConsoleInfo(&cv) == kIOReturnSuccess) ? ((UInt64)cv.v_baseAddr & ~3ull) : 0;
            if (!(con && cur && con >= cur && con < cur + bar1Size)) {
                LOG("bar1: BAR1 is %llu MB, the card offers no larger size and the console is not inside it — not placing", bar1Size >> 20);
                return false;
            }
            LOG("bar1: BAR1 is %llu MB with the console inside it (0x%llx) and no larger size — moving it at this size", bar1Size >> 20, con);
        } else {
            LOG("bar1: BAR1 is %llu MB; will resize it to %llu MB", bar1Size >> 20, newSize >> 20);
            bar1Size = newSize;
        }
    }

    UInt32 lo14 = fPCI->configRead32(0x14), lo1c = fPCI->configRead32(0x1c), hi20 = fPCI->configRead32(0x20);
    UInt32 old18 = fPCI->configRead32(0x18);
    if (((lo14 >> 1) & 3) != 2 || ((lo1c >> 1) & 3) != 2) { LOG("bar1: BAR1/BAR3 are not both 64-bit (0x%x 0x%x) — not placing", lo14, lo1c); return false; }
    UInt64 bar3Old = ((UInt64)hi20 << 32) | (lo1c & ~0xFULL);
    OSArray *mem = fPCI->getDeviceMemory();
    IOMemoryDescriptor *bar0 = NULL; UInt64 bar3Size = 0;
    UInt32 lo10 = fPCI->configRead32(0x10);
    for (unsigned i = 0; mem && i < mem->getCount(); i++) {
        IOMemoryDescriptor *m = OSDynamicCast(IOMemoryDescriptor, mem->getObject(i));
        if (!m) continue;
        if (m->getPhysicalAddress() == bar3Old) bar3Size = m->getLength();
        if (m->getPhysicalAddress() == (lo10 & ~0xFULL)) bar0 = m;
    }
    // A BAR1 the firmware already sized (>= 4 GB, nothing to resize) and placed above 4 GB - so not over the boot screen,
    // which sits below 4 GB - with that aperture already in the nub is left exactly where it is. NVIDIA's own driver does
    // the same: nv_resize_pcie_bars() returns without a config write when the BAR is already the requested size, and it
    // never picks a base address itself. Moving such a BAR1 to the top of the host window broke RTX 4060/4070 laptops
    // (GH #185, #205): the aperture list for the new address could not be built, RM saw a BAR0-only table and
    // kbusVerifyBar2 failed (RmInitAdapter 0x24:0x72:1281).
    if (!newSize) {
        const UInt64 cur1 = ((UInt64)old18 << 32) | (lo14 & ~0xFULL);
        bool inNub = false;
        for (unsigned i = 0; mem && i < mem->getCount(); i++) {
            IOMemoryDescriptor *m = OSDynamicCast(IOMemoryDescriptor, mem->getObject(i));
            if (m && m->getPhysicalAddress() == cur1 && m->getLength() >= bar1Size) { inNub = true; break; }
        }
        // Only when the boot console is not inside this BAR1 (a hybrid laptop's boot screen on the iGPU, or no console):
        // a console inside BAR1 is why the move exists - RM's VRAM would overlap what macOS still scans out (STABILITY S9).
        PE_Video v; bzero(&v, sizeof v);
        const UInt64 con = (getPlatform() && getPlatform()->getConsoleInfo(&v) == kIOReturnSuccess) ? ((UInt64)v.v_baseAddr & ~3ull) : 0;
        const bool conInside = con && con >= cur1 && con < cur1 + bar1Size;
        if (cur1 >= (4ULL << 30) && inNub && !conInside) {
            LOG("bar1: BAR1 already %llu MB at 0x%llx (above 4 GB, in the nub, console 0x%llx outside it) - left in place, as NVIDIA's driver does",
                bar1Size >> 20, cur1, con);
            return true;
        }
    }
    // 10-09 (RTX 5050, MacPro7,1 + npci=0x3000): the nub's apertures can be stale — they sit at 0x100000000+ while
    // config space and assigned-addresses agree on BAR0 0x40000000 / BAR3 0x46000000 — so neither lookup above hits
    // and BAR1 is never placed. Take the missing BAR0/BAR3 from assigned-addresses, only where it agrees with the
    // live BAR register.
    UInt64 bar0Size = 0;
    if (!bar0 || !bar3Size) {
        UInt64 a0 = 0, s0 = 0, a3 = 0, s3 = 0;
        if (OSData *aa = OSDynamicCast(OSData, fPCI->getProperty("assigned-addresses"))) {
            const UInt32 *c = (const UInt32 *)aa->getBytesNoCopy();
            for (unsigned i = 0; c && i + 5 <= aa->getLength() / 4; i += 5) {
                const UInt64 a = ((UInt64)c[i + 1] << 32) | c[i + 2], s = ((UInt64)c[i + 3] << 32) | c[i + 4];
                if ((c[i] & 0xff) == 0x10) { a0 = a; s0 = s; }
                if ((c[i] & 0xff) == 0x1c) { a3 = a; s3 = s; }
            }
        }
        if (!bar0 && s0 && a0 == (lo10 & ~0xFULL)) { bar0Size = s0; LOG("bar1: BAR0 0x%llx+0x%llx taken from assigned-addresses (nub stale)", a0, s0); }
        if (!bar3Size && s3 && a3 == bar3Old) { bar3Size = s3; LOG("bar1: BAR3 0x%llx+0x%llx taken from assigned-addresses (nub stale)", a3, s3); }
    }
    if ((!bar0 && !bar0Size) || !bar3Size) { LOG("bar1: BAR0/BAR3 apertures not found in the nub (bar0 %d bar3 %llu) — not placing", bar0 != NULL, bar3Size); return false; }
    UInt64 wMin = 0, wMax = 0;
    const unsigned physBits = nvrmPhysBits();
    const UInt64 physTop = nvrmBarAddressMax(physBits, PAGE_SHIFT);
    const UInt64 fwBar1 = ((UInt64)old18 << 32) | (lo14 & ~0xFULL);
    bool advertisedWindow = false;
    if (nvrmHostWindow(fPCI, &wMin, &wMax, physTop, &advertisedWindow)) {
        LOG("bar1: host bridge 64-bit window 0x%llx-0x%llx (ACPI _CRS), CPU reaches %u bits", wMin, wMax, physBits);
    } else if (advertisedWindow) {
        // A real _CRS resource existed, but this API cannot describe any of it.
        // Do not invent an address outside that resource or touch live BARs.
        LOG("bar1: advertised host windows exceed CPU/IOKit physical range; keeping firmware BARs untouched");
        return false;
    } else if (!nvrmFallbackWindowAllowed(fwBar1, newSize, newSize >= (4ULL << 30) && nvrmAbove4GRouted(bar3Old))) {
        // Its BAR1 is fine where it is: with no 64-bit window in _CRS and BAR1 below 4 GB, nothing above 4 GB is routed,
        // unless the card must grow past 4 GB and other devices already sit above it (see nvrmFallbackWindowAllowed).
        LOG("bar1: no 64-bit window in the host bridge's _CRS and the firmware left BAR1 below 4 GB (0x%llx) - not placing", fwBar1);
        return false;
    } else if (const char *why = nvrmIntel39Window(physBits, fwBar1)) {
        wMin = kBigBase; wMax = kHostWindowEnd;   // the pre-10-07 placement, kept only where the CPU can reach it
        LOG("bar1: no 64-bit window in the host bridge's _CRS (%s) — using 0x%llx-0x%llx (CPU reaches %u bits)", why, wMin, wMax, physBits);
    } else {
        wMin = kIntel39Min; wMax = kIntel39Max;
        LOG("bar1: no 64-bit window in the host bridge's _CRS — using Intel's 256-512 GB PCI range 0x%llx-0x%llx (CPU reaches %u bits)", wMin, wMax, physBits);
    }
    if (wMax > physTop) wMax = physTop;
    const UInt64 total = bar1Size + bar3Size;
    if (wMax <= wMin || wMax - wMin + 1 < total) { LOG("bar1: window 0x%llx-0x%llx cannot hold %llu MB — not placing", wMin, wMax, total >> 20); return false; }
    UInt64 bar1Base = ((wMax + 1 - total) / bar1Size) * bar1Size, bar3Base = bar1Base + bar1Size, winEnd = bar3Base + bar3Size - 1;
    if (bar1Base < wMin || winEnd > wMax) { LOG("bar1: no %llu MB-aligned slot in 0x%llx-0x%llx — not placing", bar1Size >> 20, wMin, wMax); return false; }

    IOService *pp = fPCI->getProvider();
    IOPCIDevice *rp = pp ? OSDynamicCast(IOPCIDevice, pp->getProvider()) : NULL;
    if (!rp || (rp->configRead8(0x0e) & 0x7f) != 1) { LOG("bar1: parent root port not found — not placing"); return false; }
    // 10-07 (RTX 3070 eGPU on a MacBookPro16,1): only the parent bridge's window is reprogrammed below. Behind
    // Thunderbolt or a PCIe switch the parent is a downstream port, and the bridges above it keep their old windows, so the
    // moved BAR1/BAR3 were unreachable and RM failed kbusVerifyBar2 (NV_ERR_MEMORY_ERROR). Move only under a Root Port.
    {
        UInt8 pcie = 0;
        for (UInt8 c = rp->configRead8(0x34) & 0xfc, n = 0; c && n < 48; c = rp->configRead8(c + 1) & 0xfc, n++)
            if (rp->configRead8(c) == 0x10) { pcie = c; break; }
        const unsigned portType = pcie ? (rp->configRead16(pcie + 2) >> 4) & 0xf : 0xff;
        if (portType != 4) { LOG("bar1: parent bridge is not a PCIe Root Port (port type %u) - BAR1 left as IOPCIFamily placed it", portType); return false; }
    }
    UInt32 busr = rp->configRead32(0x18);
    UInt8 sec = (busr >> 8) & 0xff, sub = (busr >> 16) & 0xff, mybus = fPCI->getBusNumber();
    if (sec != mybus || sub < mybus) { LOG("bar1: root port bus range %u-%u does not cover our bus %u — not placing", sec, sub, mybus); return false; }
    // Firmware can leave unused subordinate bus numbers reserved (for example, GPU bus 1 with range 1-5).
    // Accept that range only when no other PCI function occupies it: changing the root port's window would
    // otherwise stop forwarding the sibling's BARs.
    // Only the buses BELOW ours: the GPU's own bus also carries its HDMI audio (and USB-C) functions, which sit behind
    // this window today and always did - counting them refused every card whose firmware reserved spare bus numbers.
    if (sub > mybus && nvrmBusRangeHasOtherDevices(mybus + 1, sub, fPCI)) {
        LOG("bar1: root port range %u-%u has other PCI devices — not placing", sec, sub);
        return false;
    }
    UInt32 rp24 = rp->configRead32(0x24), rp28 = rp->configRead32(0x28), rp2c = rp->configRead32(0x2c);
    if ((rp24 & 0xf) != 1) { LOG("bar1: root port prefetchable window is not 64-bit (0x%x) — not placing", rp24); return false; }
    // The top slot can be taken by another device (a second GPU, an NVMe's 64-bit BAR); try the slots below it.
    for (unsigned tries = 1; nvrmRangeBusy(bar1Base, winEnd, fPCI, rp); tries++) {
        if (tries >= 16 || bar1Base < wMin + bar1Size) { LOG("bar1: destination is in use — not placing"); return false; }
        bar1Base -= bar1Size; bar3Base = bar1Base + bar1Size; winEnd = bar3Base + bar3Size - 1;
    }
    // Fully construct the replacement list before changing any hardware. Every
    // early return below releases staging without publishing it to IOPCIFamily.
    struct StagedApertures {
        OSArray *arr;
        IODeviceMemory *m1, *m3, *m0;
        ~StagedApertures() {
            OSSafeReleaseNULL(arr); OSSafeReleaseNULL(m1);
            OSSafeReleaseNULL(m3); OSSafeReleaseNULL(m0);
        }
    } staged = { OSArray::withCapacity((mem ? mem->getCount() : 0) + 3),
        IODeviceMemory::withRange(bar1Base, bar1Size),
        IODeviceMemory::withRange(bar3Base, bar3Size),
        bar0 ? NULL : IODeviceMemory::withRange(lo10 & ~0xFULL, bar0Size) };
    if (!staged.arr || !staged.m1 || !staged.m3 || (!bar0 && !staged.m0)) {
        LOG("bar1: preflight descriptor creation failed (array %d BAR1 %d BAR3 %d BAR0 %d); firmware BARs untouched",
            staged.arr != NULL, staged.m1 != NULL, staged.m3 != NULL, bar0 != NULL || staged.m0 != NULL);
        return false;
    }
    OSArray *arr = staged.arr;
    if (!arr->setObject(bar0 ? bar0 : staged.m0)
        || !arr->setObject(staged.m1) || !arr->setObject(staged.m3)) {
        LOG("bar1: preflight aperture insertion failed; firmware BARs untouched");
        return false;
    }
    const UInt64 bar1Old = ((UInt64)old18 << 32) | (lo14 & ~0xFULL);
    for (unsigned i = 0; mem && i < mem->getCount(); i++) {
        IOMemoryDescriptor *m = OSDynamicCast(IOMemoryDescriptor, mem->getObject(i));
        if (!m || m == bar0) continue;
        UInt64 pa = m->getPhysicalAddress();
        if (pa == bar3Old || pa == bar1Old) continue;
        if (!arr->setObject(m)) {
            LOG("bar1: preflight could not retain another aperture; firmware BARs untouched");
            return false;
        }
    }
    LOG("bar1: preflight ready; descriptors and complete aperture list built before PCI writes");
    LOG("bar1: placing BAR1 %llu MB @0x%llx, BAR3 %llu MB @0x%llx, root port window 0x%llx-0x%llx (was 0x%08x %08x/%08x)",
        bar1Size >> 20, bar1Base, bar3Size >> 20, bar3Base, bar1Base, winEnd, rp24, rp28, rp2c);

    UInt16 cmd = fPCI->configRead16(0x04);
    NvrmBarUndo undo = {};
    undo.cmd = cmd; undo.resized = newSize != 0; undo.rbCtlOff = rbCtlOff; undo.rbCtlOld = rbCtlOld;
    undo.lo14 = lo14; undo.hi18 = old18; undo.lo1c = lo1c; undo.hi20 = hi20;
    undo.rp24 = rp24; undo.rp28 = rp28; undo.rp2c = rp2c;
    fPCI->configWrite16(0x04, cmd & ~0x2);
    if (newSize) {   // PCIe 7.8.6: change the size only while memory decode is off
        fPCI->extendedConfigWrite32(rbCtlOff, (rbCtlOld & ~0x3f00u) | ((UInt32)(__builtin_ctzll(newSize) - 20) << 8));
        UInt32 got = fPCI->extendedConfigRead32(rbCtlOff);
        if (((got >> 8) & 0x3f) != (UInt32)(__builtin_ctzll(newSize) - 20)) {
            LOG("bar1: resize to %llu MB did not take (ctl 0x%x) — restoring", newSize >> 20, got);
            fPCI->extendedConfigWrite32(rbCtlOff, rbCtlOld); fPCI->configWrite32(0x14, lo14); fPCI->configWrite32(0x18, old18);
            fPCI->configWrite16(0x04, cmd); return false;
        }
        LOG("bar1: resized BAR1 to %llu MB", newSize >> 20);
    }
    fPCI->configWrite32(0x14, (UInt32)bar1Base | (lo14 & 0xF)); fPCI->configWrite32(0x18, (UInt32)(bar1Base >> 32));
    fPCI->configWrite32(0x1c, (UInt32)bar3Base | (lo1c & 0xF)); fPCI->configWrite32(0x20, (UInt32)(bar3Base >> 32));
    rp->configWrite32(0x24, 0x0000fff0);
    rp->configWrite32(0x28, (UInt32)(bar1Base >> 32)); rp->configWrite32(0x2c, (UInt32)(winEnd >> 32));
    UInt32 w24 = (UInt32)((bar1Base >> 16) & 0xfff0) | ((UInt32)((winEnd >> 16) & 0xfff0) << 16);
    rp->configWrite32(0x24, w24);
    bool ok = (fPCI->configRead32(0x14) & ~0xFU) == (UInt32)bar1Base && fPCI->configRead32(0x18) == (UInt32)(bar1Base >> 32)
           && (fPCI->configRead32(0x1c) & ~0xFU) == (UInt32)bar3Base && fPCI->configRead32(0x20) == (UInt32)(bar3Base >> 32)
           && (rp->configRead32(0x24) & 0xfff0fff0) == w24 && rp->configRead32(0x28) == (UInt32)(bar1Base >> 32)
           && rp->configRead32(0x2c) == (UInt32)(winEnd >> 32);
    if (!ok) {
        LOG("bar1: read-back MISMATCH (bar 0x%08x %08x, rp 0x%08x %08x %08x) — restoring what IOPCIFamily left",
            fPCI->configRead32(0x14), fPCI->configRead32(0x18), rp->configRead32(0x24), rp->configRead32(0x28), rp->configRead32(0x2c));
        rp->configWrite32(0x24, 0x0000fff0); rp->configWrite32(0x28, rp28); rp->configWrite32(0x2c, rp2c); rp->configWrite32(0x24, rp24);
        if (newSize) fPCI->extendedConfigWrite32(rbCtlOff, rbCtlOld);
        fPCI->configWrite32(0x14, lo14); fPCI->configWrite32(0x18, old18); fPCI->configWrite32(0x1c, lo1c); fPCI->configWrite32(0x20, hi20);
        fPCI->configWrite16(0x04, cmd);
        return false;
    }
    fPCI->configWrite16(0x04, cmd | 0x2);

    fPCI->setDeviceMemory(arr);
    undo.valid = true; fBarUndo = undo;
    fPCI->setProperty("nvrm-bar1-placed", bar1Base, 64);
    LOG("bar1: PLACED — BAR1 %llu MB live at 0x%llx", bar1Size >> 20, bar1Base);
    {
        PE_Video pv; bzero(&pv, sizeof pv);
        const UInt64 con = (getPlatform() && getPlatform()->getConsoleInfo(&pv) == kIOReturnSuccess) ? ((UInt64)pv.v_baseAddr & ~3ull) : 0;
        char vb[8] = { 0 };
        const bool verbose = PE_parse_boot_argn("-v", vb, sizeof vb);
        if (nvrmConsoleLostByMove(con, fwBar1, fwBar1Size, bar1Base, bar1Size, verbose)) {
            getPlatform()->setConsoleInfo(0, kPEDisableScreen);
            LOG("bar1: verbose console at 0x%llx is no longer decoded after the move - silenced until the framebuffer takes over", con);
        }
    }
    return true;
}

/*
 * One per-generation profile for the whole macOS layer, published on the PCI device as "nvrm-gpu-profile" (NVIDIA's HAL
 * idea: one driver, the chip's implementation chosen once at start). The plugin, the Vulkan driver's copy engine, the
 * video libraries and the scan-out path read it instead of guessing from a device-id range, so behaviour proven on one
 * generation is never carried onto another by accident.
 *   scanout "static-bar1": BAR1 covers all of VRAM, so VRAM is mapped once (kbusIsStaticBar1Supported);
 *           "by-handle":   a small BAR1 (Turing / GTX 16 / Ampere without Resizable BAR), so scan-out surfaces are read
 *                          by the display engine by handle and the CPU maps only what it touches (nvidia-drm-fb.c).
 */
void NVRM::publishGpuProfile()
{
    NvU32 arch = 0, impl = 0, c3d = 0, ccomp = 0, ccopy = 0, cenc = 0, cdec = 0, st = 0;
    if (!nvrm_query_gpu_profile(fNv.gpu_id, &arch, &impl, &c3d, &ccomp, &ccopy, &cenc, &cdec, &st)) {
        LOG("gpu profile: the RM did not answer (status 0x%x) - no profile published, users fall back to device ids", (unsigned)st);
        return;
    }
    const char *gen = "unknown", *pfx = "";
    switch (arch) {
        case 0x160: gen = (impl == 7 || impl == 8) ? "Turing GTX 16" : "Turing"; pfx = "TU"; break;
        case 0x170: gen = "Ampere"; pfx = "GA"; break;
        case 0x180: gen = "Hopper"; pfx = "GH"; break;
        case 0x190: gen = "Ada"; pfx = "AD"; break;
        case 0x1A0: case 0x1B0: gen = "Blackwell"; pfx = "GB"; break;
    }
    char chip[16];
    /* TU116 = impl 8 and TU117 = impl 7; every other chip is <prefix>1<arch digit 0-2>0<impl> (GA106, AD104, GB206) */
    if (arch == 0x160 && impl == 8) snprintf(chip, sizeof chip, "TU116");
    else if (arch == 0x160 && impl == 7) snprintf(chip, sizeof chip, "TU117");
    else snprintf(chip, sizeof chip, "%s%x%02x", pfx, arch == 0x1B0 ? 2 : 1, (unsigned)impl);
    const UInt64 bar1 = fNv.bars[NV_GPU_BAR_INDEX_FB].size;
    UInt64 vram = 0;
    if (OSNumber *n = OSDynamicCast(OSNumber, fPCI->getProperty("VRAM,totalsize"))) vram = n->unsigned64BitValue();
    const bool staticBar1 = vram && bar1 >= vram;
    OSDictionary *d = OSDictionary::withCapacity(16);
    if (!d) return;
    auto num = [&](const char *k, UInt64 v, int bits) { if (OSNumber *o = OSNumber::withNumber(v, (unsigned)bits)) { d->setObject(k, o); o->release(); } };
    auto str = [&](const char *k, const char *v) { if (OSString *o = OSString::withCString(v)) { d->setObject(k, o); o->release(); } };
    num("arch", arch, 32); num("impl", impl, 32);
    str("chip", chip); str("generation", gen);
    num("class-3d", c3d, 32); num("class-compute", ccomp, 32); num("class-copy", ccopy, 32);
    num("class-nvenc", cenc, 32); num("class-nvdec", cdec, 32);
    num("bar1-mb", bar1 >> 20, 32); num("vram-mb", vram >> 20, 32);
    str("scanout", staticBar1 ? "static-bar1" : "by-handle");
    str("video-layout", arch >= 0x1A0 ? "blackwell" : "classic");
    str("native-shaders", arch == 0x1B0 ? "sm_120" : "none");
    fPCI->setProperty("nvrm-gpu-profile", d);
    d->release();
    // The RM named no model for this PCI id at start (see start()), so the card would show as 10DE:xxxx only. The
    // profile knows the generation and chip; publish those so System Information and the app say what the card is.
    if (!fPCI->getProperty("model")) {
        char nm[64];
        const int len = nvrmFallbackModelName(nm, sizeof nm, gen, chip, fNv.pci_info.device_id);
        if (len > 0 && (unsigned)len < sizeof nm) {
            fPCI->setProperty("model", (void *)nm, (unsigned)len + 1);
            LOG("published model=\"%s\" from the GPU profile (the RM's released-chip table has no name for this id)", nm);
        }
    }
    LOG("gpu profile: %s %s, 3d 0x%x copy 0x%x nvenc 0x%x nvdec 0x%x, BAR1 %llu MB of %llu MB VRAM -> scanout %s",
        gen, chip, (unsigned)c3d, (unsigned)ccopy, (unsigned)cenc, (unsigned)cdec, bar1 >> 20, vram >> 20,
        staticBar1 ? "static-bar1" : "by-handle");
}

void NVRM::readBARs()
{
    nv_state_t *nv = &fNv;
    char table[256]; unsigned tn = 0;
    unsigned count = fPCI->getDeviceMemoryCount();
    unsigned curNvBar = 0, found = 0;
    for (unsigned i = 0; i < NV_GPU_NUM_BARS; i++) { nv->bars[i].offset = 0; nv->bars[i].cpu_address = 0; nv->bars[i].size = 0; }
    for (unsigned idx = 0; idx < count && found < NV_GPU_NUM_BARS; idx++) {
        IODeviceMemory *dm = fPCI->getDeviceMemoryWithIndex(idx);
        if (!dm) continue;
        UInt64 phys = dm->getPhysicalAddress();
        UInt64 size = dm->getLength();
        UInt8 off = 0;
        for (unsigned bar = 0; bar < 6; bar++) {
            UInt32 lo = fPCI->configRead32(0x10 + bar * 4);
            if (lo & 1) continue;
            bool is64 = ((lo >> 1) & 3) == 2;
            UInt64 a = lo & ~0xFULL;
            if (is64 && bar < 5) a |= (UInt64)fPCI->configRead32(0x14 + bar * 4) << 32;
            if (a == (phys & ~0xFULL)) { off = (UInt8)(0x10 + bar * 4); break; }   // BAR registers 0x10..0x24
            if (is64) bar++;
        }
        if (off == 0 || size == 0) { kprintf("NVRM-xnu: aperture idx %u phys 0x%llx size 0x%llx: %s — skipped\n", idx, phys, size, off == 0 ? "no BAR register matches" : "zero length"); continue; }
        // Each aperture goes to the slot of its BAR register. Counting them in order put BAR3 in BAR1's slot whenever
        // macOS gave BAR1 no address, and RM then failed in rm_init_adapter with nothing saying why.
        curNvBar = off == 0x10 ? NV_GPU_BAR_INDEX_REGS : off == 0x14 ? NV_GPU_BAR_INDEX_FB : off == 0x1c ? NV_GPU_BAR_INDEX_IMEM : NV_GPU_NUM_BARS;
        if (curNvBar >= NV_GPU_NUM_BARS || nv->bars[curNvBar].size) continue;
        found++;
        nv->bars[curNvBar].offset      = off;
        nv->bars[curNvBar].cpu_address = phys;
        nv->bars[curNvBar].size        = size;
        tn += (unsigned)snprintf(table + tn, sizeof table - tn, "bar%u@0x%02x:0x%llx+0x%llx ", curNvBar, off, phys, size);
    }
    // A BIOS without Above 4G Decoding or Resizable BAR can leave the device memory list empty while the BARs are
    // programmed ("BARS (empty)", os_map_kernel_space start=0x0, RM refuses the device). Take each missing BAR from
    // assigned-addresses, then from the BAR register itself, and publish the recovered apertures so the framebuffer
    // and accelerator see the same list as RM.
    {
        static const struct { unsigned slot; UInt8 off; } kSlots[] = {
            { NV_GPU_BAR_INDEX_REGS, 0x10 }, { NV_GPU_BAR_INDEX_FB, 0x14 }, { NV_GPU_BAR_INDEX_IMEM, 0x1c } };
        OSData *aa = OSDynamicCast(OSData, fPCI->getProperty("assigned-addresses"));
        const UInt32 *cells = aa ? (const UInt32 *)aa->getBytesNoCopy() : NULL;
        const unsigned nCells = aa ? aa->getLength() / 4 : 0;
        IODeviceMemory *made[3] = { NULL, NULL, NULL }; unsigned nMade = 0;
        for (unsigned k = 0; k < 3; k++) {
            const unsigned slot = kSlots[k].slot; const UInt8 off = kSlots[k].off;
            if (nv->bars[slot].size) continue;
            const UInt32 lo = fPCI->configRead32(off);
            if (lo & 1) continue;                                   // an I/O BAR, not a memory aperture
            const bool is64 = ((lo >> 1) & 3) == 2;
            UInt64 live = lo & ~0xFULL;
            if (is64) live |= (UInt64)fPCI->configRead32(off + 4) << 32;
            if (!live) { kprintf("NVRM-xnu: BAR @0x%02x has no address in config space - nothing to recover\n", off); continue; }
            UInt64 a = 0, sz = 0; const char *src = "assigned-addresses";
            if (!nvrmAssignedLookup(cells, nCells, off, &a, &sz) || a != live) {
                // Size it the way firmware does: all ones with memory decode off, then put everything back.
                src = "config space";
                const UInt16 cmd = fPCI->configRead16(0x04);
                const UInt32 hiOld = is64 ? fPCI->configRead32(off + 4) : 0;
                fPCI->configWrite16(0x04, cmd & ~0x3);
                fPCI->configWrite32(off, 0xffffffffU); if (is64) fPCI->configWrite32(off + 4, 0xffffffffU);
                const UInt32 loP = fPCI->configRead32(off), hiP = is64 ? fPCI->configRead32(off + 4) : 0;
                fPCI->configWrite32(off, lo); if (is64) fPCI->configWrite32(off + 4, hiOld);
                fPCI->configWrite16(0x04, cmd);
                a = live; sz = nvrmBarSizeFromProbe(loP, hiP, is64);
            }
            if (!sz) { kprintf("NVRM-xnu: BAR @0x%02x at 0x%llx: size unknown - not recovered\n", off, a); continue; }
            nv->bars[slot].offset = off; nv->bars[slot].cpu_address = a; nv->bars[slot].size = sz;
            tn += (unsigned)snprintf(table + tn, sizeof table - tn, "bar%u@0x%02x:0x%llx+0x%llx(%s) ", slot, off, a, sz, src);
            kprintf("NVRM-xnu: BAR @0x%02x recovered from %s: 0x%llx+0x%llx\n", off, src, a, sz);
            if (nMade < 3) made[nMade++] = IODeviceMemory::withRange(a, sz);
        }
        if (nMade) {
            OSArray *cur = fPCI->getDeviceMemory();
            OSArray *arr = OSArray::withCapacity((cur ? cur->getCount() : 0) + nMade);
            bool okArr = arr != NULL;
            for (unsigned i = 0; okArr && cur && i < cur->getCount(); i++) okArr = arr->setObject(cur->getObject(i));
            for (unsigned i = 0; i < nMade; i++) { if (okArr && made[i]) okArr = arr->setObject(made[i]); else if (!made[i]) okArr = false; }
            if (okArr) { fPCI->setDeviceMemory(arr); LOG("bar recovery: %u aperture%s added to the device memory list", nMade, nMade == 1 ? "" : "s"); }
            else LOG("bar recovery: could not build the aperture list - RM still has the recovered addresses");
            for (unsigned i = 0; i < nMade; i++) OSSafeReleaseNULL(made[i]);
            OSSafeReleaseNULL(arr);
        }
    }
    if (!nv->bars[NV_GPU_BAR_INDEX_FB].size)
        LOG("BAR1 (the VRAM window) has no address from macOS: the GPU cannot start. Enable \"Above 4G Decoding\" in the BIOS, "
            "or turn Resizable BAR off if it is on and the board cannot map it");
    setProperty("nvrm-bars", table);
    kprintf("NVRM-xnu: BARS %s\n", table);
}

IOReturn NVRM::go(uint32_t pass)
{
    // Bringing up the GPU and the RM for the first time is an administrator
    // operation. Was: any process that could open the user client could call
    // sGo with any pass number. Repeating a pass that is already done is a
    // no-op, and the userland driver runs as the logged-in user, so that path
    // stays open deliberately.
    if (pass > fPassDone && kauth_getuid() != 0) {
        kprintf("NVRM-xnu: go(%u) refused: caller is not an administrator (fPassDone %d)\n", pass, fPassDone);
        return kIOReturnNotPermitted;
    }
    static bool gGoBusy;
    { IOLockLock(gNvClientsLock); bool busy = gGoBusy; if (!busy) gGoBusy = true; IOLockUnlock(gNvClientsLock); if (busy) { kprintf("NVRM-xnu: go(%u) refused: another go is running\n", pass); return kIOReturnBusy; } }
    struct Unbusy { ~Unbusy() { IOLockLock(gNvClientsLock); gGoBusy = false; IOLockUnlock(gNvClientsLock); } } unbusy;
    kprintf("NVRM-xnu: go(%u) begins (fPassDone %d)\n", pass, fPassDone);
    static const char *const kBuildMark = "NVRM build 2026-09-14 19:30 (boot daemon + fb sampler)"; (void)kBuildMark;

    nv_state_t *nv = &fNv;
    LOG("go(pass %u) — done so far: %u", pass, fPassDone);
    LOG("anchor: os_mem_set runtime 0x%llx (slide = runtime - its nm link address)", (unsigned long long)(uintptr_t)&os_mem_set);
    if (pass >= 1 && fPassDone < 1) {
        char nopmArg[8] = { 0 };
        if (!gRmInitDone) {
            LOG("rm_init_rm()");
            if (!rm_init_rm(NULL)) { LOG("rm_init_rm FAILED"); return kIOReturnError; }
            gRmInitDone = true;
            LOG("rm_init_rm OK");
            if (!rm_init_event_locks(NULL, nv_get_ctl_state())) { LOG("rm_init_event_locks(ctl) FAILED"); return kIOReturnError; }
            if (!rm_init_event_locks(NULL, nv))                 { LOG("rm_init_event_locks(gpu) FAILED"); return kIOReturnError; }
            LOG("event locks created for the control device and the GPU");
            rm_write_registry_string(NULL, NULL, "RmMsg", ":", 1);
            rm_write_registry_dword(NULL, NULL, "ResmanDebugLevel", 0);
            // Tell a wedged GSP from a slow one: with classification on, an RPC that times out while the GSP heartbeat is
            // dead fails the GPU at the first timeout instead of the third (see nvrmGspTimeoutFatal). A live heartbeat keeps
            // the RM's own count. Set before nvregdwords so RmGspTimeoutClassification=0 there still wins.
            rm_write_registry_dword(NULL, NULL, "RmGspTimeoutClassification", 1);
            {   // NVIDIA's registry options, as NVreg_RegistryDwords: nvregdwords=Key=Value;Key2=Value2
                static char regdw[256];
                if (PE_parse_boot_argn("nvregdwords", regdw, sizeof regdw) && regdw[0]) {
                    regdw[sizeof regdw - 1] = 0;
                    rm_parse_option_string(NULL, regdw);
                    LOG("registry options from nvregdwords: %s", regdw);
                }
            }
            LOG("registry: RmMsg=\":\" ResmanDebugLevel=0 (verbose RM)");
            if (!PE_parse_boot_argn("-nvrmnopm", nopmArg, sizeof nopmArg)) {
                // Keep every video memory allocation over system sleep, not only RM's own: WindowServer's GPU
                // channels (NVK through NVAccel) keep push buffers and semaphores in VRAM, and without them the
                // compositor waited on fences forever after the first wake.
                // RM saves them through its temporary-file interface, which os-xnu2.cpp backs with pageable kernel memory.
                rm_write_registry_dword(NULL, NULL, "PreserveVideoMemoryAllocations", 1);
                LOG("registry: PreserveVideoMemoryAllocations=1 (all VRAM survives system sleep)");
            }
        }
        {
            NV_STATUS sup = rm_is_supported_device(NULL, nv);
            if (sup != NV_OK) {
                LOG("rm_is_supported_device -> 0x%x: RM will not drive device 0x%04x - stopping, card untouched",
                    (unsigned)sup, nv->pci_info.device_id);
                return kIOReturnUnsupported;
            }
            LOG("rm_is_supported_device OK - this chip has a HAL and a GSP");
        }
        LOG("rm_init_private_state()");
        if (!rm_init_private_state(NULL, nv)) { LOG("rm_init_private_state FAILED"); return kIOReturnError; }
        fPrivateState = true;
        LOG("rm_init_private_state OK");
        rm_set_rm_firmware_requested(NULL, nv);
        LOG("PASS 1 REACHED: RM initialised, private state built, GSP firmware requested");
        fPassDone = 1;
    }
    if (pass >= 2 && fPassDone < 2) {
        nv->os_state = this;
        fPCI->setMemoryEnable(true);
        fPCI->setBusMasterEnable(true);
        LOG("bus master + memory enabled (command 0x%04x)", fPCI->configRead16(0x04));
        int msi = -1;
        for (int i = 0; i < 8; i++) {
            int type = 0;
            if (fPCI->getInterruptType(i, &type) != kIOReturnSuccess) break;
            LOG("  interrupt source %d type 0x%x%s", i, type, (type & kIOInterruptTypePCIMessaged) ? " (MSI)" : "");
            if (msi < 0 && (type & kIOInterruptTypePCIMessaged)) msi = i;
        }
        if (msi < 0) { LOG("no MSI source on the device — refusing (the RM needs message-signalled interrupts)"); return kIOReturnUnsupported; }
        fWorkLoop = IOWorkLoop::workLoop();
        fIntSrc = IOFilterInterruptEventSource::filterInterruptEventSource(this,
                    OSMemberFunctionCast(IOInterruptEventAction, this, &NVRM::interruptOccurred),
                    OSMemberFunctionCast(IOFilterInterruptAction, this, &NVRM::interruptFilter), fPCI, msi);
        if (!fWorkLoop || !fIntSrc || fWorkLoop->addEventSource(fIntSrc) != kIOReturnSuccess) { LOG("interrupt source setup FAILED"); return kIOReturnError; }
        fIntSrc->enable();
        nv->interrupt_line = (NvU32)msi;
        LOG("MSI %d armed; calling rm_init_adapter() — GSP boot from inside macOS", msi);
        NvBool ok = rm_init_adapter(NULL, nv);
        LOG("rm_init_adapter -> %s", ok ? "OK" : "FAILED");
        nv_xnu_dump_live_allocs();
        nv_xnu_dump_gsp_regs(nv);
        if (!ok) return kIOReturnError;
        fAdapter = true;
        LOG("PASS 2 REACHED: the adapter is up — GSP-RM is running on the card under macOS");
        fPassDone = 2;
        enableAspmL1();
        rtd3Arm();
        {
            NvU32 ramKb = 0, totalKb = 0, qst = 0;
            if (nvrm_query_vram_kb(nv->gpu_id, &ramKb, &totalKb, &qst) && ramKb) {
                const UInt64 bytes = (UInt64)ramKb * 1024ull;
                fPCI->setProperty("VRAM,totalsize", bytes, 64);
                fPCI->setProperty("VRAM,totalMB", (UInt64)(bytes >> 20), 32);
                LOG("published VRAM,totalsize=%llu bytes (RAM_SIZE %u KB, TOTAL_RAM_SIZE %u KB) from the card",
                    bytes, (unsigned)ramKb, (unsigned)totalKb);
            } else {
                LOG("FB_GET_INFO_V2 did not answer (status 0x%x, RAM_SIZE %u KB) - publishing no VRAM figure",
                    (unsigned)qst, (unsigned)ramKb);
            }
        }
        publishGpuProfile();
        if (nvkms_xnu_load()) nvrm_display_publish(this, fNv.gpu_id);
        nvrm_power_start(&fNv);   // boot-arg nvrmtgp=1: the notebook power budget while on mains power (nvrm_power.cpp)
    }
    return kIOReturnSuccess;
}

bool NVRM::interruptFilter(IOFilterInterruptEventSource *src)
{
    unsigned long n = ++gNvIsrCount;
    if (n <= 3 || (n % 1000) == 0) kprintf("NVRM-xnu: MSI #%lu -> work loop (handled so far %lu)\n", n, gNvIsrHandled);
    return true;
}
void NVRM::interruptOccurred(IOInterruptEventSource *src, int count)
{
    NvU32 needBottomHalf = 0;
    nv_xnu_isr_thread = current_thread();
    NvBool handled = rm_isr(NULL, &fNv, &needBottomHalf);
    nv_xnu_isr_thread = NULL;
    if (handled) gNvIsrHandled++;
    if (handled && needBottomHalf) rm_isr_bh(NULL, &fNv);
}

void NVRM::stop(IOService *provider)
{
    nvrm_power_stop();
    // Keep the lock until free(): another service can retain this object and
    // call its platform interface after stop(), when it must answer NotReady.
    if (fUtilizationLock) {
        IOLockLock(fUtilizationLock);
        fUtilizationStopping = true;
        const bool haveState = fUtilizationState != nullptr;
        IOLockUnlock(fUtilizationLock);
        if (haveState) {
            // Match the query lock order: PM admission, then the state lock.
            // An unload during sleep waits for wake under the existing PM gate
            // contract; it must not send an RM free to a GPU still in standby.
            NvXnuPmUse pmUse;
            IOLockLock(fUtilizationLock);
            nvrm_free_utilization(&fUtilizationState);
            IOLockUnlock(fUtilizationLock);
        }
    }
    if (fPmJoined) { PMstop(); fPmJoined = false; }
    if (fAdapter) { rm_shutdown_adapter(NULL, &fNv); fAdapter = false; }
    if (fIntSrc) { fIntSrc->disable(); if (fWorkLoop) fWorkLoop->removeEventSource(fIntSrc); fIntSrc->release(); fIntSrc = NULL; }
    if (fWorkLoop) { fWorkLoop->release(); fWorkLoop = NULL; }
    if (fPrivateState) { rm_free_private_state(NULL, &fNv); fPrivateState = false; }
    gNVRMTab.remove(this, nvrmMultiGpu());   // a no-op without nvmultigpu=1: the old pointer was never cleared
    super::stop(provider);
}

// System power-off and restart. Nothing used to run here: the GPU was left running, mastering the bus, with the BAR
// layout placeLargeBar1 made, and the platform firmware found that on the next start. This does what Linux's
// nv_pci_shutdown does in spirit: bring the GPU down the way system sleep does (display, user channels, RM standby),
// then hand the device back (bus mastering off, the firmware's own BAR and root port values restored). The RM step is
// bounded: a hang there must not turn into a shutdown that never finishes. -nvrmnoshutdown keeps the old behaviour.
void NVRM::shutdownQuiesce(void *p0, wait_result_t)
{
    NVRM *s = (NVRM *)p0;
    s->gpuSuspend();
    OSCompareAndSwap(0, 1, &s->fShutdownDone);
    s->release();   // the retain taken before the thread started
    thread_terminate(current_thread());
}

extern "C" { void nvkms_xnu_suspend(NvU32 gpuId); }
// The full path: display off, user channels stopped, then the RM's own adapter shutdown (what stop() runs after the
// clients have gone). Reached only through nvrmShutdownMode.
void NVRM::shutdownFull(void *p0, wait_result_t)
{
    NVRM *s = (NVRM *)p0;
    OSCompareAndSwap(kGpuPmRunning, kGpuPmSuspended, &s->fGpuPm);
    s->notifyFramebuffers(0);
    nvkms_xnu_suspend(s->fNv.gpu_id);
    const int busy = nv_xnu_pm_close(3000);
    if (busy) LOG("shutdown: %d RM call(s) still running after 3 s", busy);
    rm_stop_user_channels(NULL, &s->fNv);
    if (s->fAdapter) { rm_shutdown_adapter(NULL, &s->fNv); s->fAdapter = false; }
    OSCompareAndSwap(0, 1, &s->fShutdownDone);
    s->release();
    thread_terminate(current_thread());
}

void NVRM::systemWillShutdown(IOOptionBits specifier)
{
    char off[8] = { 0 };
    if (!fShutdownHandled && !PE_parse_boot_argn("-nvrmnoshutdown", off, sizeof off) && fPCI) {
        fShutdownHandled = true;
        LOG("shutdown: %s - quiescing the GPU", specifier == kIOMessageSystemWillRestart ? "restart" : "power off");
        // The interrupt stays on through the standby, as it does on the system-sleep path this reuses (gpuSuspend turns it
        // off itself once the RM is in standby); it is switched off below, before the BAR layout goes back.
        if (fAdapter && OSAddAtomic(0, &fGpuPm) == kGpuPmRunning) {
            thread_t th = THREAD_NULL;
            char mode[16] = { 0 }; unsigned open = 0;
            for (int i = 0; i < NV_XNU_MAX_CLIENTS; i++) if (gNvClients[i]) open++;
            const bool askedFull = PE_parse_boot_argn("nvrmshutdown", mode, sizeof mode) && mode[0] == 'f';
            const bool full = nvrmShutdownMode(askedFull, open, fAdapter) == kShutdownFull;
            if (askedFull && !full) LOG("shutdown: full shutdown asked for but %u user client(s) are still open - standby instead", open);
            retain();
            if (kernel_thread_start(full ? &NVRM::shutdownFull : &NVRM::shutdownQuiesce, this, &th) == KERN_SUCCESS) {
                thread_deallocate(th);
                unsigned waited = 0;
                while (!OSAddAtomic(0, &fShutdownDone) && waited < 8000) { IOSleep(10); waited += 10; }
                LOG("shutdown: GPU %s after %u ms", OSAddAtomic(0, &fShutdownDone) ? (full ? "shut down" : "in standby") : "did not finish in time, going on", waited);
            } else { release(); LOG("shutdown: could not start the quiesce thread - going on"); }
        }
        if (fIntSrc) fIntSrc->disable();
        NvrmStep plan[16];
        const unsigned n = nvrmBuildShutdownPlan(&fBarUndo, plan, 16);
        if (n) {
            // the root port above the bridge this card sits behind (type spelled through fPCI: IOPCIDevice is deprecated)
            decltype(fPCI) rp = NULL;
            if (IOService *pp = fPCI->getProvider())
                if (IOService *up = pp->getProvider()) if (up->metaCast("IOPCIDevice")) rp = static_cast<decltype(fPCI)>(up);
            for (unsigned i = 0; i < n; i++) {
                switch (plan[i].kind) {
                    case kStepCmd16:     fPCI->configWrite16((IOByteCount)plan[i].off, (UInt16)plan[i].value); break;
                    case kStepExt32:     fPCI->extendedConfigWrite32(plan[i].off, plan[i].value); break;
                    case kStepCfg32:     fPCI->configWrite32((IOByteCount)plan[i].off, plan[i].value); break;
                    case kStepRootPort32: if (rp) rp->configWrite32((IOByteCount)plan[i].off, plan[i].value); break;
                }
            }
            LOG("shutdown: BAR1/BAR3 and the root port window put back as the firmware left them (%u writes)", n);
        } else {
            fPCI->configWrite16(0x04, (UInt16)(fPCI->configRead16(0x04) & ~0x4u));   // bus mastering off, nothing else moved
            LOG("shutdown: bus mastering off (the BAR layout was never changed)");
        }
    }
    restoreAspm();
    super::systemWillShutdown(specifier);
}

// ---- PCIe link power (ASPM L1) -------------------------------------------------------------------------------------
// The PCI Express capability's offset in a device's config space, 0 when there is none.
template <class D> static UInt8 nvrmPcieCap(D *d)
{
    for (UInt8 c = d->configRead8(0x34) & 0xfc, n = 0; c && n < 48; c = d->configRead8(c + 1) & 0xfc, n++)
        if (d->configRead8(c) == 0x10) return c;
    return 0;
}

void NVRM::enableAspmL1()
{
    char off[8] = { 0 };
    if (PE_parse_boot_argn("-nvrmnoaspm", off, sizeof off)) { fPCI->setProperty("nvrm-aspm", "off (-nvrmnoaspm)"); return; }
    // Opt-in (nvrmaspm=1). Linux's default policy keeps the link as the firmware set it, and NVIDIA's own driver turns
    // L1 on only for chipsets it lists; a desktop link pushed into L1 without the common-clock, L1 substate and LTR set-up
    // the PCI core does can drop the card off the bus when it idles. Laptop firmware that wants L1 has already set it.
    { uint32_t on = 0; if (!PE_parse_boot_argn("nvrmaspm", &on, sizeof on) || on != 1) { fPCI->setProperty("nvrm-aspm", "as the firmware set it (nvrmaspm=1 asks for L1)"); return; } }
    decltype(fPCI) up = NULL;   // the port above the card (IOPCIDevice is deprecated: spelled through fPCI)
    if (IOService *pp = fPCI->getProvider())
        if (IOService *u = pp->getProvider()) if (u->metaCast("IOPCIDevice")) up = static_cast<decltype(fPCI)>(u);
    const UInt8 ec = nvrmPcieCap(fPCI), uc = up ? nvrmPcieCap(up) : 0;
    NvrmAspmEnd ep = {}, ue = {};
    if (ec) { ep.present = true; ep.devCap = fPCI->configRead32(ec + 4); ep.lnkCap = fPCI->configRead32(ec + 0x0c); ep.lnkCtl = fPCI->configRead16(ec + 0x10); }
    if (uc) { ue.present = true; ue.devCap = up->configRead32(uc + 4); ue.lnkCap = up->configRead32(uc + 0x0c); ue.lnkCtl = up->configRead16(uc + 0x10); }
    unsigned epCtl = 0, upCtl = 0;
    const NvrmAspmWhy why = nvrmAspmPlan(&ep, &ue, &epCtl, &upCtl);
    if (why == kAspmEnabled) {
        if (fOrigEpLnkCtl == 0xffff) { fOrigEpLnkCtl = (UInt16)ep.lnkCtl; fOrigUpLnkCtl = (UInt16)ue.lnkCtl; }
        up->configWrite16(uc + 0x10, (UInt16)upCtl);   // upstream first, as the PCI core does
        fPCI->configWrite16(ec + 0x10, (UInt16)epCtl);
        const bool ok = (fPCI->configRead16(ec + 0x10) & 2) && (up->configRead16(uc + 0x10) & 2);
        fPCI->setProperty("nvrm-aspm", ok ? "L1" : "L1 requested, not accepted");
        LOG("aspm: L1 %s on the card's link (card Link Control 0x%04x, upstream 0x%04x)", ok ? "enabled" : "requested but not accepted",
            fPCI->configRead16(ec + 0x10), up->configRead16(uc + 0x10));
    } else {
        fPCI->setProperty("nvrm-aspm", nvrmAspmWhyString(why));
        LOG("aspm: L1 not changed - %s", nvrmAspmWhyString(why));
    }
}

// Puts the firmware's ASPM setting back before power-off, so the next start sees the link as it left it.
void NVRM::restoreAspm()
{
    if (fOrigEpLnkCtl == 0xffff) return;
    decltype(fPCI) up = NULL;
    if (IOService *pp = fPCI->getProvider())
        if (IOService *u = pp->getProvider()) if (u->metaCast("IOPCIDevice")) up = static_cast<decltype(fPCI)>(u);
    const UInt8 ec = nvrmPcieCap(fPCI), uc = up ? nvrmPcieCap(up) : 0;
    if (ec) fPCI->configWrite16(ec + 0x10, fOrigEpLnkCtl);
    if (uc) up->configWrite16(uc + 0x10, fOrigUpLnkCtl);
    fOrigEpLnkCtl = 0xffff;
}

// What the app shows about the BAR: sizes in MB and whether the platform decodes above 4 GB. Keys on the PCI device:
//   nvrm-bar1-firmware-mb  BAR1 as the firmware sized it (0 when the card has no Resizable BAR capability and it was not read)
//   nvrm-bar1-mb           BAR1 now
//   nvrm-bar1-addr         BAR1 base address now
//   nvrm-rebar             "none" (no capability), "off" (capability, BAR1 not above 256 MB), "on" (BAR1 above 256 MB from firmware)
//   nvrm-rebar-max-mb      the largest size the card offers (0 when there is no capability)
//   nvrm-above-4g          "yes" when anything is decoded above 4 GB, "no" otherwise
void NVRM::publishBarFacts()
{
    const UInt64 now = fNv.bars[NV_GPU_BAR_INDEX_FB].size;
    UInt64 maxSz = 0;
    for (int k = 31; fRebar.cap && k >= 0; k--) if ((fRebar.supported >> k) & 1) { maxSz = 1ULL << (20 + k); break; }
    const UInt64 fw = fRebar.cap ? fRebar.firmwareSize : 0;
    fPCI->setProperty("nvrm-bar1-mb", now >> 20, 64);
    fPCI->setProperty("nvrm-bar1-firmware-mb", fw >> 20, 64);
    fPCI->setProperty("nvrm-bar1-addr", fNv.bars[NV_GPU_BAR_INDEX_FB].cpu_address, 64);
    fPCI->setProperty("nvrm-rebar-max-mb", maxSz >> 20, 64);
    fPCI->setProperty("nvrm-rebar", nvrmRebarState(fRebar.cap, fw));
    const UInt64 bar3 = fNv.bars[NV_GPU_BAR_INDEX_IMEM].cpu_address;
    fPCI->setProperty("nvrm-above-4g", nvrmAbove4GRouted(bar3 > fNv.bars[NV_GPU_BAR_INDEX_FB].cpu_address ? bar3 : fNv.bars[NV_GPU_BAR_INDEX_FB].cpu_address) ? "yes" : "no");
}

void NVRM::free()
{
    if (fUtilizationLock) { IOLockFree(fUtilizationLock); fUtilizationLock = nullptr; }
    super::free();
}

extern "C" { void nvkms_xnu_suspend(NvU32 gpuId); void nvkms_xnu_resume(NvU32 gpuId, NvBool gpuOk); }

IOReturn NVRM::setPowerState(unsigned long ordinal, IOService *device)
{
    nv_xnu_power_refresh();        // a wake can be a change of power source; this runs before the firmware asks
    const SInt32 pm = OSAddAtomic(0, &fGpuPm);
    if (ordinal == 0 && pm == kGpuPmRunning && fAdapter) gpuSuspend();
    else if (ordinal != 0 && pm == kGpuPmSuspended) { nv_rtd3_before_system_resume(); gpuResume(); }
    else LOG("power: setPowerState %lu (gpu %s, adapter %s) - nothing to do", ordinal,
             pm == kGpuPmRunning ? "running" : pm == kGpuPmSuspended ? "suspended" : "lost", fAdapter ? "up" : "down");
    return kIOPMAckImplied;
}

// Linux order (nvidia_suspend): NVKMS first, while the GPU still serves the core channel, then RM standby. RM saves
// what it must keep from VRAM to system memory and unloads GSP-RM; the GPU then loses power with the slot.
void NVRM::gpuSuspend()
{
    uint64_t t0 = mach_absolute_time();
    fPmCycles++;
    nvrm_power_suspend();
    OSCompareAndSwap(kGpuPmRunning, kGpuPmSuspended, &fGpuPm);
    setProperty("nvrm-pm", "suspended");
    LOG("sleep #%u: suspending the GPU (NVKMS, then RM standby)", fPmCycles);
    notifyFramebuffers(0);
    // NVKMS first: once it is suspended no thread is inside it, so none can hold gKmsLock while it waits at the RM
    // gate (closing the gate first deadlocked against an NVKMS ioctl calling into RM).
    nvkms_xnu_suspend(fNv.gpu_id);
    const int busy = nv_xnu_pm_close(10000);
    if (busy) LOG("sleep #%u: %d RM call(s) still running after 10 s - suspending anyway", fPmCycles, busy);
    // nv_preempt_user_channels(): with every VRAM allocation preserved, user channels must be idle and stopped
    // while their memory is copied out, and restarted once it is back.
    NV_STATUS cst = rm_stop_user_channels(NULL, &fNv);
    pciSaveForSleep();
    NV_STATUS st = rm_power_management(NULL, &fNv, NV_PM_ACTION_STANDBY);
    if (fIntSrc) fIntSrc->disable();
    uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - t0, &ns);
    LOG("sleep #%u: user channels stopped -> 0x%x; rm_power_management(STANDBY) -> 0x%x in %llu ms%s", fPmCycles,
        (unsigned)cst, (unsigned)st, ns / 1000000ULL, st == NV_OK ? "" : " - resume will still be tried, as on Linux");
}

// The slot is powered again and IOPCIFamily restored the config space it shadowed. Put back what it does not know
// (the resized BAR1 and the root port window placeLargeBar1 set), check the GPU answers, then RM resume (GSP boots
// again through FSP) and NVKMS resume. The framebuffers re-light their heads when told the GPU is back.
void NVRM::gpuResume()
{
    uint64_t t0 = mach_absolute_time();
    LOG("wake #%u: resuming the GPU", fPmCycles);
    nv_xnu_pm_take();
    bool ok = pciRestoreAfterWake() && gpuAnswers();
    if (ok) enableAspmL1();   // the link comes back with whatever IOPCIFamily restored
    if (fIntSrc) fIntSrc->enable();
    NV_STATUS st = ok ? rm_power_management(NULL, &fNv, NV_PM_ACTION_RESUME) : NV_ERR_GPU_IS_LOST;
    ok = st == NV_OK;
    NV_STATUS cst = ok ? rm_restart_user_channels(NULL, &fNv) : NV_ERR_GPU_IS_LOST;   // nv_restore_user_channels()
    OSCompareAndSwap(kGpuPmSuspended, ok ? kGpuPmRunning : kGpuPmLost, &fGpuPm);
    nvkms_xnu_resume(fNv.gpu_id, ok);
    nv_xnu_pm_open();   // also when lost: RM then answers the waiting calls with errors instead of leaving them blocked
    if (ok) nvrm_power_resume();
    setProperty("nvrm-pm", ok ? "running" : "lost (resume failed)");
    uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - t0, &ns);
    LOG("wake #%u: rm_power_management(RESUME) -> 0x%x, user channels restarted -> 0x%x, %llu ms - %s", fPmCycles,
        (unsigned)st, (unsigned)cst, ns / 1000000ULL,
        ok ? "GPU running, telling the framebuffers" : "GPU LOST: NVKMS refuses all display work until a restart");
    notifyFramebuffers(ok ? 1 : 2);
}

// Opt-in D3cold. The driver's own system-sleep and wake paths do the suspend and resume; nv_rtd3.cpp decides when.
static IOACPIPlatformDevice *rtd3Acpi(NVRM *n)
{
    IOService *s = n; IOACPIPlatformDevice *a = NULL;
    for (int i = 0; s && i < 16 && !a; i++) { s = s->getProvider(); a = OSDynamicCast(IOACPIPlatformDevice, s); }
    return a;
}
static int rtd3Heads(void *) {
    // Every framebuffer: this GPU's own heads, and (hybrid switch on) another GPU's panel published as a head of ours.
    int hy = 0; { int lv = 0; const int present = PE_parse_boot_argn("nmhybrid", &lv, sizeof(lv)); hy = present && lv >= 1 && lv <= 4; }
    OSDictionary *m = IOService::serviceMatching("IOFramebuffer"); if (!m) return 1;   // cannot look: treat as lit
    OSIterator *it = IOService::getMatchingServices(m); m->release();
    int own = 0, foreign = 0;
    if (it) {
        while (OSObject *o = it->getNextObject()) {
            IOService *s = OSDynamicCast(IOService, o); if (!s) continue;
            if (s->metaCast("NVRMNVDAFramebuffer")) own++;
            else if (OSNumber *id = OSDynamicCast(OSNumber, s->getProperty("IOFBDependentID"))) { if (id->unsigned64BitValue() == 0x4E56524D00000001ull) foreign++; }
        }
        it->release();
    }
    return nv_rtd3_heads_lit(own, foreign, hy);
}
static int rtd3Clients(void *ctx) {
    IOService *n = (IOService *)ctx; int rm = 0, accel = 0;
    if (OSIterator *it = n->getClientIterator()) { while (OSObject *o = it->getNextObject()) if (OSDynamicCast(IOUserClient, o)) rm++; it->release(); }
    // Metal applications attach to the accelerator, not to NVRM
    if (OSDictionary *m = IOService::serviceMatching("IOAccelerator")) {
        if (OSIterator *it = IOService::getMatchingServices(m)) {
            while (OSObject *o = it->getNextObject()) {
                IOService *a = OSDynamicCast(IOService, o); const char *cn = a ? a->getMetaClass()->getClassName() : nullptr;
                if (!cn || strncmp(cn, "NV", 2)) continue;
                if (OSIterator *ci = a->getClientIterator()) { while (OSObject *c = ci->getNextObject()) if (OSDynamicCast(IOUserClient, c)) accel++; ci->release(); }
            }
            it->release();
        }
        m->release();
    }
    return nv_rtd3_clients_total(rm, accel);
}
void NVRM::rtd3Arm()
{
    const struct nv_rtd3_ops ops = {
        this, rtd3Heads, rtd3Clients,
        [](void *c) { ((NVRM *)c)->gpuSuspend(); },
        [](void *c) { ((NVRM *)c)->gpuResume(); },
        [](void *c) { return nv_rtd3_power_off(rtd3Acpi((NVRM *)c)); },
        [](void *c) { return nv_rtd3_power_on(rtd3Acpi((NVRM *)c)); },
    };
    nv_rtd3_arm(&ops);
}

void NVRM::pciSaveForSleep()
{
    PciSave &s = fPciSave;
    s = {};
    s.cmd = fPCI->configRead16(0x04);
    for (unsigned i = 0; i < 6; i++) s.bar[i] = fPCI->configRead32(0x10 + i * 4);
    for (UInt32 off = 0x100, n = 0; off >= 0x100 && off < 0x1000 && n < 64; n++) {
        UInt32 h = fPCI->extendedConfigRead32(off);
        if (h == 0 || h == 0xffffffffU) break;
        if ((h & 0xffff) == 0x15) {   // Resizable BAR: IOPCIFamily does not restore the sizes
            UInt32 nbars = (fPCI->extendedConfigRead32(off + 8) >> 5) & 7;
            for (UInt32 i = 0; i < nbars && i < 6; i++) { s.rbOff[i] = off + 8 + i * 8; s.rbCtl[i] = fPCI->extendedConfigRead32(s.rbOff[i]); }
            s.rbCount = nbars < 6 ? nbars : 6;
            break;
        }
        off = h >> 20;
    }
    IOService *pp = fPCI->getProvider();
    s.rp = pp ? OSDynamicCast(IOPCIDevice, pp->getProvider()) : NULL;
    if (s.rp) {
        s.rpCmd = s.rp->configRead16(0x04);
        for (unsigned i = 0; i < 6; i++) s.rpWin[i] = s.rp->configRead32(0x18 + i * 4);
    }
    s.valid = true;
    LOG("sleep: saved config - cmd 0x%04x BAR1 0x%08x%08x, %u ReBAR control(s), root port %s pref 0x%08x %08x/%08x",
        s.cmd, s.bar[2], s.bar[1] & ~0xFU, s.rbCount, s.rp ? s.rp->getName() : "(none)", s.rpWin[3], s.rpWin[4], s.rpWin[5]);
}

bool NVRM::pciRestoreAfterWake()
{
    PciSave &s = fPciSave;
    if (!s.valid) { LOG("wake: no saved config - leaving what IOPCIFamily restored"); return true; }
    if (fPCI->configRead16(0x00) != 0x10de) { LOG("wake: config space answers vendor 0x%04x - the GPU is not on the bus", fPCI->configRead16(0x00)); return false; }
    unsigned fixed = 0;
    if (s.rp) {
        bool winDiff = false;
        for (unsigned i = 0; i < 6; i++) {
            // 0x1c's upper half is the Secondary Status register (write-1-to-clear error bits), not a window
            const UInt32 mask = i == 1 ? 0x0000ffffU : 0xffffffffU;
            UInt32 cur = s.rp->configRead32(0x18 + i * 4);
            if ((cur ^ s.rpWin[i]) & mask) { LOG("wake: root port @0x%02x is 0x%08x, was 0x%08x", 0x18 + i * 4, cur, s.rpWin[i]); winDiff = true; }
        }
        if (winDiff) {   // same order as placeLargeBar1: close the prefetchable window, set the upper halves, reopen it
            s.rp->configWrite32(0x18, s.rpWin[0]); s.rp->configWrite16(0x1c, (UInt16)s.rpWin[1]); s.rp->configWrite32(0x20, s.rpWin[2]);
            s.rp->configWrite32(0x24, 0x0000fff0);
            s.rp->configWrite32(0x28, s.rpWin[4]); s.rp->configWrite32(0x2c, s.rpWin[5]);
            s.rp->configWrite32(0x24, s.rpWin[3]);
            fixed++;
        }
        UInt16 rc = s.rp->configRead16(0x04);
        if ((rc & 0x6) != (s.rpCmd & 0x6)) { s.rp->configWrite16(0x04, rc | (s.rpCmd & 0x6)); fixed++; }
    }
    bool gpuDiff = false;
    for (unsigned i = 0; i < s.rbCount; i++) {
        UInt32 cur = fPCI->extendedConfigRead32(s.rbOff[i]);
        if ((cur & 0x3f00) != (s.rbCtl[i] & 0x3f00)) { LOG("wake: ReBAR control @0x%x is 0x%08x, was 0x%08x", s.rbOff[i], cur, s.rbCtl[i]); gpuDiff = true; }
    }
    for (unsigned i = 0; i < 6; i++) {
        UInt32 cur = fPCI->configRead32(0x10 + i * 4);
        if (cur != s.bar[i]) { LOG("wake: BAR register @0x%02x is 0x%08x, was 0x%08x", 0x10 + i * 4, cur, s.bar[i]); gpuDiff = true; }
    }
    if (gpuDiff) {   // PCIe 7.8.6: resize only while memory decode is off; the BARs follow the new sizes
        UInt16 cmd = fPCI->configRead16(0x04);
        fPCI->configWrite16(0x04, cmd & ~0x2);
        for (unsigned i = 0; i < s.rbCount; i++) fPCI->extendedConfigWrite32(s.rbOff[i], s.rbCtl[i]);
        for (unsigned i = 0; i < 6; i++) fPCI->configWrite32(0x10 + i * 4, s.bar[i]);
        fixed++;
    }
    UInt16 cmd = fPCI->configRead16(0x04);
    if ((cmd & 0x6) != (s.cmd & 0x6)) fPCI->configWrite16(0x04, cmd | (s.cmd & 0x6));
    bool ok = true;
    for (unsigned i = 0; i < s.rbCount; i++)
        if ((fPCI->extendedConfigRead32(s.rbOff[i]) & 0x3f00) != (s.rbCtl[i] & 0x3f00)) ok = false;
    for (unsigned i = 0; i < 6; i++) if (fPCI->configRead32(0x10 + i * 4) != s.bar[i]) ok = false;
    LOG("wake: config %s (%u fix-up(s)), command 0x%04x", ok ? "matches the pre-sleep state" : "does NOT read back as saved", fixed, fPCI->configRead16(0x04));
    return ok;
}

bool NVRM::gpuAnswers()
{
    if (fNv.bars[0].size < 0x1000) return true;
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)fNv.bars[0].cpu_address, 0x1000, kIODirectionInOut);
    IOMemoryMap *map = md ? md->map(kIOMapInhibitCache) : NULL;
    UInt32 boot0 = map ? *(volatile UInt32 *)map->getVirtualAddress() : 0xffffffffU;
    OSSafeReleaseNULL(map); OSSafeReleaseNULL(md);
    const bool ok = boot0 != 0xffffffffU && boot0 != 0;
    LOG("wake: PMC_BOOT_0 = 0x%08x - %s", boot0, ok ? "the GPU answers" : "the GPU does not answer");
    return ok;
}

// state 0: the GPU goes to standby (NVKMS turns every head off); 1: resumed, re-light; 2: lost.
void NVRM::notifyFramebuffers(uintptr_t state)
{
    const OSSymbol *fn = OSSymbol::withCString("NVRMGpuPower");
    OSIterator *nubs = getChildIterator(gIOServicePlane);
    unsigned told = 0;
    while (fn && nubs) {
        IOService *nub = OSDynamicCast(IOService, nubs->getNextObject());
        if (!nub) break;
        OSIterator *fbs = nub->getChildIterator(gIOServicePlane);
        while (fbs) {
            IOService *fb = OSDynamicCast(IOService, fbs->getNextObject());
            if (!fb) break;
            if (fb->callPlatformFunction(fn, false, (void *)state, NULL, NULL, NULL) == kIOReturnSuccess) told++;
        }
        OSSafeReleaseNULL(fbs);
    }
    OSSafeReleaseNULL(nubs); OSSafeReleaseNULL(fn);
    LOG("power: told %u framebuffer(s) the GPU is %s", told, state == 0 ? "going to standby" : state == 1 ? "back" : "lost");
}

#undef super
#define super IOUserClient
OSDefineMetaClassAndStructors(NVRMUserClient, IOUserClient)

class IOAccelNVRMBridge : public NVRMUserClient {
    OSDeclareDefaultStructors(IOAccelNVRMBridge)
public:
    void setOwner(NVRM *o) { fOwner = o; }
    bool start(IOService *provider) override {
        if (!fOwner) { kprintf("NVRM-xnu: bridge started with no owner\n"); return false; }
        return IOUserClient::start(provider);
    }
};
OSDefineMetaClassAndStructors(IOAccelNVRMBridge, NVRMUserClient)

IOReturn NVRM::callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4)
{
    if (fn && fn->isEqualTo(NVRM_UTILIZATION_FN)) {
        NVRMUtilization *sample = (NVRMUtilization *)p1;
        if (!sample || sample->version != NVRM_UTILIZATION_ABI_VERSION) return kIOReturnBadArgument;
        if (!fUtilizationLock || isInactive()) return kIOReturnNotReady;
        int counted = 0;
        if (!nv_xnu_pm_try_enter(&counted)) return kIOReturnNotReady;
        IOLockLock(fUtilizationLock);
        const bool ready = !fUtilizationStopping && !isInactive() && fAdapter && fPassDone >= 2 &&
                           OSAddAtomic(0, &fGpuPm) == kGpuPmRunning;
        // A hung firmware read must not be repeated: it holds the RM's exclusive lock for its whole duration, and
        // re-issued every second it keeps other processes and the window server blocked, which stalls shutdown.
        // Longer than kUtilHangNs is a firmware that stopped answering, not a slow GPU, so the sampler stops for the
        // rest of this boot; a fast failure keeps retrying.
        const uint64_t utilT0 = mach_absolute_time();
        const bool ok = ready && nvrm_query_utilization(fNv.gpu_id, &fUtilizationState, sample);
        uint64_t utilNs = 0; absolutetime_to_nanoseconds(mach_absolute_time() - utilT0, &utilNs);
        if (ready && utilNs > kUtilHangNs) {
            fUtilizationStopping = true;
            LOG("utilization: a firmware read took %llu ms (limit %llu) - sampling stopped for this boot so the RM lock is not held again",
                (unsigned long long)(utilNs / 1000000ull), (unsigned long long)(kUtilHangNs / 1000000ull));
        }
        IOLockUnlock(fUtilizationLock);
        nv_xnu_pm_exit(counted);
        return ok ? kIOReturnSuccess : (ready ? kIOReturnIOError : kIOReturnNotReady);
    }
    if (fn && fn->isEqualTo("NVRMBootRaster")) {
        uint32_t *o4 = (uint32_t *)p1; if (!o4) return kIOReturnBadArgument;
        if (fBootHead < 0) return kIOReturnNotFound;
        o4[0] = (uint32_t)fBootHead; o4[1] = fBootPclk; o4[2] = fBootHt; o4[3] = fBootVt; return kIOReturnSuccess;
    }
    if (fn && fn->isEqualTo("NVRMBootScreen")) {
        void **outp = (void **)p1; uint32_t *dims = (uint32_t *)p2;
        if (!outp || !dims) return kIOReturnBadArgument;
        if (!fBootScreen) return kIOReturnNotFound;
        *outp = fBootScreen->getBytesNoCopy(); dims[0] = fBootW; dims[1] = fBootH; dims[2] = fBootPitch;
        return kIOReturnSuccess;
    }
    if (fn && fn->isEqualTo("NVRMBacklight")) {      // NVRMFB: laptop panel brightness. p1 = NvU32 *percent, p2 != NULL sets it
        NvU32 *percent = (NvU32 *)p1; if (!percent) return kIOReturnBadArgument;
        return nvkms_xnu_backlight(p2 != NULL, percent) ? kIOReturnSuccess : kIOReturnNotFound;
    }
    if (fn && fn->isEqualTo("NVRMDdcTransfer")) {     // NVRMFB: one DDC/CI write or read for a display (nvrm_ddc.cpp)
        if (!p1) return kIOReturnBadArgument;
        NvXnuPmUse pmUse(true);
        return nvrm_ddc_transfer((struct NVRMDdcRequest *)p1) == 0 ? kIOReturnSuccess : kIOReturnIOError;
    }
    if (fn && fn->isEqualTo("NVRMDpAux")) {            // NVRMFB: one DPCD transaction (eDP backlight fallback, nvrm_ddc.cpp)
        if (!p1) return kIOReturnBadArgument;
        NvXnuPmUse pmUse(true);
        return nvrm_dpaux_transfer((struct NVRMDpAuxRequest *)p1) == 0 ? kIOReturnSuccess : kIOReturnIOError;
    }
    if (fn && fn->isEqualTo("NVRMGpuReady"))         // NVRMFB: may the display engine be touched (not in system sleep)?
        return OSAddAtomic(0, &fGpuPm) == kGpuPmRunning ? kIOReturnSuccess : kIOReturnNotReady;
    if (fn && fn->isEqualTo("NVRMGpuPower"))         // sent down to the framebuffers; an older NVRMFB passes it back up
        return kIOReturnUnsupported;
    if (fn && fn->isEqualTo("NVRMBootScreenDone")) {
        if (fBootScreen) { fBootScreen->release(); fBootScreen = nullptr; LOG("boot screen: buffer released after the takeover"); }
        return kIOReturnSuccess;
    }
    if (fn && fn->isEqualTo("nvNewAccelClient")) {
        IOUserClient **out = (IOUserClient **)p3;
        if (!out) return kIOReturnBadArgument;
        *out = NULL;
        IOAccelNVRMBridge *uc = OSTypeAlloc(IOAccelNVRMBridge);
        if (!uc) return kIOReturnNoMemory;
        uc->setOwner(this);
        if (!uc->initWithTask((task_t)p1, NULL, (UInt32)(uintptr_t)p2, NULL)) {
            uc->release();
            kprintf("NVRM-xnu: bridge initWithTask(type %u) FAILED\n", (unsigned)(uintptr_t)p2);
            return kIOReturnNoResources;
        }
        if (nv_xnu_chat()) kprintf("NVRM-xnu: bridge client created for the accelerator (rm type %u)\n", (unsigned)(uintptr_t)p2);
        *out = uc;
        return kIOReturnSuccess;
    }
    if (fn && p1) {
        const bool rmCall = fn->isEqualTo(NVRM_GPUVA_FN_ALLOC) || fn->isEqualTo(NVRM_GPUVA_FN_MAP) || fn->isEqualTo(NVRM_GPUVA_FN_FREE)
                         || fn->isEqualTo(NVRM_SS_FN_RM) || fn->isEqualTo(NVRM_SS_FN_PAGEOFF);
        NvXnuPmUse pmUse(rmCall);
        struct NVRMGpuVaRequest *g = (struct NVRMGpuVaRequest *)p1;
        if (fn->isEqualTo(NVRM_GPUVA_FN_ALLOC))
            return nvrm_gpuva_alloc(g) ? kIOReturnIOError : kIOReturnSuccess;
        if (fn->isEqualTo(NVRM_GPUVA_FN_MAP))
            return nvrm_gpuva_map(g)   ? kIOReturnIOError : kIOReturnSuccess;
        if (fn->isEqualTo(NVRM_GPUVA_FN_FREE))
            return nvrm_gpuva_free(g)  ? kIOReturnIOError : kIOReturnSuccess;
        if (fn->isEqualTo(NVRM_SS_FN_RM))
            return nvrm_surfshare_rm((struct NVRMSurfShareRm *)p1) ? kIOReturnIOError : kIOReturnSuccess;
        if (fn->isEqualTo(NVRM_SS_FN_PAGEOFF))
            return nvrm_surfshare_pageoff((struct NVRMSurfPageoff *)p1, &fNv) ? kIOReturnIOError : kIOReturnSuccess;
    }
    if (fn && fn->isEqualTo(NVRM_DISPLAY_PTRS_FN)) {
        NVRMDisplayPointers *ptrs = (NVRMDisplayPointers *)p1;
        if (!ptrs || ptrs->version != NVRM_DISPLAY_PTRS_ABI_VERSION) return kIOReturnBadArgument;
        ptrs->kapi = nvkms_xnu_kapi_table();
        ptrs->physForVa = (void *)&nv_xnu_phys_for_kernel_va;
        return ptrs->kapi ? kIOReturnSuccess : kIOReturnNotReady;
    }
    return IOService::callPlatformFunction(fn, wait, p1, p2, p3, p4);
}

bool NVRMUserClient::start(IOService *provider)
{
    fOwner = OSDynamicCast(NVRM, provider);
    if (!fOwner) return false;
    return super::start(provider);
}
bool NVRMUserClient::initWithTask(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties)
{
    if (!super::initWithTask(owningTask, securityID, type, properties)) return false;
    // The user-client type came from user space and anything non-zero silently became a device
    // client, so a bogus type (0x4841, etc.) opened a working device client. Only two types exist:
    // 0 = ctl (escalated RM state), 1 = device. Refuse anything else rather than treating it as a
    // device client. The ctl client stays open to unprivileged callers on purpose -- the Vulkan
    // userland opens it as the logged-in user -- and the escalated operations behind it are gated
    // by the real-privilege check in _os_is_administrator and the root requirement on first init.
    if (type > 1) return false;
    fIsCtl = (type == 0);
    fGate = IOLockAlloc();
    if (!fGate) return false;
    fMapLock = IOLockAlloc();
    if (!fMapLock) return false;
    nvu_memset(&fNvfp, 0, sizeof fNvfp);
    fNvfp.ctl_nvfp_priv = this;
    if (!gNvClientsLock) gNvClientsLock = IOLockAlloc();
    // One table serves every process, so one process (or one user's processes) could fill it and leave WindowServer
    // unable to open a client. Two limits: a process holds at most kNvClientsPerPid (the busiest process measured on
    // a desktop held 154: Maps; Firefox's GPU process 96, WindowServer 78), and the last kNvClientsReserved slots
    // are kept for system accounts (uid < 500: root and _windowserver), so the desktop can always open one.
    static const int kNvClientsPerPid = 4096, kNvClientsReserved = 4096;
    const int pid = proc_selfpid();
    const bool systemCaller = kauth_getuid() < 500;
    int used = 0, mine = 0, slot = -1;
    IOLockLock(gNvClientsLock);
    for (int i = 0; i < NV_XNU_MAX_CLIENTS; i++) {
        NVRMUserClient *k = gNvClients[i];
        if (!k) { if (slot < 0) slot = i; continue; }
        used++;
        if (k->fPid == pid) mine++;
    }
    const bool overPid = mine >= kNvClientsPerPid;
    const bool overShared = !systemCaller && used >= NV_XNU_MAX_CLIENTS - kNvClientsReserved;
    if (slot >= 0 && !overPid && !overShared) { gNvClients[slot] = this; fFd = slot; }
    IOLockUnlock(gNvClientsLock);
    if (overPid || overShared) {
        static unsigned quotaLogged;
        if (quotaLogged < 8) { quotaLogged++;
            kprintf("NVRM-xnu: client refused for pid %d: %s (%d held by this process, %d of %d in use)\n", pid,
                    overPid ? "per-process limit" : "slots reserved for system accounts", mine, used, NV_XNU_MAX_CLIENTS); }
        return false;
    }
    if (fFd < 0) {
        static unsigned tableFullLogged;
        if (tableFullLogged < 8) { tableFullLogged++;
            kprintf("NVRM-xnu: CLIENT TABLE FULL (%d of %d in use) -- every further allocation will "
                    "fail until a client closes\n", NV_XNU_MAX_CLIENTS, NV_XNU_MAX_CLIENTS); }
        return false;
    }
    fPid = proc_selfpid();
    fPm = (uint64_t)get_task_pmap(owningTask);
    IOLockLock(gNvClientsLock); nv_pm_open(fPm, fPid); IOLockUnlock(gNvClientsLock);
    if (nv_xnu_chat()) kprintf("NVRM-xnu: client fd %d opened (%s)\n", fFd, fIsCtl ? "ctl" : "device");
    return true;
}
nv_state_t *NVRMUserClient::nvState() { return fIsCtl ? nv_get_ctl_state() : &fOwner->fNv; }
IOReturn NVRMUserClient::clientClose(void)
{
    // One winner only: this was a plain bool test-and-set, so two closes could both run the teardown.
    if (OSCompareAndSwap(0, 1, &fCleaned)) {
        Gate g(fGate);
        if (gNvClientsLock && fFd >= 0) { IOLockLock(gNvClientsLock); gNvClients[fFd] = NULL; nv_pm_close(fPm); IOLockUnlock(gNvClientsLock); }
        if (fOwner) { NvXnuPmUse pmUse; rm_cleanup_file_private(NULL, nvState(), &fNvfp); }
        // The ranges are NOT freed here. clientClose can run while another thread is inside
        // clientMemoryForType, which reads this array and hands its entries to
        // IOMemoryDescriptor::withPhysicalAddress - so freeing it here let a process map whatever
        // landed in the reused block. Marking the context dead is enough; free() does the release,
        // and IOKit only gets there once no method can still be running.
        IOLockLock(fMapLock);
        if (fMapList) fMapList->context.valid = NV_FALSE;
        IOLockUnlock(fMapLock);
        if (nv_xnu_chat()) kprintf("NVRM-xnu: client fd %d closed\n", fFd);
    }
    terminate();
    return kIOReturnSuccess;
}
void NVRMUserClient::free(void)
{
    if (fMapLock) IOLockLock(fMapLock);
    nvMapListFree(&fMapList);
    fMapLen = 0; fMapLenNode = nullptr; fMapGen++;
    if (fTok) {
        for (unsigned i = 0; i < fNTok; i++) if (fTok[i].md) fTok[i].md->release();
        kern_os_free(fTok); fTok = nullptr; fNTok = 0; fTokCap = 0;
    }
    if (fMapLock) IOLockUnlock(fMapLock);
    if (fGate) { IOLockFree(fGate); fGate = nullptr; }
    if (fMapLock) { IOLockFree(fMapLock); fMapLock = nullptr; }
    super::free();
}
// Token mappings - many CPU mappings on one connection.
// Type 0 is the original contract: map whatever context is pending, and leave it pending (the userland driver opens
// one connection per mapping and closes it with the mapping). That costs a user-client connection per live mapping;
// Cyberpunk held ~275 of them. With a token type (kNvMapTokenBit set, chosen by the caller, unique per mapping on this
// client) the first call consumes the pending context - the way Linux's per-file mapping list is used once per mmap -
// so RM accepts the next RM_MAP_MEMORY on the same fd, and the descriptor is kept under the token. Every later call with
// that token returns the SAME descriptor: IOKit's unmap (is_io_connect_unmap_memory_from_task) calls clientMemoryForType
// with the type it is given and finds the mapping with kIOMapReference on the returned descriptor's own mapping list,
// so a fresh descriptor would never match and the mapping would stay until the connection closed. That is the shape of
// the 10-14 bug (8494d18): a cleared context made the unmap lookup fail and VRAM stayed mapped until exit.
// The caller forgets the token (selector 8) after unmapping, which drops the descriptor. RM's own unmap
// (NV_ESC_RM_UNMAP_MEMORY -> Nv04UnmapMemoryWithSecInfo, on the ctl fd, by hClient/hDevice/hMemory/pLinearAddress) never
// reads the per-file list, and nv_free_user_mapping is a no-op here, so consuming the node takes nothing RM needs.
// The pages stay alive through the descriptor, not the node (see nv_inc_alloc_refcount in os-xnu2.cpp).
#include "nvrm_tok_table.h"
static const uint32_t kNvMapTokenBit = 0x80000000u;
static const unsigned kNvMapTokensPerClient = 4096;
IOReturn NVRMUserClient::clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory)
{
    Gate g(fGate);
    const bool tokenMode = (type & kNvMapTokenBit) != 0;
    // any other type keeps the original behaviour (it was never checked, so nothing is refused that was accepted)
    if (tokenMode) {
        IOMemoryDescriptor *have = nullptr; IOOptionBits haveOpts = 0;
        IOLockLock(fMapLock);
        for (unsigned i = 0; i < fNTok; i++)
            if (fTok[i].token == type) { have = fTok[i].md; haveOpts = fTok[i].options; have->retain(); break; }
        IOLockUnlock(fMapLock);
        if (have) { *options |= haveOpts; *memory = have; return kIOReturnSuccess; }
    }
    nv_alloc_mapping_list_node_t *snapNode = nullptr; NvU64 snapGen = 0;
    // Everything below reads this private copy. RM or a bind can replace the live context at any moment, and the ranges
    // that are checked must be the ranges that are mapped.
    nv_alloc_mapping_context_t snapshot;
    NvU64 snapLen = 0;
    MemoryRange *ranges = NULL;
    IOLockLock(fMapLock);
    if (!fMapList || !fMapList->context.valid) {
        IOLockUnlock(fMapLock);
        kprintf("NVRM-xnu: map on fd %d with no mapping context\n", fFd);
        return kIOReturnBadArgument;
    }
    snapshot = fMapList->context;
    snapNode = fMapList; snapGen = fMapGen;
    snapLen = (fMapLenNode == fMapList) ? fMapLen : 0;   // a length belongs to one node only
    if (!fIsCtl) {
        const NvU64 count = snapshot.memArea.numRanges;
        if (count == 0 || count > 4096 || !snapshot.memArea.pRanges) { IOLockUnlock(fMapLock); return kIOReturnBadArgument; }
        ranges = (MemoryRange *)kern_os_malloc((size_t)(sizeof(MemoryRange) * count));
        if (!ranges) { IOLockUnlock(fMapLock); return kIOReturnNoMemory; }
        nvu_memcpy(ranges, snapshot.memArea.pRanges, (size_t)(sizeof(MemoryRange) * count));
    }
    IOLockUnlock(fMapLock);
    snapshot.memArea.pRanges = ranges;
    struct FreeRanges { MemoryRange *p; ~FreeRanges() { if (p) kern_os_free(p); } } freeRanges = { ranges };
    nv_alloc_mapping_context_t *c = &snapshot;
    IOMemoryDescriptor *md = NULL;
    NvU32 caching = c->caching;
    if (fIsCtl) {
        NvU64 bytes = 0; NvU32 cache = 0;
        IOMemoryDescriptor *base = nv_xnu_alloc_md(c->alloc, &bytes, &cache);
        NvU64 off = c->page_index * PAGE_SIZE;
        if (!base) return kIOReturnBadArgument;
        if (off >= bytes) { base->release(); return kIOReturnBadArgument; }
        // NVIDIA 620's context carries no size and its alloc is the ROOT allocation, which can hold other objects
        // past this one. The size is the one RM validated for this mapping (fMapLen, reported by
        // nv_alloc_user_mapping); with none recorded the map is refused, never widened to the allocation's end.
        // Subtract, never add: an earlier off+len form wrapped past the clamp.
        if (snapLen == 0) { base->release(); kprintf("NVRM-xnu: map fd %d: no validated length - refused\n", fFd); return kIOReturnNotPermitted; }
        NvU64 len = (snapLen + PAGE_SIZE - 1) & ~(NvU64)(PAGE_SIZE - 1);
        if (len > bytes - off) len = bytes - off;
        md = IOSubMemoryDescriptor::withSubRange(base, (IOByteCount)off, (IOByteCount)len, kIODirectionInOut);
        base->release();
        caching = cache;
        if (nv_xnu_chat()) kprintf("NVRM-xnu: map fd %d: sysmem +0x%llx len 0x%llx cache %u\n", fFd, off, len, caching);
    } else {
        NvU64 n = c->memArea.numRanges;
        // A range that is not card memory goes down the system-RAM path below, which checks the owner.
        // A range that IS card memory must still land inside one of this card's bars, which is what stops
        // a caller handing us an arbitrary physical address and getting it mapped.
        // This was narrowed to the framebuffer bar alone, on the reasoning that a user process has no
        // business holding a writable mapping of the register file. The display stack disagrees: measured
        // on the test Mac, CoreDisplay maps phys 0x80bb0000 +0x10000 and +0x1000, which live in bar0, and
        // refusing them made WindowServer abort in CoreDisplay_CreateDisplayForCGXDisplayDevice and
        // crash-loop every ten seconds from 21 s after boot (7 reports, 23 refusals). Keeping the bar bound
        // and nothing tighter is therefore the shipping behaviour; narrowing it needs a way to tell the
        // display path's own mapping from a caller's, which we do not have here.
        if (n == 1 && !nv_pm_is_gpu(c->memArea.pRanges[0].start, c->memArea.pRanges[0].size)) {
            md = nv_pm_ram_submd(c->memArea.pRanges[0].start, c->memArea.pRanges[0].size, fPid);
            if (!md) return kIOReturnNotPermitted;
        } else if (n == 1) {
            md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)c->memArea.pRanges[0].start, (IOByteCount)c->memArea.pRanges[0].size, kIODirectionInOut);
        } else {
            for (NvU64 i = 0; i < n; i++) if (!nv_pm_is_gpu(c->memArea.pRanges[i].start, c->memArea.pRanges[i].size)) {
                nv_pm_dev_refused(c->memArea.pRanges[i].start, c->memArea.pRanges[i].size); return kIOReturnNotPermitted; }
            IOAddressRange *r = (IOAddressRange *)kern_os_malloc((size_t)(sizeof(IOAddressRange) * n));
            if (!r) return kIOReturnNoMemory;
            for (NvU64 i = 0; i < n; i++) { r[i].address = c->memArea.pRanges[i].start; r[i].length = c->memArea.pRanges[i].size; }
            md = IOMemoryDescriptor::withOptions(r, (UInt32)n, 0, NULL, kIOMemoryTypePhysical64 | kIODirectionInOut);
            kern_os_free(r);
        }
        kprintf("NVRM-xnu: map fd %d: device %llu range(s) first 0x%llx+0x%llx caching %u\n", fFd, n, c->memArea.pRanges[0].start, c->memArea.pRanges[0].size, caching);
    }
    if (!md) return kIOReturnNoMemory;
    IOOptionBits add = 0;
    if (caching == NV_MEMORY_UNCACHED) add |= kIOMapInhibitCache;
    else if (caching == NV_MEMORY_WRITECOMBINED) add |= kIOMapWriteCombineCache;
    if (!(c->prot & NV_PROTECT_WRITEABLE)) add |= kIOMapReadOnly;
    if (tokenMode) {
        IOReturn r = kIOReturnSuccess;
        IOLockLock(fMapLock);
        if (fMapList != snapNode || fMapGen != snapGen) r = kIOReturnBusy;   // RM replaced the context meanwhile
        else {
            // room for one more: the table grows with what the client holds (nvrm_tok_table.h says why)
            const unsigned cap = nvrm_tok_next_capacity(fTokCap, fNTok, kNvMapTokensPerClient);
            if (!cap) r = kIOReturnNoResources;
            else if (cap != fTokCap) {
                NvMapToken *grown = (NvMapToken *)kern_os_malloc(sizeof(NvMapToken) * cap);
                if (!grown) r = kIOReturnNoMemory;
                else {
                    if (fTok) { nvu_memcpy(grown, fTok, sizeof(NvMapToken) * fNTok); kern_os_free(fTok); }
                    fTok = grown; fTokCap = cap;
                }
            }
        }
        if (r == kIOReturnSuccess) {
            nv_alloc_mapping_list_node_t *one = fMapList;
            fMapList = one->pNext; one->pNext = nullptr;
            nvMapListFree(&one);
            fMapLen = 0; fMapLenNode = nullptr; fMapGen++;
            md->retain();
            fTok[fNTok].token = type; fTok[fNTok].options = add; fTok[fNTok].md = md; fNTok++;
        }
        IOLockUnlock(fMapLock);
        if (r != kIOReturnSuccess) { md->release(); return r; }
    }
    *options |= add;
    *memory = md;
    return kIOReturnSuccess;
}
// Selector 7: what this kext supports. bit0 = token mappings (clientMemoryForType above).
IOReturn NVRMUserClient::sCaps(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    if (!OSDynamicCast(NVRMUserClient, target)) return kIOReturnNotAttached;
    args->scalarOutput[0] = 1;
    return kIOReturnSuccess;
}
// Selector 8: forget a token after its mapping is unmapped; drops the descriptor. An unknown token is not an error.
IOReturn NVRMUserClient::sForgetToken(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    const uint64_t v = args->scalarInput[0];
    if (v > 0xFFFFFFFFull || !((uint32_t)v & kNvMapTokenBit)) return kIOReturnBadArgument;
    IOMemoryDescriptor *drop = nullptr;
    IOLockLock(me->fMapLock);
    for (unsigned i = 0; i < me->fNTok; i++)
        if (me->fTok[i].token == (uint32_t)v) { drop = me->fTok[i].md; me->fTok[i] = me->fTok[--me->fNTok]; break; }
    IOLockUnlock(me->fMapLock);
    if (drop) drop->release();
    return kIOReturnSuccess;
}
IOReturn NVRMUserClient::externalMethod(uint32_t selector, IOExternalMethodArguments *args,
                                        IOExternalMethodDispatch *dispatch, OSObject *target, void *reference)
{
    static const IOExternalMethodDispatch tbl[] = {
        { sGo,    1, 0, 0, 0 },
        { sEsc,   1, kIOUCVariableStructureSize, 0, kIOUCVariableStructureSize },
        { sGetFd, 0, 0, 1, 0 },
        { sBindFd, 1, 0, 0, 0 },
        // 4 and 5 opened an NVKMS kernel-space session from user space; retired, kept so sWaitEvent stays 6
        { sRetired, 0, 0, 0, 0 },
        { sRetired, 0, 0, 0, 0 },
        { sWaitEvent, 2, 0, 1, 0 },
        { sCaps, 0, 0, 1, 0 },
        { sForgetToken, 1, 0, 0, 0 },
    };
    if (selector >= sizeof tbl / sizeof tbl[0]) return kIOReturnBadArgument;
    IOExternalMethodDispatch d = tbl[selector];
    return super::externalMethod(selector, args, &d, this, reference);
}
extern "C" nv_state_t *nv_xnu_gpu_state(void) { NVRM *s = gNVRMTab.primary(); return s ? &s->fNv : NULL; }
extern "C" void *nv_xnu_gpu_os_device(void) { return gNVRMTab.primary(); }
// the same answers for card number `index` (0 is the first started); only meaningful with nvmultigpu=1, where the table holds every card
extern "C" nv_state_t *nv_xnu_gpu_state_at(unsigned index) { NVRM *s = gNVRMTab.at(index); return s ? &s->fNv : NULL; }
extern "C" void *nv_xnu_gpu_os_device_at(unsigned index) { return gNVRMTab.at(index); }
extern "C" unsigned nv_xnu_gpu_count(void) { return gNVRMTab.count(); }

// Selectors 4 and 5 handed a caller's buffer to NVKMS as a kernel-space client, which trusts the pointers inside it
// (flip heads, LUT ramps, clip lists) as kernel addresses: root could make the kernel read and write where it chose.
// Nothing the driver ships used them; the display path reaches NVKMS through its in-kernel API.
IOReturn NVRMUserClient::sRetired(OSObject *, void *, IOExternalMethodArguments *) { return kIOReturnUnsupported; }
IOReturn NVRMUserClient::sGetFd(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    args->scalarOutput[0] = (uint64_t)me->fFd;
    return kIOReturnSuccess;
}
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Winvalid-offsetof"
NVRMUserClient *NVRMUserClient::fromNvfp(nv_file_private_t *p)
{
    if (!p) return NULL;
    NVRMUserClient *c = (NVRMUserClient *)((char *)p - __builtin_offsetof(NVRMUserClient, fNvfp));
    return c->fEvtMagic == 0x4e564556u ? c : NULL;
}
#pragma clang diagnostic pop
// Live count of callers parked in sWaitEvent, and the ceiling. Was: no cap,
// so N processes parked N kernel threads. 256 is far above what the driver
// itself uses (one waiter per active client) and far below a thread count
// that matters to the system.
static volatile SInt32 gNvEvtSleepers = 0;
static const SInt32 kNvEvtMaxSleepers = 256;
extern "C" void NV_API_CALL nv_post_event(nv_event_t *event, NvHandle hEvent, NvU32 index, NvU32 info32, NvU16 info16, NvBool data_valid)
{
    const SInt64 k = OSIncrementAtomic64(&gNvEvtPosts);
    NVRMUserClient *c = event ? NVRMUserClient::fromNvfp(event->nvfp) : NULL;
    if (c) { OSIncrementAtomic64(&gNvEvtMatched); OSIncrementAtomic((volatile SInt32 *)&c->fEvtSeq); thread_wakeup((event_t)&c->fEvtSeq); }
    if (k == 999 || k == 99999 || k == 999999)
        OSReportWithBacktrace("NVRM-xnu: post #%lld caller chain (nv_post_event at %p, isr-thread %d)", (long long)k + 1,
                              (void *)&nv_post_event, current_thread() == nv_xnu_isr_thread);
    if (k < 4 || ((k + 1) & 0x3FFF) == 0)
        kprintf("NVRM-xnu: nv_post_event #%lld hEvent 0x%x index %u -> %s | matched %lld, waits %lld, slept %lld\n",
                (long long)k + 1, hEvent, index, c ? "connection woken" : "no connection", (long long)gNvEvtMatched,
                (long long)gNvEvtWaits, (long long)gNvEvtSlept);
}
IOReturn NVRMUserClient::sWaitEvent(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    const UInt32 seen = (UInt32)args->scalarInput[0];
    uint64_t us = args->scalarInput[1]; if (us > 100000) us = 100000;
    OSIncrementAtomic64(&gNvEvtWaits);
    if (us && me->fEvtSeq == seen) {
        // OSIncrementAtomic returns the value before the increment.
        if (OSIncrementAtomic(&gNvEvtSleepers) >= kNvEvtMaxSleepers) {
            // At the ceiling: return the current sequence without parking. This
            // is the same answer a timeout gives, so no caller can observe a
            // result it could not already have seen.
            OSDecrementAtomic(&gNvEvtSleepers);
        } else {
            assert_wait_timeout((event_t)&me->fEvtSeq, THREAD_ABORTSAFE, (uint32_t)us, NSEC_PER_USEC);
            if (me->fEvtSeq != seen) thread_wakeup((event_t)&me->fEvtSeq);
            OSIncrementAtomic64(&gNvEvtSlept);
            (void)thread_block(THREAD_CONTINUE_NULL);
            OSDecrementAtomic(&gNvEvtSleepers);
        }
    }
    args->scalarOutput[0] = me->fEvtSeq;
    return kIOReturnSuccess;
}
IOReturn NVRMUserClient::sBindFd(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    // The value is a file descriptor number, or a negative number meaning "detached": the userland driver
    // detaches a connection it is about to pool or keep alive by binding it to -2, sign-extended to 64 bits.
    // Anything else outside the 32-bit signed range is a caller error, not something to store and look up
    // later. Refusing the detach value left the connection bound to an fd number the process then reused, so
    // two connections answered for one fd and the Vulkan device in WindowServer could not be created.
    const int64_t v = (int64_t)args->scalarInput[0];
    if (v > 0x7FFFFFFFLL || v < -0x80000000LL) return kIOReturnBadArgument;
    me->fBound = (int)v;
    me->fEverBound = true;
    // A stale mapping context is cleared when the connection is bound to a real fd, the only point where it
    // could be used for a new mapping. Not on detach (a negative value): the userland driver detaches a
    // connection it keeps alive for mappings that are still live, and clearing the context there left those
    // mappings unreleasable, so VRAM filled up and stayed full after the process exited.
    // Under the mapping lock: two binds at once both saw valid and freed the same block, and a bind during a map freed
    // the ranges that map was reading, so what it checked and what it mapped could differ.
    if (me->fBound >= 0) {
        IOLockLock(me->fMapLock);
        if (me->fMapList) {
            kprintf("NVRM-xnu: client %d re-bound to fd %d with a stale mapping context - cleared\n", me->fFd, me->fBound);
            nvMapListFree(&me->fMapList);
            me->fMapLen = 0; me->fMapLenNode = nullptr; me->fMapGen++;
        }
        IOLockUnlock(me->fMapLock);
    }
    if (nv_xnu_chat()) kprintf("NVRM-xnu: client %d bound to pid %d fd %d\n", me->fFd, me->fPid, me->fBound);
    return kIOReturnSuccess;
}
IOReturn NVRMUserClient::sEsc(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me || !me->fOwner) return kIOReturnNotAttached;
    uint32_t cmd = (uint32_t)args->scalarInput[0];
    uint32_t size = args->structureInputDescriptor ? (uint32_t)args->structureInputDescriptor->getLength() : args->structureInputSize;
    if (size == 0 || size > (1u << 20)) return kIOReturnBadArgument;
    void *buf = kern_os_malloc(size);
    if (!buf) return kIOReturnNoMemory;
    if (args->structureInputDescriptor) {
        if (args->structureInputDescriptor->prepare() != kIOReturnSuccess) { kern_os_free(buf); return kIOReturnVMError; }
        args->structureInputDescriptor->readBytes(0, buf, size);
        args->structureInputDescriptor->complete();
    } else nvu_memcpy(buf, args->structureInput, size);
    IOReturn r = me->esc(cmd, buf, size);
    if (args->structureOutputDescriptor) {
        if (args->structureOutputDescriptor->prepare() == kIOReturnSuccess) {
            args->structureOutputDescriptor->writeBytes(0, buf, size);
            args->structureOutputDescriptor->complete();
        }
    } else if (args->structureOutput && args->structureOutputSize >= size) {
        nvu_memcpy(args->structureOutput, buf, size);
        args->structureOutputSize = size;
    }
    kern_os_free(buf);
    return r;
}
IOReturn NVRMUserClient::esc(uint32_t cmd, void *data, uint32_t size)
{
    Gate g(fGate);
    NvMapPendingScope pendingScope;   // a mapping-length slot never outlives this escape
    if (!fOwner || fOwner->fPassDone < 2) { static bool once; if (!once) { once = true; kprintf("NVRM-xnu: esc 0x%x refused: adapter not up (run nvrmctl go 2)\n", cmd); } return kIOReturnNotReady; }
    nv_state_t *nv = nvState();
    NvXnuPmUse pmUse;
    switch (cmd) {
    case NV_ESC_CARD_INFO: {
        if (!fIsCtl) return kIOReturnBadArgument;
        nvu_memset(data, 0, size);
        if (size < sizeof(nv_ioctl_card_info_t)) return kIOReturnBadArgument;
        nv_state_t *ad = &fOwner->fNv;
        nv_ioctl_card_info_t *ci = (nv_ioctl_card_info_t *)data;
        ci->valid              = NV_TRUE;
        ci->pci_info           = ad->pci_info;
        ci->gpu_id             = ad->gpu_id;
        ci->interrupt_line     = (NvU16)ad->interrupt_line;
        ci->reg_address        = ad->bars[NV_GPU_BAR_INDEX_REGS].cpu_address;
        ci->reg_size           = ad->bars[NV_GPU_BAR_INDEX_REGS].size;
        ci->fb_address         = ad->bars[NV_GPU_BAR_INDEX_FB].cpu_address;
        ci->fb_size            = ad->bars[NV_GPU_BAR_INDEX_FB].size;
        ci->minor_number       = 0;
        return kIOReturnSuccess;
    }
    case NV_ESC_CHECK_VERSION_STR: {
        if (!fIsCtl) return kIOReturnBadArgument;
        NV_STATUS st = rm_perform_version_check(NULL, data, size);
        return st == NV_OK ? kIOReturnSuccess : kIOReturnBadArgument;
    }
    case NVRM_ESC_SURF_DIRTY:
    case NVRM_ESC_SURF_VRAM: {
        if (size != sizeof(struct NVRMSurfShareEsc)) return kIOReturnBadArgument;
        struct NVRMSurfShareEsc *ss = (struct NVRMSurfShareEsc *)data;
        ss->status = NVRM_SS_OFF;
        static IOService *accel;
        if (!accel) { OSDictionary *m = IOService::serviceMatching("NVAccel");
                      if (m) { accel = IOService::waitForMatchingService(m, 100ULL * 1000 * 1000); m->release(); } }
        if (!accel) return kIOReturnSuccess;
        const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVACCEL_SS_FN);
        if (sym) { accel->callPlatformFunction(sym, false, ss, (void *)current_task(), (void *)(uintptr_t)cmd, NULL); sym->release(); }
        return kIOReturnSuccess;
    }
    default: {
        NV_STATUS st = rm_ioctl(NULL, nv, &fNvfp, cmd, data, size);
        if (st != NV_OK) kprintf("NVRM-xnu: esc 0x%x size %u -> RM status 0x%x\n", cmd, size, st);
        return st == NV_OK ? kIOReturnSuccess : kIOReturnError;
    }
    }
}
IOReturn NVRMUserClient::sGo(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me || !me->fOwner) return kIOReturnNotAttached;
    // Starting the GPU's passes by hand is for root only (security report #214, finding 5: any process could force a
    // GPU re-init). The driver starts itself at boot (autoGo); nothing a user runs needs this.
    if (!kauth_cred_issuser(kauth_cred_get())) return kIOReturnNotPrivileged;
    return me->fOwner->go((uint32_t)args->scalarInput[0]);
}

extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }

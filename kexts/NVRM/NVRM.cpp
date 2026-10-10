/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nvrm_off.h"
#include "nv-xnu.h"
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
kern_return_t _start(kmod_info_t *ki, void *data);
kern_return_t _stop(kmod_info_t *ki, void *data);
KMOD_EXPLICIT_DECL(com.nullmoth.NVRM, "0.1", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = NULL;
__private_extern__ kmod_stop_func_t  *_antimain = NULL;
__private_extern__ int _kext_apple_cc = __APPLE_CC__;
}

#define LOG(fmt, ...) do { kprintf("NVRM-xnu: " fmt "\n", ##__VA_ARGS__); IOLog("NVRM-xnu: " fmt "\n", ##__VA_ARGS__); } while (0)

struct nvkms_per_open;
extern "C" { NvBool nvkms_xnu_load(void); struct nvkms_per_open *nvkms_xnu_open(int pid); void nvkms_xnu_close(struct nvkms_per_open *);
             int nvkms_xnu_ioctl(struct nvkms_per_open *, NvU32 cmd, void *params, size_t size); int nvkms_xnu_events_available(struct nvkms_per_open *); }
static class NVRM *gNVRMService;
extern "C" nv_state_t *nv_xnu_gpu_state(void);
extern "C" void nv_xnu_log_init(void);
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
        nub->setProperty("nvkms-kapi", (unsigned long long)(uintptr_t)kapi, 64);
        nub->setProperty("gpu-id", (unsigned long long)gpuId, 32);
        nub->setProperty("phys-for-va", (unsigned long long)(uintptr_t)&nv_xnu_phys_for_kernel_va, 64);
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
public:
    virtual IOService *probe(IOService *provider, SInt32 *score) override;
    virtual bool start(IOService *provider) override;
    virtual void stop(IOService *provider) override;
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
    volatile UInt32 fBootHold = 0;
    virtual IOReturn callPlatformFunction(const OSSymbol *fn, bool wait,
                                         void *p1, void *p2, void *p3, void *p4) override;
private:
    IOPCIDevice *fPCI = NULL;
    nv_state_t   fNv = {};
    bool         fPrivateState = false;
    bool         fAdapter = false;
    uint32_t     fPassDone = 0;
    IOWorkLoop  *fWorkLoop = NULL;
    IOFilterInterruptEventSource *fIntSrc = NULL;
    void readBARs();
    bool placeLargeBar1();
    bool interruptFilter(IOFilterInterruptEventSource *src);
    void interruptOccurred(IOInterruptEventSource *src, int count);
};
extern "C" { extern volatile int nv_xnu_in_isr; extern volatile thread_t nv_xnu_isr_thread; void nv_xnu_dump_live_allocs(void); }
static volatile unsigned long gNvIsrCount = 0, gNvIsrHandled = 0;

static inline void nvu_memset(void *d, int c, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory"); }
static inline void nvu_memcpy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }
#define NV_XNU_MAX_CLIENTS 32768
class NVRMUserClient;
static NVRMUserClient *gNvClients[NV_XNU_MAX_CLIENTS];
static IOLock         *gNvClientsLock = NULL;
#include <kern/clock.h>
extern "C" struct pmap *get_task_pmap(task_t);
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
    IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args,
                            IOExternalMethodDispatch *dispatch, OSObject *target, void *reference) override;
    bool initWithTask(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties) override;
    nv_file_private_t *nvfp() { return &fNvfp; }
    IOReturn clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory) override;
    nv_alloc_mapping_context_t *mmapContext() { return &fMmap; }
    bool isCtl() const { return fIsCtl; }
    nv_state_t *nvState();
protected:
    NVRM *fOwner = NULL;
private:
    nv_file_private_t fNvfp = {};
    nv_alloc_mapping_context_t fMmap = {};
    bool fIsCtl = true;
    int  fFd = -1;
    int  fPid = 0;
    bool fEverBound = false;
    int  fBound = -1;
    bool fCleaned = false;
    struct nvkms_per_open *fKms = nullptr;
    IOReturn esc(uint32_t cmd, void *data, uint32_t size);
    static IOReturn sGo(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sEsc(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sGetFd(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sBindFd(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sKms(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sKmsEvents(OSObject *target, void *reference, IOExternalMethodArguments *args);
    static IOReturn sWaitEvent(OSObject *target, void *reference, IOExternalMethodArguments *args);
public:
    volatile UInt32 fEvtSeq = 0;
    UInt32 fEvtMagic = 0x4e564556u;
    static NVRMUserClient *fromNvfp(nv_file_private_t *p);
    int  pid() const { return fPid; }
    int  boundFd() const { return fBound; }
    bool everBound() const { return fEverBound; }
    int  index() const { return fFd; }
};
extern "C" nv_file_private_t *nv_xnu_lookup_fd(NvS32 fd, void **priv, NvBool *isCtl)
{
    if (!gNvClientsLock) return NULL;
    int pid = proc_selfpid();
    NVRMUserClient *c = NULL;
    IOLockLock(gNvClientsLock);
    for (int i = 0; i < NV_XNU_MAX_CLIENTS && !c; i++) { NVRMUserClient *k = gNvClients[i]; if (k && k->pid() == pid && k->boundFd() == (int)fd) c = k; }
    for (int i = 0; i < NV_XNU_MAX_CLIENTS && !c; i++) { NVRMUserClient *k = gNvClients[i]; if (k && k->pid() == pid && !k->everBound() && k->index() == (int)fd) c = k; }
    IOLockUnlock(gNvClientsLock);
    if (!c) return NULL;
    if (priv) *priv = c;
    if (isCtl) *isCtl = c->isCtl() ? NV_TRUE : NV_FALSE;
    return c->nvfp();
}
static void nvMmapContextClear(nv_alloc_mapping_context_t *c)
{
    if (c->memArea.pRanges) kern_os_free(c->memArea.pRanges);
    bzero(c, sizeof *c);
}
extern "C" nv_alloc_mapping_context_t *nv_xnu_mmap_context(void *priv)
{
    NVRMUserClient *c = OSDynamicCast(NVRMUserClient, (OSObject *)priv);
    return c ? c->mmapContext() : NULL;
}
extern "C" IOMemoryDescriptor *nv_xnu_alloc_md(void *priv, NvU64 *bytes, NvU32 *cache);
extern "C" { IOMemoryDescriptor *nv_pm_ram_submd(NvU64, NvU64); bool nv_pm_is_gpu(NvU64, NvU64); void nv_pm_dev_refused(NvU64, NvU64); bool nv_xnu_chat(void); }

#define super IOService
OSDefineMetaClassAndStructors(NVRM, IOService)
static bool gRmInitDone = false;

static inline bool nvrmNameIsUnknown(const char *s)
{
    static const char u[] = "Unknown";
    for (unsigned i = 0; i < sizeof u; i++) if (s[i] != u[i]) return false;
    return true;
}

IOService *NVRM::probe(IOService *provider, SInt32 *score)
{
    { if (nvrm_driver_off()) return NULL; }
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
    { if (nvrm_driver_off()) return false; }
    if (!super::start(provider)) return false;
    fPCI = OSDynamicCast(IOPCIDevice, provider);
    if (!fPCI) return false;
    gNVRMService = this;
    nv_xnu_log_init();
    UInt8 bus = fPCI->getBusNumber(), dev = fPCI->getDeviceNumber(), fn = fPCI->getFunctionNumber();
    nv_xnu_register_pci(fPCI, 0, bus, dev, fn);
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
    const bool bar1Placed = placeLargeBar1();
    readBARs();
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
        fAutoGoSettleMs = bar1Placed ? 500 : 100000;
        { uint32_t ms = 0; if (PE_parse_boot_argn("nvrmsettle", &ms, sizeof(ms))) fAutoGoSettleMs = ms; }
        if (PE_parse_boot_argn("-nvrmnogo", nogo, sizeof(nogo))) {
            setProperty("nvrm-autogo", "off (-nvrmnogo)"); LOG("auto-go OFF (-nvrmnogo): idle until `nvrmctl go <pass>`");
        } else {
            fBootHold = 1; adjustBusy(1); setProperty("nvrm-boot-hold", "holding");
            if (thread_call_t cap = thread_call_allocate(&NVRM::bootHoldCap, this)) {
                // The BAR fallback settles for 100 s. A 40 s timer from start() expired before
                // autoGo could run, so its display-arm CAS always failed on that path.
                // Keep the 40 s bring-up budget, measured after the configured settling delay.
                uint64_t dl, settle;
                clock_interval_to_deadline(40000, kMillisecondScale, &dl);
                clock_interval_to_absolutetime_interval(fAutoGoSettleMs, kMillisecondScale, &settle);
                dl += settle;
                retain(); thread_call_enter_delayed(cap, dl);
            } else { releaseBootHold("no cap timer — not holding"); }
            retain();
            if (kernel_thread_start(&NVRM::autoGo, this, &th) == KERN_SUCCESS) {
                thread_deallocate(th);
                setProperty("nvrm-autogo-settle-ms", fAutoGoSettleMs, 32);
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
    registerService();
    return true;
}

extern "C" int sysctlbyname(const char *, void *, size_t *, void *, size_t);
static int nvrmSysW(const char *n, int v) { return sysctlbyname(n, NULL, NULL, &v, sizeof v); }
static int nvrmSysR(const char *n, int *v) { size_t l = sizeof *v; *v = -99; return sysctlbyname(n, v, &l, NULL, 0); }
static unsigned nvrmPop(unsigned m) { unsigned c = 0; for (; m; m &= m - 1) c++; return c; }

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
        if (ok && o < (int)sizeof(out) - 40) o += snprintf(out + o, sizeof(out) - o, "%shead%u %u.%03uHz", o ? " " : "", h, mhz / 1000, mhz % 1000);
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
    if (OSCompareAndSwap(1, 0, &s->fBootHold)) {
        s->adjustBusy(-1); s->setProperty("nvrm-boot-hold", "CAP: settle + 40 s — bring-up not finished, NOT arming");
        LOG("boot hold RELEASED by the settle + 40 s cap — the bring-up will not arm with WindowServer up");
    }
    s->release();
}

void NVRM::autoGo(void *p0, wait_result_t)
{
    NVRM *s = (NVRM *)p0;
    char nb[8] = { 0 };
    // A card whose BAR1 could not be placed outside the boot screen (Resizable BAR off: 256 MB) waits 100 s here with
    // the registry held busy, so the verbose screen showed only "busy timeout ... 'NVRM'" for two minutes and users
    // reset machines that were still starting (7 reports, RTX 2070 SUPER/3050/3060/3070, GTX 1660 SUPER, Quadro T2000,
    // 2026-10-07/08). Say what is happening and how to avoid it, every 15 s, on the same screen.
    if (s->fAutoGoSettleMs > 5000) {
        LOG("starting the NVIDIA card: waiting %u s before it takes over the screen, because its memory window (BAR1) "
            "is too small to move off the boot screen. This is not a freeze. To start in about a second instead, enable "
            "\"Above 4G Decoding\" and \"Resizable BAR\" in the BIOS.", s->fAutoGoSettleMs / 1000);
        for (uint32_t left = s->fAutoGoSettleMs; left > 0;) {
            const uint32_t step = left > 15000 ? 15000 : left;
            IOSleep(step); left -= step;
            if (left) LOG("starting the NVIDIA card: %u s left (not a freeze)", left / 1000);
        }
    } else {
        IOSleep(s->fAutoGoSettleMs);
    }
    s->measureBootRaster();
    { thread_t hw = THREAD_NULL; s->retain();
      if (kernel_thread_start(&NVRM::headWatch, s, &hw) == KERN_SUCCESS) thread_deallocate(hw); else s->release(); }
    IOReturn r = s->go(2);
    kprintf("NVRM-xnu: auto-go: go(2) -> 0x%x (fPassDone %u)\n", r, s->fPassDone);
    if (r != kIOReturnSuccess) {
        s->setProperty("nvrm-autogo", r == kIOReturnBusy ? "busy (a hand-run go is running)" : "go(2) failed");
        s->releaseBootHold("go(2) failed");
    } else {
        s->setProperty("nvrm-autogo", "up");
        int want = 1; if (!PE_parse_boot_argn("nvfbheads", &want, sizeof(want)) || want < 1) want = 1;
        int mask = 0, last = -1, still = 0, i;
        for (i = 0; i < 20; i++) {
            if (nvrmSysR("debug.nvaccel_heads_published", &mask) != 0) mask = 0;
            if ((int)nvrmPop((unsigned)mask) >= want) break;
            still = (mask && mask == last) ? still + 1 : 0; last = mask;
            if (still >= 3) break;
            if (gDisplayNub0) {
                OSNumber *cd = OSDynamicCast(OSNumber, gDisplayNub0->getProperty("connected-displays"));
                if (cd && cd->unsigned32BitValue() > 0 && (int)nvrmPop((unsigned)mask) >= (int)cd->unsigned32BitValue()) break;
            }
            IOSleep(1000);
        }
        LOG("auto-go: heads published mask 0x%x (%u of nvfbheads %d) after %d s", mask, nvrmPop((unsigned)mask), want, i);
        if (!OSCompareAndSwap(1, 2, &s->fBootHold)) {
            LOG("auto-go: the hold was already released (cap) — NOT arming with WindowServer up; heads 0x%x", mask);
            s->setProperty("nvrm-display", "not armed: hold released by the cap first");
            s->release(); thread_terminate(current_thread()); return;
        }
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
        s->releaseBootHold(armed == 1 && ag == 1 && iop == 1 ? "display armed" : "display PARTIAL");
        if (armed == 1 && iop == 1 && !PE_parse_boot_argn("-nvrmnoflip", nb, sizeof(nb))) {
            IOSleep(60000);
            int f = -99; nvrmSysW("debug.nvaccel_iop_flip", 1); nvrmSysR("debug.nvaccel_iop_flip", &f);
            LOG("auto-go: flip debug.nvaccel_iop_flip=%d (60 s after the release)", f);
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
static bool nvrmHostWindow(IOService *dev, UInt64 *outMin, UInt64 *outMax)
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
                    if (tra == 0 && mn >= (4ULL << 30) && mx > mn && (!found || mx - mn > *outMax - *outMin)) {
                        *outMin = mn; *outMax = mx; found = true;
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

bool NVRM::placeLargeBar1()
{
    static const UInt64 kBigBase = 0x10000000000ULL;
    static const UInt64 kHostWindowEnd = 0x3ffbfffffffULL;
    UInt32 cap = 0;
    for (UInt32 off = 0x100, n = 0; off >= 0x100 && off < 0x1000 && n < 64; n++) {
        UInt32 h = fPCI->extendedConfigRead32(off);
        if (h == 0 || h == 0xffffffffU) break;
        if ((h & 0xffff) == 0x15) { cap = off; break; }
        off = h >> 20;
    }
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
    // 10-07: with "Resizable BAR" off in the BIOS (or a card the firmware leaves at 256 MB) the firmware places a small
    // BAR1 over the boot screen and the driver had nothing to move. The card itself can resize: take the largest size it
    // supports between 4 GB and 8 GB, written below once the destination is proven free.
    // 10-07 (RTX 3060, RTX 5080): both cards resized to 16 GB stopped during bring-up (GSP boot,
    // NVAccel start), and the same RTX 3060 runs the desktop at 8 GB. 8 GB is the size proven on hardware (RTX 5060, 3060).
    UInt64 newSize = 0;
    if (bar1Size < (4ULL << 30)) {
        for (int k = 13; k >= 12; k--) if (rbCtlOff && (rbSupported >> k) & 1) { newSize = 1ULL << (20 + k); break; }
        if (!newSize) { LOG("bar1: BAR1 is %llu MB and the card offers no 4-8 GB size — not placing", bar1Size >> 20); return false; }
        LOG("bar1: BAR1 is %llu MB; will resize it to %llu MB", bar1Size >> 20, newSize >> 20);
        bar1Size = newSize;
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
    const UInt64 physTop = physBits >= 63 ? ~0ULL : (1ULL << physBits) - 1;
    if (nvrmHostWindow(fPCI, &wMin, &wMax)) {
        LOG("bar1: host bridge 64-bit window 0x%llx-0x%llx (ACPI _CRS), CPU reaches %u bits", wMin, wMax, physBits);
    } else {
        wMin = kBigBase; wMax = kHostWindowEnd;   // the pre-10-07 placement, kept only where the CPU can reach it
        LOG("bar1: no 64-bit window in the host bridge's _CRS — using 0x%llx-0x%llx (CPU reaches %u bits)", wMin, wMax, physBits);
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
    if (sec != mybus || sub != mybus) { LOG("bar1: root port spans buses %u-%u, not only ours (%u) — not placing", sec, sub, mybus); return false; }
    UInt32 rp24 = rp->configRead32(0x24), rp28 = rp->configRead32(0x28), rp2c = rp->configRead32(0x2c);
    if ((rp24 & 0xf) != 1) { LOG("bar1: root port prefetchable window is not 64-bit (0x%x) — not placing", rp24); return false; }
    LOG("bar1: placing BAR1 %llu MB @0x%llx, BAR3 %llu MB @0x%llx, root port window 0x%llx-0x%llx (was 0x%08x %08x/%08x)",
        bar1Size >> 20, bar1Base, bar3Size >> 20, bar3Base, bar1Base, winEnd, rp24, rp28, rp2c);

    if (nvrmRangeBusy(bar1Base, winEnd, fPCI, rp)) { LOG("bar1: destination is in use — not placing"); return false; }

    UInt16 cmd = fPCI->configRead16(0x04);
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

    const unsigned memCount = mem ? mem->getCount() : 0;
    OSArray *arr = OSArray::withCapacity(memCount + 3);
    IODeviceMemory *m1 = IODeviceMemory::withRange(bar1Base, bar1Size), *m3 = IODeviceMemory::withRange(bar3Base, bar3Size);
    IODeviceMemory *m0 = bar0 ? NULL : IODeviceMemory::withRange(lo10 & ~0xFULL, bar0Size);
    if (!arr || !m1 || !m3 || (!bar0 && !m0)) { LOG("bar1: placed in hardware but the aperture list could not be built (no memory)"); OSSafeReleaseNULL(arr); OSSafeReleaseNULL(m1); OSSafeReleaseNULL(m3); OSSafeReleaseNULL(m0); return false; }
    arr->setObject(bar0 ? bar0 : m0); arr->setObject(m1); arr->setObject(m3);
    OSSafeReleaseNULL(m0);
    for (unsigned i = 0; i < memCount; i++) {
        IOMemoryDescriptor *m = OSDynamicCast(IOMemoryDescriptor, mem->getObject(i));
        if (!m || m == bar0) continue;
        UInt64 pa = m->getPhysicalAddress();
        if (pa == bar3Old || pa == (old18 & ~0xFULL)) { LOG("bar1: dropping stale aperture 0x%llx+0x%llx", pa, (UInt64)m->getLength()); continue; }
        arr->setObject(m);
    }
    fPCI->setDeviceMemory(arr);
    m1->release(); m3->release(); arr->release();
    fPCI->setProperty("nvrm-bar1-placed", bar1Base, 64);
    LOG("bar1: PLACED — BAR1 %llu MB live at 0x%llx", bar1Size >> 20, bar1Base);
    return true;
}

void NVRM::readBARs()
{
    nv_state_t *nv = &fNv;
    char table[256]; unsigned tn = 0;
    unsigned count = fPCI->getDeviceMemoryCount();
    unsigned curNvBar = 0;
    for (unsigned idx = 0; idx < count && curNvBar < NV_GPU_NUM_BARS; idx++) {
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
            if (a == (phys & ~0xFULL)) { off = 0x10 + bar * 4; break; }
            if (is64) bar++;
        }
        if (off == 0 || size == 0) { kprintf("NVRM-xnu: aperture idx %u phys 0x%llx size 0x%llx: %s — skipped\n", idx, phys, size, off == 0 ? "no BAR register matches" : "zero length"); continue; }
        nv->bars[curNvBar].offset      = off;
        nv->bars[curNvBar].cpu_address = phys;
        nv->bars[curNvBar].size        = size;
        tn += snprintf(table + tn, sizeof table - tn, "bar%u@0x%02x:0x%llx+0x%llx ", curNvBar, off, phys, size);
        curNvBar++;
    }
    setProperty("nvrm-bars", table);
    kprintf("NVRM-xnu: BARS %s\n", table);
}

IOReturn NVRM::go(uint32_t pass)
{
    static bool gGoBusy;
    { IOLockLock(gNvClientsLock); bool busy = gGoBusy; if (!busy) gGoBusy = true; IOLockUnlock(gNvClientsLock); if (busy) { kprintf("NVRM-xnu: go(%u) refused: another go is running\n", pass); return kIOReturnBusy; } }
    struct Unbusy { ~Unbusy() { IOLockLock(gNvClientsLock); gGoBusy = false; IOLockUnlock(gNvClientsLock); } } unbusy;
    kprintf("NVRM-xnu: go(%u) begins (fPassDone %d)\n", pass, fPassDone);
    static const char *const kBuildMark = "NVRM build 2026-09-14 19:30 (boot daemon + fb sampler)"; (void)kBuildMark;

    nv_state_t *nv = &fNv;
    LOG("go(pass %u) — done so far: %u", pass, fPassDone);
    LOG("anchor: os_mem_set runtime 0x%llx (slide = runtime - its nm link address)", (unsigned long long)(uintptr_t)&os_mem_set);
    if (pass >= 1 && fPassDone < 1) {
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
            LOG("registry: RmMsg=\":\" ResmanDebugLevel=0 (verbose RM)");
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
        if (nvkms_xnu_load()) nvrm_display_publish(this, fNv.gpu_id);
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
    if (fAdapter) { rm_shutdown_adapter(NULL, &fNv); fAdapter = false; }
    if (fIntSrc) { fIntSrc->disable(); if (fWorkLoop) fWorkLoop->removeEventSource(fIntSrc); fIntSrc->release(); fIntSrc = NULL; }
    if (fWorkLoop) { fWorkLoop->release(); fWorkLoop = NULL; }
    if (fPrivateState) { rm_free_private_state(NULL, &fNv); fPrivateState = false; }
    super::stop(provider);
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
    fIsCtl = (type == 0);
    nvu_memset(&fNvfp, 0, sizeof fNvfp);
    fNvfp.ctl_nvfp_priv = this;
    if (!gNvClientsLock) gNvClientsLock = IOLockAlloc();
    IOLockLock(gNvClientsLock);
    for (int i = 0; i < NV_XNU_MAX_CLIENTS; i++) if (!gNvClients[i]) { gNvClients[i] = this; fFd = i; break; }
    IOLockUnlock(gNvClientsLock);
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
    if (!fCleaned) {
        fCleaned = true;
        if (gNvClientsLock && fFd >= 0) { IOLockLock(gNvClientsLock); gNvClients[fFd] = NULL; nv_pm_close(fPm); IOLockUnlock(gNvClientsLock); }
        if (fKms) { nvkms_xnu_close(fKms); fKms = nullptr; }
        if (fOwner) rm_cleanup_file_private(NULL, nvState(), &fNvfp);
        if (fMmap.memArea.pRanges) { kern_os_free(fMmap.memArea.pRanges); fMmap.memArea.pRanges = NULL; }
        if (nv_xnu_chat()) kprintf("NVRM-xnu: client fd %d closed\n", fFd);
    }
    terminate();
    return kIOReturnSuccess;
}
IOReturn NVRMUserClient::clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory)
{
    nv_alloc_mapping_context_t *c = &fMmap;
    if (!c->valid) { kprintf("NVRM-xnu: map on fd %d with no mapping context\n", fFd); return kIOReturnBadArgument; }
    IOMemoryDescriptor *md = NULL;
    NvU32 caching = c->caching;
    if (fIsCtl) {
        NvU64 bytes = 0; NvU32 cache = 0;
        IOMemoryDescriptor *base = nv_xnu_alloc_md(c->alloc, &bytes, &cache);
        NvU64 off = c->page_index * PAGE_SIZE;
        if (!base) return kIOReturnBadArgument;
        if (off >= bytes) { base->release(); return kIOReturnBadArgument; }
        NvU64 len = c->access_size ? c->access_size : (bytes - off);
        if (off + len > bytes) len = bytes - off;
        md = IOSubMemoryDescriptor::withSubRange(base, (IOByteCount)off, (IOByteCount)len, kIODirectionInOut);
        base->release();
        caching = cache;
        if (nv_xnu_chat()) kprintf("NVRM-xnu: map fd %d: sysmem +0x%llx len 0x%llx cache %u\n", fFd, off, len, caching);
    } else {
        NvU64 n = c->memArea.numRanges;
        if (n == 1 && !nv_pm_is_gpu(c->memArea.pRanges[0].start, c->memArea.pRanges[0].size)) {
            md = nv_pm_ram_submd(c->memArea.pRanges[0].start, c->memArea.pRanges[0].size);
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
    if (caching == NV_MEMORY_UNCACHED) *options |= kIOMapInhibitCache;
    else if (caching == NV_MEMORY_WRITECOMBINED) *options |= kIOMapWriteCombineCache;
    if (!(c->prot & NV_PROTECT_WRITEABLE)) *options |= kIOMapReadOnly;
    *memory = md;
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
        { sKms,   1, kIOUCVariableStructureSize, 0, kIOUCVariableStructureSize },
        { sKmsEvents, 0, 0, 1, 0 },
        { sWaitEvent, 2, 0, 1, 0 },
    };
    if (selector >= sizeof tbl / sizeof tbl[0]) return kIOReturnBadArgument;
    IOExternalMethodDispatch d = tbl[selector];
    return super::externalMethod(selector, args, &d, this, reference);
}
extern "C" nv_state_t *nv_xnu_gpu_state(void) { return gNVRMService ? &gNVRMService->fNv : NULL; }
extern "C" void *nv_xnu_gpu_os_device(void) { return gNVRMService; }

IOReturn NVRMUserClient::sKms(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me || !me->fOwner) return kIOReturnNotAttached;
    uint32_t cmd = (uint32_t)args->scalarInput[0];
    uint32_t size = args->structureInputDescriptor ? (uint32_t)args->structureInputDescriptor->getLength() : args->structureInputSize;
    if (size == 0 || size > (1u << 20)) return kIOReturnBadArgument;
    if (!me->fKms) { me->fKms = nvkms_xnu_open(me->fPid); if (!me->fKms) { kprintf("NVRM-xnu: nvkms open failed (loaded? pass 2 done?)\n"); return kIOReturnNotReady; } }
    void *buf = kern_os_malloc(size);
    if (!buf) return kIOReturnNoMemory;
    if (args->structureInputDescriptor) {
        if (args->structureInputDescriptor->prepare() != kIOReturnSuccess) { kern_os_free(buf); return kIOReturnVMError; }
        args->structureInputDescriptor->readBytes(0, buf, size);
        args->structureInputDescriptor->complete();
    } else nvu_memcpy(buf, args->structureInput, size);
    int rc = nvkms_xnu_ioctl(me->fKms, cmd, buf, size);
    if (rc) kprintf("NVRM-xnu: nvkms ioctl cmd %u size %u -> FALSE\n", cmd, size);
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
    return rc == 0 ? kIOReturnSuccess : kIOReturnError;
}
IOReturn NVRMUserClient::sKmsEvents(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    args->scalarOutput[0] = nvkms_xnu_events_available(me->fKms);
    return kIOReturnSuccess;
}
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
static volatile SInt64 gNvEvtPosts = 0, gNvEvtMatched = 0, gNvEvtWaits = 0, gNvEvtSlept = 0;
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
        assert_wait_timeout((event_t)&me->fEvtSeq, THREAD_ABORTSAFE, (uint32_t)us, NSEC_PER_USEC);
        if (me->fEvtSeq != seen) thread_wakeup((event_t)&me->fEvtSeq);
        OSIncrementAtomic64(&gNvEvtSlept);
        (void)thread_block(THREAD_CONTINUE_NULL);
    }
    args->scalarOutput[0] = me->fEvtSeq;
    return kIOReturnSuccess;
}
IOReturn NVRMUserClient::sBindFd(OSObject *target, void *reference, IOExternalMethodArguments *args)
{
    NVRMUserClient *me = OSDynamicCast(NVRMUserClient, target);
    if (!me) return kIOReturnNotAttached;
    me->fBound = (int)args->scalarInput[0];
    me->fEverBound = true;
    if (me->fBound >= 0 && me->fMmap.valid) {
        kprintf("NVRM-xnu: client %d re-bound to fd %d with a stale mapping context - cleared\n", me->fFd, me->fBound);
        nvMmapContextClear(&me->fMmap);
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
    if (!fOwner || fOwner->fPassDone < 2) { static bool once; if (!once) { once = true; kprintf("NVRM-xnu: esc 0x%x refused: adapter not up (run nvrmctl go 2)\n", cmd); } return kIOReturnNotReady; }
    nv_state_t *nv = nvState();
    switch (cmd) {
    case NV_ESC_CARD_INFO: {
        if (!fIsCtl) return kIOReturnBadArgument;
        nvu_memset(data, 0, size);
        if (size < sizeof(nv_ioctl_card_info_t)) return kIOReturnBadArgument;
        nv_state_t *g = &fOwner->fNv;
        nv_ioctl_card_info_t *ci = (nv_ioctl_card_info_t *)data;
        ci->valid              = NV_TRUE;
        ci->pci_info           = g->pci_info;
        ci->gpu_id             = g->gpu_id;
        ci->interrupt_line     = (NvU16)g->interrupt_line;
        ci->reg_address        = g->bars[NV_GPU_BAR_INDEX_REGS].cpu_address;
        ci->reg_size           = g->bars[NV_GPU_BAR_INDEX_REGS].size;
        ci->fb_address         = g->bars[NV_GPU_BAR_INDEX_FB].cpu_address;
        ci->fb_size            = g->bars[NV_GPU_BAR_INDEX_FB].size;
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
    return me->fOwner->go((uint32_t)args->scalarInput[0]);
}

extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }

/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include "../nvrm_gpu_accounting_abi.h"
#include "iofam/IOAccelSubmitter2.h"

extern "C" {
int proc_selfpid(void);
task_t current_task(void);
void task_reference(task_t);
void task_deallocate(task_t);
// Call the exported family lifecycle only for this client. Declaring these
// overrides in the reconstructed common header would also change existing
// rendering subclasses' inherited vtable entries.
bool nv_accounting_submitter_start(IOAccelSubmitter2 *, IOService *)
    __asm__("__ZN17IOAccelSubmitter25startEP9IOService");
void nv_accounting_submitter_stop(IOAccelSubmitter2 *, IOService *)
    __asm__("__ZN17IOAccelSubmitter24stopEP9IOService");
}

/* A real IOAccelSubmitter2 makes the statistics discoverable by macOS's
 * per-process GPU scanner. Use its normal start/stop and public OSNumber
 * properties, never private object offsets or a fabricated registry class.
 * This client has no hardware channels, memory mappings or submission API. */
static volatile SInt32 gNvAccountingClients;
class NVGPUAccountingClient : public IOAccelSubmitter2 {
    OSDeclareDefaultStructors(NVGPUAccountingClient)
    task_t fTask = nullptr;
    int fPid = 0;
    IOLock *fLock = nullptr;
    OSNumber *fBusy = nullptr, *fLastSubmit = nullptr;
    uint64_t fOpened = 0;
    nv_gpu_accounting_counter fCounter = {};
    bool fCounted = false, fClosed = false;
public:
    bool initWithTask(task_t task, void *security, UInt32 type, OSDictionary *properties) override {
        if (!task || type != NV_GPU_ACCOUNTING_CLIENT || task != current_task() ||
            !IOUserClient::initWithTask(task, security, type, properties)) return false;
        // Bound registry/memory growth independently of RM's connection limits.
        const SInt32 previous = OSIncrementAtomic(&gNvAccountingClients);
        if (previous >= 256) { OSDecrementAtomic(&gNvAccountingClients); return false; }
        fCounted = true;
        fLock = IOLockAlloc();
        if (!fLock) return false;
        task_reference(task); fTask = task; fPid = proc_selfpid();
        fOpened = mach_absolute_time();
        return true;
    }
    bool start(IOService *provider) override {
        if (!nv_accounting_submitter_start(this, provider)) return false;
        fBusy = OSDynamicCast(OSNumber, getProperty("accumulatedGPUTime"));
        fLastSubmit = OSDynamicCast(OSNumber, getProperty("lastSubmittedTime"));
        if (!fBusy || !fLastSubmit) {
            fBusy = fLastSubmit = nullptr;
            nv_accounting_submitter_stop(this, provider);
            return false;
        }
        fBusy->retain(); fLastSubmit->retain();
        setProperty("API", "Metal");
        setProperty("NVGPUAccountingVersion", (unsigned long long)NV_GPU_ACCOUNTING_VERSION, 32);
        return true;
    }
    void stop(IOService *provider) override {
        IOLockLock(fLock); fClosed = true; IOLockUnlock(fLock);
        nv_accounting_submitter_stop(this, provider);
    }
    IOReturn clientClose() override {
        IOLockLock(fLock); fClosed = true; IOLockUnlock(fLock);
        terminate();
        return kIOReturnSuccess;
    }
    void free() override {
        OSSafeReleaseNULL(fBusy); OSSafeReleaseNULL(fLastSubmit);
        if (fLock) { IOLockFree(fLock); fLock = nullptr; }
        if (fTask) { task_deallocate(fTask); fTask = nullptr; }
        if (fCounted) { OSDecrementAtomic(&gNvAccountingClients); fCounted = false; }
        IOUserClient::free();
    }
    IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args,
        IOExternalMethodDispatch *, OSObject *, void *) override {
        if (current_task() != fTask) return kIOReturnNotPermitted;
        if (selector != NV_GPU_ACCOUNTING_REPORT) return kIOReturnUnsupported;
        if (!args || args->scalarInputCount || args->scalarOutputCount ||
            args->structureInputSize != sizeof(nv_gpu_accounting_report) ||
            !args->structureInput || args->structureOutputSize ||
            args->structureInputDescriptor || args->structureOutputDescriptor)
            return kIOReturnBadArgument;
        nv_gpu_accounting_report report;
        memcpy(&report, args->structureInput, sizeof report);
        IOLockLock(fLock);
        IOReturn result = kIOReturnNotAttached;
        if (!fClosed && !isInactive()) {
            const uint64_t now = mach_absolute_time();
            uint64_t elapsed = 0;
            if (now >= fOpened) absolutetime_to_nanoseconds(now - fOpened, &elapsed);
            result = nv_gpu_accounting_accept(&fCounter, &report, fOpened, now, elapsed)
                ? kIOReturnSuccess : kIOReturnBadArgument;
            if (result == kIOReturnSuccess) {
                fBusy->setValue(fCounter.busy_ns);
                fLastSubmit->setValue(fCounter.last_submit_absolute);
            }
        }
        IOLockUnlock(fLock);
        return result;
    }
    void *getOwningTask() const override { return fTask; }
    void *getGPUTask() const override { return nullptr; }
    int getOwningTaskPid() const override { return fPid; }
    void affectedByHardwareReset(bool, unsigned int) override {}
    bool shouldHardwareCommandNooped() override { return true; }
    // These defaults are not exported by Tahoe; no hardware command can reach
    // this client, so use the same no-op behavior locally on both OS variants.
    void retireCommandBuffer(IOAccelEventFence *) override {}
    void setSubmissionError(unsigned int) override {}
    bool isOpportunisticWorkload() const override { return false; }
};
OSDefineMetaClassAndStructors(NVGPUAccountingClient, IOAccelSubmitter2)

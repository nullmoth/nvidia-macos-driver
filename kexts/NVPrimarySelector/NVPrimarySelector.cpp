// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// Local experiment. Select the firmware console's GPU without initializing RM.
#include <IOKit/IOService.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <mach/kmod.h>
#include "nvrm_gpu_select.h"

extern "C" kern_return_t _start(kmod_info_t *,void *) {return KERN_SUCCESS;}
extern "C" kern_return_t _stop(kmod_info_t *,void *) {return KERN_SUCCESS;}
extern "C" {KMOD_EXPLICIT_DECL(org.hackintosh.experimental.NVPrimarySelector,"0.1.0",_start,_stop)}

class NVPrimarySelector: public IOService {
    OSDeclareDefaultStructors(NVPrimarySelector)
    IOWorkLoop *loop=nullptr;
    IOTimerEventSource *timer=nullptr;
    unsigned attempts=0;
    static void tick(OSObject *owner,IOTimerEventSource *) {
        NVPrimarySelector *self=OSDynamicCast(NVPrimarySelector,owner);
        if(self)self->scan();
    }
    void scan() {
        ++attempts;setProperty("selection-attempts",attempts,32);
        PE_Video v;bzero(&v,sizeof v);
        IOPlatformExpert *platform=getPlatform();
        UInt64 console=0,bytes=0;
        if(platform&&platform->getConsoleInfo(&v)==kIOReturnSuccess&&v.v_width&&v.v_height&&
           v.v_rowBytes&&v.v_depth&&v.v_rowBytes<=~0ULL/v.v_height) {
            console=static_cast<UInt64>(v.v_baseAddr)&~3ULL;
            bytes=static_cast<UInt64>(v.v_rowBytes)*v.v_height;
        }
        setProperty("console-address",console,64);setProperty("console-bytes",bytes,64);
        IOPCIDevice *winner=nullptr;unsigned count=0,owners=0;
        IORegistryIterator *it=IORegistryIterator::iterateOver(gIOServicePlane,kIORegistryIterateRecursively);
        if(it) {
            while(IORegistryEntry *entry=it->getNextObject()) {
                IOPCIDevice *pci=OSDynamicCast(IOPCIDevice,entry);
                if(!NvPrimaryGpu::displayDevice(pci))continue;
                ++count;
                if(NvPrimaryGpu::ownsConsole(pci,console,bytes)) {
                    ++owners;if(winner)winner->release();winner=pci;winner->retain();
                }
            }
        }
        bool stable=it&&it->isValid();if(it)it->release();
        setProperty("nvidia-display-devices",count,32);setProperty("console-owners",owners,32);
        setProperty("registry-traversal-stable",stable);
        if(!stable||owners!=1||!winner) {
            if(winner)winner->release();
            setProperty("selection-status",attempts<60?"waiting for unique console owner":"no unique console owner; no driver enabled");
            if(attempts<60)timer->setTimeoutMS(500);
            return;
        }
        // Freeze the result before the GPU driver can relocate BAR1.
        setProperty("selected-registry-id",winner->getRegistryEntryID(),64);
        UInt32 bdf=(static_cast<UInt32>(winner->getBusNumber())<<8)|
                   (static_cast<UInt32>(winner->getDeviceNumber())<<3)|winner->getFunctionNumber();
        setProperty("selected-bdf",bdf,32);
        if(OSObject *p=winner->getProperty("pcidebug"))setProperty("selected-pcidebug",p);
        if(OSObject *p=winner->getProperty("acpi-path"))setProperty("selected-acpi-path",p);
        int activate=0;
        bool arm=PE_parse_boot_argn("nvselector",&activate,sizeof activate)&&activate==1;
        setProperty("activation-enabled",arm);
        setProperty("selection-status",arm?"selected; matching requested":"selected; read-only diagnostic");
        IOLog("NVPrimarySelector: %02x:%02x.%u uniquely owns console 0x%llx+0x%llx; activation=%d\n",
              winner->getBusNumber(),winner->getDeviceNumber(),winner->getFunctionNumber(),console,bytes,arm);
        // Do not open PCI, map MMIO, change BARs or initialize GPUs. Only the
        // selected provider gets this marker. Both NVIDIA personalities require it.
        if(arm) {
            winner->setProperty("nullmoth-auto-primary",kOSBooleanTrue);
            winner->registerService(kIOServiceAsynchronous);
        }
        winner->release();
    }
public:
    bool init(OSDictionary *properties=nullptr) override {
        loop=nullptr;timer=nullptr;attempts=0;
        return IOService::init(properties);
    }
    bool start(IOService *provider) override {
        if(!IOService::start(provider))return false;
        // Matching IOResources gives one coordinator for the entire PCI tree.
        setProperty("selector-version","0.1.0");
        loop=IOWorkLoop::workLoop();
        timer=IOTimerEventSource::timerEventSource(this,tick);
        if(!loop||!timer||loop->addEventSource(timer)!=kIOReturnSuccess)return false;
        registerService();timer->setTimeoutMS(500);return true;
    }
    void stop(IOService *provider) override {
        if(timer){timer->cancelTimeout();if(loop)loop->removeEventSource(timer);timer->release();timer=nullptr;}
        if(loop){loop->release();loop=nullptr;}
        IOService::stop(provider);
    }
    void free() override {
        if(timer){timer->cancelTimeout();if(loop)loop->removeEventSource(timer);timer->release();}
        if(loop)loop->release();IOService::free();
    }
};
OSDefineMetaClassAndStructors(NVPrimarySelector,IOService)

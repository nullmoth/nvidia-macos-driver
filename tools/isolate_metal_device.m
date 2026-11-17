// Test-process-only device routing. Does not modify Metal or the installed bundle.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#include <dlfcn.h>
static id<MTLDevice> testDevice(void) {
    static id<MTLDevice> device; static dispatch_once_t once;
    dispatch_once(&once, ^{
        // Reproduce libraries loaded by the application before the driver. In particular, Apple's GL renderer loads
        // GPUCompiler's LLVM first; loading our device first otherwise hides LLVM weak-symbol collisions in tests.
        const char *preload = getenv("ISOLATED_PRELOAD_LIBRARY");
        if (preload) {
            if (!dlopen(preload, RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr,"ISOLATED_PRELOAD_FAILED: %s\n",dlerror()); return; }
            fprintf(stderr,"ISOLATED_PRELOAD: %s\n",preload);
        }
        const char *path = getenv("ISOLATED_NVMTL_PLUGIN");
        if (!path || !dlopen(path, RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr,"ISOLATED_DEVICE_LOAD_FAILED: %s\n",dlerror()); return; }
        io_service_t svc = IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel"));
        if (!svc) return;
        device = ((id(*)(id,SEL,mach_port_t))objc_msgSend)([NSClassFromString(@"NVMTLDevice") alloc],NSSelectorFromString(@"initWithAcceleratorPort:"),svc);
        IOObjectRelease(svc);
        fprintf(stderr,"ISOLATED_DEVICE: %s %s\n",path,device.name.UTF8String);
    });
    return device;
}
static id<MTLDevice> isolatedDefault(void) { return testDevice(); }
static NSArray<id<MTLDevice>> *isolatedAll(void) NS_RETURNS_RETAINED { id d=testDevice(); return d?@[d]:@[]; }
static NSArray<id<MTLDevice>> *isolatedObserved(id __autoreleasing *observer,MTLDeviceNotificationHandler handler) NS_RETURNS_RETAINED { if(observer)*observer=nil; return isolatedAll(); }
static void isolatedRemove(id observer) {}
__attribute__((used,section("__DATA,__interpose"))) static struct {const void *replacement,*original;} replacements[]={
 {(void*)isolatedDefault,(void*)MTLCreateSystemDefaultDevice},
 {(void*)isolatedAll,(void*)MTLCopyAllDevices},
 {(void*)isolatedObserved,(void*)MTLCopyAllDevicesWithObserver},
 {(void*)isolatedRemove,(void*)MTLRemoveDeviceObserver}
};

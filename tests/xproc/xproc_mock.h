// Stand-in for the plugin's NVMTLSharedEvent so NVMTLSharedEventXProc.m can be exercised on any Mac.
// The class bodies mirror NVMTLObjects.m; everything else is the plugin file itself.
#import <Metal/Metal.h>
#import <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdarg.h>
#include <time.h>

static _Atomic int gMockPumps;
static void nvlog(const char *fmt, ...) {
    if (!getenv("XPROC_LOG")) return;
    va_list a; va_start(a, fmt); fputs("nvlog: ", stderr); vfprintf(stderr, fmt, a); fputc('\n', stderr); va_end(a);
}
static void nvmtl_pump_buffers(void) { atomic_fetch_add(&gMockPumps, 1); }

@interface NVMTLSharedEvent : NSObject <MTLSharedEvent> { @public id<MTLDevice> _dev; NSString *_label; uint64_t _value; NSCondition *_condition; NSMutableArray *_notifications; void *_xp; } @end

static uint64_t nvmtl_sev_read(NVMTLSharedEvent *e);
static void nvmtl_sev_signal(NVMTLSharedEvent *e, uint64_t value);
static void nvmtl_sev_notify(NVMTLSharedEvent *e, MTLSharedEventListener *l, uint64_t wanted, MTLSharedEventNotificationBlock b);
static BOOL nvmtl_sev_wait(NVMTLSharedEvent *e, uint64_t wanted, uint64_t ms);
static MTLSharedEventHandle *nvmtl_sev_handle(NVMTLSharedEvent *e);
static void nvmtl_sev_teardown(NVMTLSharedEvent *e);
static id nvmtl_sev_import(id<MTLDevice> dev, MTLSharedEventHandle *h);

@implementation NVMTLSharedEvent
- (instancetype)init { if ((self = [super init])) { _condition = [NSCondition new]; _notifications = [NSMutableArray new]; } return self; }
- (void)dealloc { nvmtl_sev_teardown(self); }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (uint64_t)signaledValue { return nvmtl_sev_read(self); }
- (void)setSignaledValue:(uint64_t)v { nvmtl_sev_signal(self, v); }
- (MTLSharedEventHandle *)newSharedEventHandle { return nvmtl_sev_handle(self); }
- (void)notifyListener:(MTLSharedEventListener *)l atValue:(uint64_t)v block:(MTLSharedEventNotificationBlock)b { nvmtl_sev_notify(self, l, v, b); }
- (BOOL)waitUntilSignaledValue:(uint64_t)v timeoutMS:(uint64_t)ms { return nvmtl_sev_wait(self, v, ms); }
@end

#include "../../plugin/NVMTLSharedEventXProc.m"

static id<MTLSharedEvent> mock_new_event(void) { return [NVMTLSharedEvent new]; }
static id<MTLSharedEvent> mock_event_from_handle(MTLSharedEventHandle *h) { return nvmtl_sev_import(nil, h); }

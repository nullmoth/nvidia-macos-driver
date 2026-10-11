// NullMoth Boot Progress: the boot progress screen macOS shows between the kernel's boot screen and the login window,
// drawn with the NullMoth logo.
//
// macOS draws that screen with Installer Progress.app (launchd job com.apple.InstallerProgress): loginwindow asks it over
// XPC to show the screen, report progress and hide it again, and it always adds the Apple logo, whatever the boot screen
// held before. This program answers the same XPC service with the same protocol (read from Installer Progress and
// libIASUnifiedProgress on macOS 15.8.1), takes over the window server's boot windows the same way, and draws the
// NullMoth logo instead. Setup installs it as com.nullmoth.bootprogress, owning the com.apple.InstallerProgress service,
// and disables Apple's job; before a macOS update the update switcher gives the service back to Apple's app, whose
// screen also shows the installation phases.
#import <AppKit/AppKit.h>
#import <dlfcn.h>
#import <sys/sysctl.h>
#import <servers/bootstrap.h>

static void note(NSString *fmt, ...) {
    va_list ap; va_start(ap, fmt);
    NSString *s = [[NSString alloc] initWithFormat:fmt arguments:ap];
    va_end(ap);
    static NSFileHandle *fh;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        NSString *p = @"/Library/NullMoth/bootprogress.log";
        NSDictionary *a = [[NSFileManager defaultManager] attributesOfItemAtPath:p error:nil];
        if ([a fileSize] > (512 << 10)) [[NSFileManager defaultManager] removeItemAtPath:p error:nil];   // bounded
        if (![[NSFileManager defaultManager] fileExistsAtPath:p]) [[NSData data] writeToFile:p atomically:NO];
        fh = [NSFileHandle fileHandleForWritingAtPath:p];
        [fh seekToEndOfFile];
    });
    NSString *line = [NSString stringWithFormat:@"%@ %@\n", [NSDate date], s];
    [fh writeData:[line dataUsingEncoding:NSUTF8StringEncoding]];
}

// The protocol loginwindow's libIASUnifiedProgress speaks (selectors and block types from both binaries).
@protocol IASUnifiedProgressProtocol
- (void)getVersion:(void (^)(long))reply;
- (void)currentProgress:(void (^)(float))reply;
- (void)addPhase:(id)phase;
- (void)clearPhases;
- (void)hasPhases:(void (^)(BOOL))reply;
- (void)showProgressUI:(id)info;
- (void)hideProgressUI;
- (void)isShowingUI:(void (^)(BOOL))reply;
- (void)setPhaseProgress:(id)info;
- (void)setPhaseStatus:(id)info;
- (void)setPhaseAlternateStatus:(id)info;
- (void)setPhaseDone:(id)info;
- (void)setBatteryInfo:(id)info;
- (void)setProgressIndicatorInfo:(id)info;
- (void)registerClient:(id)client;
- (void)registerClient:(id)client completionHandler:(void (^)(void))done;
- (void)unregisterClient:(id)client;
@end

// Objects the client may send by class name. Decoded and ignored: this screen shows no installation phases.
@interface IASPPhase : NSObject <NSSecureCoding> @end
@implementation IASPPhase
+ (BOOL)supportsSecureCoding { return YES; }
- (instancetype)initWithCoder:(NSCoder *)c { return [super init]; }
- (void)encodeWithCoder:(NSCoder *)c {}
@end
@interface IASUPCaller : NSObject <NSSecureCoding> @end
@implementation IASUPCaller
+ (BOOL)supportsSecureCoding { return YES; }
- (instancetype)initWithCoder:(NSCoder *)c { return [super init]; }
- (void)encodeWithCoder:(NSCoder *)c {}
@end

// Window server calls Installer Progress makes, with the signatures its call sites use.
typedef int CGSConnectionID;
static CGSConnectionID (*pCGSMainConnectionID)(void);
static CFArrayRef (*pCGSCreateCinematicBootWindowArray)(CGSConnectionID);
static void (*pCGSInitialEnableOfDisplayUpdates)(void);
static bool (*pSLSInitialConfigurationWasDeferred)(void);
// The unmanaged space Installer Progress shows its windows in (call sites read from its x86_64 code, macOS 15.8.1):
// space = CGSSpaceCreate(cid, 1, {uuid: "Boot Progress Space", pid: [pid], type: 2}); CGSSpaceSetAbsoluteLevel(cid, space,
// kSLSSpaceAbsoluteLevelBootProgress); CGSSetWindowLevel(cid, wid, 0x3e2) on each boot window; CGSAddWindowsToSpaces(cid,
// [wid...], [space]); CGSShowSpaces(cid, [space]). 10-11: without it our window existed and was ordered front, yet the
// screen showed black, then the login wallpaper - a window outside that space is not composited before login.
static uint64_t (*pCGSSpaceCreate)(CGSConnectionID, int, CFDictionaryRef);
static int (*pCGSSpaceSetAbsoluteLevel)(CGSConnectionID, uint64_t, int);
static int (*pCGSAddWindowsToSpaces)(CGSConnectionID, CFArrayRef, CFArrayRef);
static int (*pCGSShowSpaces)(CGSConnectionID, CFArrayRef);
static int (*pCGSSetWindowLevel)(CGSConnectionID, uint32_t, int);
static void (*pCGSHideSpaces)(CGSConnectionID, CFArrayRef);
static void (*pCGSRemoveWindowsFromSpaces)(CGSConnectionID, CFArrayRef, CFArrayRef);
static void (*pCGSSpaceDestroy)(CGSConnectionID, uint64_t);
static const int *pBootProgressLevel;
static CFStringRef *pWindowIDKey;
static void (*pSLSPerformDeferredConfiguration)(void);
static void loadSkyLight(void) {
    void *h = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY);
    pCGSMainConnectionID = dlsym(h, "CGSMainConnectionID");
    pCGSCreateCinematicBootWindowArray = dlsym(h, "CGSCreateCinematicBootWindowArray");
    pCGSInitialEnableOfDisplayUpdates = dlsym(h, "CGSInitialEnableOfDisplayUpdates");
    pSLSInitialConfigurationWasDeferred = dlsym(h, "SLSInitialConfigurationWasDeferred");
    pSLSPerformDeferredConfiguration = dlsym(h, "SLSPerformDeferredConfiguration");
    pCGSSpaceCreate = dlsym(h, "CGSSpaceCreate"); pCGSSpaceSetAbsoluteLevel = dlsym(h, "CGSSpaceSetAbsoluteLevel");
    pCGSAddWindowsToSpaces = dlsym(h, "CGSAddWindowsToSpaces"); pCGSShowSpaces = dlsym(h, "CGSShowSpaces");
    pCGSSetWindowLevel = dlsym(h, "CGSSetWindowLevel");
    pCGSHideSpaces = dlsym(h, "CGSHideSpaces"); pCGSRemoveWindowsFromSpaces = dlsym(h, "CGSRemoveWindowsFromSpaces");
    pCGSSpaceDestroy = dlsym(h, "CGSSpaceDestroy");
    pBootProgressLevel = dlsym(h, "kSLSSpaceAbsoluteLevelBootProgress"); pWindowIDKey = dlsym(h, "kCGSWindowIDKey");
    note(@"space calls: create %p level %p add %p show %p winlevel %p bootlevel %p (=%d) widkey %p", pCGSSpaceCreate,
         pCGSSpaceSetAbsoluteLevel, pCGSAddWindowsToSpaces, pCGSShowSpaces, pCGSSetWindowLevel, pBootProgressLevel,
         pBootProgressLevel ? *pBootProgressLevel : -1, pWindowIDKey);
    note(@"SkyLight %p: main %p bootwindows %p enable %p deferred %p perform %p", h, pCGSMainConnectionID,
         pCGSCreateCinematicBootWindowArray, pCGSInitialEnableOfDisplayUpdates, pSLSInitialConfigurationWasDeferred,
         pSLSPerformDeferredConfiguration);
}

// The logo: the same artwork the NullMoth OpenCore driver draws in place of boot.efi's Apple logo, RGBA, straight alpha.
static NSImage *mothImage(void) {
    NSString *res = [[NSBundle mainBundle] resourcePath];
    NSImage *img = [[NSImage alloc] initWithSize:NSMakeSize(144, 180)];
    for (NSArray *v in @[@[@"logo1x.rgba", @144, @180], @[@"logo2x.rgba", @288, @360]]) {
        NSData *d = [NSData dataWithContentsOfFile:[res stringByAppendingPathComponent:v[0]]];
        NSInteger w = [v[1] integerValue], h = [v[2] integerValue];
        if (d.length != (NSUInteger)(w * h * 4)) { note(@"logo %@: %lu bytes, expected %ld", v[0], d.length, (long)(w * h * 4)); continue; }
        NSBitmapImageRep *r = [[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL pixelsWide:w pixelsHigh:h bitsPerSample:8
                               samplesPerPixel:4 hasAlpha:YES isPlanar:NO colorSpaceName:NSDeviceRGBColorSpace
                               bitmapFormat:NSBitmapFormatAlphaNonpremultiplied bytesPerRow:w * 4 bitsPerPixel:32];
        memcpy(r.bitmapData, d.bytes, d.length);
        r.size = NSMakeSize(144, 180);
        [img addRepresentation:r];
    }
    return img;
}

@interface ProgressView : NSView
@property double progress;     // 0..1, < 0 = no bar
@property (strong) NSImage *logo;
@end
@implementation ProgressView
- (void)drawRect:(NSRect)dirty {
    [[NSColor blackColor] setFill]; NSRectFill(self.bounds);
    NSSize ls = self.logo.size;
    NSRect lr = NSMakeRect(NSMidX(self.bounds) - ls.width / 2, NSMidY(self.bounds) - ls.height / 2, ls.width, ls.height);
    [self.logo drawInRect:lr fromRect:NSZeroRect operation:NSCompositingOperationSourceOver fraction:1.0];
    if (self.progress < 0) return;
    // the bar sits where Apple's does: centred, 200 pt wide, 6 pt tall, about 1.5 logo heights below the centre
    NSRect track = NSMakeRect(NSMidX(self.bounds) - 100, NSMidY(self.bounds) - ls.height / 2 - 70, 200, 6);
    [[NSColor colorWithWhite:0.25 alpha:1] setFill];
    [[NSBezierPath bezierPathWithRoundedRect:track xRadius:3 yRadius:3] fill];
    NSRect fill = track; fill.size.width = MAX(6, track.size.width * MIN(1, self.progress));
    [[NSColor whiteColor] setFill];
    [[NSBezierPath bezierPathWithRoundedRect:fill xRadius:3 yRadius:3] fill];
}
@end

@interface Server : NSObject <NSXPCListenerDelegate, IASUnifiedProgressProtocol>
@property (strong) NSMutableArray<NSWindow *> *windows;
@property (strong) NSImage *logo;
@property BOOL showing, deferredConfig;
@property unsigned screenWaits;
@property double progress;
@property CFArrayRef bootWindows;
@property uint64_t space;
@property (strong) NSMutableArray<NSNumber *> *spaceWids;
@end

@implementation Server
- (BOOL)listener:(NSXPCListener *)l shouldAcceptNewConnection:(NSXPCConnection *)c {
    NSXPCInterface *i = [NSXPCInterface interfaceWithProtocol:@protocol(IASUnifiedProgressProtocol)];
    NSSet *plist = [NSSet setWithObjects:[NSDictionary class], [NSArray class], [NSString class], [NSNumber class],
                    [NSData class], [NSDate class], [NSNull class], [IASPPhase class], [IASUPCaller class], nil];
    for (NSString *sel in @[@"addPhase:", @"showProgressUI:", @"setPhaseProgress:", @"setPhaseStatus:",
                            @"setPhaseAlternateStatus:", @"setPhaseDone:", @"setBatteryInfo:", @"setProgressIndicatorInfo:",
                            @"registerClient:", @"registerClient:completionHandler:", @"unregisterClient:"])
        [i setClasses:plist forSelector:NSSelectorFromString(sel) argumentIndex:0 ofReply:NO];
    c.exportedInterface = i; c.exportedObject = self;
    [c resume];
    note(@"client connected: pid %d", c.processIdentifier);
    return YES;
}
- (void)onMain:(dispatch_block_t)b { dispatch_async(dispatch_get_main_queue(), b); }
- (void)getVersion:(void (^)(long))reply { note(@"getVersion"); reply(2); }
- (void)currentProgress:(void (^)(float))reply { reply((float)self.progress); }
- (void)addPhase:(id)phase { note(@"addPhase %@", phase); }
- (void)clearPhases { note(@"clearPhases"); }
- (void)hasPhases:(void (^)(BOOL))reply { reply(NO); }
- (void)showProgressUI:(id)info { note(@"showProgressUI %@", info); [self onMain:^{ if (!self.windows.count) self.showing = NO; [self show]; }]; }
- (void)hideProgressUI { note(@"hideProgressUI"); [self onMain:^{ [self hide]; }]; }
- (void)isShowingUI:(void (^)(BOOL))reply { reply(self.showing); }
- (void)setPhaseProgress:(id)info {
    double p = -1;
    if ([info isKindOfClass:[NSNumber class]]) p = [info doubleValue];
    else if ([info isKindOfClass:[NSDictionary class]]) for (id v in [info allValues]) if ([v isKindOfClass:[NSNumber class]]) { p = [v doubleValue]; break; }
    if (p > 1) p /= 100.0;
    if (p >= 0) [self onMain:^{ self.progress = p; [self redraw]; }];
}
- (void)setPhaseStatus:(id)info {}
- (void)setPhaseAlternateStatus:(id)info {}
- (void)setPhaseDone:(id)info {}
- (void)setBatteryInfo:(id)info {}
- (void)setProgressIndicatorInfo:(id)info { note(@"setProgressIndicatorInfo %@", info); }
- (void)registerClient:(id)client { note(@"registerClient %@", client); }
- (void)registerClient:(id)client completionHandler:(void (^)(void))done { note(@"registerClient+done %@", client); done(); }
- (void)unregisterClient:(id)client { note(@"unregisterClient"); }

- (void)redraw { for (NSWindow *w in self.windows) { ((ProgressView *)w.contentView).progress = self.progress; [w.contentView setNeedsDisplay:YES]; } }

// Like Installer Progress at launch: take the window server's boot windows (the boot screen the kernel left), put our
// screen over every display, then turn display updates on.
- (void)startBootWindows {
    if (pCGSMainConnectionID && pCGSCreateCinematicBootWindowArray) {
        self.bootWindows = pCGSCreateCinematicBootWindowArray(pCGSMainConnectionID());
        note(@"boot windows: %ld", self.bootWindows ? (long)CFArrayGetCount(self.bootWindows) : -1L);
    }
    // the boot progress space, exactly as Installer Progress makes it
    const CGSConnectionID cid = pCGSMainConnectionID ? pCGSMainConnectionID() : 0;
    if (cid && pCGSSpaceCreate && pCGSSpaceSetAbsoluteLevel && pBootProgressLevel) {
        NSDictionary *opt = @{@"uuid": @"Boot Progress Space", @"pid": @[@(getpid())], @"type": @2};
        self.space = pCGSSpaceCreate(cid, 1, (__bridge CFDictionaryRef)opt);
        int rl = pCGSSpaceSetAbsoluteLevel(cid, self.space, *pBootProgressLevel);
        note(@"space %llu created, level %d -> %d", self.space, *pBootProgressLevel, rl);
        NSMutableArray *wids = [NSMutableArray array];
        for (CFIndex k = 0; self.bootWindows && k < CFArrayGetCount(self.bootWindows); k++) {
            id e = (__bridge id)CFArrayGetValueAtIndex(self.bootWindows, k);
            NSNumber *wid = [e isKindOfClass:[NSDictionary class]] && pWindowIDKey ? e[(__bridge NSString *)*pWindowIDKey]
                          : ([e isKindOfClass:[NSNumber class]] ? e : nil);
            note(@"boot window %ld: %@ -> id %@", (long)k, [e class], wid);
            if (wid) { int r = pCGSSetWindowLevel ? pCGSSetWindowLevel(cid, wid.unsignedIntValue, 0x3e2) : -1; note(@"  level 0x3e2 -> %d", r); [wids addObject:wid]; }
        }
        self.spaceWids = [wids mutableCopy];
        if (wids.count && pCGSAddWindowsToSpaces) note(@"boot windows to space -> %d", pCGSAddWindowsToSpaces(cid, (__bridge CFArrayRef)wids, (__bridge CFArrayRef)@[@(self.space)]));
        if (pCGSShowSpaces) note(@"show space -> %d", pCGSShowSpaces(cid, (__bridge CFArrayRef)@[@(self.space)]));
    }
    self.deferredConfig = pSLSInitialConfigurationWasDeferred ? pSLSInitialConfigurationWasDeferred() : NO;
    note(@"configuration deferred: %d", self.deferredConfig);
    [self show];
    if (pCGSInitialEnableOfDisplayUpdates) { pCGSInitialEnableOfDisplayUpdates(); note(@"display updates enabled"); }
}
- (void)show {
    if (self.showing) return;
    // 10-11 first boot: at launch the window server listed no screens yet ("shown on 0 screen(s)"), so nothing was drawn
    // and the boot screen behind showed through. Wait for the screens (100 ms steps, 15 s at most) before drawing.
    NSMutableArray<NSValue *> *rects = [NSMutableArray array];
    for (NSScreen *s in [NSScreen screens]) [rects addObject:[NSValue valueWithRect:s.frame]];
    if (!rects.count) {   // AppKit lists no screen yet: take the displays from CoreGraphics (one coordinate space, top-left)
        CGDirectDisplayID ids[16]; uint32_t n = 0;
        if (CGGetActiveDisplayList(16, ids, &n) == kCGErrorSuccess && n) {
            const CGFloat mainH = CGDisplayBounds(CGMainDisplayID()).size.height;
            for (uint32_t k = 0; k < n; k++) { CGRect b = CGDisplayBounds(ids[k]);
                [rects addObject:[NSValue valueWithRect:NSMakeRect(b.origin.x, mainH - b.origin.y - b.size.height, b.size.width, b.size.height)]]; }
            note(@"screens from CoreGraphics: %u", n);
        }
    }
    if (!rects.count) {
        if (self.screenWaits++ < 150) { dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{ [self show]; }); }
        else note(@"no screens after 15 s - nothing drawn");
        return;
    }
    if (self.screenWaits) note(@"screens appeared after %u ms", self.screenWaits * 100);
    self.windows = [NSMutableArray array];
    for (NSValue *rv in rects) {
        const NSRect fr = rv.rectValue;
        NSWindow *w = [[NSWindow alloc] initWithContentRect:fr styleMask:NSWindowStyleMaskBorderless
                                                    backing:NSBackingStoreBuffered defer:NO];
        w.level = CGShieldingWindowLevel(); w.opaque = YES; w.backgroundColor = [NSColor blackColor];
        w.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorStationary;
        ProgressView *v = [[ProgressView alloc] initWithFrame:NSMakeRect(0, 0, fr.size.width, fr.size.height)];
        v.logo = self.logo; v.progress = self.progress;
        w.contentView = v;
        [w setFrame:fr display:YES];
        [w orderFrontRegardless];
        [self.windows addObject:w];
    }
    // our windows go into the boot progress space too (Installer Progress: _AddWindowsToUnmanagedSpace)
    const CGSConnectionID cid = pCGSMainConnectionID ? pCGSMainConnectionID() : 0;
    if (cid && self.space && pCGSAddWindowsToSpaces) {
        NSMutableArray *wids = [NSMutableArray array];
        for (NSWindow *w in self.windows) [wids addObject:@(w.windowNumber)];
        [self.spaceWids addObjectsFromArray:wids];
        note(@"our windows %@ to space %llu -> %d", wids, self.space, pCGSAddWindowsToSpaces(cid, (__bridge CFArrayRef)wids, (__bridge CFArrayRef)@[@(self.space)]));
    }
    [NSCursor hide];
    self.showing = YES;
    note(@"shown on %lu screen(s)", (unsigned long)self.windows.count);
    // measured, not assumed: is the window on screen a second later, and at which layer
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        for (NSWindow *w in self.windows) {
            CFArrayRef info = CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, (CGWindowID)w.windowNumber);
            NSDictionary *d = info && CFArrayGetCount(info) ? (__bridge NSDictionary *)CFArrayGetValueAtIndex(info, 0) : nil;
            note(@"window %ld: onscreen %@ layer %@ alpha %@ bounds %@", (long)w.windowNumber, d[(id)kCGWindowIsOnscreen],
                 d[(id)kCGWindowLayer], d[(id)kCGWindowAlpha], d[(id)kCGWindowBounds]);
            if (info) CFRelease(info);
        }
    });
}
// 10-11 (studio): hiding only ordered our window out; the boot progress space with the window server's boot window
// stayed on top and the login window never appeared (black screen with cursor) until the job was stopped. Undo the space
// the way it was made: hide it, take the windows out, destroy it, release the boot windows.
- (void)tearDownSpace {
    const CGSConnectionID cid = pCGSMainConnectionID ? pCGSMainConnectionID() : 0;
    if (cid && self.space) {
        NSArray *sp = @[@(self.space)];
        if (pCGSHideSpaces) pCGSHideSpaces(cid, (__bridge CFArrayRef)sp);
        if (pCGSRemoveWindowsFromSpaces && self.spaceWids.count) pCGSRemoveWindowsFromSpaces(cid, (__bridge CFArrayRef)self.spaceWids, (__bridge CFArrayRef)sp);
        if (pCGSSpaceDestroy) pCGSSpaceDestroy(cid, self.space);
        note(@"space %llu hidden and destroyed", self.space);
        self.space = 0;
    }
    if (self.bootWindows) { CFRelease(self.bootWindows); self.bootWindows = NULL; }
}
- (void)hide {
    if (!self.showing) { [self tearDownSpace]; note(@"hide without a screen shown; exiting"); exit(0); }
    if (self.deferredConfig && pSLSPerformDeferredConfiguration) { note(@"performing deferred configuration"); pSLSPerformDeferredConfiguration(); self.deferredConfig = NO; }
    [NSAnimationContext runAnimationGroup:^(NSAnimationContext *ctx) {
        ctx.duration = 0.3;
        for (NSWindow *w in self.windows) w.animator.alphaValue = 0;
    } completionHandler:^{
        for (NSWindow *w in self.windows) [w orderOut:nil];
        [self.windows removeAllObjects];
        [self tearDownSpace];
        [NSCursor unhide];
        self.showing = NO;
        note(@"hidden; exiting so the window server drops everything this process owned");
        exit(0);   // launchd starts it again on demand (MachServices)
    }];
}
@end

// 10-11 (studio): launchd starts this before the window server has checked in. Its port com.apple.windowserver.active
// is HideUntilCheckIn, so the first connection attempt fell back to launching the window server, which SkyLight refuses
// before login for anything but Apple's own programs ("This program is not permitted to connect to or launch Window
// Server before login") - no screens, no boot windows, nothing drawn. Installer Progress logged the same lookup error
// and got its connection 8 s later, when the window server checked in. Wait for that port first (50 ms steps, 90 s).
static void waitForWindowServer(void) {
    for (int i = 0; i < 1800; i++) {
        mach_port_t p = MACH_PORT_NULL;
        if (bootstrap_look_up(bootstrap_port, "com.apple.windowserver.active", &p) == KERN_SUCCESS && p != MACH_PORT_NULL) {
            mach_port_deallocate(mach_task_self(), p);
            note(@"window server checked in after %d ms", i * 50);
            return;
        }
        usleep(50 * 1000);
    }
    note(@"window server not checked in after 90 s");
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        note(@"start (pid %d, args %d)", getpid(), argc);
        waitForWindowServer();
        loadSkyLight();
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        Server *srv = [Server new];
        srv.logo = mothImage();
        srv.progress = 0;
        NSXPCListener *l = [[NSXPCListener alloc] initWithMachServiceName:@"com.apple.InstallerProgress"];
        l.delegate = srv;
        [l resume];
        // Installer Progress shows the boot screen as soon as launchd starts it at boot (--showProgress)
        if (argc > 1 && strcmp(argv[1], "--showProgress") == 0) dispatch_async(dispatch_get_main_queue(), ^{ [srv startBootWindows]; });
        // never hold the screen: 90 s after start the screen goes and the process exits, whatever loginwindow said
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 90 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            note(@"90 s limit reached: tearing down"); [srv tearDownSpace]; exit(0); });
        [NSApp run];
    }
    return 0;
}

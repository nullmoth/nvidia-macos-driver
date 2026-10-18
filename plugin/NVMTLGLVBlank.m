// An OpenGL app with vsync on keeps the display's refresh rate after a late frame, in every process this plugin loads
// into.
//
// AppKit hands an OpenGL frame to the window server in NSCGLSurfaceFlush. With vsync on, it first calls -[NSCGLSurface
// synchronize]. While the window server has not yet taken the previous frame, that waits for the first time after now
// in the surface's image queue's VBL info. The window server fills that info (CoreAnimation's CAShmemImageQueueUpdate)
// with its last vblank plus one to four refresh periods, taking the period from the timestamp of its update. Its window
// updates pass none, and CoreAnimation then spaces the times 1/60 s apart. On a faster display, one frame that misses
// the window server's update makes the next wait up to four refreshes. That wait ends on the window server's vblank, so
// the frame after it reaches the window server just after its update, and the next waits again: the app runs at two to
// three refreshes a frame until a frame happens to come early.
// Here synchronize waits for the display's next vblank by the display's own refresh period, as the info says when it
// has the period: a late frame costs one refresh, and the next frame is back in time.
//
// NVMTL_NO_GL_VBLANK_FIX in a process's environment leaves AppKit's own synchronize. Another image in the process that
// makes the same correction adds the same marker method to NSCGLSurface (NVGLVBL_MARKER), and only the first installs.

#include <CoreGraphics/CoreGraphics.h>
#include <mach-o/dyld.h>
#include <mach/mach_time.h>
#include <objc/runtime.h>
#include <os/lock.h>
#include <stdatomic.h>

// QuartzCore's image queue calls synchronize makes, and a VBL info entry as CAImageQueueGetVBLInfo copies it out: the
// vblank's host time, the same time in seconds, and a rate (1)
typedef struct _CAImageQueue *NVGLVBLQueueRef;
struct nvglvbl_info { uint64_t host_time; double seconds; float rate; };
enum { NVGLVBL_ENTRIES = 4 };   // as many as synchronize asks for (and the window server writes)
#define NVGLVBL_MARKER "correctsVBlankWaitByDisplayPeriod"
#define NVGLVBL_WINDOW_RECHECK_S 1.0   // a window's display is looked up again after this (it may have moved)
enum { NVGLVBL_WINDOW_CACHE = 8 };      // windows whose display's period is remembered

static struct {
    size_t (*unconsumed)(NVGLVBLQueueRef queue, uint64_t, uint32_t);
    uint32_t (*vbl_info)(NVGLVBLQueueRef queue, struct nvglvbl_info *info, size_t count);
    // CoreGraphics, for the refresh rate of a window's display: looked up rather than linked (AppKit, which this needs,
    // brings it), so the plugin loads nothing more into the processes it is in
    __typeof__(&CGWindowListCreateDescriptionFromArray) window_descriptions;
    __typeof__(&CGRectMakeWithDictionaryRepresentation) rect_from_dictionary;
    __typeof__(&CGGetDisplaysWithRect) displays_with_rect;
    __typeof__(&CGDisplayCopyDisplayMode) copy_display_mode;
    __typeof__(&CGDisplayModeGetRefreshRate) refresh_rate;
    __typeof__(&CGDisplayModeRelease) release_display_mode;
    const CFStringRef *window_bounds_key;
    IMP synchronize;                    // AppKit's own
    ptrdiff_t queue_ivar, window_ivar;  // -[NSCGLSurface] _imageQueue and _windowID
    atomic_bool installed;
} gGLVBL;

static uint64_t nvglvbl_mach_from_seconds(double s)
{
    static mach_timebase_info_data_t tb;
    if (!tb.denom) mach_timebase_info(&tb);
    return (uint64_t)(s * 1e9 * tb.denom / tb.numer);
}

// the refresh period (mach time) of the display window is on, 0 when unknown
static uint64_t nvglvbl_window_period(uint32_t window)
{
    static os_unfair_lock lock = OS_UNFAIR_LOCK_INIT;
    static struct { uint32_t window; uint64_t period, checked; } cache[NVGLVBL_WINDOW_CACHE];
    static unsigned next;
    const uint64_t now = mach_absolute_time(), recheck = nvglvbl_mach_from_seconds(NVGLVBL_WINDOW_RECHECK_S);
    os_unfair_lock_lock(&lock);
    for (unsigned i = 0; i < NVGLVBL_WINDOW_CACHE; i++)
        if (cache[i].checked && cache[i].window == window && now - cache[i].checked < recheck) {
            const uint64_t period = cache[i].period;
            os_unfair_lock_unlock(&lock);
            return period;
        }
    os_unfair_lock_unlock(&lock);

    double hz = 0;
    const void *wid = (const void *)(uintptr_t)window;   // the array holds the CGWindowID itself
    CFArrayRef ids = CFArrayCreate(kCFAllocatorDefault, &wid, 1, NULL);
    CFArrayRef windows = ids ? gGLVBL.window_descriptions(ids) : NULL;
    CGRect bounds;
    CGDirectDisplayID display;
    uint32_t displays = 0;
    if (windows && CFArrayGetCount(windows) > 0 &&
        gGLVBL.rect_from_dictionary(CFDictionaryGetValue(CFArrayGetValueAtIndex(windows, 0), *gGLVBL.window_bounds_key),
                                    &bounds) &&
        gGLVBL.displays_with_rect(bounds, 1, &display, &displays) == kCGErrorSuccess && displays) {
        CGDisplayModeRef mode = gGLVBL.copy_display_mode(display);
        if (mode) {
            hz = gGLVBL.refresh_rate(mode);   // 0 for a display that does not say
            gGLVBL.release_display_mode(mode);
        }
    }
    if (windows) CFRelease(windows);
    if (ids) CFRelease(ids);
    const uint64_t period = hz > 0 ? nvglvbl_mach_from_seconds(1 / hz) : 0;

    os_unfair_lock_lock(&lock);
    int found = -1;
    for (unsigned i = 0; i < NVGLVBL_WINDOW_CACHE; i++)
        if (cache[i].checked && cache[i].window == window) found = (int)i;
    const unsigned slot = found >= 0 ? (unsigned)found : next++ % NVGLVBL_WINDOW_CACHE;
    cache[slot].window = window;
    cache[slot].period = period;
    cache[slot].checked = now;
    os_unfair_lock_unlock(&lock);
    return period;
}

static void nvglvbl_synchronize(id self, SEL cmd)
{
    char *surface = (char *)(__bridge void *)self;
    NVGLVBLQueueRef queue = *(NVGLVBLQueueRef *)(surface + gGLVBL.queue_ivar);
    if (!queue || !gGLVBL.unconsumed(queue, 0, 0)) return;   // the window server has the previous frame: no wait
    struct nvglvbl_info info[NVGLVBL_ENTRIES];
    const uint32_t n = gGLVBL.vbl_info(queue, info, NVGLVBL_ENTRIES);
    const uint64_t period = nvglvbl_window_period(*(uint32_t *)(surface + gGLVBL.window_ivar));
    // the entries are the window server's vblank plus 1..n times the spacing CoreAnimation took for the period: with a
    // longer spacing than the display's period, the next vblank is that vblank plus a whole number of periods
    if (n >= 2 && n <= NVGLVBL_ENTRIES && period && info[1].host_time > info[0].host_time &&
        period < info[1].host_time - info[0].host_time) {
        const uint64_t vblank = info[0].host_time - (info[1].host_time - info[0].host_time);
        const uint64_t now = mach_absolute_time();
        mach_wait_until(vblank + (now > vblank ? (now - vblank) / period + 1 : 1) * period);
        return;
    }
    ((void (*)(id, SEL))gGLVBL.synchronize)(self, cmd);
}

static void nvglvbl_marker(id self, SEL cmd) { (void)self, (void)cmd; }

// replaces -[NSCGLSurface synchronize] once AppKit is loaded and has it, and everything it needs is found
static void nvglvbl_install(void)
{
    if (atomic_load(&gGLVBL.installed)) return;
    Class surface = objc_getClass("NSCGLSurface");
    if (!surface) return;
    Method m = class_getInstanceMethod(surface, sel_registerName("synchronize"));
    Ivar queue = class_getInstanceVariable(surface, "_imageQueue"), window = class_getInstanceVariable(surface, "_windowID");
    gGLVBL.unconsumed = dlsym(RTLD_DEFAULT, "CAImageQueueGetUnconsumedImageCount");
    gGLVBL.vbl_info = dlsym(RTLD_DEFAULT, "CAImageQueueGetVBLInfo");
    gGLVBL.window_descriptions = dlsym(RTLD_DEFAULT, "CGWindowListCreateDescriptionFromArray");
    gGLVBL.rect_from_dictionary = dlsym(RTLD_DEFAULT, "CGRectMakeWithDictionaryRepresentation");
    gGLVBL.displays_with_rect = dlsym(RTLD_DEFAULT, "CGGetDisplaysWithRect");
    gGLVBL.copy_display_mode = dlsym(RTLD_DEFAULT, "CGDisplayCopyDisplayMode");
    gGLVBL.refresh_rate = dlsym(RTLD_DEFAULT, "CGDisplayModeGetRefreshRate");
    gGLVBL.release_display_mode = dlsym(RTLD_DEFAULT, "CGDisplayModeRelease");
    gGLVBL.window_bounds_key = dlsym(RTLD_DEFAULT, "kCGWindowBounds");
    if (!m || !queue || !window || !gGLVBL.unconsumed || !gGLVBL.vbl_info || !gGLVBL.window_descriptions ||
        !gGLVBL.rect_from_dictionary || !gGLVBL.displays_with_rect || !gGLVBL.copy_display_mode || !gGLVBL.refresh_rate ||
        !gGLVBL.release_display_mode || !gGLVBL.window_bounds_key) {
        nvlog("GL vblank wait: not corrected in %s (NSCGLSurface or a QuartzCore/CoreGraphics call is missing)",
              getprogname());
        atomic_store(&gGLVBL.installed, true);   // nothing to retry
        return;
    }
    if (atomic_exchange(&gGLVBL.installed, true)) return;
    if (!class_addMethod(surface, sel_registerName(NVGLVBL_MARKER), (IMP)nvglvbl_marker, "v@:")) {
        nvlog("GL vblank wait: already corrected in %s by another image", getprogname());
        return;
    }
    gGLVBL.queue_ivar = ivar_getOffset(queue);
    gGLVBL.window_ivar = ivar_getOffset(window);
    gGLVBL.synchronize = method_setImplementation(m, (IMP)nvglvbl_synchronize);
    nvlog("GL vblank wait: -[NSCGLSurface synchronize] waits by the display's refresh period in %s", getprogname());
}

// AppKit loaded after the plugin: install once its classes are there (tried again as each image loads)
static void nvglvbl_image_added(const struct mach_header *header, intptr_t slide)
{
    (void)header, (void)slide;
    nvglvbl_install();
}

__attribute__((constructor)) static void nvmtl_gl_vblank_fix(void)
{
    if (getenv("NVMTL_NO_GL_VBLANK_FIX")) return;
    nvglvbl_install();
    if (!atomic_load(&gGLVBL.installed)) _dyld_register_func_for_add_image(nvglvbl_image_added);
}

// The driver stays off when the boot picker's "1401: Remove NVIDIA driver" tool asked for it.
// The tool writes -nvoff to boot-args and sets the NVRAM flag nullmoth-remove. OpenCore rewrites boot-args at every
// start (the install lists it under NVRAM Delete), so -nvoff only reached macOS when the user picked macOS in the same
// picker session; anyone who restarted instead got the driver back and the same failed start. The flag is not rewritten
// and stays set until the recover daemon has removed the driver, so it is read too.
//
// Boot guard: a driver that cannot bring its display up must not leave the Mac on a black screen until someone types
// -nvoff. NVRM counts each start in the NVRAM variable nullmoth-nv-starts; NVRMFB clears it the moment the display path
// is up (its first head is lit, or there is no display on this card to light). Two starts in a row that never got that
// far turn the driver off on the next start, exactly like -nvoff: the firmware's framebuffer gives a desktop, and the
// install's recovery puts the previous driver back. Every check is a read, so all parts of the driver agree within a
// boot; only NVRM's start counts. A variable that cannot be written (NVRAM not native) only means the guard never trips.
#pragma once
#include <IOKit/IORegistryEntry.h>
#include <IOKit/IODeviceTreeSupport.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOKitKeys.h>         // kIONVRAMSyncNowPropertyKey
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSData.h>
#include <pexpert/pexpert.h>

#define NVRM_GUARD_VAR "nullmoth-nv-starts"
// Writes to /options stay in the kernel's copy of NVRAM until macOS syncs it (normally at shutdown). A panic never gets
// there, so a boot that panicked lost its increment: six panicking boots on one card all read "start 1" (#529) and the
// guard never tripped. Every change of the counter asks for the sync at once (the same key the nvram tool uses).
static inline void nvrm_guard_sync(IORegistryEntry *options)
{
    if (options) options->setProperty(kIONVRAMSyncNowPropertyKey, kOSBooleanTrue);
}
#define NVRM_GUARD_OK_VAR "nullmoth-nv-ok"
#define NVRM_GUARD_LIMIT 2
#define NVRM_BACKLIGHT_VAR "nullmoth-backlight"

// IODisplay logical precision is independent of NVKMS's integer-percent output.
enum { kNVRMBacklightMax = 0xFFFF, kNVRMLegacyMinBacklight = 5 };

// Inverse logarithmic curve: (8^x - 1) / 7, interpolated in hundredths of a percent.
static inline UInt32 nvrm_backlight_curve(UInt32 level)
{
    static constexpr UInt32 points[] = {
        0, 198, 424, 681, 974, 1307, 1687, 2120, 2612,
        3173, 3811, 4539, 5367, 6310, 7384, 8607, 10000
    };
    UInt32 position = level * 16;
    UInt32 i = position / kNVRMBacklightMax;
    if (i == 16) return points[16];
    UInt32 fraction = position % kNVRMBacklightMax;
    return points[i] + ((points[i + 1] - points[i]) * fraction + kNVRMBacklightMax / 2)
        / kNVRMBacklightMax;
}

static inline UInt32 nvrm_backlight_from_percent(UInt32 percent)
{
    if (percent == 100) return kNVRMBacklightMax;
    UInt32 lo = 0, hi = kNVRMBacklightMax;
    while (lo < hi) {
        UInt32 mid = lo + (hi - lo) / 2;
        if (nvrm_backlight_curve(mid) < percent * 100) lo = mid + 1;
        else hi = mid;
    }
    return lo;
}

static inline UInt32 nvrm_backlight_to_percent(UInt32 level)
{
    return (nvrm_backlight_curve(level) + 50) / 100;
}

// Version 2 stored targets over the 5..100% span.
static inline UInt32 nvrm_backlight_v2_percent(UInt32 level)
{
    return kNVRMLegacyMinBacklight +
        (level * (100 - kNVRMLegacyMinBacklight) + kNVRMBacklightMax / 2) / kNVRMBacklightMax;
}

static inline bool nvrm_backlight_decode(UInt32 version, UInt32 value, UInt32 *level)
{
    if (version == 4) { *level = value; return true; }
    if (version < 1 || version > 3) return false;
    UInt32 percent = version == 2 ? nvrm_backlight_v2_percent(value)
        : (value * 100 + kNVRMBacklightMax / 2) / kNVRMBacklightMax;
    if (version == 1 && percent < kNVRMLegacyMinBacklight) percent = kNVRMLegacyMinBacklight;
    *level = nvrm_backlight_from_percent(percent);
    return true;
}

// The old keeper sends a bare v2 level; the new keeper includes the record version.
static inline bool nvrm_backlight_decode_restore(SInt32 record, UInt32 *level)
{
    if (record < 0) return false;
    UInt32 version = (UInt32)record >> 16;
    return nvrm_backlight_decode(version ? version : 2, (UInt32)record & kNVRMBacklightMax, level);
}

static inline bool nvrm_backlight_read(IORegistryEntry *options, UInt32 *level)
{
    if (!options || !level) return false;
    OSObject *o = options->copyProperty(NVRM_BACKLIGHT_VAR);
    OSData *d = OSDynamicCast(OSData, o);
    bool valid = false;
    UInt32 value = 0;
    if (d) {
        const unsigned char *bytes = (const unsigned char *)d->getBytesNoCopy();
        if (d->getLength() == 1 && bytes[0] <= 100) {
            value = nvrm_backlight_from_percent(bytes[0] < kNVRMLegacyMinBacklight ? kNVRMLegacyMinBacklight : bytes[0]);
            valid = true;
        } else if (d->getLength() == 3) {
            valid = nvrm_backlight_decode(bytes[0], (UInt32)bytes[1] | ((UInt32)bytes[2] << 8), &value);
        }
    }
    if (o) o->release();
    if (!valid) return false;
    *level = value;
    return true;
}

// A request for exactly the maximum that arrives as the FIRST user request, while a saved level below the maximum exists,
// is the display stack initialising its parameters to full brightness (it was seen repeating at every start), not a
// choice: a person moves through intermediate levels with the keys or the slider. Such a request is applied like a power
// request (clamped to the saved target) and never persisted, which used to overwrite the saved level with the maximum on
// every boot.
static inline bool nvrm_backlight_startup_default(bool userRequest, UInt32 level, bool haveSaved, bool userSeen, SInt32 userLevel)
{
    return userRequest && haveSaved && !userSeen && level == (UInt32)kNVRMBacklightMax && userLevel < (SInt32)kNVRMBacklightMax;
}

// A level restored from outside the kernel (debug.nvrmfb_backlight_restore, written by the installed backlight job from
// its own copy of the last level) is for firmware that does not keep NVRAM variables across a restart, where /options
// comes back with an old value. It is taken only while the user has not moved the brightness this boot: a request for
// exactly the maximum does not count as moving it (the display stack sends one at every start, see above), any other
// user request does, and from then on the user's choice wins.
static inline bool nvrm_backlight_external_restore_ok(bool userMoved, SInt32 level)
{
    return !userMoved && level >= 0 && level <= (SInt32)kNVRMBacklightMax;
}

static inline bool nvrm_backlight_write(IORegistryEntry *options, UInt32 level)
{
    if (!options || level > kNVRMBacklightMax) return false;
    UInt32 current;
    if (nvrm_backlight_read(options, &current) && current == level) return true;
    // Version 4 stores the logical target on the perceptual curve.
    unsigned char bytes[] = {4, (unsigned char)level, (unsigned char)(level >> 8)};
    OSData *d = OSData::withBytes(bytes, sizeof bytes);
    if (!d) return false;
    bool ok = options->setProperty(NVRM_BACKLIGHT_VAR, d);
    d->release();
    return ok;
}

static inline unsigned nvrm_guard_starts(IORegistryEntry *options)
{
    OSObject *o = options->copyProperty(NVRM_GUARD_VAR);
    OSData *d = OSDynamicCast(OSData, o);
    unsigned n = (d && d->getLength() >= 1) ? *(const unsigned char *)d->getBytesNoCopy() : 0;
    if (o) o->release();
    return n;
}

static inline bool nvrm_driver_off(void)
{
    char arg[8];
    if (PE_parse_boot_argn("-nvoff", arg, sizeof arg))
        return true;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options)
        return false;  // NVRAM not published yet: the boot-arg alone decides, as before
    OSObject *flag = options->copyProperty("nullmoth-remove");
    const bool guard = nvrm_guard_starts(options) >= NVRM_GUARD_LIMIT;
    options->release();
    if (flag) { flag->release(); return true; }
    return guard;
}

// NVRM's start, once per boot: one more start that has not reached its display yet.
static inline void nvrm_guard_count_start(void)
{
    static bool counted;
    if (counted) return;
    counted = true;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options) return;
    unsigned char n = (unsigned char)(nvrm_guard_starts(options) + 1);
    if (OSData *d = OSData::withBytes(&n, 1)) { options->setProperty(NVRM_GUARD_VAR, d); d->release(); }
    nvrm_guard_sync(options);
    options->release();
    IOLog("NVRM: boot guard: start %u without a display so far (the driver turns itself off after %u)\n", n, NVRM_GUARD_LIMIT);
}

// NVRMFB, once the display path is up: this start counts as good, and the install's trial is over.
static inline void nvrm_guard_display_up(const char *why)
{
    static bool cleared;
    if (cleared) return;
    cleared = true;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options) return;
    options->removeProperty(NVRM_GUARD_VAR);
    nvrm_guard_sync(options);
    OSObject *ok = options->copyProperty(NVRM_GUARD_OK_VAR);
    if (ok) ok->release();
    else if (OSData *d = OSData::withBytes("1", 1)) { options->setProperty(NVRM_GUARD_OK_VAR, d); d->release(); }
    options->release();
    IOLog("NVRM: boot guard cleared: %s\n", why);
}

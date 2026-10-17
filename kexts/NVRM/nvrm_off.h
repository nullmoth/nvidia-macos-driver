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
#include <libkern/c++/OSData.h>
#include <pexpert/pexpert.h>

#define NVRM_GUARD_VAR "nullmoth-nv-starts"
#define NVRM_GUARD_OK_VAR "nullmoth-nv-ok"
#define NVRM_GUARD_LIMIT 2

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

// What this boot did to the counter, so the wait exemption below only ever undoes its own increment: true once the
// increment was written (not merely attempted), and the pause state moves 0 -> 1 -> 2 exactly once per boot even with
// one auto-go worker per NVRM instance.
static bool gNvrmGuardCounted;
static volatile SInt32 gNvrmGuardPause;   // 0 not paused, 1 paused (counter taken back), 2 resumed or given up
static thread_t gNvrmGuardPauseOwner;     // the worker that claimed the pause: only it may resume it

// NVRM's start, once per boot: one more start that has not reached its display yet.
static inline void nvrm_guard_count_start(void)
{
    static bool counted;
    if (counted) return;
    counted = true;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options) return;
    unsigned char n = (unsigned char)(nvrm_guard_starts(options) + 1);
    if (OSData *d = OSData::withBytes(&n, 1)) { gNvrmGuardCounted = options->setProperty(NVRM_GUARD_VAR, d); d->release(); }
    options->release();
    if (gNvrmGuardCounted)
        IOLog("NVRM: boot guard: start %u without a display so far (the driver turns itself off after %u)\n", n, NVRM_GUARD_LIMIT);
}

// A start that is waiting on purpose (the 10 s Turing settle, the 100 s small-BAR1 wait) must not count as a failed
// start if the Mac is reset meanwhile: the screen shows the boot logo for that long, two resets switched the driver off
// (-nvoff for the next boot) on machines where it works, and the first login then reported the driver as not loaded.
// The count is taken back for the wait and taken again just before the display is armed, so a start that hangs before
// the wait (RM never came up) or after it (the head never lit) still counts. Only this boot's own written increment is
// taken back; a counter nobody wrote this boot, or one that could not be written back, is left alone.
static inline void nvrm_guard_pause_for_wait(unsigned ms)
{
    if (!gNvrmGuardCounted) return;
    if (!OSCompareAndSwap(0, 1, &gNvrmGuardPause)) return;
    gNvrmGuardPauseOwner = current_thread();
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options) { gNvrmGuardPause = 2; return; }
    const unsigned n = nvrm_guard_starts(options);
    bool written = false;
    if (n >= 1) {
        unsigned char m = (unsigned char)(n - 1);
        if (m == 0) { options->removeProperty(NVRM_GUARD_VAR); written = nvrm_guard_starts(options) == 0; }
        else if (OSData *d = OSData::withBytes(&m, 1)) { written = options->setProperty(NVRM_GUARD_VAR, d); d->release(); }
    }
    options->release();
    if (!written) { gNvrmGuardPause = 2; return; }
    IOLog("NVRM: boot guard: start %u not counted during the %u s wait (a reset now is not a failed start)\n", n, ms / 1000);
}
// Only the worker that took the count back puts it back (a second card's worker, or one whose pause never happened,
// must not resume a pause that is still being written). `guardCleared` is the framebuffer's boot-wide word
// (debug.nvrmfb_guard_cleared) that it already cleared the guard this boot: a display was lit, or the card had none to
// light, after a hand-run go during the wait; counting again then would leave a strike behind a working boot.
static inline void nvrm_guard_resume_after_wait(bool guardCleared)
{
    if (gNvrmGuardPauseOwner != current_thread()) return;
    if (!OSCompareAndSwap(1, 2, &gNvrmGuardPause)) return;
    if (guardCleared) return;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (!options) return;
    unsigned char n = (unsigned char)(nvrm_guard_starts(options) + 1);
    bool written = false;
    if (OSData *d = OSData::withBytes(&n, 1)) { written = options->setProperty(NVRM_GUARD_VAR, d); d->release(); }
    options->release();
    if (written)
        IOLog("NVRM: boot guard: start %u counted again, arming the display (the driver turns itself off after %u)\n", n, NVRM_GUARD_LIMIT);
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
    OSObject *ok = options->copyProperty(NVRM_GUARD_OK_VAR);
    if (ok) ok->release();
    else if (OSData *d = OSData::withBytes("1", 1)) { options->setProperty(NVRM_GUARD_OK_VAR, d); d->release(); }
    options->release();
    IOLog("NVRM: boot guard cleared: %s\n", why);
}

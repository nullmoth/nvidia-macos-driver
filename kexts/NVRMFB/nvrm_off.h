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
    OSObject *ok = options->copyProperty(NVRM_GUARD_OK_VAR);
    if (ok) ok->release();
    else if (OSData *d = OSData::withBytes("1", 1)) { options->setProperty(NVRM_GUARD_OK_VAR, d); d->release(); }
    options->release();
    IOLog("NVRM: boot guard cleared: %s\n", why);
}

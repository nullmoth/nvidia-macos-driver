// NMHDA volume and mute for display audio sinks: see nmhda_volume.h.
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOKitKeys.h>
#include <IOKit/audio/IOAudioEngine.h>
#include <IOKit/audio/IOAudioLevelControl.h>
#include <IOKit/audio/IOAudioToggleControl.h>
#include <IOKit/audio/IOAudioTypes.h>
#include <pexpert/pexpert.h>
#include "nmhda_gain.h"
#include "nmhda_volume.h"

namespace {
const char kEngineClass[] = "AppleHDAEngineOutputDP";
constexpr unsigned kMaxEngines = 8;

struct Slot {
    IOAudioEngine *engine;
    volatile SInt32 level;      // 0..100
    volatile SInt32 muted;
    volatile SInt32 ready;      // the controls are attached; the clip hook may scale
};
Slot gSlots[kMaxEngines];
IOLock *gLock;
NMHDAVolumeLog gLog;
NMHDAVolumeApi gApi;
IONotifier *gNotifier;

using CreateVolume = IOAudioLevelControl *(*)(SInt32, SInt32, SInt32, IOFixed, IOFixed, UInt32, const char *, UInt32, UInt32);
using CreateMute = IOAudioToggleControl *(*)(bool, UInt32, const char *, UInt32, UInt32);

Slot *find(void *engine)
{
    for (Slot &s : gSlots)
        if (s.engine == engine)
            return &s;
    return nullptr;
}

IOReturn volumeChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 now)
{
    if (Slot *s = find(target)) {
        __atomic_store_n(&s->level, now < NMHDA_LEVEL_MIN ? NMHDA_LEVEL_MIN : now > NMHDA_LEVEL_MAX ? NMHDA_LEVEL_MAX : now,
                         __ATOMIC_RELAXED);
        return kIOReturnSuccess;
    }
    return kIOReturnBadArgument;
}

IOReturn muteChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 now)
{
    if (Slot *s = find(target)) {
        __atomic_store_n(&s->muted, now != 0, __ATOMIC_RELAXED);
        return kIOReturnSuccess;
    }
    return kIOReturnBadArgument;
}

// Runs on the engine's work loop, where IOAudioFamily expects an engine's controls to be added.
IOReturn attachControls(OSObject *owner, void *a0, void *, void *, void *)
{
    // matched by class name when published, so a plain cast is enough (OSDynamicCast would need IOAudioFamily's symbols)
    IOAudioEngine *engine = find(owner) != nullptr ? static_cast<IOAudioEngine *>(owner) : nullptr;
    Slot *slot = (Slot *)a0;
    if (engine == nullptr || slot == nullptr)
        return kIOReturnBadArgument;
    // -60 dB to 0 dB, close to what the cubed level curve spans over its audible range
    IOAudioLevelControl *vol = ((CreateVolume)gApi.createVolumeControl)(NMHDA_LEVEL_MAX, NMHDA_LEVEL_MIN, NMHDA_LEVEL_MAX,
                                   (IOFixed)(-60 * 65536), 0, kIOAudioControlChannelIDAll, "Master", 0, kIOAudioControlUsageOutput);
    IOAudioToggleControl *mute = ((CreateMute)gApi.createMuteControl)(false, kIOAudioControlChannelIDAll, "Master", 0,
                                   kIOAudioControlUsageOutput);
    IOReturn rc = kIOReturnNoMemory;
    if (vol != nullptr && mute != nullptr) {
        vol->setValueChangeHandler((IOAudioControl::IntValueChangeHandler)volumeChanged, engine);
        mute->setValueChangeHandler((IOAudioControl::IntValueChangeHandler)muteChanged, engine);
        rc = engine->addDefaultAudioControl(vol);
        if (rc == kIOReturnSuccess)
            rc = engine->addDefaultAudioControl(mute);
    }
    if (vol) vol->release();
    if (mute) mute->release();
    if (rc == kIOReturnSuccess)
        __atomic_store_n(&slot->ready, 1, __ATOMIC_RELEASE);
    if (gLog)
        gLog("vol", "controls on %s engine -> 0x%x", kEngineClass, rc);
    return rc;
}

bool published(void *, void *, IOService *service, IONotifier *)
{
    if (service == nullptr || strcmp(service->getMetaClass()->getClassName(), kEngineClass) != 0)
        return true;
    IOAudioEngine *engine = static_cast<IOAudioEngine *>(service);
    Slot *slot = nullptr;
    IOLockLock(gLock);
    if (find(engine) == nullptr)
        for (Slot &s : gSlots)
            if (s.engine == nullptr) {
                s.engine = engine;
                s.level = NMHDA_LEVEL_MAX;
                s.muted = 0;
                s.ready = 0;
                slot = &s;
                engine->retain();       // the table names the engine until the machine restarts
                break;
            }
    IOLockUnlock(gLock);
    if (slot == nullptr)
        return true;
    IOWorkLoop *wl = engine->getWorkLoop();
    if (wl == nullptr || wl->runAction(attachControls, engine, slot) != kIOReturnSuccess) {
        if (gLog)
            gLog("vol", "could not add controls to a %s engine", kEngineClass);
    }
    return true;
}
}

bool nmhdaVolumeRequested()
{
    int v = 0;
    return PE_parse_boot_argn("nmhdavol", &v, sizeof v) && v == 1;
}

bool nmhdaVolumeStart(const NMHDAVolumeApi &api, NMHDAVolumeLog log)
{
    if (api.createVolumeControl == nullptr || api.createMuteControl == nullptr || gNotifier != nullptr)
        return false;
    gApi = api;
    gLog = log;
    gLock = IOLockAlloc();
    if (gLock == nullptr)
        return false;
    gNotifier = IOService::addMatchingNotification(gIOPublishNotification, IOService::serviceMatching(kEngineClass),
                                                   (IOServiceMatchingNotificationHandler)published, nullptr, nullptr, 0);
    return gNotifier != nullptr;
}

void nmhdaVolumeClip(void *engine, void *sampleBuf, uint32_t firstFrame, uint32_t numFrames, const void *format)
{
    Slot *s = find(engine);
    if (s == nullptr || !__atomic_load_n(&s->ready, __ATOMIC_ACQUIRE) || sampleBuf == nullptr || format == nullptr)
        return;
    const IOAudioStreamFormat *f = (const IOAudioStreamFormat *)format;
    uint32_t gain = nmhda_gain_q16((int)__atomic_load_n(&s->level, __ATOMIC_RELAXED), (int)__atomic_load_n(&s->muted, __ATOMIC_RELAXED));
    if (gain >= NMHDA_UNITY_Q16)
        return;
    nmhda_gain_apply(sampleBuf, firstFrame, numFrames, f->fNumChannels, f->fBitWidth,
                     f->fNumericRepresentation == kIOAudioStreamNumericRepresentationIEEE754Float, gain);
}

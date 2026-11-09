// NMHDA volume and mute for display audio sinks (boot argument nmhdavol=1; off by default).
//
// A display's audio sink has no codec volume, so macOS draws no slider for it and the keyboard volume keys do nothing.
// For each AppleHDAEngineOutputDP that is published, NMHDA adds Core Audio's standard master level and mute controls
// to the engine and applies their value to the samples the engine writes (nmhda_gain.h). Only IOAudioFamily's public
// interface is used: the two control constructors are resolved by name from the loaded IOAudioFamily image, and the
// rest is ordinary virtual calls through the SDK's class definitions, so nothing here depends on the layout of
// Apple's private AppleHDA classes.
#ifndef NMHDA_VOLUME_H
#define NMHDA_VOLUME_H
#include <stdint.h>

class IOService;
typedef void (*NMHDAVolumeLog)(const char *tag, const char *fmt, ...);

// Constructors resolved from IOAudioFamily's image (mangled names are in nmhda_volume.cpp). Both pointers must be
// non-null before nmhdaVolumeStart is called.
struct NMHDAVolumeApi {
    void *createVolumeControl;      // IOAudioLevelControl::createVolumeControl
    void *createMuteControl;        // IOAudioToggleControl::createMuteControl
};

bool nmhdaVolumeRequested();                                    // nmhdavol=1 on the boot line
bool nmhdaVolumeStart(const NMHDAVolumeApi &api, NMHDAVolumeLog log);   // watch for display audio engines
// Called after the engine's own clipOutputSamples: scales the window it just wrote. A no-op for an engine that was not
// given controls. 'format' is the IOAudioStreamFormat the callback received.
void nmhdaVolumeClip(void *engine, void *sampleBuf, uint32_t firstFrame, uint32_t numFrames, const void *format);
#endif

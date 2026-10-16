/*
 * NMXAudio: HD Audio for macOS 26, which no longer ships AppleHDA. Without it a PC's onboard codec and its graphics
 * card's HDMI/DisplayPort audio have no driver, macOS has no audio device at all, and media playback fails with it -
 * Safari's player reports a decode error for any video with sound (YouTube shows a black frame), because AVFoundation's
 * audio renderer finds no output.
 *
 * Below: FreeBSD's HD Audio driver (hdac controller, hdacc codec, hdaa function group with its generic widget parser,
 * pin quirks and HDMI/DP ELD handling) on the port's FreeBSD-on-xnu layer, unchanged, through the port's sound core
 * (nmx_pcm.c). Above: one IOAudioDevice per PCM device hdaa creates ("Realtek ALC897 (Analog)", "NVIDIA (HDMI/DP 8ch)"),
 * each with an output engine and, where the codec records, an input engine. The engines play and record straight from
 * the DMA ring the controller runs; CoreAudio mixes and converts into it.
 *
 * Matches every PCI HD Audio controller (class 04:03). Only on macOS 26 and later unless the boot argument nmxaudio=1
 * is set: on macOS 15 AppleHDA drives these. -nvoff parks it like every NullMoth driver; nmxaudio=0 turns it off.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/IONVRAM.h>
#include <libkern/c++/OSData.h>
#include <IOKit/audio/IOAudioDevice.h>
#include <IOKit/audio/IOAudioEngine.h>
#include <IOKit/audio/IOAudioStream.h>
#include <IOKit/audio/IOAudioLevelControl.h>
#include <IOKit/audio/IOAudioToggleControl.h>
#include <IOKit/audio/IOAudioDefines.h>
#include <libkern/version.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>
#include <kern/thread.h>
#include "nmx_port.h"
#include "nmx_pcm.h"

/* FreeBSD's format and mixer constants the engine needs (sys/soundcard.h); the C side owns everything else */
#define NMX_AFMT_S16_LE 0x00000010
#define NMX_AFMT_S32_LE 0x00001000
#define NMX_FMT(f, ch) ((f) | ((uint32_t)(ch) << 20))
#define NMX_MIXER_VOLUME 0
#define NMX_MIXER_PCM 4
#define NMX_MIXER_RECLEV 11

extern "C" {
struct nmx_drvmod { const char *bus; void *driver; };
extern struct nmx_drvmod nmx_drvmod_snd_hda_pci, nmx_drvmod_snd_hda_hdac, nmx_drvmod_snd_hda_hdacc,
    nmx_drvmod_snd_hda_pcm_hdaa;
void *devclass_create(const char *name);
int devclass_add_driver(void *dc, void *driver, int pass, void **dcp);
}

/* ---- the engine: one channel (play or record) of one PCM device */

class NMXAudioEngine : public IOAudioEngine {
    OSDeclareDefaultStructors(NMXAudioEngine)
    struct snddev_info *sc_;
    struct pcm_channel *ch_;
    bool output_;
    void *ring_;
    UInt32 ringBytes_, frameBytes_, frames_, lastPtr_;
    UInt32 sampleBits_;
    IOAudioStream *stream_;
    bool started_;
    static IOReturn volumeChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 v);
    static IOReturn muteChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 v);
    SInt32 volume_ = 75;
    bool muted_ = false;
public:
    bool initFor(struct snddev_info *sc, struct pcm_channel *ch);
    bool initHardware(IOService *provider) override;
    void free() override;
    UInt32 getCurrentSampleFrame() override;
    IOReturn performAudioEngineStart() override;
    IOReturn performAudioEngineStop() override;
    IOReturn performFormatChange(IOAudioStream *, const IOAudioStreamFormat *, const IOAudioSampleRate *) override;
    IOReturn clipOutputSamples(const void *mixBuf, void *sampleBuf, UInt32 firstSampleFrame, UInt32 numSampleFrames,
                               const IOAudioStreamFormat *streamFormat, IOAudioStream *audioStream) override;
    IOReturn convertInputSamples(const void *sampleBuf, void *destBuf, UInt32 firstSampleFrame, UInt32 numSampleFrames,
                                 const IOAudioStreamFormat *streamFormat, IOAudioStream *audioStream) override;
    void blockDone();
    OSString *getGlobalUniqueID() override;
};
OSDefineMetaClassAndStructors(NMXAudioEngine, IOAudioEngine)

// CoreAudio keys a device by this ID, so it must differ per engine and survive reboots (saved volume and default
// device follow it). IOAudioEngine's own is "<class>:<index on the device>", the same for every PCM device, and
// CoreAudio kept only the last engine it saw with each ID: one of six outputs showed up.
OSString *NMXAudioEngine::getGlobalUniqueID()
{
    char loc[48], id[96];
    nmx_pcm_location(sc_, loc, sizeof(loc));
    snprintf(id, sizeof(id), "NMXAudio:%s:%s", loc, output_ ? "out" : "in");
    return OSString::withCString(id);
}

bool NMXAudioEngine::initFor(struct snddev_info *sc, struct pcm_channel *ch)
{
    if (!IOAudioEngine::init(nullptr))
        return false;
    sc_ = sc;
    ch_ = ch;
    output_ = nmx_chan_dir(ch) == NMX_DIR_PLAY;
    return true;
}

/*
 * Format: stereo at 48 kHz (44.1 kHz too when the codec has it), 32-bit samples when the codec takes them (hdaa sends
 * 20/24/32 significant bits from a 32-bit container) and 16-bit otherwise. Every HD Audio codec and HDMI sink has 48 kHz
 * stereo 16-bit; CoreAudio converts the rest.
 */
bool NMXAudioEngine::initHardware(IOService *provider)
{
    if (!IOAudioEngine::initHardware(provider))
        return false;
    uint32_t minspeed = 0, maxspeed = 0;
    const uint32_t *fmts = nmx_chan_formats(ch_, &minspeed, &maxspeed);
    bool has32 = false, has16 = false;
    for (int i = 0; fmts && fmts[i]; i++) {
        if (fmts[i] == NMX_FMT(NMX_AFMT_S32_LE, 2)) has32 = true;
        if (fmts[i] == NMX_FMT(NMX_AFMT_S16_LE, 2)) has16 = true;
    }
    if (!has16 && !has32) {
        IOLog("NMXAudio: %s has no stereo PCM format\n", nmx_pcm_name(sc_));
        return false;
    }
    sampleBits_ = has32 ? 32 : 16;
    frameBytes_ = 2 * (sampleBits_ / 8);
    if (nmx_chan_setformat(ch_, NMX_FMT(has32 ? NMX_AFMT_S32_LE : NMX_AFMT_S16_LE, 2)) != 0)
        return false;
    UInt32 rate = nmx_chan_setspeed(ch_, 48000);
    /* 16 blocks of 512 frames: an interrupt every 10.7 ms at 48 kHz, 170 ms of ring */
    nmx_chan_setfragments(ch_, 512 * frameBytes_, 16);
    UInt32 blksz = 0;
    ring_ = nmx_chan_ring(ch_, &ringBytes_, &blksz);
    if (ring_ == nullptr || ringBytes_ < frameBytes_ * 64)
        return false;
    frames_ = ringBytes_ / frameBytes_;

    setDescription(nmx_pcm_name(sc_));
    IOAudioSampleRate sr = { rate, 0 };
    setSampleRate(&sr);
    setNumSampleFramesPerBuffer(frames_);
    setSampleOffset(blksz / frameBytes_);        /* stay a block ahead of the DMA position */
    setSampleLatency(32);

    stream_ = new IOAudioStream;
    if (stream_ == nullptr || !stream_->initWithAudioEngine(this, output_ ? kIOAudioStreamDirectionOutput :
                                                            kIOAudioStreamDirectionInput, 1))
        return false;
    stream_->setSampleBuffer(ring_, ringBytes_);
    IOAudioStreamFormat fmt = {
        2, kIOAudioStreamSampleFormatLinearPCM, kIOAudioStreamNumericRepresentationSignedInt,
        (UInt8)(sampleBits_ == 32 ? 24 : 16), (UInt8)sampleBits_, kIOAudioStreamAlignmentLowByte,
        kIOAudioStreamByteOrderLittleEndian, true, 0
    };
    IOAudioSampleRate r48 = { 48000, 0 }, r44 = { 44100, 0 };
    stream_->addAvailableFormat(&fmt, &r48, &r48);
    if (minspeed <= 44100 && maxspeed >= 44100)
        stream_->addAvailableFormat(&fmt, &r44, &r44);
    stream_->setFormat(&fmt);
    addAudioStream(stream_);
    stream_->release();

    /* volume and mute go to the codec's own amplifiers (hdaa's OSS mixer: VOLUME on the output path, RECLEV on input) */
    IOAudioLevelControl *vol = IOAudioLevelControl::createVolumeControl(75, 0, 100, (-64 << 16), 0,
        kIOAudioControlChannelIDAll, kIOAudioControlChannelNameAll, 0,
        output_ ? kIOAudioControlUsageOutput : kIOAudioControlUsageInput);
    if (vol) {
        vol->setValueChangeHandler(volumeChanged, this);
        addDefaultAudioControl(vol);
        vol->release();
    }
    IOAudioToggleControl *mute = IOAudioToggleControl::createMuteControl(false, kIOAudioControlChannelIDAll,
        kIOAudioControlChannelNameAll, 0, output_ ? kIOAudioControlUsageOutput : kIOAudioControlUsageInput);
    if (mute) {
        mute->setValueChangeHandler(muteChanged, this);
        addDefaultAudioControl(mute);
        mute->release();
    }
    nmx_chan_set_engine(ch_, this);
    IOLog("NMXAudio: %s %s: %u Hz, %u-bit, ring %u frames\n", nmx_pcm_name(sc_), output_ ? "output" : "input",
          (unsigned)rate, (unsigned)sampleBits_, (unsigned)frames_);
    return true;
}

void NMXAudioEngine::free()
{
    if (ch_ != nullptr) {
        nmx_chan_set_engine(ch_, nullptr);
        if (started_)
            nmx_chan_stop(ch_);
    }
    IOAudioEngine::free();
}

IOReturn NMXAudioEngine::volumeChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 v)
{
    NMXAudioEngine *e = OSDynamicCast(NMXAudioEngine, target);
    if (e == nullptr)
        return kIOReturnBadArgument;
    e->volume_ = v;
    unsigned lvl = e->muted_ ? 0 : (unsigned)v;
    nmx_pcm_mixer_set(e->sc_, e->output_ ? NMX_MIXER_VOLUME : NMX_MIXER_RECLEV, lvl, lvl);
    return kIOReturnSuccess;
}

IOReturn NMXAudioEngine::muteChanged(OSObject *target, IOAudioControl *, SInt32, SInt32 v)
{
    NMXAudioEngine *e = OSDynamicCast(NMXAudioEngine, target);
    if (e == nullptr)
        return kIOReturnBadArgument;
    e->muted_ = v != 0;
    unsigned lvl = e->muted_ ? 0 : (unsigned)e->volume_;
    nmx_pcm_mixer_set(e->sc_, e->output_ ? NMX_MIXER_VOLUME : NMX_MIXER_RECLEV, lvl, lvl);
    return kIOReturnSuccess;
}

UInt32 NMXAudioEngine::getCurrentSampleFrame()
{
    return (nmx_chan_getptr(ch_) % ringBytes_) / frameBytes_;
}

IOReturn NMXAudioEngine::performAudioEngineStart()
{
    bzero(ring_, ringBytes_);
    lastPtr_ = 0;
    takeTimeStamp(false);
    if (nmx_chan_start(ch_) != 0)
        return kIOReturnError;
    started_ = true;
    return kIOReturnSuccess;
}

IOReturn NMXAudioEngine::performAudioEngineStop()
{
    started_ = false;
    nmx_chan_stop(ch_);
    return kIOReturnSuccess;
}

IOReturn NMXAudioEngine::performFormatChange(IOAudioStream *, const IOAudioStreamFormat *, const IOAudioSampleRate *sr)
{
    if (sr != nullptr) {
        UInt32 got = nmx_chan_setspeed(ch_, sr->whole);
        if (got != sr->whole)
            return kIOReturnUnsupported;
    }
    return kIOReturnSuccess;
}

/* a DMA block finished: the ring wrapped when the position moved backwards, which is when CoreAudio wants a time stamp */
void NMXAudioEngine::blockDone()
{
    if (!started_)
        return;
    UInt32 ptr = nmx_chan_getptr(ch_) % ringBytes_;
    if (ptr < lastPtr_)
        takeTimeStamp();
    lastPtr_ = ptr;
}

IOReturn NMXAudioEngine::clipOutputSamples(const void *mixBuf, void *sampleBuf, UInt32 first, UInt32 n,
                                           const IOAudioStreamFormat *, IOAudioStream *)
{
    const float *in = (const float *)mixBuf + first * 2;
    UInt32 count = n * 2;
    if (sampleBits_ == 32) {
        SInt32 *out = (SInt32 *)sampleBuf + first * 2;
        for (UInt32 i = 0; i < count; i++) {
            float s = in[i];
            s = s > 1.0f ? 1.0f : (s < -1.0f ? -1.0f : s);
            out[i] = s >= 1.0f ? 0x7fffffff : (SInt32)(s * 2147483648.0f);
        }
    } else {
        SInt16 *out = (SInt16 *)sampleBuf + first * 2;
        for (UInt32 i = 0; i < count; i++) {
            float s = in[i];
            s = s > 1.0f ? 1.0f : (s < -1.0f ? -1.0f : s);
            out[i] = s >= 1.0f ? 0x7fff : (SInt16)(s * 32768.0f);
        }
    }
    return kIOReturnSuccess;
}

IOReturn NMXAudioEngine::convertInputSamples(const void *sampleBuf, void *destBuf, UInt32 first, UInt32 n,
                                             const IOAudioStreamFormat *, IOAudioStream *)
{
    float *out = (float *)destBuf;
    UInt32 count = n * 2;
    if (sampleBits_ == 32) {
        const SInt32 *in = (const SInt32 *)sampleBuf + first * 2;
        for (UInt32 i = 0; i < count; i++)
            out[i] = (float)in[i] / 2147483648.0f;
    } else {
        const SInt16 *in = (const SInt16 *)sampleBuf + first * 2;
        for (UInt32 i = 0; i < count; i++)
            out[i] = (float)in[i] / 32768.0f;
    }
    return kIOReturnSuccess;
}

/* ---- the device: one per PCM device (an analog codec output, an HDMI/DP port) */

class NMXAudioDevice : public IOAudioDevice {
    OSDeclareDefaultStructors(NMXAudioDevice)
    struct snddev_info *sc_;
public:
    struct snddev_info *softc() const { return sc_; }
    bool initFor(struct snddev_info *sc) { sc_ = sc; return IOAudioDevice::init(nullptr); }
    bool initHardware(IOService *provider) override;
};
OSDefineMetaClassAndStructors(NMXAudioDevice, IOAudioDevice)

bool NMXAudioDevice::initHardware(IOService *provider)
{
    if (!IOAudioDevice::initHardware(provider))
        return false;
    const char *name = nmx_pcm_name(sc_);
    setDeviceName(name);
    setDeviceShortName(name);
    setManufacturerName("NullMoth");
    int engines = 0;
    for (int i = 0; i < nmx_pcm_nchan(sc_); i++) {
        NMXAudioEngine *e = new NMXAudioEngine;
        if (e && e->initFor(sc_, nmx_pcm_chan(sc_, i)) && activateAudioEngine(e) == kIOReturnSuccess)
            engines++;
        if (e)
            e->release();
    }
    return engines > 0;
}

/* ---- the PCI driver */

class NMXAudio : public IOService {
    OSDeclareDefaultStructors(NMXAudio)
    IOPCIDevice *pci_;
    void *dev_;
    volatile int attach_;
    static void attachThread(void *arg, wait_result_t);
public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn setPowerState(unsigned long state, IOService *device) override;
};
OSDefineMetaClassAndStructors(NMXAudio, IOService)

static IOLock *g_lock;
static bool g_started;
static NMXAudio *g_attaching;          /* the controller whose drivers are attaching (attaches run one at a time) */
static OSArray *g_devices;             /* the NMXAudioDevice objects published */

static bool enabled()
{
    int v = 0;
    if (PE_parse_boot_argn("-nvoff", &v, sizeof(v)))
        return false;
    if (PE_parse_boot_argn("nmxaudio", &v, sizeof(v)))
        return v != 0;
    return version_major >= 25;        /* Darwin 25 = macOS 26, the first macOS without AppleHDA */
}

IOService *NMXAudio::probe(IOService *provider, SInt32 *score)
{
    if (OSDynamicCast(IOPCIDevice, provider) == nullptr || !enabled())
        return nullptr;
    return IOService::probe(provider, score);
}

static int register_drivers()
{
    struct nmx_drvmod *mods[] = { &nmx_drvmod_snd_hda_pci, &nmx_drvmod_snd_hda_hdac, &nmx_drvmod_snd_hda_hdacc,
                                  &nmx_drvmod_snd_hda_pcm_hdaa };
    for (struct nmx_drvmod *m : mods) {
        int err = devclass_add_driver(devclass_create(m->bus), m->driver, 0, nullptr);
        if (err)
            return err;
    }
    return 0;
}

bool NMXAudio::start(IOService *provider)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || g_lock == nullptr || !IOService::start(provider))
        return false;
    IOLockLock(g_lock);
    if (!g_started) {
        int err = nmx_port_start();
        if (err == 0)
            err = register_drivers();
        if (err) {
            IOLockUnlock(g_lock);
            IOLog("NMXAudio: the driver layer did not start (%d)\n", err);
            IOService::stop(provider);
            return false;
        }
        g_started = true;
    }
    IOLockUnlock(g_lock);
    pci->setMemoryEnable(true);
    pci->setBusMasterEnable(true);
    pci_ = pci;
    dev_ = nullptr;
    attach_ = 0;
    /* codec probing sends hundreds of verbs; it runs on its own thread so boot matching does not wait for it */
    retain();
    thread_t t = THREAD_NULL;
    if (kernel_thread_start(&NMXAudio::attachThread, this, &t) != KERN_SUCCESS) {
        attach_ = -1;
        release();
        return true;
    }
    thread_deallocate(t);
    return true;
}

/*
 * Boot guard, as NMXNPU's. A driver that panics while it attaches at boot would panic again on every boot. Before the
 * first attach of a boot the kext sets an NVRAM marker and writes it out; it clears it two minutes after the devices are
 * up (attach, engines, first interrupts). A boot that finds the marker set follows one that did not get there - a panic
 * or a hang - so NMXAudio stays off for that boot and says so; the boot after that tries again.
 */
static const char *const kAttachMarker = "nullmoth-audio-attaching";

static IODTNVRAM *nmx_nvram()
{
    IORegistryEntry *e = IORegistryEntry::fromPath("/options", gIODTPlane);
    IODTNVRAM *n = OSDynamicCast(IODTNVRAM, e);
    if (n == nullptr && e != nullptr)
        e->release();
    return n;
}

static bool nmx_marker_set()
{
    IODTNVRAM *n = nmx_nvram();
    if (n == nullptr)
        return false;
    OSObject *v = n->copyProperty(kAttachMarker);
    bool set = v != nullptr;
    if (v)
        v->release();
    n->release();
    return set;
}

static void nmx_marker_write(bool on)
{
    IODTNVRAM *n = nmx_nvram();
    if (n == nullptr) {
        IOLog("NMXAudio: no NVRAM; the boot guard is off\n");
        return;
    }
    const OSSymbol *k = OSSymbol::withCString(kAttachMarker);
    if (k != nullptr) {
        if (on) {
            OSData *d = OSData::withBytes("1", 1);
            if (d == nullptr || !n->setProperty(k, d))
                IOLog("NMXAudio: could not set the boot guard\n");
            if (d)
                d->release();
        } else {
            n->removeProperty(k);
        }
        k->release();
    }
    n->sync();
    n->release();
}

static int g_guard;     /* 0 = not checked this boot, 1 = armed by this boot, -1 = the last boot never cleared it */

void NMXAudio::attachThread(void *arg, wait_result_t)
{
    NMXAudio *self = (NMXAudio *)arg;
    IOLockLock(g_lock);
    if (g_guard == 0) {
        if (nmx_marker_set()) {
            g_guard = -1;
            nmx_marker_write(false);     /* the next boot tries again */
        } else {
            g_guard = 1;
            nmx_marker_write(true);
        }
    }
    if (g_guard < 0) {
        IOLockUnlock(g_lock);
        IOLog("NMXAudio: the last boot stopped while audio was starting; audio stays off this boot\n");
        self->setProperty("nullmoth-audio-state", "parked: the last boot stopped while audio was starting");
        self->attach_ = -1;
        self->release();
        return;
    }
    g_attaching = self;
    self->dev_ = nmx_port_attach_pci(self->pci_);
    g_attaching = nullptr;
    IOLockUnlock(g_lock);
    if (self->dev_ == nullptr) {
        IOLog("NMXAudio: no driver took %04x:%04x\n", self->pci_->configRead16(kIOPCIConfigVendorID),
              self->pci_->configRead16(kIOPCIConfigDeviceID));
        self->attach_ = -1;
    } else {
        static IOPMPowerState states[2] = {
            { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
            { kIOPMPowerStateVersion1, kIOPMPowerOn, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
        };
        self->PMinit();
        self->pci_->joinPMtree(self);
        self->registerPowerDriver(self, states, 2);
        self->registerService();
        self->attach_ = 1;
    }
    /* the guard comes off once audio has run for two minutes (the first controller's thread does it) */
    static volatile SInt32 clearing;
    if (g_guard > 0 && OSCompareAndSwap(0, 1, &clearing)) {
        IOSleep(120 * 1000);
        nmx_marker_write(false);
        IOLog("NMXAudio: up for two minutes; boot guard cleared\n");
    }
    self->release();
}

/* pcm_register: hdaa finished a PCM device; publish it as an audio device under the controller being attached */
extern "C" void nmx_audio_publish(struct snddev_info *sc)
{
    NMXAudio *owner = g_attaching;
    if (owner == nullptr || nmx_pcm_nchan(sc) == 0)
        return;
    NMXAudioDevice *d = new NMXAudioDevice;
    if (d == nullptr || !d->initFor(sc) || !d->attach(owner) || !d->start(owner)) {
        IOLog("NMXAudio: %s could not be published\n", nmx_pcm_name(sc));
        if (d) {
            d->detach(owner);
            d->release();
        }
        return;
    }
    if (g_devices)
        g_devices->setObject(d);
    d->release();
}

extern "C" void nmx_audio_unpublish(struct snddev_info *sc)
{
    if (g_devices == nullptr)
        return;
    for (unsigned i = 0; i < g_devices->getCount(); i++) {
        NMXAudioDevice *d = OSDynamicCast(NMXAudioDevice, g_devices->getObject(i));
        if (d && d->softc() == sc) {
            IOService *owner = d->getProvider();
            d->stop(owner);
            d->detach(owner);
            g_devices->removeObject(i);
            return;
        }
    }
}

extern "C" void nmx_audio_block_done(void *engine, struct pcm_channel *)
{
    ((NMXAudioEngine *)engine)->blockDone();
}

void NMXAudio::stop(IOService *provider)
{
    for (int i = 0; attach_ == 0 && i < 600; i++)
        IOSleep(100);
    if (attach_ == 1)
        PMstop();
    nmx_port_detach(dev_);
    dev_ = nullptr;
    IOService::stop(provider);
}

IOReturn NMXAudio::setPowerState(unsigned long state, IOService *)
{
    if (dev_ != nullptr) {
        int err = nmx_port_power(dev_, state != 0);
        if (err)
            IOLog("NMXAudio: %s returned %d\n", state ? "resume" : "suspend", err);
    }
    return kIOPMAckImplied;
}

extern "C" {
kern_return_t nmx_audio_kmod_start(kmod_info_t *ki, void *data);
kern_return_t nmx_audio_kmod_stop(kmod_info_t *ki, void *data);
}

kern_return_t nmx_audio_kmod_start(kmod_info_t *, void *)
{
    g_lock = IOLockAlloc();
    g_devices = OSArray::withCapacity(8);
    return g_lock && g_devices ? KERN_SUCCESS : KERN_FAILURE;
}

kern_return_t nmx_audio_kmod_stop(kmod_info_t *, void *)
{
    if (g_started)
        nmx_port_stop();
    if (g_devices)
        g_devices->release();
    if (g_lock)
        IOLockFree(g_lock);
    return KERN_SUCCESS;
}

extern "C" {
kern_return_t _start(kmod_info_t *ki, void *data);
kern_return_t _stop(kmod_info_t *ki, void *data);
KMOD_EXPLICIT_DECL(com.nullmoth.NMXAudio, "1.0.0", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = nmx_audio_kmod_start;
__private_extern__ kmod_stop_func_t *_antimain = nmx_audio_kmod_stop;
}

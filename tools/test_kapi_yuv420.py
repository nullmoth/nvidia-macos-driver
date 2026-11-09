#!/usr/bin/env python3
"""A display that only takes YUV 4:2:0 at 4K60 gets the mode, in hardware, with no per-rig settings.

The functions are lifted from the added lines of kexts/NVRM/patches/620-yuv420-hw.patch and compiled against a mocked
NVKMS that refuses a 4K60 mode as RGB but validates it with the 4:2:0 preference. Arms are named sentences. The control is
the unpatched kapi: ValidateDisplayMode is the RGB check alone, so the mode is refused and the display stays at 4K30."""
import pathlib, re, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
patch = (root / 'kexts/NVRM/patches/620-yuv420-hw.patch').read_text()
added = '\n'.join(l[1:] for l in patch.splitlines() if l.startswith('+') and not l.startswith('+++'))
def func(name):
    a = added.index('static NvBool ' + name)
    return added[a:added.index('\n}\n', a) + 3]
same = func('Y420SameTiming')
helper = func('Y420HwValidateMode')
# the diff shares the parameter list with the function it sits next to, so those lines are context, not additions
helper = ('static NvBool Y420HwValidateMode(struct NvKmsKapiDevice *device, NvKmsKapiDisplay display, const struct NvKmsKapiDisplayMode *mode)\n{\n'
          + helper.split('\n', 1)[1])
assert 'yuv420Mode.val = NV_YUV420_MODE_HW' in added and 'preferYUV420 = NV_TRUE' in added, 'the head is not set as hardware 4:2:0'
assert 'nvyuv420' not in added and '3840' not in added, 'the fix must not carry rig-specific constants'
wrapper = 'static NvBool ValidateDisplayMode2(struct NvKmsKapiDevice *device, NvKmsKapiDisplay display, const struct NvKmsKapiDisplayMode *mode)\n{ return ValidateDisplayModeRgb(device, display, mode) || Y420HwValidateMode(device, display, mode); }\n'
assert 'ValidateDisplayModeRgb(device, display, mode) ||' in added, 'ValidateDisplayMode is not RGB-then-4:2:0'
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include <cstdlib>
typedef int NvBool; typedef unsigned NvU32; typedef unsigned NvKmsKapiDisplay;
#define NV_TRUE 1
#define NV_FALSE 0
enum { NV_YUV420_MODE_NONE, NV_YUV420_MODE_SW, NV_YUV420_MODE_HW };
enum { NVKMS_IOCTL_VALIDATE_MODE = 1, NVKMS_IOCTL_VALIDATE_MODE_INDEX = 2 };
struct NvModeTimings { unsigned pixelClockHz, hVisible, hSyncStart, hSyncEnd, hTotal, vVisible, vSyncStart, vSyncEnd, vTotal; bool interlaced, doubleScan; };
struct NvKmsMode { NvModeTimings timings; };
struct Val { unsigned deviceHandle, dispHandle; unsigned dpyId; struct { bool preferYUV420; unsigned overrides; } modeValidation; NvKmsMode mode; };
struct Rep { NvBool valid; int yuv420Mode; NvKmsMode mode; };
struct NvKmsValidateModeParams { Val request; Rep reply; };
struct NvKmsKapiDisplayMode { NvModeTimings t; };
struct NvKmsKapiDevice { unsigned hKmsDevice, hKmsDisp; void *pKmsOpen; };
static unsigned gCalls, gPrefer;
static bool gSwOnly, gRgbOk;
static NvBool nvkms_ioctl_from_kapi(void *, unsigned, void *p, size_t) {
    NvKmsValidateModeParams *v = (NvKmsValidateModeParams *)p; gCalls++;
    if (v->request.modeValidation.preferYUV420) { gPrefer++; v->reply.valid = 1; v->reply.yuv420Mode = gSwOnly ? NV_YUV420_MODE_SW : NV_YUV420_MODE_HW; }
    else { v->reply.valid = gRgbOk; v->reply.yuv420Mode = NV_YUV420_MODE_NONE; }
    return 1;
}
struct AllocProxy { void *p; template <class T> operator T *() const { return (T *)p; } };
static AllocProxy nvKmsKapiCalloc(size_t n, size_t s) { return {calloc(n, s)}; }
static void nvKmsKapiFree(void *p) { free(p); }
static unsigned nvNvU32ToDpyId(unsigned d) { return d; }
static void InitNvKmsModeValidationParams(const NvKmsKapiDevice *, decltype(Val::modeValidation) *p) { p->overrides = 1; }
static void NvKmsKapiDisplayModeToKapi(const NvKmsKapiDisplayMode *m, NvKmsMode *k) { k->timings = m->t; }
static NvBool ValidateDisplayModeRgb(NvKmsKapiDevice *, NvKmsKapiDisplay, const NvKmsKapiDisplayMode *) { gCalls++; return gRgbOk; }
'''
fixture += same + '\n' + helper + wrapper
main = r'''
int main() {
    NvKmsKapiDevice dev{1, 2, nullptr}; NvKmsKapiDisplayMode m4k{}; m4k.t.pixelClockHz = 594000000; m4k.t.hVisible = 3840; m4k.t.vVisible = 2160;
    // arm 1: RGB refuses the 4K60 mode, hardware 4:2:0 validates it: the mode is offered (display no longer stuck at 4K30)
    gRgbOk = false; gSwOnly = false; gCalls = gPrefer = 0;
    assert(ValidateDisplayMode2(&dev, 5, &m4k) && gPrefer == 1);
    // arm 2: a mode RGB accepts is never retried with the 4:2:0 preference
    gRgbOk = true; gCalls = gPrefer = 0;
    assert(ValidateDisplayMode2(&dev, 5, &m4k) && gPrefer == 0 && gCalls == 1);
    // arm 3: a retry decided as SOFTWARE 4:2:0 is refused (no client converts the surface)
    gRgbOk = false; gSwOnly = true; gPrefer = 0;
    assert(!ValidateDisplayMode2(&dev, 5, &m4k) && gPrefer == 1);
    // arm 4: timing sameness is exact on every field the display sees
    NvModeTimings a{594000000,3840,4016,4104,4400,2160,2168,2178,2250,false,false}, b = a;
    assert(Y420SameTiming(&a, &b)); b.hTotal++; assert(!Y420SameTiming(&a, &b)); b = a; b.pixelClockHz--; assert(!Y420SameTiming(&a, &b));
    b = a; b.interlaced = true; assert(!Y420SameTiming(&a, &b));
    // OLD kapi (control): the RGB check alone refuses the mode
    gRgbOk = false; assert(!ValidateDisplayModeRgb(&dev, 5, &m4k));
    puts("PASS: RGB-refused modes are set as hardware 4:2:0, accepted modes untouched, software 4:2:0 refused, timing must match exactly");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d / 't.cpp').write_text(fixture + main)
    r = subprocess.run(['clang++', '-std=c++17', '-Wall', str(d / 't.cpp'), '-o', str(d / 't')], capture_output=True, text=True)
    if r.returncode: print(r.stderr[-3000:]); raise SystemExit(1)
    subprocess.run([str(d / 't')], check=True)

#!/usr/bin/env python3
"""Exercise the NVIDIA modeset interrupt lifecycle with the production repair.

Use --ogkm with a pristine NVIDIA 620.70.06 checkout. The patched and unpatched
unchanged-head branches, global disable loop and restore helper are compiled
against small display-engine doubles. No driver or display settings change.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def block(source, anchor):
    start = source.index(anchor)
    opening = source.index("{", start)
    end, depth = opening + 1, 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


FIXTURE = r'''
#include <array>
#include <cstdio>
#include <cstdlib>
#include <cstdint>
using NvU32 = uint32_t;
using NvBool = bool;
constexpr NvU32 NV_INVALID_HEAD = UINT32_MAX;
constexpr bool TRUE = true;
struct NVDevEvoRec;
struct NVDispEvoRec;
using NVDispEvoPtr = NVDispEvoRec *;
using NVDevEvoPtr = NVDevEvoRec *;
struct NVEvoUpdateState { unsigned updates = 0; };
struct RgState { bool configured = false; };
struct Head { RgState rgSemaInfo; NvU32 mask = 0; unsigned hz = 0, arms = 0; };
struct NVDispEvoRec {
    NVDevEvoRec *pDevEvo = nullptr;
    std::array<Head,4> apiHeadState;
    std::array<unsigned,4> rgInterruptCallbackList{};
};
static void marker() {}
struct Hal { void (*SetupVBlankRgSemaphoreInterrupt)() = marker; };
struct NVDevEvoRec {
    NvU32 numApiHeads = 4, displays = 2;
    Hal ownHal; Hal *hal = &ownHal;
    bool rgSemaDmaAvailable = true;
    NVDispEvoRec disp[2];
    NVDevEvoRec() { for (auto &d:disp) d.pDevEvo = this; }
};
#define FOR_ALL_EVO_DISPLAYS(p,i,d) \
    for ((i)=0; (i)<(d)->displays && (((p)=const_cast<NVDispEvoRec *>(&(d)->disp[i])),true); ++(i))
static bool nvListIsEmpty(const unsigned *n) { return *n == 0; }
static bool nvIsUpdateStateEmpty(const NVDevEvoRec *, const NVEvoUpdateState *s) { return !s->updates; }
static void nvEvoUpdateAndKickOff(NVDispEvoPtr, bool, NVEvoUpdateState *s, bool) { s->updates = 0; }
static void DisableVBlankRgSemaphoreInterrupt(NVDispEvoPtr d, NvU32 h, NVEvoUpdateState *s) {
    auto &v = d->apiHeadState[h];
    if (!v.rgSemaInfo.configured) return;
    v.rgSemaInfo.configured = false; ++s->updates;
}
static void SetupVBlankRgSemaphoreForApiHead(NVDispEvoPtr d, NvU32 a, NvU32 h, NVEvoUpdateState *s) {
    if (h == NV_INVALID_HEAD || h >= 8) { puts("FAIL: inactive hardware head programmed"); exit(1); }
    auto &v = d->apiHeadState[a];
    if (v.rgSemaInfo.configured) return;
    v.rgSemaInfo.configured = true; ++v.arms; ++s->updates;
}
DISABLE
ENABLE
struct Proposed { bool changed; NvU32 mask; unsigned hz; };
static NvU32 primary(NvU32 mask) { return mask ? __builtin_ctz(mask) : NV_INVALID_HEAD; }
static void preUpdate(NVDispEvoPtr pDispEvo, NvU32 apiHead,
                      const Proposed *pProposedApiHead, NVEvoUpdateState *updateState) {
    const NvU32 proposedPrimaryHead = primary(pProposedApiHead->mask);
    UNCHANGED
    // The existing changed-head path, whose enable call is checked in Python.
    auto &h = pDispEvo->apiHeadState[apiHead];
    h.mask = pProposedApiHead->mask; h.hz = pProposedApiHead->hz;
    if (proposedPrimaryHead != NV_INVALID_HEAD)
        EnableVBlankRgSemaphoreInterruptOneApiHead(pDispEvo, apiHead, proposedPrimaryHead, updateState);
}
using Plan = std::array<std::array<Proposed,4>,2>;
static Plan current(NVDevEvoRec &dev) {
    Plan p{};
    for (unsigned d=0;d<dev.displays;++d) for (unsigned h=0;h<dev.numApiHeads;++h) {
        auto &v=dev.disp[d].apiHeadState[h]; p[d][h]={false,v.mask,v.hz};
    }
    return p;
}
static void modeset(NVDevEvoRec &dev, const Plan &p) {
    DisableVBlankRgSemaphoreInterruptsForModeset(&dev);
    for (unsigned d=0;d<dev.displays;++d) {
        NVEvoUpdateState update;
        for (unsigned h=0;h<dev.numApiHeads;++h) preUpdate(&dev.disp[d],h,&p[d][h],&update);
    }
}
static void require(bool good, const char *message) {
    if (!good) { printf("FAIL: %s\n",message); exit(1); }
}
static void init(NVDevEvoRec &dev) {
    const unsigned rates[]={165,144,60};
    for (auto &d:dev.disp) for (unsigned h=0;h<3;++h) {
        d.apiHeadState[h]={{true},1u<<h,rates[h],0}; d.rgInterruptCallbackList[h]=1;
    }
}
static void live(const NVDevEvoRec &dev) {
    for (const auto &d:dev.disp) for (unsigned h=0;h<dev.numApiHeads;++h) {
        const auto &v=d.apiHeadState[h];
        const bool expected=v.mask && d.rgInterruptCallbackList[h] &&
            dev.hal->SetupVBlankRgSemaphoreInterrupt && dev.rgSemaDmaAvailable;
        require(v.rgSemaInfo.configured==expected,"active unchanged head lost hardware vblank, or inactive head enabled");
    }
}
int main() {
    NVDevEvoRec dev; init(dev);
    // Single-head changed modes remain on the existing path.
    dev.displays=1;
    for (unsigned h=1;h<4;++h) { dev.disp[0].apiHeadState[h]={}; dev.disp[0].rgInterruptCallbackList[h]=0; }
    auto p=current(dev); p[0][0].changed=true; p[0][0].hz=75;
    modeset(dev,p); live(dev); require(dev.disp[0].apiHeadState[0].hz==75,"single-head mode changed");
    puts("PASS: existing single-display modeset");
    dev.displays=2; init(dev); p=current(dev);
    p[0][1].changed=true; p[0][1].hz=120;
    modeset(dev,p); live(dev);
    require(dev.disp[0].apiHeadState[0].hz==165 && dev.disp[0].apiHeadState[2].hz==60,"unchanged display timing overwritten");
    for (auto &d:dev.disp) for (unsigned h=0;h<3;++h)
        require(d.apiHeadState[h].arms==1 && d.rgInterruptCallbackList[h]==1,"duplicate arm or callback replacement");
    puts("PASS: partial modeset restores all active callbacks, preserving unchanged modes and registrations");
    // Disconnect/reconnect one head and leave a callback on an inactive head.
    p=current(dev); p[0][2]={true,0,0}; dev.disp[1].rgInterruptCallbackList[3]=1;
    modeset(dev,p); live(dev);
    p=current(dev); p[0][2]={true,1u<<2,60}; modeset(dev,p); live(dev);
    puts("PASS: disconnect/reconnect and inactive head guard");
    dev.disp[0].rgInterruptCallbackList[0]=0;
    p=current(dev); p[1][1].changed=true; modeset(dev,p); live(dev);
    dev.rgSemaDmaAvailable=false; modeset(dev,p); live(dev);
    dev.rgSemaDmaAvailable=true; dev.ownHal.SetupVBlankRgSemaphoreInterrupt=nullptr;
    modeset(dev,p); live(dev);
    puts("PASS: absent callback, DMA and hardware capability retain their fallbacks");
    dev.ownHal.SetupVBlankRgSemaphoreInterrupt=marker;
    for (unsigned i=0;i<200;++i) {
        p=current(dev); const unsigned d=i%2,h=i%3;
        p[d][h].changed=true; p[d][h].hz=60+(i%4)*15;
        modeset(dev,p); live(dev);
    }
    puts("PASS: 200 alternating partial modesets across two display engines");
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ogkm", required=True, type=Path)
    parser.add_argument("--work-dir", type=Path)
    args = parser.parse_args()
    source = args.ogkm / "src/nvidia-modeset/src/nvkms-modeset.c"
    pristine = source.read_text()
    patch = ROOT / "kexts/NVRM/patches/620-vblank-restore-unchanged-heads.patch"
    with tempfile.TemporaryDirectory(dir=args.work_dir) as directory:
        work = Path(directory)
        target = work / "src/nvkms-modeset.c"
        target.parent.mkdir()
        target.write_text(pristine)
        subprocess.run(["patch", "-p1", "--fuzz=0", "--batch", "--input", str(patch)], cwd=work, check=True)
        repaired = target.read_text()
        for label, text in (("candidate", repaired), ("negative-control", pristine)):
            changed = block(text, "ApplyProposedModeSetStateOneApiHeadPreUpdate(\n")
            unchanged = block(changed, "if (!pProposedApiHead->changed)")
            assert "EnableVBlankRgSemaphoreInterruptOneApiHead(" in changed[len(changed.split(unchanged)[0])+len(unchanged):]
            disable = block(text, "static void DisableVBlankRgSemaphoreInterruptsForModeset(")
            enable = block(text, "static void EnableVBlankRgSemaphoreInterruptOneApiHead(")
            assert "DisableVBlankRgSemaphoreInterruptsForModeset(pDevEvo);" in text
            fixture = FIXTURE.replace("DISABLE", disable).replace("ENABLE", enable).replace("UNCHANGED", unchanged)
            cpp, binary = work / (label + ".cpp"), work / label
            cpp.write_text(fixture)
            subprocess.run(["clang++", "-std=c++17", "-O1", "-g", "-fsanitize=address,undefined",
                            str(cpp), "-o", str(binary)], check=True)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            if label == "candidate":
                print(result.stdout, end="")
                assert result.returncode == 0 and not result.stderr, result.stderr
            else:
                assert result.returncode == 1 and "active unchanged head lost hardware vblank" in result.stdout, result
                assert not result.stderr, result.stderr
                print("PASS: unpatched negative control loses unchanged-head hardware vblank as expected")


if __name__ == "__main__":
    main()

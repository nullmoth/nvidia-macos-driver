#!/usr/bin/env python3
"""A failed boot mode set no longer fails the framebuffer's open.

IOFramebuffer::open() marks a framebuffer whose enableController() failed as dead, WindowServer's IOServiceOpen on it
then fails (xnu's IOService::newUserClient masks the error as 0xe00002c7) and CoreDisplay aborts in
CGXMappedDisplayStart ("Driver Error: FB: x of y opened"). On 2.7.2 "applyModeSetConfig -> 0" came 0.7 s before
"IOServiceOpen failed(0xe00002c7), index(0)". NVRMNVDAFramebuffer::enableController is lifted out of nvrm-fb.cpp and
compiled against a stand-in framebuffer whose applyMode() succeeds or fails. The negative control is the 2.8.1 release's version (tag v2.8.1, which every clone has; a branch name it may not),
which returns kIOReturnError on a failed mode set."""
import os
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = "kexts/NVRMFB/fb/nvrm-fb.cpp"
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({detail.strip()[-300:]})"))


def body(src):
    head = "IOReturn NVRMNVDAFramebuffer::enableController()\n{"
    i = src.index(head)
    return src[i + len(head) - 1:src.index("\n}\n", i) + 2]


HARNESS = r'''
#include <cstdio>
typedef int IOReturn;
static const IOReturn kIOReturnSuccess = 0, kIOReturnError = (IOReturn)0xe00002bc;
#define OSSafeReleaseNULL(p) do { delete p; p = nullptr; } while (0)
struct IOWorkLoop { int addEventSource(void *) { return 0; } };
struct IOCommandGate { static IOCommandGate *commandGate(void *) { return new IOCommandGate; } };
#define FBLOG(...) (printf(__VA_ARGS__), printf("\n"))
#define FBLOGV(...) (printf(__VA_ARGS__), printf("\n"))
static unsigned long long mach_absolute_time() { return 1; }
struct NVRMNVDAFramebuffer {
    void *fDdcBrightness = nullptr; IOCommandGate *fDdcBrightnessGate = nullptr;
    IOWorkLoop *getWorkLoop() { return nullptr; }
    bool modeOk; unsigned long long fEnableAbs = 0; bool fHotplugBorn = true, fConnectFired = false; unsigned fIndex = 0;
    bool fProxy = false;   // an NVIDIA head, not the proxy for another GPU's panel (which returns before the mode set)
    int notified = 0, audio = 0;
    explicit NVRMNVDAFramebuffer(bool ok) : modeOk(ok) {}
    bool applyMode() { return modeOk; }
    void refreshDisplayAudioProperties() { audio++; }
    void notifyConnectionChanged(int) { notified++; }
    IOReturn enableController();
};
IOReturn NVRMNVDAFramebuffer::enableController() BODY
int main() {
    NVRMNVDAFramebuffer good(true), bad(false);
    IOReturn g = good.enableController(), b = bad.enableController();
    printf("RESULT good=%#x bad=%#x good_notified=%d bad_notified=%d bad_audio=%d\n", (unsigned)g, (unsigned)b,
           good.notified, bad.notified, bad.audio);
    return 0;
}
'''


def run(src):
    d = pathlib.Path(tempfile.mkdtemp(prefix="fbopen-"))
    c = d / "t.cpp"
    c.write_text(HARNESS.replace("BODY", body(src)))
    cc = subprocess.run(["clang++", "-std=c++17", "-w", str(c), "-o", str(d / "t")], capture_output=True, text=True)
    if cc.returncode:
        return "COMPILE " + cc.stderr
    return subprocess.run([str(d / "t")], capture_output=True, text=True, timeout=30).stdout


out = run((ROOT / SRC).read_text())
arm("a framebuffer whose boot mode set worked opens", "good=0 " in out, out)
arm("a framebuffer whose boot mode set failed still opens (no dead framebuffer, no WindowServer abort)", "bad=0 " in out, out)
arm("  and says so in the log", "the boot mode set failed -- opening anyway" in out, out)
arm("  and does not announce a hot-plugged display it never lit", "bad_notified=0" in out and "good_notified=1" in out, out)
arm("  nor publish its audio properties", "bad_audio=0" in out, out)

# the base file can be named directly where the tree has no git history (a build host's copy)
base_file = os.environ.get("NVRMFB_BASE_SRC")
old = (subprocess.CompletedProcess([], 0, pathlib.Path(base_file).read_text(), "") if base_file else
       subprocess.run(["git", "-C", str(ROOT), "show", f"v2.8.1:{SRC}"], capture_output=True, text=True))
if old.returncode == 0:
    o = run(old.stdout)
    arm("negative control: 2.8.1's enableController fails the open on a failed mode set", "bad=0xe00002bc" in o, o)
else:
    arm("negative control: 2.8.1's nvrm-fb.cpp is readable", False, old.stderr)
print(f"{fails} failed")
sys.exit(1 if fails else 0)

from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
begin = source.index('IOReturn NVRMNVDAFramebuffer::writeDdcBrightness(')
end = source.index('\n}\n', begin) + 3
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <cstdio>
using UInt64 = uint64_t; using UInt32 = uint32_t; using IOReturn = int;
using IOInterruptState = int;
enum {kIOReturnSuccess, kIOReturnOffline, kIOReturnCannotLock, kNvrmDdcWrite, kMillisecondScale};
static int sinkLock;
static uint64_t now = 100;
static int OSAddAtomic(int, int* value) {return *value;}
static uint64_t mach_absolute_time() {return now;}
static void clock_interval_to_deadline(unsigned ms, int, uint64_t* deadline) {*deadline = now + ms;}
static void absolutetime_to_nanoseconds(UInt64 ticks, UInt64 *ns) {*ns = ticks * 1000000;}
static IOInterruptState IOSimpleLockLockDisableInterrupt(void *) {return 0;}
static void IOSimpleLockUnlockEnableInterrupt(void *, IOInterruptState) {}
struct NVRMDdcTransaction {
    bool *flag;
    bool acquired;
    explicit NVRMDdcTransaction(bool *busy) : flag(busy), acquired(!__atomic_exchange_n(flag, true, __ATOMIC_ACQUIRE)) {}
    ~NVRMDdcTransaction() { if (acquired) __atomic_store_n(flag, false, __ATOMIC_RELEASE); }
    bool held() const { return acquired; }
};
struct NVRMDisplayGate {
    explicit NVRMDisplayGate(void *lock) { assert(lock == &sinkLock); }
    bool valid() const {return true;}
};
struct NVRMNVDAFramebuffer {
    int fStopping = 0, fDdcBrightnessCall = 0;
    bool fDpyLost = false, fEdidSwapped = false, fHidden = false;
    bool fDdcPowered = true, fDdcInFlight = false, gpuReady = true;
    UInt32 fDdcSinkGeneration = 2;
    UInt64 fDdcPending = 0, fDdcPendingAt = 0, fDdcWriteAfter = 0, scheduled = 0;
    UInt32 fDdcBusyRetries = 0, timing[5] = {};
    void *fFlipCacheLock = this;
    void *fDdcSinkLock = &sinkLock;
    bool rmGpuReady() const {return gpuReady;}
    unsigned writes = 0;
    IOReturn writeDdcBrightness();
    IOReturn ddcTransfer(unsigned op, unsigned addr, unsigned char* msg, unsigned count) {
        assert(op == kNvrmDdcWrite && addr == 0x37 && count == 7);
        const unsigned char expected[] = {0x51, 0x84, 0x03, 0x10, 0, 23, 0xbf};
        assert(!std::memcmp(msg, expected, count));
        ++writes;
        now += 22;
        return kIOReturnSuccess;
    }
    void scheduleCall(int, UInt64 deadline) {scheduled = deadline;}
    void setProperty(const char*, UInt32, int) {}
    void setProperty(const char *key, void *data, size_t size) {
        assert(!std::strcmp(key, "NVRMExternalBrightnessWriteTiming") && size == sizeof timing);
        std::memcpy(timing, data, size);
    }
};
'''
checks = r'''
int main() {
    NVRMNVDAFramebuffer fb;
    auto fire = [&]{return fb.writeDdcBrightness();};
    fb.fDdcPending = (UInt64(1) << 32) | 24; // old monitor
    assert(fire() == kIOReturnOffline && fb.writes == 0);
    fb.fDdcPending = (UInt64(2) << 32) | 24;
    fb.fDdcPendingAt = now;
    fb.fDdcWriteAfter = 140;
    assert(fire() == kIOReturnSuccess && fb.scheduled == 140 && fb.fDdcPending && fb.writes == 0);
    now = 141;
    fb.fDdcInFlight = true;
    assert(fire() == kIOReturnCannotLock && fb.fDdcPending && fb.writes == 0);
    fb.fDdcInFlight = false;
    assert(fire() == kIOReturnSuccess && fb.writes == 1 && !fb.fDdcPending && fb.fDdcWriteAfter == 203);
    const UInt32 expectedTiming[] = {41000, 22000, 40000, 1, 0};
    assert(!std::memcmp(fb.timing, expectedTiming, sizeof expectedTiming));
    fb.fDdcPending = (UInt64(2) << 32) | 24;
    now = 210;
    fb.fDdcPowered = false;
    assert(fire() == kIOReturnOffline && fb.writes == 1);
    fb.fDdcPowered = true;
    fb.gpuReady = false;
    fb.fDdcPending = (UInt64(2) << 32) | 24;
    assert(fire() == kIOReturnOffline && fb.writes == 1);
    fb.gpuReady = true;
    fb.fEdidSwapped = true;
    fb.fDdcPending = (UInt64(2) << 32) | 24;
    assert(fire() == kIOReturnOffline && fb.writes == 1);
    puts("PASS: brightness uses the sink lock, preserves I2C pacing, and drops stale or offline writes");
}
'''
with tempfile.TemporaryDirectory() as name:
    src = Path(name) / 'native.cpp'
    exe = Path(name) / 'native'
    src.write_text(fixture + source[begin:end] + checks)
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Werror', str(src), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)

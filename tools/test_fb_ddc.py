#!/usr/bin/env python3
"""DDC/CI through the framebuffer's I2C bus: request translation, address whitelist, reply framing, opt-in on both kexts.

The real doI2CRequest is compiled from nvrm-fb.cpp against a mocked transfer; the ABI header functions are compiled as
they are. Arms are named. The control is the old driver: it published no I2C bus and had no doI2CRequest, so an
IOFBCopyI2CInterfaceForBus client found nothing."""
import filecmp, pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
rm = (root / 'kexts/NVRM/nvrm_ddc.cpp').read_text()
hook = (root / 'kexts/NVRM/NVRM.cpp').read_text()
assert filecmp.cmp(root / 'kexts/NVRMFB/nvrm_ddc_abi.h', root / 'kexts/NVRM/nvrm_ddc_abi.h', shallow=False), 'the two copies of the DDC ABI header differ'
assert 'IOReturn NVRMNVDAFramebuffer::doI2CRequest(' in fb and 'kIOFBI2CInterfaceInfoKey' in fb, 'the framebuffer no longer publishes an I2C bus'
assert 'PE_parse_boot_argn("nvrmfbddc"' in fb and 'PE_parse_boot_argn("nvrmfbddc"' in rm, 'DDC must be opt-in on both kexts'
assert fb.index('publishDdcBus();') < fb.index('if (!super::start(provider))'), 'the bus must be published before super::start'
assert 'isEqualTo("NVRMDdcTransfer")' in hook and 'nvrm_ddc_transfer' in hook, 'NVRM does not answer NVRMDdcTransfer'
assert 'ddcSetPowered(ordinal != 0)' in fb, 'isPowered() is never made true'
s = fb.index('IOReturn NVRMNVDAFramebuffer::doI2CRequest(')
method = fb[s:fb.index('\n}\n', s) + 3]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>
#include "nvrm_ddc_abi.h"
typedef int IOReturn; typedef uint32_t UInt32; typedef uint32_t IOOptionBits;
enum { kIOReturnSuccess=0, kIOReturnUnsupported=-1, kIOReturnUnsupportedMode=-2, kIOReturnBadArgument=-3, kIOReturnNoDevice=-4, kIOReturnBadArgumentX=-9 };
enum { kIOI2CNoTransactionType=0, kIOI2CSimpleTransactionType=1, kIOI2CDDCciReplyTransactionType=2, kIOI2CUseSubAddressCommFlag=2 };
struct IOI2CBusTiming; struct IOI2CRequest {
    IOOptionBits sendTransactionType, replyTransactionType; uint32_t sendAddress, replyAddress; uint8_t sendSubAddress, replySubAddress;
    uint64_t minReplyDelay; IOReturn result; IOOptionBits commFlags; uint32_t sendBytes; uint32_t replyBytes;
    uintptr_t sendBuffer, replyBuffer; void (*completion)(IOI2CRequest *);
};
static void absolutetime_to_nanoseconds(uint64_t a, uint64_t *n) { *n = a; }
static unsigned gSlept; static void IOSleep(unsigned ms) { gSlept += ms; }
struct Call { unsigned op, addr, len; std::vector<unsigned char> w; };
static std::vector<Call> gCalls; static IOReturn gFail = 0; static std::vector<unsigned char> gReply;
struct FB {
    bool fDdcOn = true, fDpyLost = false; void *fKms = this;
    IOReturn ddcTransfer(unsigned op, unsigned addr, unsigned char *d, unsigned len) {
        Call c{op, addr, len, {}};
        if (op == kNvrmDdcWrite) c.w.assign(d, d + len); else memcpy(d, gReply.data(), len < gReply.size() ? len : gReply.size());
        gCalls.push_back(c); return gFail;
    }
    bool fDdcFunction = false;
    IOReturn doDdcBrightnessRequest(UInt32, IOI2CBusTiming *, IOI2CRequest *) { return kIOReturnBadArgumentX; }
    IOReturn doI2CRequest(UInt32 bus, IOI2CBusTiming *, IOI2CRequest *rq);
};
'''
checks = r'''
static IOI2CRequest req(unsigned char *w, unsigned wn, unsigned char *r, unsigned rn) {
    IOI2CRequest q{}; q.sendTransactionType = wn ? kIOI2CSimpleTransactionType : 0; q.sendAddress = 0x6E; q.sendBytes = wn; q.sendBuffer = (uintptr_t)w;
    q.replyTransactionType = rn ? kIOI2CDDCciReplyTransactionType : 0; q.replyAddress = 0x6F; q.replyBytes = rn; q.replyBuffer = (uintptr_t)r; return q;
}
int main() {
    FB fb;
    fb.fDdcFunction = true;
    assert(fb.doI2CRequest(0, nullptr, nullptr) == kIOReturnBadArgumentX);
    fb.fDdcFunction = false;
    unsigned char w[6] = {0x51, 0x82, 0x01, 0x10, 0x00, 0x00}, r[40] = {};
    // DDC/CI "get VCP 0x10": write 0x6E, wait, read 0x6F; the reply says 8 payload bytes of 40 asked for
    gReply = {0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x32, 0xAA};
    IOI2CRequest q = req(w, 6, r, 40);
    assert(fb.doI2CRequest(0, nullptr, &q) == kIOReturnSuccess && q.result == kIOReturnSuccess);
    assert(gCalls.size() == 2 && gCalls[0].op == kNvrmDdcWrite && gCalls[0].addr == 0x37 && gCalls[0].w[1] == 0x82);   // 0x6E -> 0x37
    assert(gCalls[1].op == kNvrmDdcRead && gCalls[1].addr == 0x37);
    assert(gSlept >= 40);                              // the monitor gets its 40 ms before the reply is read
    assert(q.replyBytes == 11);                        // 0x88 & 0x7f = 8 payload + 3 framing bytes
    // set VCP: write only, no reply, no wait
    gCalls.clear(); gSlept = 0; IOI2CRequest s = req(w, 6, nullptr, 0);
    assert(fb.doI2CRequest(0, nullptr, &s) == kIOReturnSuccess && gCalls.size() == 1 && gSlept == 0);
    // an address that is not the DDC/CI channel or the EDID EEPROM is refused before it reaches the display
    gCalls.clear(); IOI2CRequest bad = req(w, 6, nullptr, 0); bad.sendAddress = 0xA0 /* HDCP-adjacent 0x50<<1 is allowed; 0xA0 = 0x50 */; 
    bad.sendAddress = 0x74;                            // 0x3A
    assert(fb.doI2CRequest(0, nullptr, &bad) != kIOReturnSuccess && gCalls.empty() && bad.result == kIOReturnBadArgument);
    IOI2CRequest dir = req(w, 6, nullptr, 0); dir.sendAddress = 0x6F;      // read bit on a write
    assert(fb.doI2CRequest(0, nullptr, &dir) != kIOReturnSuccess && gCalls.empty());
    IOI2CRequest sub = req(w, 6, nullptr, 0); sub.commFlags = kIOI2CUseSubAddressCommFlag;
    assert(fb.doI2CRequest(0, nullptr, &sub) == kIOReturnUnsupportedMode && gCalls.empty());
    IOI2CRequest big = req(w, 200, nullptr, 0);
    assert(fb.doI2CRequest(0, nullptr, &big) != kIOReturnSuccess && gCalls.empty());
    assert(fb.doI2CRequest(1, nullptr, &q) == kIOReturnUnsupported);           // only bus 0
    fb.fDpyLost = true; assert(fb.doI2CRequest(0, nullptr, &q) == kIOReturnUnsupported); fb.fDpyLost = false;
    FB off; off.fDdcOn = false; assert(off.doI2CRequest(0, nullptr, &q) == kIOReturnUnsupported && gCalls.empty());   // opt-in
    gFail = -5; IOI2CRequest dead = req(w, 6, nullptr, 0);                     // the display does not answer
    assert(fb.doI2CRequest(0, nullptr, &dead) == kIOReturnNoDevice); gFail = 0;
    // the EDID EEPROM takes only the one-byte read offset; a longer write would program it
    gCalls.clear(); unsigned char off1[1] = {0}, eep[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    IOI2CRequest rd = req(off1, 1, nullptr, 0); rd.sendAddress = 0xA0;
    assert(fb.doI2CRequest(0, nullptr, &rd) == kIOReturnSuccess && gCalls.size() == 1 && gCalls[0].addr == 0x50);
    gCalls.clear(); IOI2CRequest prog = req(eep, 8, nullptr, 0); prog.sendAddress = 0xA0;
    assert(fb.doI2CRequest(0, nullptr, &prog) != kIOReturnSuccess && gCalls.empty() && prog.result == kIOReturnBadArgument);
    assert(nvrmDdcWriteLenAllowed(0x37, 6) && nvrmDdcWriteLenAllowed(0x50, 1) && !nvrmDdcWriteLenAllowed(0x50, 2));
    // ABI helpers
    assert(nvrmDdcAddr7(0x6E, false) == 0x37 && nvrmDdcAddr7(0x6F, true) == 0x37 && nvrmDdcAddr7(0xA0, false) == 0x50);
    assert(nvrmDdcAddr7(0x6F, false) == 0 && nvrmDdcAddr7(0xA8, false) == 0);
    unsigned char unframed[4] = {1, 2, 3, 4}; assert(nvrmDdcReplyLength(unframed, 4) == 4);
    assert(nvrmDdcReplyDelayMs(0) == 40 && nvrmDdcReplyDelayMs(100000000ull) == 100 && nvrmDdcReplyDelayMs(9000000000ull) == 500);
    puts("PASS: DDC/CI request translation, address whitelist, 40 ms reply wait, reply framing, dead display, opt-in");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / 't.cpp').write_text(fixture + method.replace('NVRMNVDAFramebuffer::', 'FB::').replace('struct IOI2CBusTiming *', 'IOI2CBusTiming *').replace('struct IOI2CRequest *rq', 'IOI2CRequest *rq') + checks)
    subprocess.run(['clang++', '-std=c++17', '-Wall', f'-I{root}/kexts/NVRMFB', str(d / 't.cpp'), '-o', str(d / 't')], check=True)
    subprocess.run([str(d / 't')], check=True)

/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// DDC/CI transfers between NVRMFB (the I2C interface macOS sees) and NVRM (the resource manager). Identical copies live in
// kexts/NVRMFB and kexts/NVRM; tools/test_fb_ddc.py checks they match. NVRMFB calls
// NVRM::callPlatformFunction("NVRMDdcTransfer", &request); NVRM resolves the display's DDC port, finds the I2C adapter the
// resource manager registered for it, and runs one write or one read through the resource manager's own I2C transfer, which
// takes its locks and handles both DisplayPort AUX and plain DDC lines.
#pragma once
#define NVRM_DDC_FN "NVRMDdcTransfer"
enum { kNvrmDdcMax = 128 };
enum { kNvrmDdcWrite = 1, kNvrmDdcRead = 2 };
struct NVRMDdcRequest {
    unsigned gpuId;            // NVKMS device GPU id (the framebuffer's fGpuId)
    unsigned displayId;        // NVKMS display handle (the resource manager's display id)
    unsigned op;               // kNvrmDdcWrite or kNvrmDdcRead
    unsigned addr7;            // 7-bit slave address
    unsigned len;              // bytes to write, or bytes to read
    unsigned status;           // out: resource manager status (0 = ok)
    unsigned char data[kNvrmDdcMax];
};
// The slave addresses a framebuffer I2C client may reach: the DDC/CI channel and the EDID EEPROM. Nothing else on a
// display's DDC lines (scalers' firmware, HDCP ports) is reachable.
static inline bool nvrmDdcAddrAllowed(unsigned addr7) { return addr7 == 0x37 || addr7 == 0x50; }
// IOI2CRequest addresses are 8-bit: write 0x6E / read 0x6F for DDC/CI. Returns the 7-bit address, or 0 when the address is
// not an allowed one or its direction bit contradicts the part of the request it is used for.
static inline unsigned nvrmDdcAddr7(unsigned addr8, bool forRead)
{
    if (((addr8 & 1) != 0) != forRead) return 0;
    const unsigned a = addr8 >> 1;
    return nvrmDdcAddrAllowed(a) ? a : 0;
}
// DDC/CI replies carry their own length: byte 1 is 0x80 | payload length, and the frame is payload + 3 bytes (source,
// length, checksum). Returns how many of `got` bytes belong to the frame, or `got` when the reply is not framed.
static inline unsigned nvrmDdcReplyLength(const unsigned char *buf, unsigned got)
{
    if (got < 3 || !(buf[1] & 0x80)) return got;
    const unsigned frame = (unsigned)(buf[1] & 0x7f) + 3;
    return frame < got ? frame : got;
}
// Minimum wait between the write and the read of a DDC/CI exchange: the spec asks monitors for 40 ms; clients may ask for
// more, never more than 500 ms here.
static inline unsigned nvrmDdcReplyDelayMs(unsigned long long requestedNs)
{
    unsigned long long ms = requestedNs / 1000000ull;
    if (ms < 40) ms = 40;
    if (ms > 500) ms = 500;
    return (unsigned)ms;
}

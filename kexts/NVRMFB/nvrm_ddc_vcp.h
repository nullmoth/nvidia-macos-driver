#ifndef NVRM_DDC_VCP_H
#define NVRM_DDC_VCP_H

#include <stdint.h>
#include <stddef.h>

constexpr uint8_t kNvrmVcpBrightness = 0x10;
constexpr uint8_t kNvrmVcpSpeakerVolume = 0x62;

enum class NVRMVcpReply { retry, unsupported, valid, muted };

inline void nvrmVcpGet(uint8_t code, uint8_t (&message)[5])
{
    message[0] = 0x51;
    message[1] = 0x82;
    message[2] = 0x01;
    message[3] = code;
    message[4] = 0x6e ^ message[0] ^ message[1] ^ message[2] ^ message[3];
}

inline void nvrmVcpSet(uint8_t code, uint16_t value, uint8_t (&message)[7])
{
    message[0] = 0x51;
    message[1] = 0x84;
    message[2] = 0x03;
    message[3] = code;
    message[4] = value >> 8;
    message[5] = value & 0xff;
    message[6] = 0x6e;
    for (unsigned i = 0; i < 6; ++i) message[6] ^= message[i];
}

inline NVRMVcpReply nvrmVcpParse(uint8_t code, const uint8_t *reply, size_t size,
                                uint32_t &value, uint32_t &maximum)
{
    if (size != 11 || reply[0] != 0x6e || reply[1] != 0x88 || reply[2] != 2 || reply[4] != code)
        return NVRMVcpReply::retry;
    uint8_t checksum = 0x50;
    for (size_t i = 0; i < size; ++i) checksum ^= reply[i];
    if (checksum) return NVRMVcpReply::retry;
    if (reply[3] == 1) return NVRMVcpReply::unsupported;
    if (reply[3] || reply[5]) return NVRMVcpReply::retry;
    const uint32_t max = (uint32_t(reply[6]) << 8) | reply[7];
    const uint32_t current = (uint32_t(reply[8]) << 8) | reply[9];
    // MCCS 2.2 reserves 0 and 255 for fixed/default volume and mute.
    if (code == kNvrmVcpSpeakerVolume) {
        if (!current) return NVRMVcpReply::unsupported;
        if (current == 255 && max) {
            // The reply carries no prior level. Start the control at its minimum.
            value = 1;
            maximum = max >= 255 ? 254 : max;
            return NVRMVcpReply::muted;
        }
        if (current > 255) return NVRMVcpReply::retry;
    }
    if (!max || current > max) return NVRMVcpReply::retry;
    value = current;
    maximum = code == kNvrmVcpSpeakerVolume && max >= 255 ? 254 : max;
    return NVRMVcpReply::valid;
}

#endif

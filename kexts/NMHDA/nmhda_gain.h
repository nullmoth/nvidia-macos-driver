// NMHDA: software volume and mute for the PCM an NVIDIA HDMI/DisplayPort audio sink plays.
//
// A display's audio sink has no codec volume of its own, so macOS shows no volume slider for it. NMHDA publishes the
// standard level and mute controls on the sink's audio engine and applies their value here, to the samples the engine
// writes into the controller's ring. Plain C with no kernel types, so the arithmetic is tested on the host
// (tools/test_nmhda_gain.py).
#ifndef NMHDA_GAIN_H
#define NMHDA_GAIN_H
#include <stdint.h>
#include <string.h>

// Control range published to Core Audio: 0 (silent) to 100 (unity).
#define NMHDA_LEVEL_MIN 0
#define NMHDA_LEVEL_MAX 100
#define NMHDA_UNITY_Q16 65536u

// Gain in 16.16 fixed point for a level. The curve is the cube of the level fraction: loudness is perceived roughly
// logarithmically, so a linear slider is far too loud at its low end; the cube puts 50 at -18 dB and 10 at -60 dB.
// Level 100 is exactly unity (the samples are not touched), level 0 is exactly silent.
static inline uint32_t nmhda_gain_q16(int level, int muted)
{
    if (muted || level <= NMHDA_LEVEL_MIN)
        return 0;
    if (level >= NMHDA_LEVEL_MAX)
        return NMHDA_UNITY_Q16;
    uint64_t l = (uint64_t)level;
    return (uint32_t)((l * l * l * NMHDA_UNITY_Q16 + 500000u) / 1000000u);
}

// Scales 'nframes' frames starting at frame 'first' of the buffer at 'base' (the engine's sample buffer: little-endian,
// interleaved, 'channels' per frame) by gain_q16. bit_width is the container width of a sample in bits: 16, 24 (packed
// in three bytes) or 32; is_float selects IEEE 754 single precision for a 32-bit container. Returns 0 when the format
// is handled (including unity gain, which leaves the buffer as it is) and -1 when it is not, with the buffer untouched.
static inline int nmhda_gain_apply(void *base, uint32_t first, uint32_t nframes, uint32_t channels, uint32_t bit_width,
                                   int is_float, uint32_t gain_q16)
{
    if (base == NULL || channels == 0 || channels > 32)
        return -1;
    if (!((bit_width == 16 || bit_width == 24 || bit_width == 32) && (!is_float || bit_width == 32)))
        return -1;
    if (gain_q16 >= NMHDA_UNITY_Q16)
        return 0;
    const uint64_t count = (uint64_t)nframes * channels;
    const uint64_t start = (uint64_t)first * channels;
    const uint32_t bytes = bit_width / 8;
    uint8_t *p = (uint8_t *)base + start * bytes;
    if (gain_q16 == 0) {
        memset(p, 0, (size_t)(count * bytes));
        return 0;
    }
    if (bit_width == 16) {
        for (uint64_t i = 0; i < count; i++, p += 2) {
            int16_t s;
            memcpy(&s, p, 2);
            s = (int16_t)(((int32_t)s * (int64_t)gain_q16 + 32768) >> 16);
            memcpy(p, &s, 2);
        }
    } else if (bit_width == 24) {
        for (uint64_t i = 0; i < count; i++, p += 3) {
            int32_t s = (int32_t)(((uint32_t)p[0] << 8) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 24)) >> 8;
            s = (int32_t)(((int64_t)s * gain_q16 + 32768) >> 16);
            p[0] = (uint8_t)s;
            p[1] = (uint8_t)(s >> 8);
            p[2] = (uint8_t)(s >> 16);
        }
    } else if (is_float) {
        const float g = (float)gain_q16 / (float)NMHDA_UNITY_Q16;
        for (uint64_t i = 0; i < count; i++, p += 4) {
            float s;
            memcpy(&s, p, 4);
            s *= g;
            memcpy(p, &s, 4);
        }
    } else {
        for (uint64_t i = 0; i < count; i++, p += 4) {
            int32_t s;
            memcpy(&s, p, 4);
            s = (int32_t)(((int64_t)s * gain_q16 + 32768) >> 16);
            memcpy(p, &s, 4);
        }
    }
    return 0;
}
#endif

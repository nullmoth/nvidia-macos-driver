#ifndef NVRM_AUDIO_MATCH_H
#define NVRM_AUDIO_MATCH_H
#include <stdint.h>
#include <stddef.h>

static inline int nvrm_audio_codec_valid(const uint8_t *data, size_t size)
{
    return data && size == 4 && data[0] < 16 && data[1] && data[2] && !data[3];
}

static inline int nvrm_audio_port_number(uint8_t pin, unsigned external, uint8_t *port)
{
    if (!port) return 0;
    if (external <= 1) { *port = 0; return 1; }
    if (pin < 4) return 0;
    *port = pin - 4;
    return 1;
}

static inline int nvrm_audio_snapshot_current(uint32_t version, uint64_t sampled,
                                              uint64_t current, unsigned unavailable,
                                              unsigned truncated, unsigned count)
{
    return version == 1 && sampled == current && !unavailable && !truncated && count <= 64;
}

static inline int nvrm_audio_fallback_allowed(unsigned external, int mst, int config_error)
{
    return external == 1 && !mst && !config_error;
}

struct nvrm_audio_identity {
    uint8_t product[4], connection, name_size, sad_count;
    uint8_t name[31], sad[45];
};

static inline int nvrm_audio_identity_parse(const uint8_t *eld, size_t size,
                                           struct nvrm_audio_identity *out)
{
    if (!eld || size < 20 || (eld[0] >> 3) != 2) return 0;
    size_t total = 4u + 4u * eld[2];
    unsigned name = eld[4] & 31, count = eld[5] >> 4;
    if (total > size || total < 20u + name + count * 3u || !count) return 0;
    for (size_t i = 0; i < sizeof *out; ++i) ((uint8_t *)out)[i] = 0;
    for (unsigned i = 0; i < 4; ++i) out->product[i] = eld[16 + i];
    out->connection = (eld[5] >> 2) & 3;
    if (out->connection > 1) return 0;
    for (unsigned i = 0; i < name && eld[20 + i] && eld[20 + i] != '\n'; ++i)
        out->name[out->name_size++] = eld[20 + i];
    while (out->name_size && out->name[out->name_size - 1] == ' ') --out->name_size;
    // Canonicalize the SAD set: wire order and duplicate descriptors are not identity.
    for (unsigned i = 0; i < count; ++i) {
        const uint8_t *sad = eld + 20 + name + i * 3;
        unsigned at = 0;
        for (; at < out->sad_count; ++at) {
            const uint8_t *old = out->sad + at * 3;
            unsigned a = ((unsigned)sad[0] << 16) | ((unsigned)sad[1] << 8) | sad[2];
            unsigned b = ((unsigned)old[0] << 16) | ((unsigned)old[1] << 8) | old[2];
            if (a <= b) break;
        }
        if (at < out->sad_count && out->sad[at*3] == sad[0] &&
            out->sad[at*3+1] == sad[1] && out->sad[at*3+2] == sad[2]) continue;
        for (unsigned j = out->sad_count * 3; j > at * 3; --j)
            out->sad[j + 2] = out->sad[j - 1];
        for (unsigned j = 0; j < 3; ++j) out->sad[at*3+j] = sad[j];
        ++out->sad_count;
    }
    return 1;
}

static inline int nvrm_audio_identity_equal(const struct nvrm_audio_identity *a,
                                            const struct nvrm_audio_identity *b)
{
    for (size_t i = 0; i < sizeof *a; ++i)
        if (((const uint8_t *)a)[i] != ((const uint8_t *)b)[i]) return 0;
    return 1;
}

// A match must be unique in both directions, independently of enumeration order.
static inline int nvrm_audio_unique_match(const struct nvrm_audio_identity *displays,
                                          size_t display_count, size_t display,
                                          const struct nvrm_audio_identity *pins,
                                          size_t pin_count)
{
    if (display >= display_count) return -1;
    int match = -1;
    for (size_t p = 0; p < pin_count; ++p) {
        if (!pins[p].sad_count || !nvrm_audio_identity_equal(displays + display, pins + p)) continue;
        if (match >= 0) return -1;
        match = (int)p;
    }
    if (match < 0) return -1;
    for (size_t d = 0; d < display_count; ++d)
        if (d != display && nvrm_audio_identity_equal(displays + d, pins + match)) return -1;
    return match;
}
#endif

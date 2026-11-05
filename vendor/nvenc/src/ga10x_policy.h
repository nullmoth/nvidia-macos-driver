// Local GA10x integration policy; hardware submission lives in enc.c.
#pragma once
#include <stdbool.h>
#include <string.h>

// The outer NVMTL_NVENCVT loader remains opt-in. An explicit 0 retains the
// previous decoder-only GA10x behavior; invalid values do not enable encoders.
static inline bool nvenc_ga10x_encode_enabled(const char *setting)
{
    return !setting || !strcmp(setting, "1");
}

// H.264 decoding has a separate rollout from HEVC/MPEG decoding. Require the
// actual consumer Ampere NVDEC class, even when the PCI family matched.
static inline bool nvenc_ga10x_h264_decode_enabled(const char *setting, unsigned nvdec_class)
{
    return nvdec_class == 0xc7b0 && (!setting || !strcmp(setting, "1"));
}

// Consumer GA10x has one NVENC. Do not probe an absent NVENC1 for a second
// session. Reject an explicit unsupported engine instead of masking its index.
static inline int nvenc_ga10x_engine(const char *setting)
{
    return !setting || !strcmp(setting, "0") ? 0 : -1;
}

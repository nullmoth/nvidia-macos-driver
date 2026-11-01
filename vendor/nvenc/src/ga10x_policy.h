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

// Consumer GA10x has one NVENC. Do not probe an absent NVENC1 for a second
// session. Reject an explicit unsupported engine instead of masking its index.
static inline int nvenc_ga10x_engine(const char *setting)
{
    return !setting || !strcmp(setting, "0") ? 0 : -1;
}

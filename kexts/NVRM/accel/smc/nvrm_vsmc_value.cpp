/*
 * Local implementation of the small VirtualSMCValue ABI surface needed by
 * NVAccel's read-only telemetry keys.
 *
 * init(data), get, and update follow acidanthera/VirtualSMC 1.3.8's
 * VirtualSMC/kern_value.cpp (Copyright 2017 vit9696; BSD-3-Clause).
 * Byte copies are written locally to avoid linking against Lilu helpers.
 * See vendor/LICENSE.VirtualSMC.txt.
 */

#include "nvrm_vsmc_sdk.hpp"

#include <stdarg.h>

extern "C" void NVRMVirtualSMCSDKLog(const char *format, ...) {
    va_list args;
    va_start(args, format);
    IOLogv(format, args);
    va_end(args);
}

bool NVRMVirtualSMCValue::init(const SMC_DATA *src, SMC_DATA_SIZE srcSize,
                               SMC_KEY_TYPE srcType, SMC_KEY_ATTRIBUTES srcAttr,
                               NVRMSerializeLevel srcSerialization) {
    if (srcSize > SMC_MAX_DATA_SIZE) return false;

    if (src) {
        for (SMC_DATA_SIZE i = 0; i < srcSize; ++i) data[i] = src[i];
    }

    size = srcSize;
    type = srcType;
    attr = srcAttr;
    serializeLevel = srcSerialization;
    return true;
}

const SMC_DATA *NVRMVirtualSMCValue::get(SMC_DATA_SIZE &outSize) const {
    outSize = size;
    return data;
}

SMC_RESULT NVRMVirtualSMCValue::update(const SMC_DATA *src) {
    for (SMC_DATA_SIZE i = 0; i < size; ++i) data[i] = src[i];
    return SmcSuccess;
}

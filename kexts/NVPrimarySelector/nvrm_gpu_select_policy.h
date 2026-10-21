/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include <stdint.h>

namespace NvPrimaryGpu {
// A firmware console must fit entirely inside the GPU's memory aperture.
// Subtraction avoids overflow when firmware reports an invalid high address.
inline bool contains(uint64_t base, uint64_t length, uint64_t console, uint64_t bytes)
{
    return base && length && console >= base && bytes && bytes <= length &&
           console - base <= length - bytes;
}

}

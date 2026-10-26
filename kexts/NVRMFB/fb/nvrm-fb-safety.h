/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#pragma once
static inline bool nvrmCheckedPitch(unsigned width, unsigned alignment, unsigned *out)
{
    if (!out || !width || !alignment || (alignment & (alignment - 1))) return false;
    const unsigned long long pitch = ((unsigned long long)width * 4 + alignment - 1) &
                                      ~((unsigned long long)alignment - 1);
    if (pitch > 0xffffffffULL) return false;
    *out = (unsigned)pitch;
    return true;
}
static inline bool nvrmKmsRasterValid(unsigned width, unsigned height)
{
    return width && height && width <= 0xffffu && height <= 0xffffu;
}
static inline unsigned long long nvrmApertureBytes(unsigned pitch, unsigned height,
                                                  unsigned long long owned)
{
    if (!pitch || !height) return 0;
    const unsigned long long need = (unsigned long long)pitch * height;
    if (need > owned) return 0;
    const unsigned long long spare = owned - need;
    return need + (spare < 128 ? spare : 128);
}

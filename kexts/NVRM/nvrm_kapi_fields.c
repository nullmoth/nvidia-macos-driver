/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// NVIDIA's open-gpu-kernel-modules headers (third-party) carry their own integer-conversion warnings under the kernel
// compiler's defaults; the exception is scoped to these includes and our code below stays checked.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wsign-conversion"
#pragma clang diagnostic ignored "-Wimplicit-int-conversion"
#pragma clang diagnostic ignored "-Wshorten-64-to-32"
#include "nvkms-kapi-internal.h"
#pragma clang diagnostic pop

unsigned int nvrm_kapi_mem_handle(const void *mem)
{
    const struct NvKmsKapiMemory *m = (const struct NvKmsKapiMemory *)mem;
    return m ? (unsigned int)m->hRmHandle : 0u;
}

int nvrm_kapi_dev_handles(const void *dev, unsigned int *hClient,
                          unsigned int *hDevice, unsigned int *hSubDevice)
{
    const struct NvKmsKapiDevice *d = (const struct NvKmsKapiDevice *)dev;
    if (!d) return 0;
    if (hClient)    *hClient    = (unsigned int)d->hRmClient;
    if (hDevice)    *hDevice    = (unsigned int)d->hRmDevice;
    if (hSubDevice) *hSubDevice = (unsigned int)d->hRmSubDevice;
    return 1;
}

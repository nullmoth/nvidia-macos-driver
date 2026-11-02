/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */

#pragma once

#include <nvtypes.h>

//
// This file was generated with FINN, an NVIDIA coding tool.
// Source file:      ctrl/ctrl208f/ctrl208fce.finn
//

#include "ctrl/ctrl208f/ctrl208fbase.h"

/*
 * NV208F_CTRL_CMD_CE_GET_NUM_SHIM_INSTANCES
 *
 * This command returns the number of CE shims on the GPU.
 *
 * [out] numShimInstances
 *    Returns the number of CE shims on the GPU.
 */
#define NV208F_CTRL_CMD_CE_GET_NUM_SHIM_INSTANCES (0x208f1c01) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_GET_NUM_SHIM_INSTANCES_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_GET_NUM_SHIM_INSTANCES_PARAMS_MESSAGE_ID (0x1U)

typedef struct NV208F_CTRL_CE_GET_NUM_SHIM_INSTANCES_PARAMS {
    NvU32 numShimInstances;
} NV208F_CTRL_CE_GET_NUM_SHIM_INSTANCES_PARAMS;

/*
 * NV208F_CTRL_CMD_CE_GET_MAX_NUM_PCES_PER_SHIM
 *
 * This command returns the maximum number of PCEs per CE shim on the GPU.
 *
 * [out] numPces
 *    Returns the maximum number of PCEs on the GPU.
 */
#define NV208F_CTRL_CMD_CE_GET_MAX_NUM_PCES_PER_SHIM (0x208f1c02) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_GET_MAX_NUM_PCES_PER_SHIM_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_GET_MAX_NUM_PCES_PER_SHIM_PARAMS_MESSAGE_ID (0x2U)

typedef struct NV208F_CTRL_CE_GET_MAX_NUM_PCES_PER_SHIM_PARAMS {
    NvU32 numPces;
} NV208F_CTRL_CE_GET_MAX_NUM_PCES_PER_SHIM_PARAMS;

/*
 * NV208F_CTRL_CMD_CE_GET_AVAILABLE_LCE_MASK
 *
 * This command gets the mask of LCEs that are available for the specified CE shim.
 *
 * [in] shimInstance
 *    Specify which CE shim instance to operate on.
 * [out] lceMask
 *    Returns a 32-bit mask of which LCEs in the given shim are available.
 */
#define NV208F_CTRL_CMD_CE_GET_AVAILABLE_LCE_MASK (0x208f1c03) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_GET_AVAILABLE_LCE_MASK_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_GET_AVAILABLE_LCE_MASK_PARAMS_MESSAGE_ID (0x3U)

typedef struct NV208F_CTRL_CE_GET_AVAILABLE_LCE_MASK_PARAMS {
    NvU32 shimInstance;
    NvU32 lceMask;
} NV208F_CTRL_CE_GET_AVAILABLE_LCE_MASK_PARAMS;

/*
 * NV208F_CTRL_CMD_CE_GET_FLOORSWEPT_PCE_MASK
 *
 * This command gets the mask of floorswept PCEs for the specified CE shim.
 *
 * [in] shimInstance
 *    Specify which CE shim instance to operate on.
 * [out] pceMask
 *    Returns a 32-bit mask of which PCEs in the given shim are available.
 */
#define NV208F_CTRL_CMD_CE_GET_FLOORSWEPT_PCE_MASK (0x208f1c04) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_GET_FLOORSWEPT_PCE_MASK_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_GET_FLOORSWEPT_PCE_MASK_PARAMS_MESSAGE_ID (0x4U)

typedef struct NV208F_CTRL_CE_GET_FLOORSWEPT_PCE_MASK_PARAMS {
    NvU32 shimInstance;
    NvU32 pceMask;
} NV208F_CTRL_CE_GET_FLOORSWEPT_PCE_MASK_PARAMS;

/*
 * NV208F_CTRL_CMD_CE_ECC_INJECTION_SUPPORTED
 *
 * Reports if error injection is supported for the CE
 *
 * bCorrectableSupported [out]:
 *      Boolean value that shows if correcatable errors can be injected.
 * bUncorrectableSupported [out]:
 *      Boolean value that shows if uncorrecatable errors can be injected.
 *
 * Return values:
 *      NV_OK
 *      NV_ERR_NOT_SUPPORTED
 *      NV_ERR_INVALID_ARGUMENT
 *      NV_ERR_INSUFFICIENT_PERMISSIONS
 */
#define NV208F_CTRL_CMD_CE_ECC_INJECTION_SUPPORTED (0x208f1c05) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_ECC_INJECTION_SUPPORTED_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_ECC_INJECTION_SUPPORTED_PARAMS_MESSAGE_ID (0x5U)

typedef struct NV208F_CTRL_CE_ECC_INJECTION_SUPPORTED_PARAMS {
    NvBool bCorrectableSupported;
    NvBool bUncorrectableSupported;
} NV208F_CTRL_CE_ECC_INJECTION_SUPPORTED_PARAMS;

/*
 * NV208F_CTRL_CMD_CE_ECC_ENABLE_ERROR_INJECTION
 *
 * This ctrl call enables CE ECC error injection for a given group and PCE.
 * A copy using the target PCE is required to inject the error.
 *
 * Parameters:
 *
 * dietletLoc [in]
 *   Specifies the CE dielet location where the injection will occur. Specify using NV2080_ECC_LOCATION_INFO_DIELET_*.
 * pceIdx [in]
 *   Specifies the PCE index where the injection will occur.
 * errorType [in]
 *   Specifies whether the injected error will be correctable or uncorrectable.
 *   Correctable errors have no effect on running programs while uncorrectable
 *   errors will cause all channels to be torn down.
 *
 * Possible status values returned are:
 *   NV_OK
 *   NV_ERR_NOT_SUPPORTED
 *   NV_ERR_INVALID_ARGUMENT
 *   NV_ERR_INSUFFICIENT_PERMISSIONS
 */
#define NV208F_CTRL_CMD_CE_ECC_ENABLE_ERROR_INJECTION (0x208f1c06) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | NV208F_CTRL_CE_ECC_ENABLE_ERROR_INJECTION_PARAMS_MESSAGE_ID" */

#define NV208F_CTRL_CE_ECC_ENABLE_ERROR_INJECTION_PARAMS_MESSAGE_ID (0x6U)

typedef struct NV208F_CTRL_CE_ECC_ENABLE_ERROR_INJECTION_PARAMS {
    NvU32 dietletLoc;
    NvU32 pceIdx;
    NvU8  errorType;
} NV208F_CTRL_CE_ECC_ENABLE_ERROR_INJECTION_PARAMS;



/*
 * NV208F_CTRL_CMD_CE_ECC_DISABLE_ERROR_INJECTION
 *
 * This ctrl call disables CE ECC error injection for all groups and PCEs.
 *
 * Possible status values returned are:
 *   NV_OK
 *   NV_ERR_NOT_SUPPORTED
 *   NV_ERR_INVALID_ARGUMENT
 *   NV_ERR_INSUFFICIENT_PERMISSIONS
 */
#define NV208F_CTRL_CMD_CE_ECC_DISABLE_ERROR_INJECTION     (0x208f1c07) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_DIAG_CE_INTERFACE_ID << 8) | 0x7" */

/* _ctrl208fce_h_ */

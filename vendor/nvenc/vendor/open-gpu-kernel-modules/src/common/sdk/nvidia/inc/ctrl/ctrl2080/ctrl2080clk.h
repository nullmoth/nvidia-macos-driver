/*
 * SPDX-FileCopyrightText: Copyright (c) 2005-2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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
// Source file:      ctrl/ctrl2080/ctrl2080clk.finn
//

#include "nvfixedtypes.h"
#include "ctrl/ctrl2080/ctrl2080base.h"
#include "ctrl/ctrl2080/ctrl2080boardobj.h"
#include "ctrl/ctrl2080/ctrl2080gpumon.h"
#include "ctrl/ctrl2080/ctrl2080clkavfs.h"
#include "ctrl/ctrl2080/ctrl2080volt.h"
#include "ctrl/ctrl2080/ctrl2080pmumon.h"

#define NV2080_CTRL_CLK_DOMAIN_TEGRA_UNDEFINED             (0x00000000U)
#define NV2080_CTRL_CLK_DOMAIN_TEGRA_GPCCLK                (0x00000001U)
#define NV2080_CTRL_CLK_DOMAIN_TEGRA_NVDCLK                (0x00000002U)



/*!
 * NV2080_CTRL_CMD_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES
 *
 * Control call to query the samples within the PWR_CHANNELS PMUMON queue.
 */
#define NV2080_CTRL_CMD_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES (0x20801037) /* finn: Evaluated from "(FINN_NV20_SUBDEVICE_0_CLK_INTERFACE_ID << 8) | NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS_MESSAGE_ID" */

/*!
 * @brief   With sample period being potentially as fast every 20ms, this gives
 *          us 5 seconds worth of data.
 */
#define NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_COUNT    (250U)

/*!
 * Temporary until an INFO control call is stubbed out that exposes the supported
 * feature set of the sampling.
 */
#define NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_INVALID  (NV_U32_MAX)

/*!
 * A single sample of the power channels at a particular point in time.
 */
typedef struct NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE {
    /*!
     * Ptimer timestamp of when this data was collected.
     */
    NV_DECLARE_ALIGNED(NV2080_CTRL_PMUMON_SAMPLE super, 8);

    /*!
     * Point sampled programmed GPCCLK frequency in KHz.
     *
     * NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_INVALID if not supported.
     */
    NvU32 gpcClkFreqKHz;

    /*!
     * Point sampled programmed DRAMCLK frequency in KHz.
     *
     * NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_INVALID if not supported.
     */
    NvU32 dramClkFreqKHz;

    /*!
     * Point sampled programmed NVDCLK frequency in KHz.
     *
     * NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_INVALID if not supported.
     */
    NvU32 nvdClkFreqKHz;

    /*!
     * Point sampled programmed active GPCCLK frequency in KHz.
     *
     * NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_INVALID if not supported.
     */
    NvU32 activeGpcClkFreqKHz;
} NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE;
typedef struct NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE *PNV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE;

/*!
 * Input/Output parameters for @ref NV2080_CTRL_CMD_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES
 */
#define NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS_MESSAGE_ID (0x37U)

typedef struct NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS {
    /*!
     * [in/out] Meta-data for the samples[] below. Will be modified by the
     *          control call on caller's behalf and should be passed back in
     *          un-modified for subsequent calls.
     */
    NV2080_CTRL_PMUMON_GET_SAMPLES_SUPER super;

    /*!
     * [out] Between the last call and current call, samples[0...super.numSamples-1]
     *       have been published to the pmumon queue. Samples are copied into
     *       this buffer in chronological order. Indexes within this buffer do
     *       not represent indexes of samples in the actual PMUMON queue.
     */
    NV_DECLARE_ALIGNED(NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE samples[NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_SAMPLE_COUNT], 8);
} NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS;
typedef struct NV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS *PNV2080_CTRL_CLK_PMUMON_CLK_DOMAINS_GET_SAMPLES_PARAMS;

/* _ctrl2080clk_h_ */

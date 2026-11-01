/*
 * SPDX-FileCopyrightText: Copyright (c) 2020-2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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
// Source file:      ctrl/ctrl2080/ctrl2080pmumon.finn
//



#include "ctrl/ctrl2080/ctrl2080base.h"

/*!
 * Enumeration of retrieval modes that dictate how many 
 * samples to grab from the PMUMON queue.
 */
typedef enum NV2080_CTRL_PMUMON_RETRIEVAL_MODE {
    NV2080_CTRL_PMUMON_RETRIEVAL_MODE_ALL_SAMPLES = 0,
    NV2080_CTRL_PMUMON_RETRIEVAL_MODE_LATEST_SAMPLE_ONLY = 1,
} NV2080_CTRL_PMUMON_RETRIEVAL_MODE;

/*!
 *  This structure represents base class of PMUMON sample.
 */
typedef struct NV2080_CTRL_PMUMON_SAMPLE {
    /*!
    * Ptimer timestamp in nano-seconds at the time this sample was taken.
    */
    NV_DECLARE_ALIGNED(NvU64 timestamp, 8);
} NV2080_CTRL_PMUMON_SAMPLE;

/*!
 * Common meta-data to be used in control call parameters regarding sampling of
 * FB Circular Queues.
 */
typedef struct NV2080_CTRL_PMUMON_GET_SAMPLES_SUPER {
    /*!
     * [in/out] Requested starting index for reading into the circular queue.
     *          This field will be set to (headIndex + 1) % QUEUE_SIZE
     *          automatically by the control call.
     */
    NvU32                             tailIndex;

    /*!
     * [in/out] Current sequence ID of the underlying PMUMON queue. Not to be
     *          modified by the caller. Will be modified automatically by the
     *          control call.
     */
    NvU32                             sequenceId;

    /*!
     * [out] Current head of the queue. The entry at this index in the queue
     *       is the most recently published.
     */
    NvU32                             headIndex;

    /*!
     * [out] Number of elements copied into samples[] starting from samples[0]
     *       of parent structure. This is the number of elements between the
     *       input tailIndex and current headIndex
     */
    NvU32                             numSamples;

    /*!
     * [out] Number of times the PMUMON queue in FB has been reset
     *       (i.e. pmuStateLoad). When resetCount of the RM and the client
     *       differ, the RM will correctly resume reading of queue from index 0
     *       instead of attempting to catch up from last read.
     */
    NvU32                             resetCount;

    /*!
     * [in] @copydoc NV2080_CTRL_PMUMON_RETRIEVAL_MODE
     */
    NV2080_CTRL_PMUMON_RETRIEVAL_MODE retrievalMode;
} NV2080_CTRL_PMUMON_GET_SAMPLES_SUPER;

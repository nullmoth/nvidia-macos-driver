/*
 * SPDX-FileCopyrightText: Copyright (c) 2017-2021 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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
// Source file:      ctrl/ctrlc370/ctrlc370event.finn
//

#include "ctrl/ctrlc370/ctrlc370base.h"
/* C370 is partially derived from 0073 */
#include "ctrl/ctrl0073/ctrl0073event.h"

/* 
* headId
*   This parameter indicates the ID of head on which we received interrupt
* RgSemId
*   This parameter indicates the RG Semaphore Index for given head
*/
typedef struct NVC370_RG_SEM_NOTIFICATION_PARAMS {
    NvU32 headId;
    NvU32 rgSemId;
} NVC370_RG_SEM_NOTIFICATION_PARAMS;

/* NVC370_SUBDEVICE_XX event-related control commands and parameters */

/*
 * NVC370_CTRL_CMD_EVENT_SET_NOTIFICATION
 *
 * This command sets event notification state for the associated subdevice.
 * This command requires that an instance of NV01_EVENT has been previously
 * bound to the associated subdevice object.
 *
 *   subDeviceInstance
 *     This parameter specifies the subdevice instance within the NV50_DISPLAY
 *     parent device to which the operation should be directed.  This parameter
 *     must specify a value between zero and the total number of subdevices
 *     within the parent device.  This parameter should be set to zero for
 *     default behavior.
 *   hEvent
 *     This parameter specifies the handle of the NV01_EVENT instance
 *     to be bound to the given subDeviceInstance.
 *   event
 *     This parameter specifies the type of event to which the specified
 *     action is to be applied.  This parameter must specify a valid
 *     NV2080_NOTIFIERS value (see cl2080.h for more details) and should
 *     not exceed one less NV2080_NOTIFIERS_MAXCOUNT.
 *   action
 *     This parameter specifies the desired event notification action.
 *     Valid notification actions include:
 *       NVC370_CTRL_SET_EVENT_NOTIFICATION_DISABLE
 *         This action disables event notification for the specified
 *         event for the associated subdevice object.
 *       NVC370_CTRL_SET_EVENT_NOTIFICATION_SINGLE
 *         This action enables single-shot event notification for the
 *         specified event for the associated subdevice object.
 *       NVC370_CTRL_SET_EVENT_NOTIFICATION_REPEAT
 *         This action enables repeated event notification for the specified
 *         event for the associated system controller object.
 *
 * Possible status values returned are:
 *   NV_OK
 *   NV_ERR_INVALID_PARAM_STRUCT
 *   NV_ERR_INVALID_ARGUMENT
 *   NV_ERR_INVALID_STATE
 */
#define NVC370_CTRL_CMD_EVENT_SET_NOTIFICATION (0xc3700901) /* finn: Evaluated from "(FINN_NVC370_DISPLAY_EVENT_INTERFACE_ID << 8) | NVC370_CTRL_EVENT_SET_NOTIFICATION_PARAMS_MESSAGE_ID" */

#define NVC370_CTRL_EVENT_SET_NOTIFICATION_PARAMS_MESSAGE_ID (0x1U)

typedef NV0073_CTRL_EVENT_SET_NOTIFICATION_PARAMS NVC370_CTRL_EVENT_SET_NOTIFICATION_PARAMS;


/* valid action values */
#define NVC370_CTRL_EVENT_SET_NOTIFICATION_ACTION_DISABLE NV0073_CTRL_EVENT_SET_NOTIFICATION_ACTION_DISABLE
#define NVC370_CTRL_EVENT_SET_NOTIFICATION_ACTION_SINGLE  NV0073_CTRL_EVENT_SET_NOTIFICATION_ACTION_SINGLE
#define NVC370_CTRL_EVENT_SET_NOTIFICATION_ACTION_REPEAT  NV0073_CTRL_EVENT_SET_NOTIFICATION_ACTION_REPEAT

/* _ctrlc370event_h_ */

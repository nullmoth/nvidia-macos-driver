// SPDX-License-Identifier: MIT
/*
 * xe's HECI GSC child device. On Linux it publishes an auxiliary device that Linux's MEI driver binds to (firmware
 * update, HDCP and PXP sessions through the graphics security controller). macOS has no MEI driver to bind, so the
 * port does not create the device and its interrupts are acknowledged by the caller and otherwise ignored.
 */
#include "xe_heci_gsc.h"

void xe_heci_gsc_init(struct xe_device *xe)
{
}

void xe_heci_gsc_fini(struct xe_device *xe)
{
}

void xe_heci_gsc_irq_handler(struct xe_device *xe, u32 iir)
{
}

void xe_heci_csc_irq_handler(struct xe_device *xe, u32 iir)
{
}

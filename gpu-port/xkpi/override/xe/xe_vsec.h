/* SPDX-License-Identifier: MIT */
/*
 * Intel platform telemetry (PMT) through the VSEC capability: not offered on macOS (Linux's xe_vsec is GPL-2.0 and
 * exports to Linux's intel_vsec driver, which has no counterpart here). Initialising it does nothing.
 */
#ifndef _XE_VSEC_H_
#define _XE_VSEC_H_

struct xe_device;

static inline void xe_vsec_init(struct xe_device *xe) { }

#endif

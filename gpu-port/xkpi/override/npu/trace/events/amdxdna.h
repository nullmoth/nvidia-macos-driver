/* SPDX-License-Identifier: GPL-2.0 */
/* XDNA's tracepoints, as no-ops: macOS has no ftrace. */
#ifndef _NMX_TRACE_AMDXDNA_H_
#define _NMX_TRACE_AMDXDNA_H_
#define trace_xdna_job(...) do { } while (0)
#define trace_amdxdna_debug_point(...) do { } while (0)
#define trace_mbox_set_tail(...) do { } while (0)
#define trace_mbox_set_head(...) do { } while (0)
#define trace_mbox_rx_worker(...) do { } while (0)
#define trace_mbox_irq_handle(...) do { } while (0)
#endif

/*
 * Included first in every translation unit of the macOS port of LinuxKPI + drm-kmod.
 *
 * The xnu headers are pulled in here, before LinuxKPI defines Linux's macros (fallthrough, __aligned, __packed,
 * container_of, min/max, ...). Their include guards then make every later #include of them a no-op, so Apple's
 * headers never see a Linux macro that changes their meaning.
 *
 * The FreeBSD kernel API that LinuxKPI is written against is supplied by the other headers in this directory,
 * implemented on xnu. __FreeBSD__ is defined because LinuxKPI and drm-kmod select their FreeBSD code paths with it.
 */
#ifndef NMXKPI_PRELUDE_H
#define NMXKPI_PRELUDE_H

/*
 * xnu declares scnprintf/vscnprintf extern; LinuxKPI defines them static inline. Seen in that order, every object
 * got its own global copy (duplicate symbols at link). xnu's declarations are taken under other names here.
 */
#define scnprintf nmx_xnu_scnprintf
#define vscnprintf nmx_xnu_vscnprintf
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdarg.h>
#include <string.h>
#include <sys/cdefs.h>
#include <sys/types.h>
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/errno.h>
#include <sys/time.h>
#include <sys/proc.h>
#include <sys/conf.h>
#include <sys/ioccom.h>
#include <sys/fcntl.h>
#include <sys/uio.h>
#include <sys/kernel_types.h>
#include <kern/locks.h>
#include <kern/thread_call.h>
#include <kern/clock.h>
#include <kern/thread.h>
#include <kern/debug.h>
#include <mach/mach_types.h>
#include <mach/vm_param.h>
#include <machine/machine_routines.h>
#include <libkern/OSAtomic.h>
#include <libkern/OSByteOrder.h>
#include <libkern/libkern.h>
#include <IOKit/IOLib.h>
#include <miscfs/devfs/devfs.h>
#include <pexpert/pexpert.h>
#include <sys/kauth.h>
#undef scnprintf
#undef vscnprintf
/* mach/error.h names error subsystems err_vm, err_kern...; i915 uses err_vm as a goto label */
#undef err_vm
#undef err_kern
#undef err_ipc
#undef err_local
#undef err_server
#undef err_us

/* xnu's strcpy macro deliberately takes three arguments to break two-argument callers; use the checked builtin */
#undef strcpy
#define strcpy(dst, src) __builtin___strcpy_chk((dst), (src), __builtin_object_size((dst), 1))

/* FreeBSD's atomic(9) API (machine/atomic.h of this port), after every xnu header */
#include <machine/atomic.h>

/* xnu's kauth guid_t (a union) and Linux's guid_t (a struct) share a name; xnu's headers are done, so the name
 * belongs to Linux from here on. */
#define guid_t linux_guid_t

#define __FreeBSD__ 15
#define __FreeBSD_version 1500000
#define NMXKPI_XNU 1

#endif

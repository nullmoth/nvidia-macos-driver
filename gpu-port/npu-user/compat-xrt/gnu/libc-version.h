/* SPDX-License-Identifier: Apache-2.0 */
/* glibc's version query, which XRT prints in system reports: on macOS the C library is libSystem. */
#ifndef NMX_COMPAT_GNU_LIBC_VERSION_H
#define NMX_COMPAT_GNU_LIBC_VERSION_H
static inline const char *gnu_get_libc_version(void) { return "libSystem"; }
static inline const char *gnu_get_libc_release(void) { return "macOS"; }
#endif

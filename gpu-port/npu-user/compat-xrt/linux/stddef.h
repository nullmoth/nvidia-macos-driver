/* SPDX-License-Identifier: Apache-2.0 */
/*
 * Linux's <linux/stddef.h> for userland on macOS: the annotations Linux UAPI headers put on structures. They change
 * no layout; flexible arrays are declared as C99/C++ allow.
 */
#ifndef NMX_COMPAT_LINUX_STDDEF_H
#define NMX_COMPAT_LINUX_STDDEF_H
#ifndef __counted_by
#define __counted_by(m)
#endif
#ifndef __DECLARE_FLEX_ARRAY
#define __DECLARE_FLEX_ARRAY(TYPE, NAME) \
	struct { struct { } __empty_##NAME; TYPE NAME[]; }
#endif
#endif

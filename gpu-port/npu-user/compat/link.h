/* SPDX-License-Identifier: MIT */
/*
 * <link.h> for the Level Zero loader on macOS. The loader only uses it to print where dlopen found a library
 * (dlinfo RTLD_DI_LINKMAP), which macOS cannot answer through dlinfo; the call reports failure and the debug line is
 * skipped. Loading itself uses dlopen/dlsym, which macOS has.
 */
#ifndef NMX_COMPAT_LINK_H
#define NMX_COMPAT_LINK_H
struct link_map {
	const char *l_name;
};
#ifndef RTLD_DI_LINKMAP
#define RTLD_DI_LINKMAP 2
#endif
static inline int dlinfo(void *handle, int request, void *info) { (void)handle; (void)request; (void)info; return -1; }
#endif

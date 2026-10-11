/* FreeBSD <sys/resourcevar.h>: process resource limits. xnu enforces address-space limits itself when it maps memory,
 * so a driver's own check against the limit sees no limit. */
#pragma once
#include <sys/resource.h>
#ifndef RLIMIT_VMEM
#define RLIMIT_VMEM RLIMIT_AS
#endif
#define lim_cur_proc(p, which) ((rlim_t)RLIM_INFINITY)

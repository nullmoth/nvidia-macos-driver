/*
 * Linux args.h: COUNT_ARGS counts up to 15 arguments (LinuxKPI's stops at 10). xe's workaround tables pass more
 * than ten rules to one entry, so the shorter count turned them into calls to undeclared functions.
 */
#ifndef _NMX_LINUX_ARGS_H_
#define _NMX_LINUX_ARGS_H_
#define _LINUXKPI_LINUX_ARGS_H_	/* LinuxKPI's own copy must not redefine these */

#define __COUNT_ARGS(_0, _1, _2, _3, _4, _5, _6, _7, _8, _9, _10, _11, _12, _13, _14, _15, _n, X...) _n
#define COUNT_ARGS(X...) __COUNT_ARGS(, ##X, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0)

#define __CONCAT_ARGS(a, b) a ## b
#define CONCATENATE(a, b) __CONCAT_ARGS(a, b)

#endif

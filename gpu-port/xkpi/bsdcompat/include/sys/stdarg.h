/* FreeBSD <sys/stdarg.h> on xnu. LinuxKPI's <stdarg.h> shadows the compiler's, so va_list comes from the builtins. */
#ifndef NMXKPI_SYS_STDARG_H
#define NMXKPI_SYS_STDARG_H
#ifndef _VA_LIST
#define _VA_LIST
typedef __builtin_va_list va_list;
#endif
#ifndef __GNUC_VA_LIST
#define __GNUC_VA_LIST
typedef __builtin_va_list __gnuc_va_list;
#endif
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)
#define va_copy(dst, src) __builtin_va_copy(dst, src)
#define __va_copy(dst, src) __builtin_va_copy(dst, src)
#endif

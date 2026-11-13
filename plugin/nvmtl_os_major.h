/* nvmtl_os_major.h - the macOS major version the device check compares against.

   kern.osproductversion is not the system's version for every process: a program built against an SDK older than
   macOS 11 is answered "10.16" by the kernel (the compatibility version), whatever macOS is running. A device check that
   read only that key refused every such program on macOS 15 and 26, and the refusal itself crashed them (see
   initWithAcceleratorPort:). kern.osrelease, the Darwin kernel version, carries no compatibility answer, so "10.16" is
   resolved from it: Darwin 20-24 are macOS 11-15, and Darwin 25 is macOS 26 (the numbering jumped there).

   Pure C, no system calls, so tools/test_device_refusal.py can run it on any machine. */
#ifndef NVMTL_OS_MAJOR_H
#define NVMTL_OS_MAJOR_H
#include <stdlib.h>

/* product: kern.osproductversion ("15.7.2", "10.16", or NULL/"" when unreadable); darwin: kern.osrelease ("24.6.0").
   Returns the macOS major version, or 0 when neither answers. */
static inline int nvmtl_os_major_from(const char *product, const char *darwin)
{
    int major = (product && product[0]) ? atoi(product) : 0;
    int compat = 0;
    if (major == 10 && product) {
        const char *dot = product; while (*dot && *dot != '.') dot++;
        compat = *dot == '.' && atoi(dot + 1) == 16;
    }
    if (major && !compat) return major;
    const int d = (darwin && darwin[0]) ? atoi(darwin) : 0;
    if (d >= 25) return d + 1;           /* Darwin 25 = macOS 26 */
    if (d >= 20) return d - 9;           /* Darwin 20 = macOS 11 ... Darwin 24 = macOS 15 */
    return major;                        /* an older kernel: 10.16 cannot occur there; keep what was read */
}
#endif

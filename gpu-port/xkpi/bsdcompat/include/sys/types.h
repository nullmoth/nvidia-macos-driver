/* FreeBSD <sys/types.h> on xnu: xnu's, plus the FreeBSD kernel types LinuxKPI declares its own types with. */
#ifndef NMXKPI_SYS_TYPES_H
#define NMXKPI_SYS_TYPES_H
#include_next <sys/types.h>
#include <stdint.h>
#include <stdbool.h>
typedef uint64_t vm_paddr_t;        /* physical address: 64-bit on x86_64 */
typedef int64_t vm_ooffset_t;
typedef uint64_t vm_pindex_t;
typedef char vm_memattr_t;          /* memory attribute (cacheability), as FreeBSD's x86 pmap */
typedef uint64_t bus_addr_t;
typedef uint64_t bus_size_t;
typedef uint64_t rman_res_t;
typedef int32_t lwpid_t;
typedef int64_t sbintime_t;
typedef uint64_t u_register_t;
typedef int cpuwhich_t;
typedef int cpulevel_t;
typedef int cpusetid_t;
typedef uint8_t u_char_t;
typedef int64_t __intmax_t;
typedef uint64_t __uintmax_t;
typedef uintptr_t __uintptr_t;
typedef intptr_t __intptr_t;
typedef uint64_t uoff_t;
typedef size_t __size_t;
typedef ssize_t __ssize_t;
typedef uint64_t pci_addr_t;
#endif

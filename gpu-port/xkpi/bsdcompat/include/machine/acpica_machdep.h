/*
 * ACPICA machine definitions (x86_64) for the macOS kernel port. ACPI methods are evaluated by the Apple ACPI
 * platform expert (IOACPIPlatformDevice), never by an ACPICA interpreter inside this kext, so the global-lock and
 * interrupt hooks below exist only so the headers compile; nothing in the port calls them.
 */
#ifndef __ACPICA_MACHDEP_H__
#define __ACPICA_MACHDEP_H__
#define ACPI_SYSTEM_XFACE
#define ACPI_EXTERNAL_XFACE
#define ACPI_INTERNAL_XFACE
#define ACPI_INTERNAL_VAR_XFACE
#define ACPI_ASM_MACROS
#define BREAKPOINT3
#define ACPI_DISABLE_IRQS() do { } while (0)
#define ACPI_ENABLE_IRQS() do { } while (0)
#define ACPI_FLUSH_CPU_CACHE() __asm__ __volatile__("wbinvd" ::: "memory")
#define ACPI_ACQUIRE_GLOBAL_LOCK(GLptr, Acq) do { (Acq) = 1; } while (0)
#define ACPI_RELEASE_GLOBAL_LOCK(GLptr, Acq) do { (Acq) = 0; } while (0)
#endif

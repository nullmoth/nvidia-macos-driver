/*
 * FreeBSD <dev/acpica/acpivar.h> for the port: the ACPI helpers LinuxKPI and the AMD driver use. ACPI itself is
 * macOS's (AppleACPIPlatform runs every method); src/nmx_acpica.c implements the ACPICA calls they make on it.
 */
#ifndef NMXKPI_DEV_ACPICA_ACPIVAR_H
#define NMXKPI_DEV_ACPICA_ACPIVAR_H
#include <sys/bus.h>
#include <contrib/dev/acpica/include/acpi.h>

/* the ACPI node of a device: a PCI function's companion in the ACPI plane, or the node an ACPI device stands for */
ACPI_HANDLE acpi_get_handle(device_t dev);
/* the device record for an ACPI node (made on first use; LinuxKPI's struct acpi_device is a device_t) */
device_t acpi_get_device(ACPI_HANDLE handle);

ACPI_STATUS acpi_GetInteger(ACPI_HANDLE handle, char *path, UINT32 *number);
ACPI_STATUS acpi_ConvertBufferToInteger(ACPI_BUFFER *bufp, UINT32 *number);
UINT64 acpi_DSMQuery(ACPI_HANDLE h, const uint8_t *uuid, int revision);
ACPI_STATUS acpi_EvaluateDSM(ACPI_HANDLE handle, const uint8_t *uuid, int revision, UINT64 function,
                             ACPI_OBJECT *package, ACPI_BUFFER *out_buf);
ACPI_STATUS acpi_EvaluateDSMTyped(ACPI_HANDLE handle, const uint8_t *uuid, int revision, UINT64 function,
                                  ACPI_OBJECT *package, ACPI_BUFFER *out_buf, ACPI_OBJECT_TYPE type);

#define ACPI_DEVINFO_PRESENT(x, flags) (((x) & (flags)) == (flags))
#define ACPI_DEVICE_PRESENT(x) ACPI_DEVINFO_PRESENT(x, ACPI_STA_DEVICE_PRESENT | ACPI_STA_DEVICE_FUNCTIONING)
#endif

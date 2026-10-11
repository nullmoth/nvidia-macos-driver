# fork_linux_dmi.py <override/src/linux_dmi.c>: DMI strings from SMBIOS through IOKit instead of FreeBSD's kenv.
import pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
start = s.index("static void\nlinux_dmi_preload(void *arg)\n")
end = s.index("SYSINIT(linux_dmi_preload,")
assert s.count("static void\nlinux_dmi_preload(void *arg)\n") == 1
s = s[:start] + r'''#include <sys/malloc.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_NMXDMI, "nmxdmi", "DMI strings");

/*
 * macOS port: FreeBSD's loader publishes SMBIOS as smbios.* kenv strings; here each Linux DMI field is read from
 * the SMBIOS table the platform expert holds (nmx_xnu_dmi_field takes the Linux field number). A field the table
 * does not have stays NULL, which every matcher below treats as "does not match".
 */
static void
linux_dmi_preload(void *arg)
{
	char buf[256];
	int f;

	for (f = 0; f < DMI_STRING_MAX; f++) {
		size_t n = nmx_xnu_dmi_field(f, buf, sizeof(buf));
		if (n == 0)
			continue;
		dmi_data[f] = malloc(n + 1, M_NMXDMI, M_WAITOK);
		if (dmi_data[f] != NULL)
			strlcpy(dmi_data[f], buf, n + 1);
	}
}

static void
linux_dmi_unload(void *arg)
{
	int f;

	for (f = 0; f < DMI_STRING_MAX; f++) {
		free(dmi_data[f], M_NMXDMI);
		dmi_data[f] = NULL;
	}
}
SYSUNINIT(linux_dmi_unload, SI_SUB_DRIVERS, SI_ORDER_ANY, linux_dmi_unload, NULL);

''' + s[end:]
p.write_text(s)
print("linux_dmi.c fork applied")

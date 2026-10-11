/*-
 * Copyright (c) 2020 The FreeBSD Foundation
 *
 * This software was developed by Emmanuel Vadot under sponsorship
 * from the FreeBSD Foundation.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR AND CONTRIBUTORS ``AS IS'' AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 * ARE DISCLAIMED. IN NO EVENT SHALL THE AUTHOR OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
 * OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
 * HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
 * LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY
 * OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF
 * SUCH DAMAGE.
 */

#include <sys/param.h>
#include <sys/systm.h>
#include <sys/kernel.h>

#include <linux/dmi.h>

static char *dmi_data[DMI_STRING_MAX];

#include <sys/malloc.h>
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

SYSINIT(linux_dmi_preload, SI_SUB_DRIVERS, SI_ORDER_ANY, linux_dmi_preload, NULL);

/* Match a system against a field */
bool
linux_dmi_match(enum dmi_field f, const char *str)
{

	if (f < DMI_STRING_MAX &&
	    dmi_data[f] != NULL &&
	    strcmp(dmi_data[f], str) == 0)
		return(true);
	return (false);
}

/* Match a system against the struct, all matches must be ok */
static bool
linux_dmi_matches(const struct dmi_system_id *dsi)
{
	enum dmi_field slot;
	int i;

	for (i = 0; i < nitems(dsi->matches); i++) {
		slot = dsi->matches[i].slot;
		if (slot == DMI_NONE)
			break;
		if (slot >= DMI_STRING_MAX ||
		    dmi_data[slot] == NULL)
			return (false);
		if (dsi->matches[i].exact_match) {
			if (dmi_match(slot, dsi->matches[i].substr))
				continue;
		} else if (strstr(dmi_data[slot],
			dsi->matches[i].substr) != NULL) {
			continue;
		}
		return (false);
	}
	return (true);
}

/* Return the string matching the field */
const char *
linux_dmi_get_system_info(int field)
{

	if (field < DMI_STRING_MAX)
		return (dmi_data[field]);
	return (NULL);
}

/* 
 * Match a system against the structs list
 * If a match is found return the corresponding structure.
 */
const struct dmi_system_id *
linux_dmi_first_match(const struct dmi_system_id *list)
{
	const struct dmi_system_id *dsi;

	for (dsi = list; dsi->matches[0].slot != 0; dsi++) {
		if (linux_dmi_matches(dsi))
			return (dsi);
	}

	return (NULL);
}

/*
 * Match a system against the structs list
 * For each match call the callback with the corresponding data
 * Return the number of matches.
 */
int
linux_dmi_check_system(const struct dmi_system_id *sysid)
{
	const struct dmi_system_id *dsi;
	int matches = 0;

	for (dsi = sysid; dsi->matches[0].slot != 0; dsi++) {
		if (linux_dmi_matches(dsi)) {
			matches++;
			if (dsi->callback && dsi->callback(dsi))
				break;
		}
	}

	return (matches);
}

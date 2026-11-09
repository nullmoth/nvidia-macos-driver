// test_nvrm_dsm_route.c: which ACPI _DSM families the RM may reach.
//
//   cc -std=c11 -Wall -Wextra -I../kexts/NVRM -o /tmp/test_nvrm_dsm_route test_nvrm_dsm_route.c && /tmp/test_nvrm_dsm_route
//
// Negative controls that must FIRE: a router that answered GPS or NVPCF without its boot-arg, mixed the families up,
// or let an unknown NBCI function through fails here.

#include "nvrm_dsm_route.h"
#include <stdio.h>

static int g_fail;
#define CHECK(cond, msg) do { if (!(cond)) { printf("  FAIL: %s\n", msg); g_fail++; } } while (0)

int main(void)
{
    static const uint8_t other[16] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };

    CHECK(nvrm_dsm_classify(nvrm_guid_gps, 0, 0x2, 0, 0, 0) == NVRM_DSM_REFUSE, "GPS is refused with no boot-arg");
    CHECK(nvrm_dsm_classify(nvrm_guid_gps, 0, 0x2, 0, 1, 0) == NVRM_DSM_GPS, "GPS is served with nvrmgps=1");
    CHECK(nvrm_dsm_classify(nvrm_guid_gps, 0, 0x2, 1, 0, 1) == NVRM_DSM_REFUSE, "the other boot-args do not open GPS");
    CHECK(nvrm_dsm_classify(nvrm_guid_nvpcf, 0, 0x1, 0, 1, 0) == NVRM_DSM_REFUSE, "NVPCF is refused without nvrmnvpcf=1");
    CHECK(nvrm_dsm_classify(nvrm_guid_nvpcf, 0, 0x1, 0, 0, 1) == NVRM_DSM_NVPCF, "NVPCF is served with nvrmnvpcf=1");
    CHECK(nvrm_dsm_classify(other, 1, 0x1, 0, 0, 1) == NVRM_DSM_NVPCF, "the NVPCF flag from RM selects NVPCF whatever the GUID");
    CHECK(nvrm_dsm_classify(other, 1, 0x1, 0, 0, 0) == NVRM_DSM_REFUSE, "the NVPCF flag is still refused without the boot-arg");
    CHECK(nvrm_dsm_classify(nvrm_guid_nbci, 0, 0, 1, 0, 0) == NVRM_DSM_NBCI, "NBCI all-functions query is served with nvrmacpibl=1");
    CHECK(nvrm_dsm_classify(nvrm_guid_nbci, 0, NVRM_NBCI_FUNC_GETBACKLIGHT, 1, 0, 0) == NVRM_DSM_NBCI, "NBCI GETBACKLIGHT is served");
    CHECK(nvrm_dsm_classify(nvrm_guid_nbci, 0, 0x15, 1, 1, 1) == NVRM_DSM_REFUSE, "an NBCI function we do not know is refused");
    CHECK(nvrm_dsm_classify(nvrm_guid_nbci, 0, 0, 0, 1, 1) == NVRM_DSM_REFUSE, "NBCI stays behind its own boot-arg");
    CHECK(nvrm_dsm_classify(other, 0, 0, 1, 1, 1) == NVRM_DSM_REFUSE, "an unknown GUID is refused with every boot-arg set");

    CHECK(nvrm_dsm_is_notebook(0, 1, 1) == 0, "off: never a notebook, as before");
    CHECK(nvrm_dsm_is_notebook(1, 1, 0) == 1, "the NVPCF device alone makes a notebook");
    CHECK(nvrm_dsm_is_notebook(1, 0, 1) == 1, "a battery alone makes a notebook");
    CHECK(nvrm_dsm_is_notebook(1, 0, 0) == 0, "neither: a desktop");

    printf(g_fail ? "FAILED %d\n" : "ok\n", g_fail);
    return g_fail != 0;
}

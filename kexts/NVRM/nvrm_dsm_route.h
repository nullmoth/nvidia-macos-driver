// nvrm_dsm_route.h: which ACPI _DSM families the RM may reach, and through which boot-arg.
//
// RM asks each _DSM GUID for its functions separately, so every GUID can be answered or refused on its own. Three
// families are served, each behind its own boot-arg so a machine whose firmware misbehaves can leave it out:
//   nvrmacpibl=1  NBCI (panel backlight tables), as before
//   nvrmgps=1     GPS, the platform request handler's _DSM on the GPU's own ACPI node (notebook power budget)
//   nvrmnvpcf=1   NVPCF on the platform's NVPCF device (Dynamic Boost and the CPU/GPU power split)
// Everything else (NVHG, MXM, NVOP, JT) stays "not supported". Pure C, host-tested.

#ifndef NVRM_DSM_ROUTE_H
#define NVRM_DSM_ROUTE_H

#include <stdint.h>
#include <string.h>

enum nvrm_dsm_kind { NVRM_DSM_REFUSE = 0, NVRM_DSM_NBCI, NVRM_DSM_GPS, NVRM_DSM_NVPCF };

#define NVRM_NBCI_FUNC_GETOBJBYTYPE 0x10u
#define NVRM_NBCI_FUNC_GETBACKLIGHT 0x14u

// GUIDs as they sit in memory: {D4A50B75-65C7-46F7-BFB7-41514CEA0244}, {A3132D01-8CDA-49BA-A52E-BC9D46DF6B81},
// {36B49710-2483-11E7-9598-0800200C9A66} (public identifiers from the open kernel modules' acpidsmguids.h).
static const uint8_t nvrm_guid_nbci[16]  = { 0x75, 0x0B, 0xA5, 0xD4, 0xC7, 0x65, 0xF7, 0x46, 0xBF, 0xB7, 0x41, 0x51, 0x4C, 0xEA, 0x02, 0x44 };
static const uint8_t nvrm_guid_gps[16]   = { 0x01, 0x2D, 0x13, 0xA3, 0xDA, 0x8C, 0xBA, 0x49, 0xA5, 0x2E, 0xBC, 0x9D, 0x46, 0xDF, 0x6B, 0x81 };
static const uint8_t nvrm_guid_nvpcf[16] = { 0x10, 0x97, 0xB4, 0x36, 0x83, 0x24, 0xE7, 0x11, 0x95, 0x98, 0x08, 0x00, 0x20, 0x0C, 0x9A, 0x66 };

static inline enum nvrm_dsm_kind nvrm_dsm_classify(const uint8_t guid[16], int nvpcfFlag, uint32_t subfn,
                                                   int blOn, int gpsOn, int pcfOn)
{
    if (nvpcfFlag || memcmp(guid, nvrm_guid_nvpcf, 16) == 0) return pcfOn ? NVRM_DSM_NVPCF : NVRM_DSM_REFUSE;
    if (memcmp(guid, nvrm_guid_gps, 16) == 0) return gpsOn ? NVRM_DSM_GPS : NVRM_DSM_REFUSE;
    if (!blOn || memcmp(guid, nvrm_guid_nbci, 16) != 0) return NVRM_DSM_REFUSE;
    if (subfn != 0 /* all-functions query */ && subfn != NVRM_NBCI_FUNC_GETOBJBYTYPE && subfn != NVRM_NBCI_FUNC_GETBACKLIGHT)
        return NVRM_DSM_REFUSE;
    return NVRM_DSM_NBCI;
}

// A machine counts as a notebook for the firmware's mobile policy when the platform power device is there or a
// rechargeable battery is: the battery service is not always published (some firmware reports the battery absent under
// macOS), the NVPCF device is the stronger sign.
static inline int nvrm_dsm_is_notebook(int platpowerOn, int haveNvpcfDevice, int haveBattery)
{
    return platpowerOn && (haveNvpcfDevice || haveBattery);
}

#endif

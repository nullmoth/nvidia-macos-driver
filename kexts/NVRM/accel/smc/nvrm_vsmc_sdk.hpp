/*
 * VirtualSMC SDK ABI 1 adapter for NVAccel.
 * Upstream versions and licenses are recorded in vendor/VERSIONS.md.
 *
 * These token renames keep the official SDK layouts while preventing this
 * kext's local value vtable and methods from exporting VirtualSMC's symbols.
 */
#ifndef NVRM_VSMC_SDK_HPP
#define NVRM_VSMC_SDK_HPP

// Lilu's evector logs only on allocation failure. Route that diagnostic through
// NVAccel's own IOLogv bridge implemented in nvrm_vsmc_value.cpp.
#define lilu_os_log NVRMVirtualSMCSDKLog

// Keep the public SDK declarations and ABI unchanged, while giving their C++
// symbols private NVAccel names.
#define VirtualSMCAPI NVRMVirtualSMCAPI
#define VirtualSMCKeyValue NVRMVirtualSMCKeyValue
#define VirtualSMCValue NVRMVirtualSMCValue
#define VirtualSMCKeystore NVRMVirtualSMCKeystore
#define SerializeLevel NVRMSerializeLevel
#define SMCInfo NVRMSMCInfo

#include <VirtualSMCSDK/kern_vsmcapi.hpp>

#undef SMCInfo
#undef SerializeLevel
#undef VirtualSMCKeystore
#undef VirtualSMCValue
#undef VirtualSMCKeyValue
#undef VirtualSMCAPI
#undef lilu_os_log

extern "C" void NVRMVirtualSMCSDKLog(const char *format, ...);

static_assert(NVRMVirtualSMCAPI::Version == 1, "VirtualSMC ABI version changed");
static_assert(sizeof(NVRMVirtualSMCValue) == 144, "VirtualSMCValue ABI layout changed");
static_assert(sizeof(NVRMVirtualSMCKeyValue) == 24, "VirtualSMCKeyValue ABI layout changed");
static_assert(sizeof(NVRMVirtualSMCAPI::KeyStorage) == 24, "VirtualSMC KeyStorage ABI layout changed");
static_assert(sizeof(NVRMVirtualSMCAPI::Plugin) == 72, "VirtualSMC Plugin ABI layout changed");
static_assert(__builtin_offsetof(NVRMVirtualSMCAPI::Plugin, data) == 24,
              "VirtualSMC Plugin::data ABI offset changed");
static_assert(__builtin_offsetof(NVRMVirtualSMCAPI::Plugin, dataHidden) == 48,
              "VirtualSMC Plugin::dataHidden ABI offset changed");

#endif // NVRM_VSMC_SDK_HPP

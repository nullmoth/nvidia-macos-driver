# NVAccel VirtualSMC SDK adapter

This directory vendors the small public-header surface needed to describe a VirtualSMC ABI 1
plugin and read-only sensor values. The original SDK C++ names are preprocessor-renamed in
`nvrm_vsmc_sdk.hpp` to `NVRMVirtualSMCAPI`, `NVRMVirtualSMCValue`, and
`NVRMVirtualSMCKeyValue` (plus the dependent helper types). `SMC_MAKE_IDENTIFIER`,
`SmcKeyTypeFpe2`, SMC key constants, field order, and the ABI 1 `Plugin` layout remain unchanged.

`nvrm_vsmc_value.cpp` supplies the raw-data `init`, `get`, and `update` definitions for the renamed
base class. They follow the behavior in VirtualSMC 1.3.8 `VirtualSMC/kern_value.cpp`, with a local
byte-copy loop instead of Lilu's `lilu_os_memcpy` alias. The SDK's `evector` failure logger is
renamed to `NVRMVirtualSMCSDKLog`, which forwards to kernel `IOLogv`.

## Build include paths

Use the active Xcode SDK's Kernel.framework headers; this adapter does not need a vendored
MacKernelSDK. For NVAccel's existing build flags, add these include paths when compiling
`nvrm_vsmc_value.cpp` and any translation unit that includes the wrapper:

```sh
-I"$SRC/accel/smc/vendor"
-I"$SRC/accel/smc/vendor/Lilu"
```

Here `SRC` is the NVRM source directory, as in `build/accel_build.sh`.
Keep the build's existing `-I"$KHDR"`, where `KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"`.
The source includes `<VirtualSMCSDK/kern_vsmcapi.hpp>` through the first path and Lilu's
`<Headers/kern_util.hpp>` through the second. Compile as the surrounding NVAccel sources do
(`-fapple-kext -mkernel -nostdinc`, `-DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT`,
`-std=c++17 -fno-rtti -fno-exceptions`). Add `nvrm_vsmc_value.cpp` to the NVAccel object list.

The wrapper contains ABI layout assertions. The SDK version and source commits are recorded in
`vendor/VERSIONS.md`; upstream licenses are included beside the copied headers.

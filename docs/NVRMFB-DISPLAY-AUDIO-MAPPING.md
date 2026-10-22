# NVRMFB display audio mapping

This extends the display-codec association introduced by [Hi-Jiajun in PR #284](https://github.com/nullmoth/nvidia-macos-driver/pull/284). The five AppleHDA properties and shared CEA scan follow that contribution; this extension reads the connector type from NVKMS, copies the owning GPU's `hda-gfx` value without changing its `OSData` or `OSString` type, and lets a machine name its own codec/pin mapping.

Set `NVRMFBDisplayAudioCodecInfo` as a four-byte `OSData` property on the NVRMFB personality or framebuffer service. The property is passed through as `audio-codec-info`; it must identify the codec and pin used by the GPU. For example, `00 01 05 00` describes the local RTX 5080 Laptop test mapping to pin 05. Without the property the driver uses `00 01 04 00` (codec 0, pin 04), the mapping measured on an RTX 5060 desktop in #284. A board whose display audio is on another pin sets the property; a malformed value publishes no association.

The current mapping describes one external audio display per GPU: one codec/pin value is shared by the framebuffer personality, and `port-number` is zero. Do not apply the same mapping to multiple active external displays unless they are known to share the same codec route. Concurrent multi-display audio and per-connector mapping are not implemented.

Without a mapping, NVRMFB keeps its existing ELD and `hda-gfx` publication behavior and does not publish an effective five-property audio route. The five route properties are published only for an external display with a valid mapping, an NVKMS HDMI/DisplayPort connector, and at least one CEA Short Audio Descriptor. Disconnects clear the ELD and all association properties; reconnects and monitor swaps refresh them.

## Checks

Run `python3 tools/check_nvrmfb_display_audio_mapping.py` to compile and execute the production connector-type and CEA-scan helpers in a host fixture, and to check the publication/cleanup path. This check is not a kext build or a substitute for a physical boot. The 5080 Laptop/macOS 26.7/pin 05 playback result comes from the submitter's prior hardware validation; other GPUs, connectors, and concurrent displays remain unverified.

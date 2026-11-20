# NMXAudio display output patch

The patch applies at the root of [Liberator at edd4489e](https://github.com/nullmoth/liberator/tree/edd4489e4ab62dbdbdda5ab593efc1ffeb382d9f). Retain the Liberator and FreeBSD licenses when building the modified source.

- Publish a display output only for a single HDMI/DP playback pin with presence, valid ELD and a supported stereo PCM format. The ELD supplies the display name and transport. Polling updates the HDA route on connect, disconnect and sink replacement, including when no unsolicited event arrives.
- Publish the display's two standard stereo volume controls for Core Audio's virtual main volume and balance. Match the PCM amplitude to the HAL volume scalar and apply mute to both channels. The display's DDC/CI hardware volume remains available separately.
- Localize NMXAudio's own labels in English, Simplified and Traditional Chinese, Japanese, Korean, French, German and Spanish. Set `nmxaudiolang=<locale>` to override `prev-lang:kbd`. Display product names are taken as supplied by the monitor.
- Read onboard jack role, location and color from the active HD Audio pin configuration. Localize the shared description of an endpoint and retain a generic label when pins disagree. `NMXAudioPinMetadata` in the I/O Registry lists the individual pins and their configured values. [AppleALC #972](https://github.com/acidanthera/AppleALC/pull/972) provides comparison data for one board. NMXAudio uses the local codec's configuration on each system.

The user-facing controls use IOAudioFamily, with no hook into AppleGFXHDA or SimpleHDA. [DisplayAudio](https://github.com/Xiaoheixu/DisplayAudio) documents the PCM gain approach. [SimpleHDA](https://github.com/laobamac/SimpleHDA) documents using the boot locale for device labels.

## Build

Use Xcode with macOS Kernel.framework headers, Git and Python 3. The helper
takes local Git repositories containing the pinned Liberator, FreeBSD and
historical NullMoth driver commits. It builds in a temporary directory and
writes the signed kext and build record into a new output directory.

```sh
kexts/NMXAudio/build.sh /path/to/liberator /path/to/freebsd-src /path/to/nvidia-macos-driver ./nmx-audio-out
```

# NMXAudio display output patch

Apply `connected-display-audio.patch` to [Liberator at edd4489e](https://github.com/nullmoth/liberator/tree/edd4489e4ab62dbdbdda5ab593efc1ffeb382d9f) at the repository root. The build still requires the FreeBSD-on-XNU C and C++ bridge objects referenced by that source tree. Retain the Liberator and FreeBSD licenses when building the modified source.

- Publish a display output only for a single HDMI/DP playback pin with presence, valid ELD and a supported stereo PCM format. The ELD supplies the display name and transport. Polling updates the HDA route on connect, disconnect and sink replacement, including when no unsolicited event arrives.
- Apply the system volume, mute and independent left/right levels to the display's PCM. The display's DDC/CI hardware volume remains available separately.
- Localize NMXAudio's own labels in English, Simplified and Traditional Chinese, Japanese, Korean, French, German and Spanish. Set `nmxaudiolang=<locale>` to override `prev-lang:kbd`. Display product names are taken as supplied by the monitor.

The user-facing controls use IOAudioFamily, with no hook into AppleGFXHDA or SimpleHDA. [DisplayAudio](https://github.com/Xiaoheixu/DisplayAudio) documents the PCM gain approach. [SimpleHDA](https://github.com/laobamac/SimpleHDA) documents using the boot locale for device labels.

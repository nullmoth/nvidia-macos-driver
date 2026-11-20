# Volume for external displays

## Monitor hardware volume over DDC/CI

With `nvrmfbddc=1`, NVRMFB can publish the IOKit `speaker-volume` display
parameter for external displays that advertise audio in EDID and return a
valid DDC/CI VCP `0x62` volume range. Each control follows its framebuffer
through hotplug. An existing display volume provider retains ownership.
`NVRMDisplayProductName` on the framebuffer reports the EDID model name.
The same name is carried in ELD when an audio association is available.

Check for `speaker-volume` with `IODisplayGetIntegerRangeParameter`. Write a
value in the reported range with `IODisplaySetIntegerParameter` and verify the
result on the monitor or with a fresh Get VCP request. The control uses the
monitor's hardware range and excludes the reserved fixed-volume and mute
values in MCCS 2.2. Control Center and the Sound settings still depend on a
separate CoreAudio output and its controls.

The probe and write scheduling follow the per-display approach used by
[DisplayBrightness](https://github.com/Xiaoheixu/DisplayBrightness).

## Volume and mute for CoreAudio outputs (NMHDA, off by default)

A display connected to an NVIDIA card gets an audio output from NMHDA, but a display has no volume of its own, so macOS shows
no slider for it and the keyboard volume keys do nothing. `nmhdavol=1` in the boot arguments adds the missing control.

## What it does

- When `AppleHDAEngineOutputDP` engines are published (one per display with audio), NMHDA adds a master level control
  (0 to 100) and a master mute control to each, through IOAudioFamily's public classes. They appear in Sound settings, the
  menu bar and for the keyboard keys like any other output.
- The value is applied in software: after the engine writes a window of samples into its ring (`clipOutputSamples`), NMHDA
  scales that window. Level 100 touches nothing. The curve is the cube of the level fraction (50 is about -18 dB).
  16-bit, packed 24-bit, 32-bit integer and 32-bit float samples are handled; any other format is left alone.
- Nothing reads or writes the layout of Apple's private classes. Engines are recognised by class name, and the two
  control constructors are looked up by name in the loaded IOAudioFamily image. If either lookup or the hook fails, no
  control is added.

## Proof and what is not proved

- `tools/test_nmhda_gain.py` runs the arithmetic on the host (unity, mute, curve, each sample format, ring windows, refusal of
  unknown formats). `kexts/NMHDA/nmhda_volume.cpp` compiles against the SDK's kernel headers with no new warnings of its own.
- Not proved without a display on an NVIDIA card: that `addDefaultAudioControl` is accepted for an engine that is already
  running, and that the symbol `AppleHDAEngine::clipOutputSamples` exists in the AppleHDA of macOS 15 and 26.

## First run on hardware

1. Boot with `nmhdavol=1`. `sysctl debug.nmhda` should list `sample hook installed`, `watching for display audio engines`
   and, for each display, `controls on AppleHDAEngineOutputDP engine -> 0x0`.
2. Select the display in Sound settings: the output volume slider should appear and change the loudness; mute silences it.
3. If a line says the hook could not be installed or a control was refused, send `debug.nmhda` with the report; the default
   (flag absent) path is unchanged.

# Optional HD Audio (Tahoe)

The complete driver package carries NMXAudio 1.0.2 under `Optional/`. The
installer asks separately and defaults to **No**. Skip it if your audio already
works. This component supports PCI HD Audio controllers on macOS 26.

HDMI and DisplayPort playback devices appear when the connected monitor
reports stereo audio support. macOS can adjust output volume, mute and balance
for those devices. Onboard labels display jack roles, locations and colors from
the codec's active configuration when the pin group reports them consistently.

For noninteractive installs, use `sudo bash install.sh --audio=yes` to opt in,
or `--audio=no` to remove audio previously installed by this fork. Updates retain
that opt-in. The independent Intel-panel prompt is unchanged.

An independently installed or subsequently replaced NMXAudio is preserved.
Remove it through its own installer before selecting the bundled copy. Audio
installed by this fork participates in kernel preflight, backup, interrupted
installation recovery, boot fallback and uninstall.

The 1.0.2 diagnostic build was booted on an RTX 3080 with macOS 26.7.4.
T2751U DisplayPort playback, native volume, mute, balance and ALC897 naming
passed. Virtual-display attachment did not create an audio endpoint. Physical
HDMI/DisplayPort cable hot-plug remains unverified.

The package includes the Liberator and FreeBSD HD Audio licenses under
`licenses/`.

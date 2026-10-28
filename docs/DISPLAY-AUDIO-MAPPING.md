# Display audio mapping

NMHDA reads NVIDIA HDA pins and valid ELD through IOHDAFamily GET verbs.
NVRMFB requests snapshots from its asynchronous worker outside the display gate.
The controller must share the GPU's PCI bus/device and parent. Snapshots carry
an interface version and connection generation; stale results are discarded.
NMHDA retains the resolved IOHDAFamily GET entry across unrelated Lilu image
callbacks, so later driver loads cannot clear the topology transport.

HDMI and DP SST displays match only when EDID-derived and pin ELD identities
are unique in both directions (manufacturer/product, trimmed name, connection
type and SAD set). Identical sinks remain ambiguous. No GPU-model pin table is
used, and DP MST is excluded from automatic selection and default fallback.

Set NVRMFBDisplayAudioOverrides on the framebuffer personality to an array of
dictionaries containing PhysicalIndex (integer) and CodecInfo (four-byte data:
codec address, function-group NID, pin NID, zero). Read PhysicalIndex from the
NVRMFBDisplayAudioPhysicalIndex diagnostic; it is not the framebuffer ordinal or
AppleHDA port-number. Duplicate physical indices or codec/function/pin targets
are rejected. Explicitly reserved pins are excluded from automatic selection.

Priority: per-interface override; legacy NVRMFBDisplayAudioCodecInfo for one
external display; unique automatic match; codec 0/function 1/pin 4 fallback for
one non-MST external display. Invalid explicit configuration blocks association.
Multi-display endpoints without a reliable match publish no audio-codec-info.
Single-display SST keeps port-number zero for compatibility. With multiple
connected external displays, port-number follows the NVIDIA HDA pin mapping
(pin NID minus 4), so pins 04/05/06/07 publish ports 0/1/2/3. Source, probe
availability, generation and physical index are exposed as NVRMFBDisplayAudio*
diagnostics.

Startup, service/codec readiness, hotplug, successful NVKMS mode commits and
successful head wake refresh the mapping. A mode-commit refresh runs on the
existing asynchronous worker, after NVKMS has had a chance to publish the HDA
ELD; it does not wait for the next display-connect event. An invalid ELD marks
that pin unavailable for matching without invalidating other pins in the same
topology snapshot. Unmatched non-MST endpoints retry at 300 ms, at most six
times after the initial request. Stop removes readiness notifications and
drains the worker.

Portable checks (clang++ with AddressSanitizer and UndefinedBehaviorSanitizer):

    clang++ -std=c++17 -Wall -Wextra -Werror -fsanitize=address,undefined tools/test_nvrmfb_audio_match.cpp -o /tmp/nvrmfb-audio-match
    /tmp/nvrmfb-audio-match
    python3 tools/test_nvrmfb_audio_config.py
    python3 tools/test_nmhda_topology_callback.py

The portable tests use synthetic monitor data and test production matching,
extracted configuration selection and topology callback ordering. They do not
exercise kernel scheduling or HDA transport. On RTX 5080 Laptop/macOS 26.7,
the submitter confirms that Mi Monitor HDMI output appears and plays sound
after the GET callback correction. Other GPU models, pin 04, DP playback,
wake and multi-monitor hotplug remain unverified on hardware. This change does
not modify the backlight handler or PR #312.

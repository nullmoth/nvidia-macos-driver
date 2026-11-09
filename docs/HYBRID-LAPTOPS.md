# Hybrid laptops (NVIDIA GPU plus an Intel or AMD GPU that owns the panel)

Most gaming laptops wire the built-in panel to the integrated GPU. The NVIDIA GPU has no connection to that panel, so a
driver that can only scan out of the GPU it drives shows nothing there. This page describes the opt-in path that lets the
NVIDIA GPU do the rendering while the integrated GPU keeps the panel, what a tester switches on, and which log line
proves each step.

Status: built and tested offline (host tests, kernel builds for macOS 15 and 26). It has not run on a hybrid laptop. It is
off unless the boot-arg `nmhybrid` is set, and with the boot-arg absent every code path behaves as before.

## How the other systems do it

- **Apple's dual-GPU MacBook Pros.** The integrated GPU drives the display and WindowServer. The discrete GPU is a second
  Metal device (`isLowPower` false); an app that asks for it renders there, and the finished frames are copied into
  surfaces the integrated GPU scans out. A gmux chip can also re-route the panel's lanes to the discrete GPU, which is why
  AppleMuxControl exists. No software can do that on a PC laptop whose panel is hard-wired to one GPU.
- **Linux PRIME render offload.** The application renders on the discrete GPU into a buffer exported as a dma-buf. The
  integrated GPU imports the same pages and scans them out, or copies them when it cannot. Nothing switches lanes.
- **Windows (Optimus, Advanced Optimus).** Same shape as PRIME. Laptops with a hardware mux can also re-route the panel
  to the NVIDIA GPU (Advanced Optimus); when that setting is on in the BIOS, none of this page is needed and the normal
  driver drives the panel.

## What this driver does

WindowServer composites on the NVIDIA GPU. The panel's driver (an Intel or AMD framebuffer) publishes the panel as head 4 of
the NVIDIA accelerator and hands out a CPU mapping of its scan-out buffer with no VRAM address. The accelerator's display
pipe sees that, and for every composited frame does one of two things.

| level | boot-arg | what happens to a frame |
|---|---|---|
| off | absent, `0`, or any other value | nothing here runs |
| copy | `nmhybrid=1` | the pixels that changed since the previous frame are written into the panel's scan-out buffer. A copy of the previous frame, held in ordinary memory, is compared with the new one so the write-combined scan-out receives stores only for changed pixels (a blinking cursor is one store). |
| import | `nmhybrid=2` | the composited surface's own pages are written into unused entries of the Intel GPU's graphics translation table and the display plane is pointed at them. No pixel is copied. Intel display versions 12 and 13 only (Tiger, Rocket, Alder and Raptor Lake). Every other panel, every login-window frame (10-bit), and any surface that does not fit is copied instead, one frame at a time. |

Panels covered: Intel display versions 12 to 20 through `NMIntelFB` (copy, import on 12 and 13); AMD and Intel through
`NMXGPU` (copy). The NMXGPU part is in the NMX tree: `xkpi/xnu/nmx_hybrid.h` holds the answer the panel gives the display
pipe, `xkpi/xnu/nmx_fb.cpp` has the three-hunk hook that uses it.

Not part of this path: acceleration on the integrated GPU (apps do not run on it as a Metal device), switching the panel to
the NVIDIA GPU, per-application GPU choice, powering the NVIDIA GPU down when idle.

## Before you start

1. If the BIOS has a graphics mode setting (Advanced Optimus, MUX switch, "discrete graphics only"), set it to discrete.
   The panel is then wired to the NVIDIA GPU and you do not need this page.
2. Run the 1401 scan so the report says which GPU owns the internal panel and each external port. An external display on
   an HDMI or DisplayPort connector that belongs to the NVIDIA GPU works without this path.
3. Be able to remove the boot-arg without the screen: boot to the OpenCore picker, or keep a second boot entry. Level 2
   writes display registers; level 1 and the framebuffer's start-up do not.

## Switching it on and off

    sudo tools/hybrid_switch.sh copy      # nmhybrid=1
    sudo tools/hybrid_switch.sh import    # nmhybrid=2
    sudo tools/hybrid_switch.sh off       # removes nmhybrid
    tools/hybrid_switch.sh status
    tools/hybrid_switch.sh copy --dry-run # prints the boot-args it would write

Other boot-args are kept. Restart after changing it. The Intel panel driver (`NMIntelFB`, built by
`kexts/NMIntelFB/build.sh`) is a separate opt-in kext and is not in the standard package; it declines to start unless the
boot-arg is 1 or 2, so installing it without the switch changes nothing. The AMD and Intel NMXGPU kext reads the same
boot-arg.

If the screen is dark after a restart: remove `nmhybrid` (OpenCore picker, reset NVRAM, or edit boot-args), restart. With
the boot-arg gone the panel driver does not start and the firmware framebuffer owns the panel again.

## Check it

After the restart:

    tools/hybrid_verify.sh                       # reads this boot's log and the live sysctls
    log show --last boot --predicate 'eventMessage CONTAINS "hybrid:"' > hybrid.log   # to send us
    tools/hybrid_verify.sh hybrid.log

It prints one line per step and a verdict: `copy proven`, `import proven`, `not proven: <first missing step>` or `off`.

| step | log line (source) | what it proves |
|---|---|---|
| S1 | `hybrid: switch level N (boot-arg nmhybrid=N)` (accelerator) | the accelerator read the switch |
| | `auto-go: hybrid: waiting up to 20 s for the other GPU's panel (head 4) before arming` then `the other GPU's panel published as head 4` (NVRM) | arming waited for the panel instead of arming without it |
| S2 | `hybrid: provider NMIntelFB WxH pitch P display version V level N` or `hybrid: provider NMXGPUFB WxH pitch P` | the panel driver took the panel and adopted its mode |
| S3 | `hybrid: first frame presented by copy (R rows, P pixels, head 4)` (accelerator) | WindowServer's first frame reached the panel |
| S4 | `sysctl debug.nmhybrid_frames` above 0 and `debug.nmhybrid_words_skipped` above 0 | only changed pixels are stored |
| S5 | `hybrid: import ring 4 slots x K KiB at table offset 0x... (entries a..b of n)` (level 2) | a run of unused translation-table entries was found and reserved |
| S6 | `hybrid: zero-copy flip #1 surface 0x.. slot N live` (level 2) | the plane showed an imported surface (live surface register matched) |

Other lines you may see, none of them an error by itself:

- `hybrid: import unavailable: <reason>`: level 2 is not possible here (display version, BAR0 size, no unused entries).
  Frames are copied; S1 to S4 still apply.
- `hybrid: fallback to copy: <reason> (surface 0x..)`: one surface was refused and copied. Reasons: `surface is not 8-bit BGRA`
  (login window), `surface is not the scan-out size`, `row pitch unusable for a display plane`, `all import slots in use`.
  Shown for the first eight only; `debug.nmhybrid_import_fail` counts all.
- `hybrid: flip to an import slot did not reach the screen in 40 ms` followed by `hybrid: fallback to copy: a flip never
  reached the screen`: import is over for this boot; the plane is put back on the firmware surface and frames are copied.
  Send us this log.
- `hybrid: plane back on the boot surface`: the copy path took over from an imported surface.

Counters (all read-only sysctls): `debug.nmhybrid` (level), `nmhybrid_frames`, `nmhybrid_words`, `nmhybrid_words_skipped`,
`nmhybrid_flips`, `nmhybrid_import_fail`. The existing `debug.nvaccel_iop_ok_h4` counts frames delivered to head 4.

Measuring the cost: sample `nmhybrid_words` and `nmhybrid_words_skipped` over ten seconds of desktop use. Skipped over
total is the share of the screen that did not need storing.

## Why the pieces are shaped this way

- The copy uses a shadow of the previous frame because stores to the panel's scan-out buffer are the slow part (write
  combined, uncached reads), while reading ordinary memory is cheap. The first frame, a frame of another size, and the
  first frame after the plane returns from an imported surface are stored in full.
- Import writes only into table entries that already read as one repeated value (the unused or scratch entries), never
  into the firmware's surface range or the top 2 MiB, and every entry's address must fit the 46-bit field. If no such run of
  `4 x frame size` exists, import declines and the copy continues.
- The CPU wrote the surface's pixels; the display engine reads memory. The surface is flushed from the CPU caches before
  each flip, and the flip is not counted until the plane's live-surface register shows it (checked for 40 ms).
- Copy and import never write the same surface at the same time: the copy path writes the firmware's surface, so the plane
  is moved back to it first.
- Import is limited to display versions 12 and 13 because that is where the table layout (upper half of BAR0, 64-bit
  entries, size in the GMCH control register) is documented in the Linux i915 driver. Later generations need their own
  layout and stay on copy.
- NVRM waits up to 20 s for head 4 before arming, only when the switch is on. Arming without it leaves the panel without a
  pipe until a second arm and a WindowServer restart.

## Known limits

- A copy into the single scan-out buffer can tear. The only cure is the import path (the plane switches surface at a
  vertical blank).
- No hybrid laptop has run this. Nothing here has measured frame rate, tearing, the 40 ms flip wait on real hardware, or
  whether an Intel part leaves the unused-entry run this code expects.
- Backlight: `NMIntelFB` has a backlight handler; the AMD panel through NMXGPU has none yet (the NMXGPU lane).
- The panel is the only head on the integrated GPU. External ports owned by the integrated GPU follow the same path only
  if their driver publishes them as heads.
- The NVIDIA GPU stays powered while the desktop is up.

## Windows comparison (features this does not have)

NVIDIA's Windows driver on the same laptop also offers per-application GPU choice, Advanced Optimus mux switching without
restart, powering the GPU down when idle, and variable refresh on the internal panel. None of these exist on macOS here;
they are listed in the release notes as open items, not as part of this switch.

## Tests (host, no hardware)

- `tools/test_hybrid_present.c`: the present logic (changed-pixel copy, pitches, 10-bit narrowing, import checks, table
  entries, free-run search, slot cache, flip tracking, state machine). Build line in the file. A `-DLEGACY_COPY` build runs
  the same arms against the old full-frame loop and fails the changed-pixel arms.
- `tools/test_hybrid_wiring.py`: the switch is off by default, bounded waits, guarded table writes. Fails on a tree without
  this change.
- `tools/test_hybrid_scripts.sh`: the switch script against a stub `nvram`, and the verifier against saved logs
  (`tools/hybrid_fixtures`), including a log where the flip never lands.
- NMX tree: `tools/test_nmx_hybrid.cpp`.
- `tools/hy_nometal_bucket.py`: sorts kernel logs from machines with no Metal device into the causes the accelerator logs
  (accelerator never started, no head and no other accelerated GPU, panel on a GPU macOS cannot accelerate, late head).

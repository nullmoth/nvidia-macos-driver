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

| owned | `nmhybrid=3` | as import, and a frame import refuses (10-bit login window, a pitch the plane cannot take) is written into one of three buffers the panel driver owns (cached memory, mapped into the graphics table) which the plane is then flipped to. No write-combined stores and no tearing for those frames. The buffers are lent to one writer at a time, never written while on screen or queued, and all freed when the plane goes back to the boot surface. Intel display versions 12 and 13. |
| copy engine | `nmhybrid=4` | as owned, and a frame whose newest pixels are still only in the GPU's memory is written by the GPU's copy engine straight into the pages of an owned buffer (the same engine that pages a surface out to system memory), then the plane is flipped to that buffer. No CPU copy, no mapping of the application's surface. The order is fixed and tested: the GPU has finished drawing (no lock held), the accelerator lock is taken for the resource lookup and the copy only, an owned buffer is taken and flushed from the CPU caches, the copy engine writes it and the call returns only when it is done, the lock is released, and only then is the plane flipped, whose wait for the next vertical blank (up to 40 ms) never holds the accelerator lock. A copy that fails cleanly returns the buffer and the frame is copied the old way; a copy that timed out may still be writing, so that buffer is quarantined, never shown and never handed out again. Needs the surface to have exactly the panel's size and pitch (a linear byte copy), 8-bit BGRA. Intel display versions 12 and 13. This is done in the kernel through the resource manager's copy-engine call, not in the Metal plugin: the plugin has no mapping of the panel driver's pages and does not need one. |

Panels covered: Intel display versions 12 to 20 through `NMIntelFB` (copy, import on 12 and 13); AMD integrated GPUs macOS
has no driver for through `NMGopFB` (copy, see the next section); AMD and Intel through `NMXGPU` (copy). The NMXGPU part is in the NMX tree: `xkpi/xnu/nmx_hybrid.h` holds the answer the panel gives the display
pipe, `xkpi/xnu/nmx_fb.cpp` has the three-hunk hook that uses it.

Not part of this path: acceleration on the integrated GPU (apps do not run on it as a Metal device) and switching the
panel to the NVIDIA GPU (see "Mux switching" below).

## AMD integrated GPUs: NMGopFB

Laptops with a Ryzen processor and an NVIDIA card usually wire the panel to the Radeon in the processor, and macOS has no
driver for it. `NMGopFB` is the panel provider for them. It does not drive the Radeon at all: the firmware lit the panel and
left the GPU scanning out a plain 32-bit framebuffer, and `NMGopFB` publishes that framebuffer as head 4 (the same slot
`NMIntelFB` uses), mapped write-combined, so the accelerator copies each composited frame into it as in level 1 above. It
writes no display register, no PCI config space and nothing else on the Radeon. If the framebuffer is not 32 bits per pixel,
has a pitch below width times four, or is not wholly inside one of the Radeon's memory ranges, it refuses, logs why, and the
firmware framebuffer keeps the screen.

Which driver takes which GPU (`kexts/NMGopFB/nm_gopfb.h`, tested on the host):

| GPU | Driver |
|---|---|
| Intel with a display version from 12 to 20 | `NMIntelFB` |
| AMD integrated: Van Gogh `163F`, Mendocino `1506`, Rembrandt `1681`, Raphael `164E`, Granite Ridge `13C0`, Phoenix and Hawk Point `15BF 15C8 1900`, Strix Point `150E`, Strix Halo `1586` | `NMGopFB` |
| AMD discrete RDNA 3 and 4 (`7448-745F`, `7470-749F`, `7550-756F`, `7590-75AF`) | `NMGopFB` |
| Everything macOS drives itself (Polaris, Vega, Navi 1x and 2x, Raven Ridge and older Vega APUs, older Intel) | none: the driver declines |
| Any GPU while the AMD / Intel test driver (`NMXGPU`) is loaded | none: `NMXGPU` owns the display |

### What a tester on an AMD-iGPU laptop does

1. Install the NVIDIA driver and the optional `NMGopFB.kext` component (it is not in the standard package; it does nothing
   without the switch).
2. In the BIOS, give the integrated GPU the primary display if there is a setting for it, and leave a UEFI GOP framebuffer
   in the firmware (the default).
3. In OpenCore, turn the `Fix PAT` kernel patch off (the AMD Vanilla `_mtrr_update_action | Fix PAT` entry). With it on,
   macOS maps write-combined memory as uncached and every row of the copy takes about twenty times longer. (PrimeBridge,
   the community project this follows, measured 39.7 microseconds per 7680-byte row with the patch on against 2.0 with it
   off.) Test from a USB stick first.
4. Switch it on: `sudo tools/hybrid_switch.sh copy` (boot-arg `nmhybrid=1`). Level 2 behaves as level 1 here.
5. Restart. To see the log: `log show --last boot --predicate 'eventMessage CONTAINS "NMGopFB"'`. A healthy start prints
   `selected (hybrid level 1)`, `firmware framebuffer WxH pitch P at 0x... (BARn +0x...)`, `provider NMGopFB ...` and
   `built-in panel published`. `tools/hybrid_verify.sh` then reports `copy proven` when the first frame arrives.
6. Black panel afterwards: plug a monitor into the NVIDIA card, or remove `nmhybrid` (OpenCore picker, NVRAM reset), or add
   `-nmgopfboff` for one boot. A GPU the table does not know can be named with `nmgopid=0xVVVVDDDD` (for example
   `nmgopid=0x10021681`); this is for reports, not for daily use.

Limits, as for any copy path: the single scan-out buffer can tear; the pacing timer is 60 Hz because the Radeon's display
engine is not read; no backlight slider (the panel is not driven by this kext, the keys still reach the firmware on many
laptops); the Radeon's resolution is the firmware's. Proven on no laptop yet. The method comes from PrimeBridge (myvoow,
MIT), which runs on a Radeon 680M laptop with NullMoth 2.6.3.

## Start-up controls and diagnosis

- **Stands down on its own.** If the NVIDIA boot guard has the driver off (boot-arg `-nvoff`, the boot picker's remove
  flag, or two starts in a row that never lit a display) the panel driver does not take the panel; the firmware framebuffer
  keeps it. Log: `hybrid: standing down, the firmware framebuffer keeps the panel: <reason>`.
- **`nmhybriddelay=<s>`** (0 to 120, default 0): wait before taking the panel. Some machines froze without a wait after
  the NVIDIA driver started. The guard's verdict is read again after the wait.
- **Where did it freeze?** The panel driver records the last step it reached: sysctl `debug.nmintelfb_step`, the log line
  `hybrid: step <n> <name>`, and the NVRAM variable `nullmoth-ifb-step` where the platform lets a kext write it (read after
  a frozen boot with `nvram nullmoth-ifb-step`). Steps in order: probe, delay, mmio-mapped, mode-adopted,
  framebuffer-started, published, import-ring, first-flip, suspend, resume-begin, resume-done or resume-failed; `stood-down`
  if it declined. `tools/hybrid_verify.sh` prints the last one it finds.
- **`nmhybrid20=1`**: allow the wake replay on display versions 20 and 30 (Lunar Lake, Panther Lake); **`nmhybridtc=1`**: log Type-C
  cable events (versions 12 and 13); **`nmhybridext=1`**: also light a plugged Type-C monitor with colour bars on a second pipe; **`nmhybridp2p=1|2`**: the zero-copy decision (see its section).
- **`nmflatskip=<n>`** (0 to 8, default 0): on the NVIDIA GPU's own display path, a composited frame that is one flat
  colour (64 samples equal) is not flipped to for up to n frames in a row, so a momentary black frame never reaches the
  screen; the n+1th flat frame is shown. A surface is judged only when its system-memory copy is the newest one. Counter:
  `debug.nmflat_held`.

## Sleep and wake (`nmhybridresume=1`)

After S3 the Intel display engine returns with every register zero and the panel's link state gone. With this switch on,
the panel driver captures the firmware's mode while the panel is lit (the registers that make it up, and the sink's link
settings read over the AUX channel) and on wake replays it in the order Linux's i915 enables an eDP panel, waiting for
each handshake and stopping at the first that does not come: power wells, display clock PLL, DPLL and clock select, the
combo PHY block, the panel power sequence, sink to D0, transcoder and plane registers (plane surface last), transcoder
function and DDI buffer with training pattern 1, link training (clock recovery, channel equalisation; skipped for panels
that declare no AUX handshake), idle then normal pattern, transcoder on, backlight. The firmware surface's entries in the
graphics table are restored first. A panel that is still lit on wake (display sleep, not S3) is left alone.
Logs: `hybrid: resume armed: panel state captured (sink link 0x14 x2, ...)` at start, then on wake
`hybrid: resume step: <step>` for each step and `hybrid: resume: panel relit` or the reason it stopped
(`a power well did not come up`, `the display clock PLL did not lock`, `the DPLL did not come up or lock`, `the panel power
sequence did not complete`, `the sink did not answer on the AUX channel`, `link training failed`, `the pipe is not scanning
out after the link came up`). On a failed training the DDI buffer is switched off again.

Link training follows the DisplayPort adjust-request loop as Linux does: the sink's requested swing and pre-emphasis are
applied to the combo PHY first (the TX registers of all four lanes, from the generation's own level tables), then written to
the sink; five tries at one swing, ten loops, at most six equalisation rounds, a lost clock recovery starts over. The whole
combo PHY register block (768 words) is captured and replayed.

Limits, all untested on hardware: display versions 12, 13 and 14 (version 14 below), a panel on a combo port (DDI A to C on
version 12, A and B on version 13 and 14), any pipe A to D. Type-C ports (DDI D and later on version 12, C and later on 13) are not
driven as the panel: no laptop wires its internal panel to one (the connect and hot-plug half is in the Type-C section below).
Display versions 20 and 30 (Lunar Lake, Panther Lake) have their own section below and their own boot-arg.

**Drive tables for eDP panels.** The video BIOS names, per panel type, whether the panel uses the low-voltage-swing table and
whether it uses the reduced-power table. The panel driver reads the BIOS tables through the OpRegion (`nm_hybrid_vbt.h`: every
offset checked against the buffer, a table that names no panel type is not guessed) and picks the table Linux's i915 would:
above 5.4 Gb/s the eDP table of the generation, else reduced-power, else low swing, else the DisplayPort table. The reduced-power
table has nine rows, so a sink asking for swing 3 gets a refusal and the training fails rather than guess; its PHY override bits
are set when it is in use. If the video BIOS picks the panel type by the panel's EDID identifier, the panel driver reads the first ten bytes of the
panel's EDID over the AUX channel while the panel is lit (manufacturer, product, serial, week, year) and matches them against
the BIOS panel table as Linux does; only if that fails too is the type unknown and the DisplayPort tables used. Level 0 is always the firmware's own.

**Display version 14 (Meteor Lake, Arrow Lake).** The PHY is a C10 block reached through a per-port message bus. The panel
driver reads the firmware's PLL, common, transmitter, width and level-override registers over the bus while the panel is lit, and
on wake replays them in the order of Linux's `intel_cx0pll_enable` and `mtl_ddi_pre_enable_dp`: power wells, display clock, the
power-management demand, panel power, the port clock control, lane reset and power state ready, the PLL registers (lane 0
master), the transmitters, the clock frequency, the PLL request and acknowledge, the sink, the pipe registers, the D2D link, the
port buffer width, the DDI buffer, link training with the C10 override registers as the source levels, the transcoder and the
backlight. Log lines: `Cx0 PHY` and `power-management demand` join the step list. Version 14 has no display-clock voltage
handshake in Linux, so none is sent. On version 14 the PHY latches the firmware holds across sleep (`HOLD_PHY_PG1_LATCH`,
`HOLD_PHY_CLKREQ_PG1_LATCH`) are released once power well 1 is up, as Linux's display core init does.

**Display versions 20 and 30 (Lunar Lake, Panther Lake) and the C20 PHY (`nmhybrid20=1`).** Ported from Linux's xe display
code (`intel_cx0_phy.c`, `intel_cx0_phy_regs.h`, `intel_ddi.c`, `intel_cdclk.c`, `intel_display_power.c`; MIT), run by the same
replay as version 14 with these differences, each with an arm in `tools/test_hybrid_resume20.c`:

- The PHY port registers (port buffer control 1 to 3, message bus, bus timer, clock control) sit in the second register range,
  at 0x16F200 + 0x200 x (4 + port), not at 0x64000 + 0x100 x port.
- The D2D link is enabled in the DDI buffer control register (bits 29 and 28) before the DDI buffer, not in port buffer control.
- Version 20 pulses the GMBUS unit clock gate once in core init; the MDCLK-to-CDCLK ratio throttle and MBUS join register is
  replayed. The display clock's own MDCLK source bit travels in the clock control register that is already replayed.
- Version 30 does not reset the message bus after every transaction, and its DDI clock select field is one bit wider.
- PHY kind follows Linux: C10 on ports A and B for Lunar Lake, C10 on port A only for Panther Lake (port B is C20).
- C20: the PHY's PLL state is 16-bit words in its own SRAM, two contexts. The panel driver reads the active context while the
  panel is lit (transmit 3 words, common 4, MPLLB 11 or MPLLA 10) and works out the link rate from them (Linux's
  `intel_c20pll_calc_port_clock`). A rate that none of Linux's C20 tables carries is refused, not guessed. On wake the words are
  written into the idle context through the SRAM window registers, the DisplayPort custom width and serdes rate are set, and
  the context bit is toggled. The tables (`nm_hybrid_c20tables.h`, 13 states and 4 lists) are copied value for value from
  Linux by `tools/igfx/gen_c20_tables.py`; the test computes each entry's link rate back from its words. Signal levels use the
  C20 DisplayPort table (`mtl_c20_trans_dp14`) through the same override registers as C10.

Lunar Lake and Panther Lake PCI ids are in the table the panel driver reads. Not run on any hardware: without `nmhybrid20=1`
the resume declines on these parts with a log line. The Intel display firmware (DMC) for these parts is not part of this path.

**Display clock handshake and core init (versions 12 and 13).** Before the display clock PLL is touched the platform controller
is asked to prepare (mailbox 0x7, ready-for-change), and after the clock is set the voltage level for it is sent (level from the
clock: 480 MHz is level 2); a controller that never answers stops the resume. From version 13 the squash register is replayed
between the PLL and the clock control. Around the power wells and the clock the resume also replays the display core init state
the firmware left: the reset handshake with the platform controller, the display buffer slices (requested, then the state bit is
waited for), and on versions 12 and 13 the MBUS and bandwidth-buddy registers, the combo PHY power-down register and the clock
gating registers. A slice that never powers stops the resume before the PHY and the pipe.

**Display microcontroller (DMC) firmware.** Generations 12 and 13 scan out without it: Linux's i915 uses it for the
deeper power states (DC5, DC6) and per-pipe features, not for the scan-out, and macOS never loaded it on these panels, so
there is nothing to reload after sleep. `kexts/NVRM/accel/nm_hybrid_dmc.h` reads the package format (checks every length,
offset and register range, refuses damaged files; tested on synthetic packages, on 4000 corrupted ones under the address and
undefined-behaviour sanitisers, and on the real Tiger Lake, Alder Lake P and Meteor Lake files) and turns it into the list of
writes that loads it. `package/dmc_fetch.sh` fetches Intel's files unmodified at a pinned linux-firmware commit and checks
their SHA-256 on the user's Mac; the licence is `licenses/Intel-i915-firmware-LICENSE.txt` (redistribution in binary form,
unmodified; no reverse engineering, which this does not do). With the boot-arg `nmhybriddmc=1` the panel driver loads it: a root helper (`package/nm-dmc-load.sh`, with the small `nm_dmc_push`)
asks the driver which file this GPU needs (`sysctl debug.nmintelfb_dmc_want`: Tiger Lake, Alder Lake P, Meteor Lake and Arrow
Lake have one; Rocket Lake and Alder Lake S do not), makes sure the verified file is on disk, checks its SHA-256 again and writes
it to `debug.nmintelfb_dmc` (root only). The driver parses and range-checks the bytes itself before keeping anything, writes the
program and the register list with every event handler left disabled, and loads it again after sleep. Run the helper once per
boot after the panel driver has started. Linux keeps the main program's events enabled for the deep display power states; this
driver never enables those states, so the loaded program has nothing to do. The display stepping picks the package entry as in Linux: the PCI revision id through the
sub-platform's table on versions 12 and 13 (Tiger Lake U/Y and H, Alder Lake P, N, Raptor Lake P), the display step register on
version 14, the generic entry for a part not listed. (Reading no stepping picked the A-stepping program on every Alder Lake P;
the real file shows two different main programs.) The program is loaded first on wake, before any power well, as Linux does.

## Which GPU an application gets

With the switch on and a second Metal GPU present (an integrated GPU macOS still drives, such as Coffee Lake), the plugin
decides per process whether to offer the NVIDIA device. With no second Metal GPU the NVIDIA device is always offered,
because hiding it would leave the process without Metal.

Optional file `/Library/GPUBundles/nvmtl-gpu-policy.txt`:

    mode nvidia        # default: every process gets the NVIDIA device
    mode apple         # Apple's rule: an app whose Info.plist sets NSSupportsAutomaticGraphicsSwitching to true is
                       # content with the low-power GPU and does not get the NVIDIA device; every other app keeps it
    igpu Safari        # this process never gets the NVIDIA device
    nvidia Blender     # this process always gets it

A per-process line beats the mode, the last matching line wins. Each decision is logged as `gpu policy for <process>: ...`
in the plugin log. The plugin can offer a device or not; it cannot list a device without making it the default, so a
process sent to the low-power GPU does not see the NVIDIA device in `MTLCopyAllDevices` either. That is why the default is
`mode nvidia`. WindowServer is never offered the device when the NVIDIA GPU is headless (unchanged).

Apple's System Settings "automatic graphics switching" checkbox and the Activity Monitor GPU column come from Apple's
graphics-control kexts, which a PC laptop does not have, so they do not appear; the policy file is the replacement. A
per-process GPU-time figure is not published by the accelerator and is an open feature item.

## Idle power-down

The opt-in D3cold monitor (boot-args `nvrtd3=1` and `nvrtd3res=<ACPI path>`, `kexts/NVRM/nv_rtd3.cpp`) powers the GPU's
power resource off after 60 s with no lit head and no client. On a hybrid laptop it now counts the right things:

- Clients include the user clients of the accelerator, where every Metal application attaches (before, only clients of the
  resource-manager service were counted, so a running app looked like an idle GPU).
- Framebuffers include the NVIDIA GPU's own outputs (the check used a class name that matches nothing) and, with
  `nmhybrid` on, the other GPU's panel, because then the NVIDIA GPU draws the desktop and is never idle.

So the combinations are: `nmhybrid` off plus `nvrtd3=1`: the integrated GPU draws the desktop, the NVIDIA GPU powers off
after 60 s without a Metal application and powers on when one starts. `nmhybrid` on: the GPU stays on, with or without
`nvrtd3`. Not measured on hardware: the wake latency of the first Metal application, and whether the power resource path
is right for a given laptop (the 1401 scan reports the candidates).

## Mux switching (Advanced Optimus)

Not built, for these reasons. (1) There is no common interface: each vendor exposes the display mux through its own ACPI,
WMI or embedded-controller method, with its own method numbers, and a wrong write can blank the only panel. (2) Most laptops
apply the change only after a restart, through a firmware setting; the no-restart switch on Windows is done by NVIDIA's
closed display-mux driver, which may not be disassembled (licence 2.2/2.3). (3) The result a user wants is already reached
by the BIOS setting in "Before you start": set to discrete, the panel is on the NVIDIA GPU and the normal driver drives it.
What the 1401 scan records per machine (mux present, its setting, which GPU owns each port) is what decides which path a
machine takes.

## Before you start

1. If the BIOS has a graphics mode setting (Advanced Optimus, MUX switch, "discrete graphics only"), set it to discrete.
   The panel is then wired to the NVIDIA GPU and you do not need this page.
2. Run the 1401 scan so the report says which GPU owns the internal panel and each external port. An external display on
   an HDMI or DisplayPort connector that belongs to the NVIDIA GPU works without this path.
3. Be able to remove the boot-arg without the screen: boot to the OpenCore picker, or keep a second boot entry. Level 2
   writes display registers; level 1 and the framebuffer's start-up do not.

## Switching it on and off

The order to try every switch in, with what to send back, is in `docs/HYBRID-TESTER-PLAN.md`.

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

## Type-C ports (versions 12 and 13): what is built and what is not

`kexts/NVRM/accel/nm_hybrid_tc.h` is Linux's Type-C connect flow (`intel_tc.c`, `intel_display_power_well.c`; MIT) for Tiger Lake
(version 12) and Alder Lake P (version 13): the FIA instance and index, the live status of the cable (DisplayPort-alt,
Thunderbolt-alt, legacy), the target mode with Linux's legacy-flag correction and its fall back to Thunderbolt-alt, the PHY
ownership handshake (version 12: block TC-cold with the platform controller, check ready, set not-safe in the FIA; version 13:
set the ownership bit in the DDI buffer control, check ready in the TCSS status, then block TC-cold), the lane check, the
disconnect order, and hot-plug events. `tools/test_hybrid_tc.c` runs it against a model of the Type-C subsystem's side.

With the boot-arg `nmhybridtc=1` the panel driver starts an observe-only monitor on versions 12 and 13: every 2 s it reads the live
status of the four Type-C ports and logs `hybrid: Type-C port N: cable plugged/unplugged/changed, live mode ...`. It never
takes a PHY, so a dock keeps its USB lanes. That is the hardware check of the hot-plug decoding.

What is built toward an external Type-C display (`nm_hybrid_ext.h`, `nm_hybrid_dkl.h`, `tools/test_hybrid_ext.c`; formulas from Linux's
i915, MIT): the EDID read over I2C-over-AUX and its preferred mode, the DPCD capabilities, the cheapest link that carries the mode
(lowest rate, fewest lanes), the link and data M/N values, the transcoder timing, source-size and function words, and the Dekel PHY's
PLL (divider search for 1.62 to 8.1 Gb/s on 19.2, 24 and 38.4 MHz references, checked by computing the rate back from the registers;
the banked register writes through the HIP index; power, program, enable, lock). Without the library nothing drives these ports.

Also built, each with a model test (formulas and sequences from Linux's i915, MIT): the Dekel transmitter lanes (swing and pre-emphasis for
both lanes from the generation's own tables, `nm_hybrid_dkl.h`) and the clock routing from the PLL to the port and the port to the transcoder;
the second pipe's watermarks and display-buffer fit (`nm_hybrid_wm.h`: memory latencies from the platform controller, methods 1 and 2, the
level fit, the buffer range taken from the free part of the display buffer, with a floating-point cross-check that every enabled level holds
the lines the pipe reads during its latency, and a refusal when level 0 cannot be met); and the whole bring-up as one sequence that is undone
in reverse from any step (`nm_hybrid_extmode.h`: PHY ownership, power wells, sink and EDID, mode and link, buffer fit, PLL and lanes, clocks,
pipe and plane, transcoder function and DDI buffer, link training, transcoder on). `tools/test_hybrid_extmode.c` unplugs the cable at every AUX
transaction of the bring-up (EDID, DPCD and link training included) and checks that the result is "unplugged" and that no register is left enabled,
for versions 12 and 13. `nm_hybrid_heads.h` is the framebuffer lifecycle for any number of heads sharing the device (create, bring up, publish; unpublish,
take down, release; replug during teardown; stop order), tested in `tools/test_hybrid_heads.c`.

With the boot-arg `nmhybridext=1` (versions 12 and 13) the panel driver runs this for real: when a cable is plugged into a Type-C port it allocates a
1920x1080 buffer, maps it into free graphics-table entries, brings the display up on a free pipe, fills it with colour bars, and takes everything down
on unplug and when the driver stops. This lights a monitor for a tester without touching the window system. Log lines start with
`hybrid: external head on Type-C port N`.

What is not built, and why. (1) Publishing the head to the window system: the NVIDIA accelerator has five framebuffer slots (four NVIDIA heads and the
panel at index 4), and its pipe count is exposed to the system as five; an external head needs the count raised and per-head state added, which changes the
topology every hybrid user sees and cannot be shown without hardware, so it is left until a tester has confirmed the colour bars; the second
`IOFramebuffer` per connector is the other half of the same step. (2) Unverified assumptions that only a machine settles: the Type-C AUX channel
register layout on Alder Lake P (taken to follow Tiger Lake's), how the display-buffer range maps to the hardware's slices for a second pipe, and
the SAGV and transition watermarks (left as the firmware had them, transition off).

## Zero-copy scan-out from the NVIDIA GPU's memory (`nmhybridp2p`)

`kexts/NVRM/accel/nm_hybrid_p2p.h` decides whether the integrated GPU's display engine may read a frame straight out of the NVIDIA
GPU's BAR1 window: refused unless the boot-arg `nmhybridp2p` is 1 (host bridge on a known-good list) or 2 (forced, for testers),
and refused in every case when the NVIDIA GPU is not on the integrated GPU's root complex, its BAR1 is not a 64-bit prefetchable
window bigger than the frame, or the integrated GPU's DMA is translated. Linux's peer-to-peer rule is the model (a path across
the host bridge only on a known-good chipset, never by default); the list starts empty because no client chipset has a measured
result, so by the rule no laptop qualifies until one is measured. `tools/test_hybrid_p2p.c` tests the decision and the page list. What is built, and what only a
machine can settle (whether the read works): the page list from the NVIDIA resource's BAR1 range (`nmp_pages`: consecutive pages,
refused beyond bit 45, which the table entry cannot address) is wired into the import path, which uses it for a VRAM-resident frame
when the decision allows it and otherwise keeps the system-memory import and the copy paths. The table entry for a bus address that is
not RAM is the same entry (present bit and page address); no other flag exists. Counter `debug.nmhybrid_p2p`, log
`hybrid: zero-copy from VRAM, head N: <reason>`. Read the heading as "built, never run": the allowlist is empty, so it stays off unless a
tester sets `nmhybridp2p=2`.

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

NVIDIA's Windows driver on the same laptop also offers per-application GPU choice (here: the policy file), powering the GPU
down when idle (here: opt-in D3cold), Advanced Optimus switching without restart (not possible, see above), and variable
refresh on the internal panel. The last has no equivalent on a panel the other GPU owns; per-process GPU time is the other
open row.

## Tests (host, no hardware)

- `tools/test_hybrid_present.c`: the present logic (changed-pixel copy, pitches, 10-bit narrowing, import checks, table
  entries, free-run search, slot cache, flip tracking, state machine). Build line in the file. A `-DLEGACY_COPY` build runs
  the same arms against the old full-frame loop and fails the changed-pixel arms.
- `tools/test_gopfb.c`: the NMGopFB decisions on the host: which driver takes which GPU, and the firmware framebuffer's
  geometry (32 bpp, pitch, wholly inside a BAR, no wrap-around). `tools/test_gopfb_wiring.py`: the provider is off by default,
  never writes the Radeon, and the accelerator counts it as a head only with the switch on.
- `tools/test_hybrid_wiring.py`: the switch is off by default, bounded waits, guarded table writes. Fails on a tree without
  this change.
- `tools/test_hybrid_scripts.sh`: the switch script against a stub `nvram`, and the verifier against saved logs
  (`tools/hybrid_fixtures`), including a log where the flip never lands.
- `tools/test_hybrid_resume20.c`: display versions 20 and 30 and the C20 PHY (register range, D2D in the DDI buffer, MBUS and GMBUS
  steps, SRAM context switch, table self-check), `tools/test_hybrid_tc.c`: the Type-C handshake and hot-plug, and
  `tools/test_hybrid_p2p.c`: the zero-copy decision.
- `tools/test_hybrid_ext.c`: EDID over AUX, preferred mode, link choice, M/N, transcoder words, the Dekel PLL and lanes, clock routing, watermarks.
- `tools/test_hybrid_extmode.c`, `tools/test_hybrid_heads.c`: the external-display bring-up and teardown (unplug at every step) and the head lifecycle.
- `tools/test_hybrid_resume.c`: the wake replay against a model of the display engine, panel power sequencer, combo PHY drive
  registers, AUX channel and sink, for a panel on pipe A / DDI A, pipe B / DDI B, pipe D / DDI A and pipe C / DDI B (order,
  handshakes, the sink's swing and pre-emphasis requests reaching the PHY registers, every failure stops at its handshake);
  `-DNAIVE_RESTORE` runs the same arms against a plain register restore and fails 84.
- `tools/test_hybrid_resume14.c`: the message bus, the C10 register file, the lane-reset, power-state, PLL, D2D and DDI-buffer handshakes and the training levels on display version 14, panel on DDI A and DDI B, with bus errors injected.
- `tools/test_hybrid_vbt.c`: the video BIOS reader (6000 corrupted tables under the sanitisers, the OpRegion locations).
- `tools/test_hybrid_dmc.c`, `tools/test_dmc_fetch.sh`: the firmware package reader and load plan (and the real files when `NMH_DMC_DIR` names them), the fetch script and the root helper.
- `tools/test_gpu_policy.c`: the per-process choice. `tools/test_hybrid_rtd3.c`: idle power-down counts.
- NMX tree: `tools/test_nmx_hybrid.cpp`.
- `tools/hy_nometal_bucket.py`: sorts kernel logs from machines with no Metal device into the causes the accelerator logs
  (accelerator never started, no head and no other accelerated GPU, panel on a GPU macOS cannot accelerate, late head).

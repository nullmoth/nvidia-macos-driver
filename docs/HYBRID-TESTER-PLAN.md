# Hybrid laptop tester plan

For someone with a laptop whose screen is wired to the Intel or AMD integrated GPU and an NVIDIA GPU that renders. Nothing here has run on
real hardware yet; every step below is a test of something that was built and checked only against models. Do the steps in order and stop at
the first one that fails: each later step assumes the earlier ones. Send back what each step asks for, even when it works.

Background and the meaning of each switch: `docs/HYBRID-LAPTOPS.md`. All switches are boot-args, off by default. Change one thing per boot.

## Before you start

- Save your work. A dark or frozen screen is possible at every step. The way back is always the same: remove the boot-arg you added
  (OpenCore picker, reset NVRAM, or edit boot-args from another machine) and restart; the firmware framebuffer owns the panel again.
- Set boot-args with `sudo nvram boot-args="..."`, keeping what is already there (`nvram boot-args` shows it). The helper
  `sudo tools/hybrid_switch.sh copy|import|off` only handles `nmhybrid=1`, `2` and removal; for `nmhybrid=3` and `4` write the boot-arg by hand.
- Collect the log after every boot with:

      log show --last boot --predicate 'eventMessage CONTAINS "hybrid:"' > hybrid-<step>.log
      tools/hybrid_verify.sh hybrid-<step>.log           # prints the verdict line
      sysctl debug.nmintelfb_step                        # the last start-up step the panel driver reached (also if the screen froze)

- Always send: the machine model, the CPU and its Intel/AMD generation, the macOS version, `nvram boot-args`, and the hybrid log of the boot.
- If the machine freezes at start-up, read `nvram nullmoth-ifb-step` after the next (switch removed) boot and send it.

## Step 1: copy (`nmhybrid=1`)

Boot with `nmhybrid=1`. Optional if the machine froze before: `nmhybriddelay=10`.

Should show: the desktop on the laptop panel, drawn by the NVIDIA GPU. `tools/hybrid_verify.sh` says `copy proven`.
Send: `hybrid-1.log`; `sysctl debug.nmhybrid debug.nmhybrid_frames debug.nmhybrid_words debug.nmhybrid_words_skipped debug.nvaccel_iop_ok_h4`;
whether tearing or flicker is visible, and a rough feel of smoothness.
Unlocks: nothing else works without it. If `not proven: <step>` appears, that step names the next code fix (panel not taken, first frame not
delivered, accelerator never armed).

## Step 2: import (`nmhybrid=2`), Intel display versions 12 and 13 only

Boot with `nmhybrid=2`. Look for `hybrid: import ring ...` and `hybrid: zero-copy flip #1 surface ... live`.
Should show: the same desktop, less tearing, `debug.nmhybrid_flips` rising. `import proven`.
Send: the log; the sysctls above plus `debug.nmhybrid_flips debug.nmhybrid_import_fail`; any `hybrid: import unavailable: <reason>`,
`hybrid: fallback to copy: <reason>`, or `flip ... did not reach the screen in 40 ms` lines (they are results, not errors).
Unlocks: level 3 and 4 (they build on the ring). A `flip did not reach the screen` line is the most useful single finding; send the log in full.

## Step 3: owned buffers (`nmhybrid=3`) then copy engine (`nmhybrid=4`)

Boot with `nmhybrid=3`, check as step 2, then with `nmhybrid=4`.
Should show: unchanged picture; `debug.nmhybrid_owned` rising at 3; `debug.nmhybrid_dma` rising at 4 and `debug.nmhybrid_dma_timeouts` staying 0.
Send: logs, `debug.nmhybrid_owned debug.nmhybrid_dma debug.nmhybrid_dma_timeouts`, and any `hybrid: owned buffer N quarantined` line.
Unlocks: a nonzero timeout count points at the copy-engine ordering (code step: the lock/wait order in the accelerator); zero means level 4 can become
the recommended level.

## Step 4: sleep and wake (`nmhybridresume=1`, added to the working level)

Add `nmhybridresume=1`. Expect `hybrid: resume armed: panel state captured (...)` at start. Close the lid or sleep from the menu, wake.
Should show: the panel relit after wake, log `hybrid: resume step: <step>` for each step, then `hybrid: resume: panel relit`.
Send: the log of the boot (steps and the final line; on failure the reason line such as `link training failed`), `debug.nmintelfb_step`.
Unlocks: a failing step names exactly which handshake to fix (power well, display clock, PLL, panel power, sink, training, pipe). A success on
display 12 or 13 makes the resume path the default for them. On display 14 (Meteor Lake, Arrow Lake) the same switch runs the Cx0 path; send the
`Cx0 PHY` and `power-management demand` step lines.

## Step 5: Lunar Lake and Panther Lake (`nmhybrid20=1`, with `nmhybridresume=1`)

Only for display versions 20 and 30. Without `nmhybrid20=1` the log says `resume unavailable: display version N needs the boot-arg nmhybrid20=1`.
With it: as step 4. A Panther Lake panel on port A is C10; anything on a C20 port logs `Cx0 PHY (C20)`.
Send: the log and the display version line `8086:XXXX display version N`.
Unlocks: success makes C10/C20 resume on those parts real; a `Cx0 PHY` failure points at the message-bus or PLL step; `no snapshot` after a C20 read
means the link rate is not in Linux's tables (send the log).

## Step 6: display firmware (`nmhybriddmc=1`), versions 12, 13, 14

Boot with `nmhybriddmc=1`, then once per boot run the helper after the panel driver has started: `sudo package/nm-dmc-load.sh`. First check what the
driver wants: `sysctl debug.nmintelfb_dmc_want` (a file name, or `none` for Rocket Lake and Alder Lake S).
Should show: no visible change (the program is loaded with every event handler disabled); after sleep and wake it is loaded again before the first
power well.
Send: the helper's output, `debug.nmintelfb_dmc_want`, and the log lines mentioning the firmware.
Unlocks: a clean load keeps it as an opt-in; a rejected package gives the exact parse error to fix.

## Step 7: Type-C cable events (`nmhybridtc=1`), versions 12 and 13

Boot with `nmhybridtc=1`. Plug and unplug a DisplayPort-alt dock or USB-C monitor, a Thunderbolt device, and (if you have one) a legacy Type-C
display, one at a time on each Type-C port, waiting a few seconds between.
Should show: `hybrid: Type-C monitor armed for N ports (observe only)` and, per cable, `hybrid: Type-C port N: cable plugged, live mode <mode>`
with the right mode, and `cable unplugged` on removal. It takes no PHY, so a dock keeps working.
Send: the log, a note of which cable went into which port, and any `Type-C monitor: port N unavailable` line.
Unlocks: correct modes confirm the hot-plug decoding and the PHY handshake's inputs; a wrong mode or a port that stays silent names the register
to fix. This must pass before step 8.

## Step 8: colour bars on an external monitor (`nmhybridext=1`), versions 12 and 13

Boot with `nmhybridext=1` (it includes step 7's monitor). Plug a DisplayPort-alt monitor into a Type-C port. The internal panel keeps working through
the level from step 1 to 4.
Should show: the external monitor lights with eight vertical colour bars (white, yellow, cyan, green, magenta, red, blue, black) at its preferred
mode, normally 1920x1080; the log shows `hybrid: external display: <step>` lines (PHY, power, sink, mode, PLL, pipe and plane, transcoder function and
DDI buffer, link training, transcoder on) and `hybrid: external head on Type-C port N is up: WxH, L lanes ...`. Unplug: the monitor goes dark and the
log shows `external display: tearing down`. Plug again: it must come back. Shut down or sleep with it plugged in: no hang.
Send: the full log of the boot, a photo of the screen, and for a failure the line `hybrid: external head on Type-C port N failed: <reason>`
(reasons: the PHY could not be taken, a power well, the sink, no usable EDID, the mode does not fit the link or display buffer, the Dekel PLL,
link training, the cable was unplugged, the pipe is not scanning out).
Unlocks, by result:
- Colour bars correct and stable: build the next code step, publishing the head to WindowServer (raise the accelerator's five-framebuffer slot count,
  a second `IOFramebuffer` per connector, `docs/HYBRID-LAPTOPS.md` section "Type-C ports").
- Bars lit but wrong (shifted, snow, flicker): the transcoder words or the watermarks are off; send the photo and the mode line.
- Picture lost after seconds: the display-buffer range or the watermarks (underrun); send the log and say whether it is worse at higher modes.
- `the Dekel PLL did not come up` or `link training failed`: the Dekel register values or the lane levels; send the full log.
- Dark with no failure line: the clock routing or the AUX channel for the Alder Lake P Type-C ports (assumed to follow Tiger Lake's); say which
  Type-C port and which laptop.

## Step 9: zero copy from the NVIDIA GPU's memory (`nmhybridp2p=2`), with level 2 or higher

Boot with `nmhybridp2p=2` added to a working level (2, 3 or 4). Read the decision line `hybrid: zero-copy from VRAM, head 4: <verdict> (host bridge
VVVV:DDDD)`, then use the desktop for a minute.
Should show: either a refusal reason (the BAR, the path), which is a result, or `allowed: forced` and a counter `debug.nmhybrid_p2p` that rises, with
the picture intact. A garbled or black picture, or a hang, means the integrated GPU cannot read the NVIDIA GPU's window on this chipset: remove the
boot-arg, and send the host bridge ids and the log.
Send: the log lines above, `debug.nmhybrid_p2p debug.nmhybrid_flips debug.nmhybrid_import_fail`, and "picture good / garbled / hang".
Unlocks: a machine where it works adds its host bridge ids to the known-good list in `nm_hybrid_p2p.h`, which is what lets `nmhybridp2p=1`
work without forcing. Nothing is added to the list without a result from a real machine.

## What to send, in one list

For every step: `hybrid-<step>.log`, `nvram boot-args`, the sysctls named in the step, and a line saying "worked / partly / failed".
For a freeze: `nvram nullmoth-ifb-step`. For a dark screen: which boot-arg was last added and whether removing it restored the picture.

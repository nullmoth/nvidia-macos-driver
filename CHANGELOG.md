# Changelog

## 2.4.2

- Fix the black screen and the startup that stops on the verbose text after updating to 2.4 or 2.4.1. The display driver checks for a part of the display interface that the 2.4 and 2.4.1 core driver was built without, so it refused to start and the screen never moved off the startup text. 2.4.2 builds the core driver with it again; nothing else changes.

## 2.4.1

- Fix a black screen after updating to 2.4. The 2.4 package carried a framebuffer kext built from an outdated copy of its source, missing the display code the earlier releases had; 2.4.1 ships the complete framebuffer with the 2.4 changes.

## 2.4

- Hardware H.264 and HEVC video encode on RTX 20 (Turing) and RTX 30 (Ampere) cards, selected automatically per card; earlier builds used the hardware encoder on RTX 50 only.
- Security hardening: caller values are bounded, each client is serialised, and kernel addresses, registers and firmware stay private.
- macOS 26: the system icons and the glass menus and panels draw correctly again.
- Install from a root-owned copy of the package, with the previous driver kept as an automatic fallback.
- The kernel driver reads the power source (battery or mains) and reports it to the GPU firmware.
- Clear a stale shader cache left by an earlier build, which could show a black screen at login after an update.
- A boot counts as good once the desktop is up.

## 2.3.6

Desktop and windows
- Dragging and scrolling windows: a surface whose video memory had been set aside is put back before its update is
  recorded, so the driver no longer copies the whole window back to system memory on every frame (PR #245, thanks
  HighDelay; #253 measured the same readback on an RTX 5070 Ti at 4K)
- Moving the mouse no longer stalls the compositor: the cursor position is applied on its own thread instead of under
  the system's cursor lock, and OpenGL apps wait for the display's real refresh rate instead of 60 Hz on faster
  panels (thanks pengubow)
- Core Animation passes that read the colour attachment with framebuffer fetch off now read a defined image instead of an
  unbound descriptor, which could put garbage on screen inside WindowServer (PR #270, thanks watchthelight)
- Text and badge masks drawn into A8 targets land in the right lane, and Dock badges show their numbers again (PRs #271
  and #274, thanks watchthelight)

Apps and games
- OpenGL: Minecraft's vertex shaders translate, the draw path no longer allocates per draw, texel-buffer descriptors and
  unchanged bindings are reused, depth bias and per-instance step rates take effect, samplers work inside argument buffers
  (thanks HighDelay)
- Chrome and Chromium use Metal through ANGLE by default, so their GPU process starts; Electron apps stay opt-in with
  NVMTL_ANGLE_METAL=1, and NVMTL_ANGLE_METAL=0 turns it off everywhere (PR #263, thanks Hi-Jiajun)
- Shaders with `[[visible]]` function references that function constants turn off are no longer refused, which lost
  pipelines in the login window, Spotlight, icon services and the Dock
- Metal reports the card's whole video memory as its working set, as a Mac does (8 GB cards said 5.9 GB, 12 GB cards 8.9 GB)

## 2.3.5

Stability
- The login screen no longer stays black for a minute after the display comes up. The driver waited 60 seconds before
  letting the compositor's frames reach the panel, on the assumption that the screen was already showing them another
  way; on macOS 26 it was not, so every boot sat on a black login screen for exactly that minute

## 2.3.4

Pre-release for the early-updates testers; it becomes the general release only after every GPU generation passes the
field gate.

Stability
- Window dragging, Launchpad and games stutter less: each screen update waits at most one refresh period for the
  previous one, instead of a fixed tenth of a second; the wait wakes on the display's own event
- macOS Sequoia screenshots and window captures: the capture ordering fix from 2.3.2 returns, and it now follows what the
  installed kernel driver supports, so a Metal driver and a kernel driver from different versions never mismatch again
- Turing (GTX 16 and RTX 20): the login screen no longer stays black with a cursor when the first frame has not been
  drawn yet; the driver only shows video memory the Metal driver has written
- Turing: the ten-second start wait the installer used to add is now the driver's own default, so a laptop card or a Mac
  whose boot arguments were rewritten gets it too (a value you set yourself still wins)
- Laptops with a hybrid display: the driver no longer copies the boot screen from another card's memory, which could
  stop the Mac during startup. When WhateverGreen is present next to the driver, the installer notes the gfxrst setting it needs
- A Mac reset during the driver's own start-up wait (the ten-second Turing settle, the 100-second wait when the card's
  memory window cannot be moved) no longer counts toward switching the driver off. Two such resets used to leave a
  working Mac on the fallback driver, reported at the next login as "driver loaded: no"
- A GPU that is busy is not a GPU that is gone: the Metal driver keeps waiting for a late frame instead of disabling
  itself for the rest of the process. In WindowServer that disablement meant every later command buffer was refused
  and the session restarted ("Failed to get a MTLCommandBuffer")
- Every kernel driver now carries the package version. A Mac that still runs an older kernel driver after an update
  (the kernel collection was not rebuilt) is named in Send logs and in the "reached the desktop" report instead of
  counting as the new version

Logging (so a report says what happened)
- The kernel driver logs its own version at start
- A full video-memory window logs the first refusals and then every 256th, with one line saying who holds the window
- Spare framebuffer instances (more instances than displays) say so instead of reporting a failure
- A display that stops flipping logs how long the frame had been pending and the flip counters
- Every ten seconds of screen activity, one line per display records the flip rate against the panel rate, missed
  refreshes and skipped frames
- A refused mode set or flip in the kernel names the stage that refused it
- The Metal driver says why it could not begin a command buffer (a lost GPU, a failed allocation), the line
  WindowServer crashes without

## 2.3.3

Stability
- 2.3.2 made some Macs worse: blank or torn windows and a Dock missing its apps on macOS Sequoia, WindowServer
  crashes on some Tahoe desktops. Its display changes (screenshots and the lock screen, the brightness and video-memory
  logging, the Turing flip change) are withdrawn from the package until they pass a staged test with the testers.
  2.3.3 ships the 2.3.1 driver, which has the longest clean record in the field, on both macOS 15 and macOS 26
- macOS Sequoia: installing and updating works again (the 2.3.2 fix is kept: the Metal driver is built for macOS 15)
- After a rollback, the installed version record follows the files that are really installed (kept from 2.3.2)

## 2.3.2

Fixes
- macOS Sequoia: installing or updating to 2.3.0 or 2.3.1 stopped with "runtime payload requires macOS 26.0.0". The
  Metal driver in those packages was built for macOS 26 only; it is built for macOS 15 again, and a release can no
  longer ship anything that needs a newer macOS than 15
- Screenshots, window captures and the lock screen showed a blurred or empty window. macOS reads a window right
  after drawing it, and the driver now finishes the drawing first, as Apple's drivers do
- Send logs now includes how much of the card's video memory window is in use, so a black screen with only a
  cursor can be told apart from a leak
- After an update was rolled back, Send logs still named the new version. The installed version now follows the files
  that are actually installed, so reports say what your Mac is really running
- The Metal driver never hands the GPU a missing texture or sampler; it uses an empty one and logs it (a cause of
  Safari crashes in older versions)
- Laptops with the screen on the NVIDIA GPU: every step of the brightness setup is logged, so brightness that stays
  greyed out can be traced from Send logs

AMD / Intel test program
- A test driver that fails to start no longer panics the Mac: memory still mapped when the start gave up is kept
  until it is unmapped, instead of being freed under the mapping
- Intel 11th and 12th gen graphics (Tiger Lake, Rocket Lake, Alder Lake-S) and DG1 now reach the Intel driver. Two
  drivers list these chips and the device was offered to the wrong one, so neither took it
- When a start fails, its first error lines are kept and reported on the next start, even if the Mac stopped
- The test driver's log names AMD and Intel devices correctly

## 2.3.1

Safer updates
- If an update cannot bring your display up, your Mac no longer stays on a black screen. After two starts in a row
  that never reached the display, the driver turns itself off so macOS still starts, and the driver you had before the
  update is put back automatically with one restart. Nothing stays running once an update has worked
- Releases now also have to pass app checks on the new Metal driver (Terminal text, window capture) and may not do
  worse than the release before

## 2.3.0

Fixes
- Plugging a different monitor into the same port: macOS sees the new monitor (its name, size and modes) instead
  of keeping the one it replaced
- Terminal and other apps that draw text with programmable blending: text no longer comes out missing or garbled
  (GTX 16 and others). Draws that read colour from a pass without every attachment were skipped; they are now drawn,
  and overlapping triangles in one draw read each other's colour in order (thanks pengubow)
- Above 60 Hz: the display and the accelerator woke up once per millisecond to check for a finished flip, which
  users' flicker recordings showed as about 56 flips a second on a 143 Hz panel while windows moved. They now wake on
  the flip itself (the display on macOS 15 and 26, the accelerator on macOS 26)
- When a display's mode set is refused, the driver now tries the mode the firmware is already running before giving
  up, and the log says why the mode was refused (reports of a black screen at login on some RTX 3060 boards)
- Draws the Metal driver had to drop are now in the logs that Send logs collects

AMD / Intel test program
- 1401 can join the AMD and Intel GPU test program (AMD/Intel panel, Join). Testers get the newest signed test build
  of the AMD/Intel driver, and after every restart 1401 sends what the driver did, so a start that hangs or panics
  still reaches us. Leave the program from the same panel at any time

## 2.2.0

This release makes the driver do one thing: drive NVIDIA GPUs. Everything else it used to carry is now its own
product or is gone, which makes the package smaller, leaves less running on your Mac, and leaves less to go wrong.

Leaner
- The package carries the NVIDIA driver only: the AMD/Intel GPU driver, the NPU driver and the audio driver are
  separate products with their own packages and installers (the package drops from about 1 GB)
- The boot logo is gone: the OpenCore logo driver, the boot progress screen and the display driver's code that kept
  the logo on screen. Installing, updating or removing the driver takes an existing logo out of OpenCore
- Three background jobs are gone: a display tracker that streamed the driver's log all the time, a WindowServer
  restart at every boot (a flicker workaround from before the flicker fix in 2.1.0), and a crash-check login item.
  Logs are collected only when you press Send logs. Installing or removing the driver takes out what an earlier
  version left
- The 1401 Mac app moved to the 1401 project; it installs this driver with the package's own installer

Fixes
- A display whose firmware boot timing NVIDIA's display code does not accept (seen on an RTX 3060 at 1920x1080) now
  switches to the validated timing for the same resolution instead of crashing WindowServer at login
- Checksum lists are checked strictly: a malformed line in SHA256SUMS used to be skipped while the check still passed
- install.sh fixes PATH before running anything as root, like uninstall.sh
- Crash reports sent with Send logs keep the library identifiers needed to read them (machine identifiers are still
  removed)

## 2.1.0

Driver
- Each GPU generation (GTX 16, RTX 20, 30, 40 and 50) is now described by the card itself: the driver reads the
  architecture and the newest engine classes from the GPU and publishes them, and the Metal driver picks the video
  decoder and the scan-out model from that instead of a table of device IDs
- Cards with a small BAR1 (Resizable BAR off, or a board that does not offer it): the driver moves BAR1 away from the
  boot screen at its own size, so GTX 16 / Turing cards get the same zero-copy display path as the others. A BAR1 that
  macOS left unassigned is reported instead of failing with no reason
- The desktop no longer waits for the GPU on every frame (#196): each window the compositor draws is marked when the
  GPU finishes it, and the display waits only for that mark
- A BAR1 the firmware already placed large is left as it is, and the old setup comes back if a resize fails
- Gamma tables are applied; HiDPI desktop modes can be turned on with nvfbhidpi
- The installer keeps your own settings block in nvrm610.conf

Metal
- Blender: Eevee renders lit scenes. NVIDIA GPUs cannot write the stencil value from a shader, which Blender's Metal
  backend relies on to sort its lighting; the driver now rebuilds that value by drawing once per stencil bit. Also
  fixed: a light loop that hung the GPU (the first active SIMD lane, and loop values lost when helpers were inlined),
  render passes with no attachments, layered 3D colour targets, framebuffer fetch, samplers held in argument buffers,
  and image atomics in helper functions. Blender's shadow pages no longer crash it
- Indirect compute dispatches read their grid size on the GPU instead of stopping to read it on the CPU
- Hardware video decoders are registered with macOS's decode service (#175)

AMD / Intel test driver
- The log now says which driver started, which PCI drivers registered and which device each one matched, so a GPU
  no driver took can be traced from a user's log

Mac app
- Map my GPU (experimental, opt-in)

## 2.0.4

Mac app
- Prepare this Mac for Tahoe is back: on macOS 15 one button installs or updates the driver (it carries the Tahoe
  driver too), parks it and sets the installer's GPU settings, then opens Software Update. After the update the
  first start sets the driver up for Tahoe and restarts once by itself. Cancel puts everything back, also with a 1401
  stick plugged in next to the internal disk (it changes back the config the preparation changed)
- Intel and AMD laptops and PCs whose own GPU OpenCore switches off (disable-gpu, or -wegnoigpu on Intel) can try the
  AMD / Intel test driver: the app no longer says "no GPU" and stops; turning the test driver on turns that GPU back
  on (also on Core Ultra, where the NPU used to hide it), and turning it off switches it off again exactly as it was

1401 for Windows
- When the stick's disk number is gone at write time, the log now lists the USB disks Windows shows instead

## 2.0.3

Driver
- System sleep and wake (PR #182, thanks danlt150): the GPU's memory is kept across sleep, the PCI setup and
  Resizable BAR are restored on wake, and the display driver does the wake steps macOS expects. Before, the GPU
  lost its state in sleep and the desktop could freeze on wake
- The Metal driver finds a submission's copy buffers once instead of on every check (PR #223, thanks HighDelay)
- The display driver no longer fills the log with the same "hdcs" line many times a second (#216)
- Security (#214): diagnostic sysctls that let any user read physical addresses and memory are gone from normal
  builds (they exist only with the nvrmdiag=1 boot argument, and only root can read them); starting the GPU by
  hand is root-only; the kernel addresses used for debugging are left out of shipped builds

Mac app
- AMD boards (AM4 and AM5) whose OpenCore config rewrites the firmware memory map get DisableVariableWrite, so
  macOS cannot write NVRAM through that map (boards that stopped starting after an install)
- When no NVIDIA card is visible, the app says why: -wegnoegpu or -wegonogpu in boot-args, a disable-gpu property,
  or a laptop BIOS set to integrated graphics only
- Macs with only an AMD or Intel GPU can turn on the experimental AMD / Intel GPU driver; it installs from the
  downloaded driver package, without the NVIDIA driver
- Laptops whose built-in screen is wired to the NVIDIA GPU get an opt-in switch for brightness control

1401 for Windows
- Intel PCs (12th gen and newer) that stop at EB.MM.AKM because no kernel slide fit: rebuilding with the stick
  plugged in reads its startup log and turns on DevirtualiseMmio for the next start
- AMD PCs get the same NVRAM write guard as the Mac app (DisableVariableWrite when the memory map is rewritten)
- A stick whose disk number changes during the write is found again instead of stopping the build; a volume that
  disappears is skipped
- When Apple's download server resets the connection, the stop message says what to try on the network
- When Windows cannot format the stick, its own error is now in the log before the fallback

## 2.0.2

1401 for Windows
- A PC with no network adapter that works in the macOS installer gets its stick written, with a note to connect a
  supported Ethernet or Wi-Fi adapter before installing
- A stick that Windows gives a new disk number just before the erase is found again and written

No changes to the driver or the Mac app since 2.0.0.

## 2.0.1

1401 for Windows
- PCs with an NVIDIA card can build for macOS 26 Tahoe. 2.0.0 offered only macOS 15 for them, so picking
  Tahoe stopped the build

No changes to the driver or the Mac app since 2.0.0.

## 2.0.0

macOS 15 Sequoia and macOS 26 Tahoe.

Security
- Fixes for every finding in the security report by @Mahasvan (#214). Thank you for reporting them to us first.
- The driver's administrator check now reads the caller's real user id; it used to answer yes for everyone
- Copies between NVKMS and a process can no longer reach kernel memory, and the NVKMS entry point is root-only
- First-time GPU bring-up requires root
- A process cannot map memory that belongs to another process
- Unknown client types are refused, the bind call checks its range and always drops a stale mapping, waits on GPU
  events are capped, and one process can no longer fill the client table (slots are kept for the system)
- The kernel-address check refuses to arm on any macOS build it was not made for
- The Mac app verifies a signature on its own updates before unpacking them, runs with the hardened runtime, and
  refuses to run its root scripts from a folder other users can write to

Driver
- GeForce RTX 50 series: compute kernels compiled by NVIDIA's own compiler now run on the card
- Fix the black screen with only a cursor after updating to 1.15 (video memory reserved over the boot screen was handed
  out again), including displays larger than the boot screen's reserved memory, such as 3840x2560 on cards with a
  256 MB memory window
- Fix flicker caused by a reused buffer showing an old frame
- A second monitor keeps its high refresh rates, and panels faster than 240 Hz are paced correctly
- Ultrawide, 4K and HiDPI modes are no longer capped at twice the boot resolution
- Cards with a small memory window: the display's own share is counted, so the desktop stops running out of space;
  a Turing card left at 64 MB is resized to the largest size it offers
- A display connected after startup comes up without a restart (#142, thanks @lsCoding666)
- macOS learns the panel's physical size, so scaling is right instead of assuming 72 dpi (#199, thanks @GeekAudio)
- Laptops with the screen on the Intel GPU: the driver no longer takes the Intel GPU's display slot (#131, #119, thanks
  @TararaDotJoshua), and muxless Turing GPUs get the startup settle they need
- Tahoe: ship the macOS 26 accelerator built from this release (1.15 carried an older one: black screen on Tahoe)
- Fix crashes in Photos and media analysis, in apps that split a command buffer at an event, in Chess, and in apps
  that free Vulkan memory (thanks @Exop63)
- Fix memory running out during Super Resolution and other long machine-learning work
- A texture copied for display stays in step on every submission (thanks @KolliasG7); work is reported as scheduled
  only after IOSurface writes finish (#147, thanks @imyago9); a surface keeps its video memory across page-off (#200,
  thanks @imbrahiam)
- World of Warcraft Classic and other Metal games: cube-map arrays, bounds-checked buffer reads and several shader fixes
  (#171, thanks @razvanccr); Firefox WebRender shaders translate (thanks @advokat11); MSAA sample positions on cards
  without programmable 1x positions (#136, thanks @eisda)
- Faster pipeline builds: built in parallel, one hash per shader module, planner results reused, and one private image
  per IOSurface (#173, #177, #181, #188, #189, thanks @modesttao)
- Chrome, Edge and Electron apps draw on the CPU again by default, which ends the artifacts, stutter and voice-call
  crashes in Discord on several cards; `NVMTL_ANGLE_METAL=1` turns their GPU path on
- Optional asynchronous texture uploads, off by default (`NVMTL_ASYNC_TEX_UPLOAD=1`)
- Multi-GPU display selection workaround documented in docs/MULTI-GPU.md (#197, thanks @is-pan)

Experimental, off unless you turn it on
- AMD and Intel GPU support (NMXGPU) and the NPU driver, for macOS 15 and 26: Setup > "AMD / Intel GPU driver (test)"

Mac app
- Finish a macOS update the update helper never finished, and keep this Mac's own BAR settings afterwards
- "Remove NVIDIA driver" stops with an explanation, instead of failing half-way, when System Integrity Protection is on
- The Remove NVIDIA driver startup entry is always visible, and removing the driver keeps the screen on
- Warn when the Mac starts OpenCore only from the USB stick and the internal disk has none
- Explain a failed config backup on a read-only OpenCore partition, and say which step comes first when Install is
  greyed out
- Send logs copies files before running log queries and includes crash reports under any name

1401 for Windows
- Pick macOS 15 Sequoia or macOS 26 Tahoe before the build; see the 1401 changelog for the full list

Known issues
- Preview not showing pictures and lost window transparency on some macOS 15 setups
- Chrome decodes video on the CPU while its GPU sandbox is on (#175)
- Thunderbolt eGPUs: no display
- Laptops with the screen on the Intel GPU: the built-in screen is not accelerated
- HDMI/DisplayPort audio: the display now reports its audio formats to macOS, but sound over HDMI/DisplayPort is not
  working yet

## 1.15.0

The last release for macOS 15 Sequoia.

Driver
- Fix black screen with only a cursor on GTX 16 and other cards without Resizable BAR (regression in 1.14)
- Fix Unity 6 games hanging on the first frame
- Fix windows and web pages showing old content after a redraw
- Fix high WindowServer CPU and low frame rate at 4K after a few minutes
- Fix slow scrolling (window copies up to 35x faster)
- Fix monitors shown as a 30-inch TV in About This Mac
- Fix 165 Hz mode missing on some monitors (since 1.14.2)
- Retry a display that stays dark after sleep on the next wake
- Fix Chrome, Edge and Electron apps drawing everything on the CPU (no WebGL, slow scrolling); their GPU path is on
  by default again and `NVMTL_ANGLE_METAL=0` turns it off
- Allow Chrome, Edge and Electron apps to use the GPU with their sandbox on
- Fix 5120x1440 and other ultrawide modes missing when the firmware started at a smaller resolution
- Fix green lines in Photos and other VideoToolbox scaling (#179, #180, thanks @modesttao)
- Add T400, T600, T1000 and T-series laptop GPUs (#132, thanks @TararaDotJoshua)

Mac app
- Show download progress in Update driver, which looked stuck at "Downloading the newest driver..."
- Ask for access to USB sticks instead of failing with "no OpenCore config"
- Say when the boot stick is unplugged
- Recover from an interrupted install instead of stopping at "installer guard already exists"
- Download from the NullMoth mirror when GitHub is blocked
- Send logs: always finish, keep the start of each boot, stop re-sending old logs

1401 for Windows
- Fix the build stopping on ACPI tables that declare a method twice (BIOSTAR B650 and similar)

Other
- Grammar fixes in the release notes (#128, #129, thanks @lunar-me)
- Translator test fix (#172, thanks @modesttao)
- Release notes moved into this file; add CONTRIBUTING.md and a pull request template

Known issues
- If Chrome pages show corrupted areas on your card, start it with `NVMTL_ANGLE_METAL=0` and send logs
- Ghost or black areas after windows overlap with a BAR of 4 GB or more (#127)
- Chrome decodes video on the CPU while its GPU sandbox is on: VideoToolbox picks a decoder before the driver loads (#175)
- Thunderbolt eGPUs: no display
- Laptops with the screen on the Intel GPU: the built-in screen is not accelerated
- No HDMI/DisplayPort audio

## 1.14.7
- Stop driver updates from turning the EFI boot logo back on
- Keep the existing OpenCore BAR settings when installing
- Show install, cancel and failure in the updater after the download finishes

## 1.14.6
- Fix Firefox WebGL inside the GPU sandbox
- Turn off the Chromium ANGLE Metal path by default after corruption reports on RTX 30 and 50
- Retry Apple recovery downloads that return HTTP 403

## 1.14.5
- Fix Unity 6 games hanging on the first frame
- Keep Chromium WebGL on the GPU when the sandbox blocks a memory query
- Send logs: give each log query its own time limit

## 1.14.4
- Fix full-screen flicker on machines that copy frames to the screen
- Send logs includes the AMD/Intel test driver

## 1.14.3
- Stop macOS writing to firmware memory on Ryzen 7000/8000/9000 (AM5)

## 1.14.2
- Fix black login window on high refresh rate monitors
- Fix SecurityAgent and Installer crashes on some RTX 40 PCs
- Fix apps quitting on a GPU check (Safari, photo analysis)
- Fix the AMD/Intel test driver firmware download

## 1.14.1
- Fix PCs that stopped starting after 1.14.0
- Fix the boot logo and test driver switches doing nothing

## 1.14.0
- Fix black screen with a cursor about a minute after start-up (#16, #26)
- Fix 4K at 120 Hz and other DSC modes going black (#77)
- Turn the monitor off when the display sleeps (#90)
- Bring a display back after switching inputs (#110)
- Fix black screen after 100 seconds on older Intel boards without Resizable BAR

## 1.13.0
- Fix Turing cards stuck at start-up after updating
- Fix "Remove NVIDIA driver" not removing the driver, or hanging
- Crash reports no longer show crashes from before the update

## 1.12.0
- Use the NVIDIA GPU on laptops whose screen is on the Intel GPU (as a second Metal device)
- Record display state from the first second of each boot for flicker reports
- Fix Live Text crashing in the Vulkan driver

## 1.11.0
- Download Lilu and AMFIPass when they are missing from the EFI
- Fix Firefox crashing at launch (`ipc_control_port_options=0`)
- Fix "STOP back up nvmtl"
- Add USBToolBox when it is missing

## 1.10.0
- Fix AM5 Ryzen PCs stuck at `EB.MM.AKM`
- Drop the DMAR table on Intel Core Ultra
- Reach Apple's recovery server directly when DNS fails

## 1.9.0
- Try one more memory setting on AM5 boards
- Erase stubborn USB sticks with diskpart

## 1.8.0
- Use known-good settings for boards that have booted before
- Fix "Format-Volume: Invalid property" and "WinError 2" when erasing

## 1.7.0
- Pass the board's DSDT to macOS again (was dropped on about 1 in 5 PCs)
- Send logs delivers the flicker capture and OpenCore config

## 1.6.0
- Fix the driver removing itself after login on some boards
- Fix `RmInitAdapter failed` on Ryzen with a custom EFI
- Send logs includes the OpenCore config (serials removed)

## 1.5.0
- Finish the macOS download on networks that cut connections

## 1.4.0
- Clear flicker at login with a compositor restart
- Build on PCs whose ACPI tables crash the disassembler

## 1.3.0
- Fix Send logs freezing the app
- Fix the crash loop on 16 GB RTX 50 cards
- Light up a monitor plugged in after login

## 1.2.0
- Fix "Remove NVIDIA driver" so the driver stays off until removed
- Show the picture with macOS's display driver while ours is off

## 1.1.0
- Fix recycled allocations reaching queued GPU work
- Fail a rejected display request on its own instead of stopping the display

## 1.0.x
First public releases: installer, updater, removal and USB map fixes.

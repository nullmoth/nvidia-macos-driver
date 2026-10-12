# NullMoth NVIDIA driver and Mac app 1.14.0

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

Every fix below came from the logs, chats and GitHub issues you sent. For each: what was wrong, and what changed.

## Display
- **Black screen with only the cursor about a minute after start-up** (issues #16, #26, and many reports in chat). One
  minute after start-up the driver switched to showing frames straight from the card's memory. On a PC whose desktop is
  drawn on the Intel or AMD integrated GPU, or whose card has the small 256 MB BAR window, that memory never held the
  desktop. The driver now keeps the copy path on those machines, which is what `-nvrmnoflip` did for everyone who used it.
- **4K at 120 Hz and other fast modes black or offline** (#77). Modes faster than DisplayPort 1.4 can carry need
  compression (DSC). The driver now tells macOS the compression settings, so macOS stops judging those modes impossible.
- **The monitor kept the last frame when the display slept** (#90). The display now turns off, and on again on wake.
- **No picture after switching the monitor to another input and back** (#110). The driver only brought up displays it had
  never seen; a known display that comes back now gets its picture again.
- **Black screen after about 100 seconds on older Intel boards without Resizable BAR** (B150 and similar). Their firmware
  does not list a 64-bit PCI range, so the driver found no room for the card's memory window. On Intel CPUs it now uses
  the range every Intel board from 6th gen on decodes (fix found and tested by a community member).
- **Small-BAR cards refusing graphics memory** (#27, #28). The memory the driver hands out grows with the BAR window instead
  of a fixed 192 MB, and the boot screen's region no longer blocks the last megabyte.
- **The NullMoth boot logo** replaces the Apple logo while macOS starts and stays through the hand-over to the driver. On by
  default; Mac app → Boot logo off, or `-nmlogooff` for one start.

## macOS 26 (Tahoe)
- **Black desktop on macOS 26** (#51). Every package since 1.1.0 carried an out-of-date macOS 26 accelerator that could not
  talk to the current framebuffer. It is now built from the same source as the macOS 15 one.

## Apps
- **Windows and images going black or stale until clicked; Preview showing blocks** (#73, #74, #8). An image's contents
  were marked up to date before the GPU drew them (community patch, #98). 3D-texture copies now copy every layer (#84).
- **Photos face detection crashing (mediaanalysisd)** (#107). A Metal object was released after it was freed.
- **"Your browser does not support WebGL"**. Shaders that read the sample count failed to translate.
- **Chess quit about two seconds in.** Moving the mouse makes Chess read pixels back, and the graphics driver did that
  copy as an internal draw that still carried the 4x anti-aliasing setting of the board's last pass. A debugging check
  in the driver stopped the app on that mismatch; the copy now draws with its own setting. Apps that ask for the
  standard anti-aliasing sample positions also get them now instead of an empty list.
- **Hardware video encoding on RTX 5070 Ti and similar cards** (#105). The encoder only tried the card's first encoder
  engine, which those cards do not have; it now uses the first one the card has.

## Installing
- **Quadro T1200 and other Turing workstation cards refused as "older than the driver supports"**. Every Turing or newer
  card is accepted now.
- **The wrong OpenCore partition edited** when the macOS disk holds two (#81).
- **Start once without the driver**: a new boot-picker entry starts macOS or Recovery one time without the driver.
- **The app updates itself**: Updates → Check for updates finds a newer app too, and Update the app replaces it in place.
- **Send logs** now records every GPU of any brand (gpu-all.txt), so GPUs the driver does not cover yet can be built.
- **The display recorder never started** (#108), so Send logs and Record the flicker had no display trace. It now waits
  for the driver at start-up. The kernel log in Send logs is no longer filled by one line repeated every frame (1401 #16).

- **Hardware video encoding checked end to end**: apps that encode through VideoToolbox (Final Cut, OBS, Screen Sharing)
  get the card's H.264 and HEVC encoders, in hardware.

## AMD and Intel GPUs, and NPUs (test)
New in 1.14, off unless you turn it on.

**What it is.** A second driver, NMXGPU, runs Linux's own drivers for AMD and Intel graphics (amdgpu, i915, xe) on
macOS, with Metal on top through the same Vulkan path the NVIDIA driver uses. It is for the GPUs macOS has no driver
for: Ryzen APUs, Radeon RX 7000/9000, Intel Iris Xe and Arc. A third, NMXNPU, runs Linux's NPU drivers (Intel's ivpu for
Core Ultra: Meteor Lake, Arrow Lake, Lunar Lake; AMD's amdxdna for Ryzen AI), so apps can run AI models on the NPU.

**How to turn it on.** Mac app → *AMD / Intel GPU and NPU driver (test)* → **Turn the test driver on**, then restart.
It downloads only the firmware your GPU and NPU need (an NPU-only PC needs 8 files, an AMD GPU about 700), each one
checked against linux-firmware's published hash. **Turn it off** in the same place. If a GPU it takes over drives your
screen and the screen stays dark, pick **1401: Start once without NVIDIA driver** in the boot picker, then turn it off.

**Using the NPU.** Everything installs in `/Library/NullMoth/npu`:
- Intel NPU: OpenVINO with its NPU plugin, Intel's NPU compiler and user-mode driver.
  `/Library/NullMoth/npu/openvino/bin/hello_query_device` lists `NPU` when it works;
  `benchmark_app -m model.xml -d NPU` runs a model on it.
- AMD Ryzen AI: AMD's runtime (XRT) with its NPU plug-in.
  `/Library/NullMoth/npu/xrt/bin/xrt-smi examine` shows the NPU, and `xrt-smi validate` runs AMD's own NPU tests.
- `/Library/NullMoth/npu/bin/nmx-npu-probe` shows what the kernel driver reports for each NPU.

**Status, honestly.** None of this has run on an AMD or Intel GPU yet. On the NPU side, Intel's NPU compiler built for
macOS compiles models for Meteor/Arrow Lake and Lunar Lake NPUs, and Intel's driver tests pass (164 of 164) against
their simulated NPU. On a real Arrow Lake NPU the driver now loads, finds the NPU and loads its firmware, but the NPU's
firmware does not report ready yet, so the NPU is not usable in this release; Linux on the same PC starts it, and the
remaining difference is being traced. If you turn it on, **Send logs** now includes `npu.txt`: every layer from the PCI device to the runtimes,
with their debug logging on, so a failure on your machine names the layer that broke.

**Why it took fixes in the driver layer it runs on** (found before and during the first hardware run):
- **No GPU device for Vulkan, no NPU device at all.** The layer created none of the device files the user-side drivers
  open (`/dev/dri/renderD128`, `/dev/accel/accel0`), and used up every device slot at start-up. Devices are now created
  one per GPU or NPU under the names Linux uses, readable by apps as on Linux.
- **The NPU drivers never registered.** They registered with a PCI bus class that did not exist yet at that point in
  start-up, and the error was dropped. The class is now created on demand, and a driver that fails to register says so.
- **Start-up stopped at the boot logo with the NPU on (a kernel panic 7 seconds in, every start).** Memory the Linux
  drivers allocate in blocks larger than a page was freed through the wrong allocator. Fixed. The drivers also attach a
  moment later now, once the disks are mounted.
- **A crash or hang can no longer repeat at every start.** Before attaching, each test driver writes a marker to NVRAM
  and clears it once the attach returns. If a start finds the marker, the last attach never finished, so the driver
  leaves the GPU or NPU alone and says so in Send logs. Turning the test driver off and on clears it.
- **Every memory range of 2 GB or more was wrong.** The 2 GB size constant overflowed to minus 2 GB in the layer the
  Linux drivers run on, which broke the NPU's address space and would have broken large ranges in amdgpu, i915 and xe.
- **Memory the NPU reads could be stale.** Pages switched to write-combining were not flushed from the CPU's caches
  first; the NPU reads them without looking in those caches. They are flushed now, as on Linux.
- **The NPU now starts from a reset** (function level reset before the driver attaches), as Linux does.
- **Turning the test driver on always failed** with "a GPU firmware file did not match its checksum": the check was
  given a list format it does not read. The download also asked for every vendor's files at once and GitLab refused it
  ("429 Too Many Requests"); it now fetches only what your hardware needs, retries, and falls back to kernel.org's copy
  of the same files.

## Still open
- HDMI and DisplayPort audio from the NVIDIA card.
- AMD and Intel GPU acceleration has not run on those GPUs yet: turn the test driver on and Send logs to help.
- The Intel NPU's firmware does not start yet (see above).
- Plugging in a monitor the driver has not seen before while macOS runs.

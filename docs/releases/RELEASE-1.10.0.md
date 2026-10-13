# 1401 and NVIDIA driver 1.10.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Windows (1401 1.10.0)
- Ryzen 7000, 8000 and 9000 PCs (AM5 and Zen 4/5 laptops) stuck at the prohibited sign / `EB.MM.AKM`: the first build now uses the memory settings these CPUs boot with. Build the stick again with 1.10 — rebooting an old stick changes nothing.
- Intel Core Ultra: VT-d's DMAR table is dropped for macOS, which every Core Ultra that booted needed.
- When the network's DNS fails or refuses Apple's recovery server, 1401 reaches Apple's own server directly. A download that keeps stalling now says what each connection did and what to try (antivirus web scanning off, a phone hotspot, or a VPN).
- When the USB stick itself fails or disconnects during the write, 1401 says so and what to do instead of showing a bare error.
- No-name sticks that Windows doesn't show after the erase are used through the drive letter diskpart gives them.
- The stick gets the driver package that matches the app, even when older versions are in the same folder, and a driver download that stops early is fetched again.

## Driver
- OpenCL works: Geekbench 6 OpenCL runs all eight workloads to the end. Kernels with `__local` buffers, integer images or very large grids run.
- Apps that check whether a texture or buffer is writable get the right answer (it was always "read only" before).
- OpenCL and Metal buffers handed to the driver as managed memory now live on the GPU: image-heavy work that crossed the PCIe bus on every access is many times faster.
- Hardware H.264 decoding on GPUs before the RTX 50 series.
- More resolutions and refresh rates are offered: modes listed only in the monitor's DisplayID block (most 100-240 Hz modes) and resolutions above the one the firmware started with.
- Moving the mouse no longer re-shows an older frame.

## Mac app
- Setup turns on Lilu and AMFIPass in OpenCore when they are off. With AMFIPass off, macOS refused to load the driver and the screen stayed black with only a cursor.
- The flicker workaround restarts the window server only at the login window, so auto-login and a quick login are never interrupted.
- **Remove driver** works when the driver was installed from the stick or by hand (no install record): its settings come out of the OpenCore config this Mac started from and its files are removed.
- New **Record the flicker** button: 20 seconds of display timing evidence, sent with your logs.

## Still open
- Screen flicker on some high refresh rate and multi-monitor setups (the workaround stays on).
- HDMI and DisplayPort audio from the NVIDIA card.

## Scope
macOS 15 Sequoia. Requires OpenCore.

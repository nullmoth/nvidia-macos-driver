# 1401 and NVIDIA driver 1.12.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Dual GPU laptops (Optimus / MUXless)
- **The NVIDIA GPU now works on laptops whose screen is wired to the Intel GPU.** It becomes a second Metal device, the way the discrete GPU works on Apple's own two-GPU MacBook Pros: the Intel GPU drives the screen and WindowServer, and apps that want the fast GPU render on the NVIDIA card while macOS copies their frames to the screen. Before, the driver hid the card from Metal when no monitor was connected to it.
- Intel UHD/Iris from 6th to 10th generation (Skylake through Comet Lake) and Ice Lake are accelerated by macOS's own Intel driver, so these laptops get both GPUs fully accelerated.
- On 11th generation and newer Intel (Tiger Lake, Alder Lake, Raptor Lake and later), macOS has no Intel driver: the built-in screen shows macOS without acceleration, and apps still get the NVIDIA GPU for Metal.
- It switches on about a minute after startup, only when no monitor is connected to the NVIDIA card. Boot argument `-nvaccelnoheadless` turns it off. This is new and was built from the reports in issues #76 and #106: if your laptop does not show the NVIDIA GPU in System Information > Graphics after a minute, press **Send logs**.

## Mac app
- **Display tracker.** Flicker and black screens (above 60 Hz, two monitors, after changing the refresh rate) are now recorded from the first second of every boot: the driver's display state every second, the refresh rate of each display, every WindowServer start, WindowServer's own messages about mode changes, and the monitor's EDID. The trace is kept on disk, so it survives a forced restart. **Send logs** uploads it, so if your screen went black, restart, then press Send logs; we can see what happened. Press **Record the flicker** while it happens to mark the moment.
- Live Text and other apps that drew with an unusual depth range no longer crash (mediaanalysisd aborted in the Vulkan driver).
- Browsers: the Vulkan driver no longer asks for write access to `/dev/null`, which browser GPU sandboxes refuse. This is the first of the WebGL fixes.
- Laptops whose built-in screen is wired to the Intel graphics keep that screen: setup no longer installs the generic display path that turned it off.
- **Send logs** no longer re-sends old setup logs from earlier versions, and every setup log now records the app version and time.
- A **Buy me a coffee** button sits beside the crash report buttons.

## Windows (1401 1.12.0)
- Laptops whose built-in panel runs on an Intel GPU macOS has no driver for keep that GPU on, so the panel shows macOS.
- "The EFI changed after its machine profile was recorded" now names the files that changed, and says to allow the 1401 folder in your antivirus if it removed one.
- A **Buy me a coffee** link sits beside Scan for logs.

## If you saw these on 1.10/1.11
- "AMFIPass.kext is not in this OpenCore EFI": update to 1.11 or later; setup adds it for you. Logs that still show it after updating were old logs.
- `EB.MM.AKM` / prohibited sign on Ryzen 7000/9000: build the stick again with 1401 1.11 or later; an old stick does not change by restarting it.
- The recovery download stops at 1 MB: something on the network (often antivirus web scanning) cuts Apple's download. Turn off web scanning, or use a phone hotspot or VPN.

## Still open
- Black screen or flicker above 60 Hz on some monitors: 1.12 records it; please **Send logs** after it happens.
- macOS installer freezing at "19 minutes remaining" on some AMD PCs: send a photo of a verbose boot (`-v`) and your NVMe drive model.
- Acceleration for the screen itself on 11th-generation and newer Intel and on AMD integrated graphics.
- WebGL in browsers (two more fixes), HDMI and DisplayPort audio from the NVIDIA card.

## Scope
macOS 15 Sequoia. Requires OpenCore.

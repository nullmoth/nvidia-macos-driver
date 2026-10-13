# 1401 and NVIDIA driver 1.2.0


Changes
- The install leaves the Metal allow list (`/Library/GPUBundles/nvmtl-allow.txt`) and the driver bundles readable by WindowServer, and stops if WindowServer cannot read them. 1.1.0 copied the list with the app's private permissions, so WindowServer got no Metal device and CoreDisplay stopped it on every start ("Failed to create MetalDevice"): the black screen and verbose loop after installing 1.1.0. Already stuck on 1.1.0? Start with the driver off, run `sudo chmod 644 /Library/GPUBundles/nvmtl-allow.txt`, and restart.
- "1401: Remove NVIDIA driver" in the boot picker now keeps the driver off until it has been removed, even if you restart instead of choosing macOS. Before, OpenCore cleared the setting at the next start and the driver loaded again.
- While the driver is off, macOS's own display driver shows the picture, so the screen no longer stays dark until the second restart.
- Installing again clears a removal that did not finish, so the new driver loads.
- A Metal device that is refused reaches the system log with the reason.
- Windows: the stick's log of a failed start reaches the next build even when the stick is plugged in after the build (it used to be erased unread); sticks with EFI or MSR partitions are accepted; stalled Apple downloads resume; macOS 10.13 and 10.14 targets; mirror downloads retry; firmware tables that define one device twice no longer stop the build.

Validation
- macOS 15.8.1, RTX 5060: installed through the Mac app's setup path with the app's own permissions; the allow list is world-readable after install.
- Install, recovery, removal, EFI ownership, OS update and card-support regression suites pass; the install test fails on the 1.1.0 installer.
- Kernel extensions are 1.1.0 plus the removal flag check only (strings and symbols compared with 1.1.0).
- After a restart: four kernel extensions loaded, WindowServer steady with no crash reports, Metal 3 on two displays.
- Remove flag set, then a plain restart (no boot picker): the driver stayed off, the screen stayed lit, the driver was removed, the flag cleared and the Mac restarted by itself. 1.2.0 then installed again cleanly.

Known issue
- With two displays connected, changing the refresh rate or resolution of one display can make the other display flash or go blank until the Mac is restarted.

Scope
This update installs on macOS 15 only. It keeps the 1.1.0 compiler, GPU userland and firmware.

# 1401 and NVIDIA driver 1.1.0


Changes
- Pair framebuffer, allocation and accelerator interfaces so queued work cannot use a recycled allocation identity.
- Retain display backing until the replacement flip is latched; bound the surface cache and serialize display and allocation transactions.
- Query and reset the mapped NVIDIA flip notifier for each committed surface. Accept the initial console handoff without prematurely releasing its backing.
- A display request the GPU rejects (for example an unsupported cursor size) now fails only that request. Previously one rejected request could stop presentation and block every later resolution and refresh change until restart.
- Balance asynchronous callback references across scheduling, cancellation and shutdown.
- Move the Mac Send logs authorization dialog onto the main thread and show its authorization status. Retain local reports and exact-byte upload confirmations from the logging maintenance updates.
- Require startup-partition ownership evidence before internal EFI operations; remove the timestamp-only fallback.
- Windows: repair EFI Doctor panic parsing, retry recovery downloads and forced USB dismounts, handle duplicate kexts, improve firmware guidance and display scaling, and use verified dependency mirrors.
- Preserve signed recovery loading with SecureBootModel enabled.
- Allow bounded cold-start CIM queries to finish inside the existing hardware-worker timeout.
- Include matching 1.1.0 Mac companion and driver payload in the Windows bundle.

Validation
- macOS 15.8.1, RTX 5060, two physical displays: guarded reboot, refresh changes (60, 75 and 100 Hz) committed on the changed display with WindowServer running throughout, and a verified 4096-byte Metal buffer copy.
- Native allocation, callback and mapped completion tests with address/undefined-behavior sanitizers; paired kernel builds and macOS 15 auxiliary collection validation.
- Windows source and packaged startup, report lifecycle, native hardware capture and regression checks are recorded in VALIDATION.json.
- Public artifacts and source are scanned before publication; downloadable payloads have SHA-256 checksums.

Known issue
- With two displays connected, changing the refresh rate or resolution of one display can make the other display flash or go blank until the Mac is restarted. A fix is planned for 1.2.

Scope
This update installs the revised paired driver on macOS 15 only. Device-table coverage is broader than physical validation. RTX 5090, macOS 26, switching between integrated and discrete graphics, DRM and full Firefox/Blender/Geekbench behavior are not qualified by these checks. Optional DTrace capture and automatic internal EFI migration remain unavailable pending their separate security and runtime qualification. Existing compiler, GPU userland and firmware binaries are preserved; this is not a new direct machine-code compiler release.

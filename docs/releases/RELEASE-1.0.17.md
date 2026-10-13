# 1401 Mac 1.0.17

Driver helper 1.0.11 creates a unique recovery directory for every install, preventing recovery snapshots from being merged when two runs start in the same second. A guarded driver install now refuses overlapping cooperating installers before kernel-collection preflight and retains its guard if restoration cannot complete. Existing guards, symlinked parents, unexpected ownership, writable parents and unreviewed access-control entries stop the install for review; existing permissions and foreign files are preserved. If a guard directory identity changes during the install, cleanup retains its current path for review.

Backup failure output retains the component, command exit status and last 4096 bytes of error output. Backup refusal still stops before driver replacement. The installer guard covers the driver installer only; earlier OpenCore configuration changes made by the setup companion are outside it. This update does not establish the cause of an unlinked Tahoe backup report.

Automatic USB-to-internal EFI promotion remains disabled. Keep the startup OpenCore stick or disk attached for every restart. Optional DTrace capture remains unavailable; ordinary saved logs and explicit Send logs consent are unchanged.

All 47 protected GPU/kernel/compiler/firmware/removal files and the two runtime links remain byte-identical to helper 1.0.10. The existing NVIDIA userland runtime requires macOS 15.5 or later. This is an installer maintenance repair, with no new graphics, per-card, sleep, protected-playback or macOS 26 hardware qualification.

Validation: twenty isolated production-shell backup, overlap, parent ownership, directory replacement, copy failure and rollback checks pass. The fixture copies use only private temporary files; no live install or hardware workload is performed. Distribution builds are separately bound to reviewed source and checked for signatures, versions, complete archive contents and hashes before publication.

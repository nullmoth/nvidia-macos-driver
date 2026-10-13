# 1401 Mac 1.0.16

This update repairs USB-to-internal OpenCore promotion. Earlier versions could replace the entire destination EFI folder, removing active Windows and vendor boot files even when a backup existed.

Automatic USB-to-internal copying is now disabled. Driver installation continues using the verified or explicitly selected startup partition. Existing Windows BCD, native loaders, vendor files and fallback paths remain in place. Keep the OpenCore stick attached for every restart until the internal boot setup is separately reviewed. A secure migration helper remains future work.

The hardware-log improvements from 1.0.15 remain available. Optional DTrace capture remains unavailable; ordinary logs retain the explicit submission consent. The obsolete Tahoe preparation action remains removed.

Driver helper package 1.0.10 and its GPU payload remain unchanged. This companion repair does not add graphics, macOS 26 or per-machine functional qualification.

Validation: production EFI-segment no-write fixtures and negative controls, existing EFI selection/removal/runtime checks, full companion build, strict individual and bundle signatures, and final archive verification are required before release.

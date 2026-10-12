# Queued IOSurface contents

A committed command buffer could clear an IOSurface successfully, while a
following buffer read black pixels without reporting a command-buffer error.
The second buffer imported the old CPU pixels before the first buffer finished
publishing its GPU contents. A partial update could consequently erase all
pixels outside its updated rectangle.

Committed writers now suppress that stale import for the same GPU backing.
Recording alone does not register a writer, so abandoned command buffers still
leave CPU contents available. Completion removes weak ownership entries. An
application must still provide queue order or an event dependency between its
GPU operations.

Private fallback images now share backing across compatible wrappers of the
same surface. CPU import and GPU publication use a monitor shared by wrappers
of one surface plane. This also protects the interval where publication has
changed the IOSurface seed but has not yet finished copying pixels. A subsequent
CPU edit still invalidates the cached seed and imports its new pixels.

Separately, an NVK client with no live mappings must detach its file descriptor
before entering the connection pool. Failed detachment closes the connection
instead of pooling it. This prevents descriptor reuse from finding an older
kernel client's binding.

## Regression checks

- `python3 tools/test_surface_queue.py`: committed ownership, aliases, CPU seed
  changes, queues, planes, weak lifetime, and repeated cleanup.
- `python3 tools/test_surface_publication.py`: force GPU publication to overlap
  CPU import through both a private view and independent shared-memory wrappers;
  verify preserved GPU pixels and a later CPU edit.
- `python3 tools/test_fd_pool.py`: descriptor reuse, detach ordering, detach
  failure, live mappings, pool exhaustion, and repeated reuse.
- `tools/bench_surface_order.m`: 100 pixel-checked frames per mode. Modes are
  `queued`, `alias`, `alias_read`, `cross`, `partial`, `partial_alias`, `same`,
  `wait`, `cpu`, `abandoned`, and `private`. It accepts `NM_TEST_PLUGIN` for
  loading a standalone candidate in the probe process.
- `tools/bench_memory_mapping.c`: allocate, map, validate, and free host-visible
  Vulkan memory without GPU submissions.

On macOS 15.8.1 with an RTX 5060, all eleven surface modes passed with shared
family memory and forced private fallback, using the corrected NVK. The release
plugin also passed the eleven modes with the installed NVK. The Apple M1 Pro
comparison passed the same eleven modes. Core Image resizing passed at input
sizes 1280x800, 1600x1000, 1920x1080, 2200x1000, and 2560x1440. A deterministic
publication test fails when synchronization is removed.

These checks establish the reproduced queue/import fixes. They do not establish
high-refresh display modes, small-BAR kernel reclamation, sleep/wake, or successful
boot on other hardware.

## Kernel resource lifetime and display power

Pending dirty-surface entries now identify their resource owner. Resource destruction removes only that owner's entries, without dereferencing a device cache or clearing a newer owner's mark. The production-destructor test covers 10000 lifetimes, full-table recovery, pointer reuse, and concurrent owners. Removing the cleanup reproduces the failure.

Display power requests now share the display gate. A rejected wake keeps the sleeping state so the next request can retry, and an unsuccessful wake emits no successful-power-on notification. Connection-power requests perform the same transition and report failure when it is rejected. Head-off commits use the tracked submission path, completing earlier primary work before disabling the head. The production-transition test covers failed sleep, failed wake and retry, repeated requests, shutdown, and the existing DPMS opt-out.

The accelerator builds for both supported OS layouts, and the framebuffer builds for macOS 15. These kernel changes still require boot and display-power qualification on hardware before a binary release.

## Boot raster publication and surface upload

Initial timing publication now includes a desktop detailed timing at the boot raster's own refresh rate. It retains the restriction against smaller rasters macOS did not propose, checks scanout capacity, and avoids duplicate or scaled-only suppression. The production mode builder reproduces a missing 2560x1600 at 165 Hz timing without this fix; the corrected builder publishes stable ID 0x10100.

Monitor connection flags no longer unconditionally request CEA underscan. The flag value zero leaves ordinary monitors on their normal display policy.

On x86 CPUs with SSE4.1, aligned large surface imports use streaming loads for write-combining pages. Unaligned sources, short copies, other CPUs, and unsupported features retain memcpy. Alignment, tails, destination guards, and protected read boundaries passed on ARM and x86. On the qualification rig, five byte-verified copies of a 33177600-byte write-combining IOSurface averaged 134.074 ms with memcpy and 3.689 ms with streaming loads. This is a copy benchmark, not a measured desktop frame-rate result. The updated release plugin passed the eleven 100-frame surface modes.

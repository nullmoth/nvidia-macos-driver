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

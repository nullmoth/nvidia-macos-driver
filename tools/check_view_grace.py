#!/usr/bin/env python3
"""check_view_grace.py - every image view and texel buffer view dies through the grace, never beside it.

NVK keeps image and buffer views as indices into its device-wide descriptor table, and a bindless heap element stores
that index; a destroyed view's index is reissued to the next view created, so a submit that read the element before the
slot was rewritten stores through the new owner. plugin/nvmtl_vk.c therefore destroys views through two routines that
defer the destroy past the submits in flight when NVMTL_IMAGE_GRACE=1 (nvmtl_vk_image_view_destroy and
nvmtl_vk_texel_view_destroy) and the grace reaper itself. This check reads every raw vkDestroyImageView /
vkDestroyBufferView call in nvmtl_vk.c, names the function it sits in, and fails when one is outside the allowed set:
the two deferring routines (each must consult nvmtl_image_grace_on), the reaper's own destroy paths, image creation
failure, the per-command-buffer attachment-view retire (attachment views never take a heap slot), the swizzle probe view
(destroyed before anything can index it), image migration's failure path, and the grave-reaped backing release.

    python3 tools/check_view_grace.py              # exit 1 on a raw destroy outside the grace, or a routine without it
    python3 tools/check_view_grace.py <nvmtl_vk.c> # over another copy (the source before the texel grace must fail)
"""
import os, re, sys
here = os.path.dirname(os.path.abspath(__file__))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin", "nvmtl_vk.c")
src = open(path).read().split("\n")
ALLOWED = {
    "nvmtl_vk_image_view_destroy", "nvmtl_vk_texel_view_destroy",          # the deferring routines
    "nvmtl_vk_image_destroy_now", "nvmtl_vk_texel_view_destroy_now", "nvmtl_image_grace_reap",   # the reaper's destroy paths
    "nvmtl_failed_image",                                                   # creation failure: never indexed
    "nvmtl_cmd_retire", "nvmtl_vk_cmd_retire_view",                        # attachment views, never heap slots
    "nvmtl_vk_image_view_create_range_swz",                                # a probe view destroyed at once
    "nvmtl_vk_image_migrate", "nvmtl_vk_image_free_backing",               # failure path / grave-reaped backing
}
MUST_DEFER = ("nvmtl_vk_image_view_destroy", "nvmtl_vk_texel_view_destroy")
func_re = re.compile(r"^(?:static\s+)?(?:inline\s+)?[A-Za-z_][\w\s\*]*?\b([A-Za-z_]\w*)\s*\([^;]*$")
current = None; bodies = {}; bad = []
for n, line in enumerate(src, 1):
    m = func_re.match(line)
    if m and not line.startswith((" ", "\t")) and not line.strip().endswith(";"):
        current = m.group(1); bodies.setdefault(current, [])
    if current: bodies[current].append(line)
    if re.search(r"\bpvkDestroy(ImageView|BufferView)\s*\(", line) and not line.strip().startswith("//"):
        if current not in ALLOWED: bad.append((n, current, line.strip()[:100]))
missing = [f for f in MUST_DEFER if f not in bodies or not any("nvmtl_image_grace_on()" in l for l in bodies[f])]
print("raw view destroys outside the allowed routines: %d; deferring routines without the grace: %d" % (len(bad), len(missing)))
for n, f, l in bad: print("RAW DESTROY line %d in %s: %s" % (n, f, l))
for f in missing: print("NO GRACE in %s" % f)
if not any(re.search(r"\bpvkDestroy(ImageView|BufferView)\s*\(", l) for l in src): print("FAIL: no view destroy found at all - the check cannot run"); sys.exit(2)
sys.exit(1 if (bad or missing) else 0)

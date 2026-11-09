# Curve geometry for Metal ray tracing

Status in 2.9: written, tested offline, **off by default**. `NVK_CURVES=1` turns it on for a process (NVK and the plugin read the
same variable). It is software traversal on the shader cores: the RT cores have no curves, and `NVK_HWRT_QUERY` neither helps nor
hinders (a hybrid query sends curve boxes through the same software walk as other boxes).

## What is carried

`MTLAccelerationStructureCurveGeometryDescriptor`: round and flat curves; B-spline, Catmull-Rom, linear and Bezier bases; 2, 3 or 4
control points per segment (linear 2, Catmull-Rom 4, Bezier and B-spline 3 or 4); end caps none, disk, sphere; control points
float3 or half3, radii float or half, indices uint16 or uint32. Anything else (other formats, a missing buffer, a basis and control
point count that do not go together) is built EMPTY with a log line, as the other uncarried geometry is; it is never guessed.

## How it flows

1. Plugin (`nvmtl_curve_from_descriptor`): reads the descriptor and hands NVK a box geometry whose `pNext` carries a
   `VkNvkCurveGeometryMESA` (`nvk/curves/nvk_curve.h`: addresses, strides and one flags word).
2. NVK build (`curves/curves.patch`): `bvh/curve_aabb.comp` writes one box per segment into the tail of the scratch buffer (scratch
   size grows by 24 bytes per segment, 128-aligned per geometry); the shared builder builds the tree over them; the appendix and a
   table of `nvk_curve_entry` (one per geometry) are written into the structure afterwards.
3. Ray query (`nvk_nir_lower_ray_queries.c`): when the software walk reaches a box whose geometry has a table entry, the lowering
   intersects the curve itself (`curve_math.h`), commits a hit like an opaque triangle (`curve_parameter` in the first barycentric,
   `t`, primitive id), and goes on walking. The shader never sees the box as a candidate. Ray flags that cull opaque or non-opaque
   geometry apply as for any geometry; curves are opaque.
4. Translator: `air.intersect.*curve_data` is parsed as a family with one extra float result, `curve_parameter`, read from the committed
   hit's first barycentric (`meta/intersections.rs`, `passes/air_calls/ray_query.rs`).

## The shape (the definition the tests check)

A segment is cut into 8 equal steps of its parameter. Round curve: each step is the convex hull of its two end spheres (a cone frustum
tangent to both plus the spheres; joints are spheres, so no cracks on bends). Ends: sphere caps keep the end sphere; disk caps cut the
end piece by the plane through the end point and close it with a flat disk; none cuts and leaves it open. Flat curve: a ribbon of
half-width equal to the radius, facing the ray. This is an approximation of the true swept surface: 8 steps against 48 steps differ by
at most 0.05 in distance on curves of scale 3 with radii 0.08 to 0.35 (worst measured 0.049, test `info cut accuracy`).
Whether Apple cuts, caps and orients exactly this way is **unmeasured** (no Apple GPU work is allowed on the build machines): the
M-series reference run that settles it is listed below.

## Checks

| check | what it proves |
|---|---|
| `nvk/curves/run_host_tests.sh` | the analytic hull equals an implicit-surface reference (a ray marched through the sampled swept sphere, entry bisected) for all six basis and count combinations with sphere caps, for disk caps, open ends equal disk caps except where the disk is hit, `curve_parameter` is the position along a straight curve, flat curves equal a plane construction, the segment box contains every sphere. Controls: wrong basis weights and wrong cap handling are caught; a Catmull-Rom segment can leave the hull of its own points, which is why the box shader converts it |
| `curve_nir_cpu.c` (needs a built NVK tree) | the NIR form, executed by NIR's own constant folder on 160 random curves and rays, equals the float form (0 differ); it compiles through NAK for sm120 (2805 instructions) |
| `nvk/hwrt/run_cpu_checks.sh`-style compile with `NVK_CURVES=1` | the ray-query shader with curves compiles: 56,352 B software, 116,592 B hybrid; with the switch off the code is the 11,328 B it was |
| `tools/test_app_profile.c` etc. | unchanged |
| `cargo test --lib intersections` | the `curve_data` family parses, result type is `{ i32, float, i32, i32, ptr addrspace(1), i32, i32, float }` for the instancing form, duplicate tokens are refused |

## Measurements still to take (M-series, then a card)

1. On an M-series Mac: ground truth for the cut and cap rules. The oracle is written and compiles: `nvk/curves/oracle/` builds a curve
   acceleration structure for each of 24 cases (six basis and count pairs, round with three cap modes, flat) on the machine's own GPU,
   intersects 576 fixed rays, and dumps hit, distance, `curve_parameter` and primitive id as bits. `compare_oracle` judges the dump against
   `curve_math.h` (hit/miss on 98 percent of rays, distance within 0.02, parameter within 0.03) and is proven without a GPU
   (`oracle/selftest.sh`: a dump made from the same curves passes, one made from curves with 1.5 times the radii fails 24 of 24 cases).
   `oracle/run_oracle.sh <dir>` runs it and refuses while the Mac is near swap. **It has not been run**: the laptop was at 84 to 92 percent
   of its swap file with a load of 58 to 84 when this was written, and the other Apple machines are not allowed Metal work. Run it, check the
   dump in as `oracle/fixture_<device>.txt`, and adjust `CV_SUBDIV`, the cap rule or the flat orientation where it differs; each is one
   place in `curve_math.h`. If the GPU refuses curves, the dump names the error on the case line.
2. On the card with `NVK_CURVES=1`: the 14 blocks of the hybrid ray-query probe plus a curve scene; compare against the float model.
3. Cost: curves are tested with 8 pieces and about 2,800 instructions per candidate box. Measure before turning anything on by default.

## Limits

- Software path only. Motion curves and curve geometry in a multi-level instance hierarchy are not carried.
- Curve geometry must be built with `NVK_CURVES=1` set in the process; a structure built without it holds plain boxes.
- Control point count 2 with a basis other than linear, or 3 with Catmull-Rom, is refused at the plugin.

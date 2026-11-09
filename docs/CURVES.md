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

A segment is cut into 16 equal steps of its parameter (`CV_SUBDIV`; 8 until measured against Apple's own curves). Round curve: each step is the convex hull of its two end spheres (a cone frustum
tangent to both plus the spheres; joints are spheres, so no cracks on bends). Ends: sphere caps keep the end sphere; disk caps cut the
whole curve by the plane through its first point perpendicular to the first piece (and likewise at the last point) and close it with a flat disk of the end point's own radius; none cuts and leaves it open. Flat curve: a ribbon of
half-width equal to the radius, facing the ray. This is an approximation of the true swept surface, measured against an Apple GPU below.

## Measured against Apple (M6, macOS 27.2)

`oracle/fixtures/fixture_apple_m6.txt` is the oracle's dump: 24 cases x 576 rays, every case accepted by the GPU (no refusal).
`oracle/compare_oracle` against `curve_math.h` at the default 16 steps:

- **Match.** Linear curves match Apple to 1e-4 in distance for all three cap modes and flat: the hull, the caps, the end disks and the
  flat orientation are what Apple does. The B-spline, Bezier and Catmull-Rom evaluations are the same curves as Apple's (round with
  sphere caps converges to 0.001 in distance at 32 steps), so the basis weights are right.
- **Steps.** The error is the straight-piece approximation: Bezier-4 round sphere has a 95th percentile distance error of 0.046 at 8
  steps, 0.014 at 16, 0.003 at 32. 16 was chosen (two times the 8-step cost); 32 would be four times.
- **21 of 24 cases pass** (hit/miss on 98 percent of rays, 95th percentile of distance and parameter error within 0.03).
- **Three do not, and the cause is not known:** `bezier4_round_disk` (95th percentile distance 0.17, the end-disk rays), `catmull4_round_nocap`
  (95th percentile fine, a few rays with errors up to 0.11) and `catmull4_flat` (0.047, worst 0.4 with parameter errors up to 0.86). Bezier-4
  with disk and open ends gets worse as the steps get finer, which says Apple's end cut is not the plane perpendicular to the first piece
  (the chord of the curve was tried and is worse for B-spline). The Catmull-Rom flat case may be a self-overlapping sample curve where
  the nearest of several ribbon hits differs. These need a targeted oracle run (one curve, rays at the ends), not more guessing.
- **M4 mini: not run.** The workflow classifier on this machine refused the Metal run on the M4 (Raven shares it) although the lead allowed small
  bounded tests; I did not work around it. Only the M6 is measured; there is no comparison between generations.
- **The oracle needed two fixes before it measured anything**: a Metal `float3` member takes 16 bytes in a struct, so the 32-byte ray
  on the host did not line up (every ray missed); the ray is now two `float4`. `intersector<curve_data>` alone works for a primitive
  acceleration structure, without instancing.

## Checks

| check | what it proves |
|---|---|
| `nvk/curves/run_host_tests.sh` | the analytic hull equals an implicit-surface reference (a ray marched through the sampled swept sphere, entry bisected) for all six basis and count combinations with sphere caps, for disk caps, open ends equal disk caps except where the disk is hit, `curve_parameter` is the position along a straight curve, flat curves equal a plane construction, the segment box contains every sphere. Controls: wrong basis weights and wrong cap handling are caught; a Catmull-Rom segment can leave the hull of its own points, which is why the box shader converts it |
| `curve_nir_cpu.c` (needs a built NVK tree) | the NIR form, executed by NIR's own constant folder on 160 random curves and rays, equals the float form (0 differ); it compiles through NAK for sm120 (6627 instructions at 16 steps, 2805 at 8) |
| `nvk/hwrt/run_cpu_checks.sh`-style compile with `NVK_CURVES=1` | the ray-query shader with curves compiles: 56,352 B software, 116,592 B hybrid; with the switch off the code is the 11,328 B it was |
| `tools/test_app_profile.c` etc. | unchanged |
| `cargo test --lib intersections` | the `curve_data` family parses, result type is `{ i32, float, i32, i32, ptr addrspace(1), i32, i32, float }` for the instancing form, duplicate tokens are refused |

## Measurements still to take (M-series, then a card)

1. Done on the M6 (above); open: the three cases, the M4 and the M1. The oracle: `nvk/curves/oracle/` builds a curve
   acceleration structure for each of 24 cases (six basis and count pairs, round with three cap modes, flat) on the machine's own GPU,
   intersects 576 fixed rays, and dumps hit, distance, `curve_parameter` and primitive id as bits. `compare_oracle` judges the dump against
   `curve_math.h` (hit/miss on 98 percent of rays, distance within 0.02, parameter within 0.03) and is proven without a GPU
   (`oracle/selftest.sh`: a dump made from the same curves passes, one made from curves with 1.5 times the radii fails 24 of 24 cases).
   `oracle/run_oracle.sh <dir>` runs it and refuses while the Mac is near swap. It ran on the M6 only: the laptop was at 84 to 92 percent of its swap file when this was written and the M4 run was refused by the
   classifier. `oracle/run_oracle.sh <dir>` still refuses under pressure. If a GPU refuses curves, the dump names the error on the case line.
2. On the card with `NVK_CURVES=1`: the 14 blocks of the hybrid ray-query probe plus a curve scene; compare against the float model.
3. Cost: curves are tested with 16 pieces and about 6,600 instructions per candidate box (it was 2,800 at 8). Measure before turning anything on by default.

## Limits

- Software path only. Motion curves and curve geometry in a multi-level instance hierarchy are not carried.
- Curve geometry must be built with `NVK_CURVES=1` set in the process; a structure built without it holds plain boxes.
- Control point count 2 with a basis other than linear, or 3 with Catmull-Rom, is refused at the plugin.

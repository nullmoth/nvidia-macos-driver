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

- Match. Linear curves match Apple to 1e-4 in distance for all three cap modes and flat: the hull, the caps, the end disks and the
  flat orientation are what Apple does. The B-spline, Bezier and Catmull-Rom evaluations are the same curves as Apple's (round with
  sphere caps converges to 0.001 in distance at 32 steps), so the basis weights are right.
- Steps. The error is the straight-piece approximation: Bezier-4 round sphere has a 95th percentile distance error of 0.046 at 8
  steps, 0.014 at 16, 0.003 at 32. 16 was chosen (two times the 8-step cost); 32 would be four times.
- 22 of 24 cases pass (hit/miss on 98 percent of rays, 95th percentile of distance and parameter error within 0.03).
- Apple's round curves are the exact swept sphere. The targeted probe (`oracle/fixtures/fixture_apple_m6_probe.txt`: one curve per
  failing case, 192 rays aimed at each end and 192 at the middle) shows every Apple hit on a round curve lies within 0.004 of the exact
  swept sphere (distance to the curve minus the radius, minimum over the parameter), except the end-disk hits. So the radius law, the
  basis and the shape are right, and the remaining differences are my straight pieces and the end rules.
- The end cut was the bug in `bezier4_round_disk`. The plane through each end point applied to the whole curve, which cuts away real
  surface when a curve bends back on itself (the sample Bezier does). Apple does not. Each plane now applies only to the pieces in its own
  quarter of the curve. The plane is perpendicular to the curve's tangent at the end point (two close evaluations; `CV_END_PIECE_AXIS`
  selects the first piece's axis instead). Measured on the probe: the disk hits are at an axial distance of 0.000 from the end plane and
  inside the end radius, as modelled. Other variants tried and rejected because they pass fewer cases: a plane behind the end point by a
  fixed slack (0.02 to 0.15 times the end radius), the chord direction as the normal, more steps (32 to 128 do not help).
- The end sweep (`oracle/curve_sweep.m`, `oracle/sweep.h`, `oracle/sweep_analyze.c`; dumps `fixtures/sweep_apple_m6.txt` and `sweep_apple_m4.txt`):
  one Catmull-Rom curve, nocap and flat, 64 x 64 parallel rays in a 1.2-wide box around each end point, from three sides (along each of the
  two normals, and head-on), 49,152 rays per device. M4 and M6 differ on 39 of them (last bit of a distance or parameter).
  - Open ends: solved. The side views agree to within 2 rays of 4,096. Head-on, looking into the open end, Apple has no hit at all where
    the model had 287 and 732: a ray that passes the end plane inside the end radius is inside the solid, and every surface it meets next
    is one it leaves through, so it does not hit. `curve_math.h` now (a) keeps only the surface a ray enters through (front faces of the
    cone, the spheres and the disks) and (b) for no caps, ends all hits beyond a crossing of the end plane inside the end radius heading in.
    After that `catmull4_round_nocap` passes and the head-on maps agree to within 3 rays.
  - Flat ribbons: solved. The sweep was repeated for B-spline (4 and 3 control points), Bezier (4 and 3) and linear flat curves
    (`oracle/fixtures/sweep_apple_*.txt` now hold seven cases, 42 batches). The extension past the end points is the same for every
    basis, linear included, so it is not basis dependent. Its shape: a **half disc of the end point's radius that faces the ray** (its plane
    is perpendicular to the ray through the end point), only beyond the end point, `curve_parameter` 0 or 1. Fitted on the 43 cap hits in the
    24-case fixture: the facing plane matches 43 of 43 distances to 2e-3; the ribbon's own plane matches 4, the plane perpendicular to the
    tangent matches 2. The earlier disc failed on B-spline because it sat in the ribbon plane, and because the whole disc (not only the half
    beyond the end) hid ribbon hits in front of it. Flat curves only get it with sphere caps (the case the oracle measures; the other cap
    modes for flat were not measured and get no extension).
- Head-on residual: measured, not a rule. A ray along the end tangent of a straight flat curve (`oracle/headon.c`, the 64 x 64 head-on
  batches of the sweep) meets two caps, the near one at distance 2.000 and the far end's at 4.492. Apple returns the near cap for 267 of the
  415 pixels it hits and the far cap for the other 148, and the choice is a speckle with no radial or angular pattern (near-cap pixels reach
  rho 0.227 = the near radius, far-cap pixels sit at rho 0.16 to 0.20 inside it). Apple's near-disc radius is the end radius, so the extent
  is as modelled; which of two coplanar caps a ray exactly parallel to the ribbon plane reports is not the nearest, and it is the same on
  the M6 and the M4 (the two dumps agree on every one of those pixels), so it is deterministic but depends on the ray's rounding, not on
  geometry. The model reports the nearest. **Known limit:** for rays within rounding of parallel to an end tangent, the distance may differ
  from Apple's (the near-versus-far cap choice); the hit and its `curve_parameter` (0 or 1) are right. Adopted: nothing.
- Result: all 24 cases pass on both the M6 and the M4 fixtures, and on the probe. Flat cases agree to 0.0000 at the 95th percentile.
  Residual in the sweeps: the side views agree to within 40 of 4,096 rays per batch (at the rim of the half disc, one batch of Bezier-4 has
  40, B-spline-4 13); the head-on views (ray along the end tangent, where a ribbon has no orientation) differ by up to 15 percent of
  the rays that hit (t of the near disc against the far end's), not matched, and not a configuration the oracle's cases reach.
- M4 mini (macOS 26.5.1): the same. One short oracle process on the M4 (24 cases, then the probe), 120 s alarm, scratch directory, removed afterwards:
  all 24 cases accepted, no refusal, no hiccup. `oracle/fixtures/fixture_apple_m4.txt` and `fixture_apple_m4_probe.txt` are the dumps. Against the
  model it passes and fails the same 22 and 2 cases as the M6. M4 against M6, ray by ray: identical hit type, primitive id and parameter on all
  13,824 rays of the 24-case run except 3 rays whose distance differs in the last bit (`408b2bc1` against `408b2bc2`), and 11 of the 1,728
  probe rays that differ by 1 to 6 units in the last place of the distance or the parameter. No difference between the two Apple generations
  is visible at this resolution, so the Catmull-Rom end behaviour is Apple's, not the M6's.
- The oracle needed two fixes before it measured anything: a Metal `float3` member takes 16 bytes in a struct, so the 32-byte ray
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

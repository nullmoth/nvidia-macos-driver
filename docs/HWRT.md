# Ray queries on the RT cores

Status in 2.9: written, tested offline, **off by default**. Software traversal on the shader cores is what runs until the
measurements at the end of this document pass on a card.

What is proven and what is not: the lowering's algorithm (flags, candidates, boxes) is checked against a brute-force reference
on the CPU; the machine code of the query and of the lowering is checked on the CPU; the encode pass compiles and its index
mapping is checked by a C port. **The node and leaf contents the encoder writes (layout generation 1) have never been run on a
card**: the header rows R01..R13 are measured, the rest are our own rule. A structure the card cannot read faults the GPU
channel, so the first run follows the P0 ladder below and nothing else turns the switch on.

- `NVK_HWRT_QUERY=1` in the process environment (or in the user's block of `nvrm610.conf`) switches the hardware part on for
  that process: acceleration structures then carry a second part for the RT cores, and ray queries use it.
- `NVMTL_HWRT=1` does the same for the Cyberpunk 2077 profile only (`plugin/nvmtl_app_profile.h`). A user's own
  `NVK_HWRT_QUERY` always wins.
- Unset, nothing in the build or the shaders changes: the software structure and the software walk are what they were.

## What the RT cores answer

One instruction region (`nvk/hwrt/ttu_abi.h`) asks the hardware for **the nearest triangle hit in [tmin, tmax] for an instance
mask**, with every triangle treated as opaque, no culling and no flags. Everything else a ray query needs is built around
that one answer:

| ray-query feature | where it is done |
|---|---|
| nearest opaque triangle | the RT cores |
| non-opaque triangles (any-hit) | the lowering asks again from just past each hit, nearest first, and offers each to the shader |
| ray flags: opaque / no-opaque override, cull opaque, cull no-opaque, cull front, cull back, skip triangles, terminate on first hit | classified in the shader from the hit's facing bit and the instance and geometry flags |
| instance flags: facing-cull disable, flip facing, force opaque, force no-opaque | the same, read from the software instance leaf of the hit's instance |
| geometry flags (opaque) | a per-geometry table beside the structure (`nvk_hwrt_geometry`) |
| procedural boxes (intersection functions) | the existing software walk with SkipTriangles forced on, run after the triangles; skipped when the structure has no boxes |
| transforms, custom index, SBT offset, instance id, triangle vertex positions | the software leaves, found from the hit's instance index and primitive index |

Software traversal remains the fallback in three places: no hardware part in the structure, a layout generation the compiler
does not know, and a top level that contains an instance whose bottom level has no hardware part.

Known limit: two non-opaque triangles at a **bit-identical** distance. The next ask starts one step past the first hit, so
the second is not offered. For opaque triangles this cannot matter (the nearest wins either way). The model test pins the
behaviour so a change shows up.

Not applicable to Metal: `VK_KHR_ray_tracing_pipeline` (NVK does not implement it, and Metal's ray tracing is inline queries
with intersection functions, which the translator already specializes into the query loop (`translator/src/linked_functions.rs`)). Compaction queries and update builds
are not advertised by NVK for software structures either, so there is nothing to carry over; a copy of a finished structure
is supported and rewrites the structure's own address (`mode 1` of `encode_ttu.comp`).

## Where the code is

| file (under `nvk/hwrt/` unless noted) | what |
|---|---|
| `ttu-nak.patch` | NAK: one NIR intrinsic `ttu_query_nv` and one op `OpTtuQuery` encoded as the 15-instruction region |
| `ttu-rq.patch` | NVK: the hybrid ray-query lowering, `nvk_hwrt.h` consumers, the encode pass, build sizes, copy relocation |
| `nvk_hwrt.h`, `nvk_hwrt_layout.h` | appendix and layout constants, shared by C and GLSL; `build/build_nvk.sh` copies them into the Mesa tree |
| `hwrt_model.[ch]`, `hwrt_model_test.c` | the lowering's algorithm in C, against a brute-force reference of the ray-query rules |
| `hwrt_ref.c` | a C port of the encode mapping, a walk over its output, bit-exact against the model |
| `check_ttu_region.py` | judges the machine-code region against the field table below |
| `check_ttu_abi.py`, `test_ttu_abi.c`, `ttu_abi.h`, `data/` | request and result words against recorded device output |
| `hwrt_layout_check.py` | judges recorded structures against the measured rows of the layout table |
| `run_host_tests.sh` | all of the above that need no GPU and no NVK build |
| `run_cpu_checks.sh` | machine-code checks that need a built NVK tree (see below) |
| `probe/hwrt_probe.c`, `probe/hwrt_probe.comp` | the on-card test for the measurement procedure |

NVK patch order (appended to the chain in `docs/HOW-IT-WORKS.md` section 7, applied by `build/build_nvk.sh` for 2.9):
`nvk-macos`, `nvk-fixes-after-def16`, `nvk-620-kernel`, `nvk-recycle`, `nvk-slab`, `nvk-slabdef`, `nvk-hubfix`,
`nvk-perf-boost-burst`, `nvk-rated-tdp`, `nvk-gps-maxpower`, `nvk-metal-wsi`, `nvk-card-select`, **`hwrt/ttu-nak`**,
**`hwrt/ttu-rq`**, then the two shared headers are copied in and `nvk/dlss/patch_dlss_cache.py` runs.

## Structure of an acceleration structure with the hardware part

```
AS base +0                 software BVH (nvk_bvh.h); header word hw_offset points at the appendix, 0 = none
AS base +hw_offset         struct nvk_hwrt_appendix (32 B): magic, generation, sizes, table offset, has_procedural
appendix +256              the hardware structure (header 0x380 B, then 128-byte nodes, then leaves)
appendix +table_offset     struct nvk_hwrt_geometry[geometry_count]  (bottom level)
```

`bvh/encode_ttu.comp` runs after `encode.comp` from the same intermediate BVH. Mode 2 clears the appendix, mode 0 writes
it, mode 1 rewrites the structure's own address after a copy or deserialize.

## Layout table

Status is **MEASURED** (read from structures the vendor driver built on the card, recorded in our own dumps, and judged by
`hwrt_layout_check.py` row by row) or **PROVISIONAL** (our own rule in layout generation 1: the encoder writes something
self-consistent, the walk in `hwrt_ref.c` agrees with it, and nobody has run it on the card). The lowering refuses any
generation it does not know, so replacing a provisional row never leaves two layouts in the same build.

| id | field | status | evidence or procedure |
|---|---|---|---|
| R01 | word 0 of a bottom level is `0x00100003` | MEASURED | 8 recorded builds |
| R02 | word 1 is 7 for prefer-fast-trace | MEASURED | 8 recorded builds |
| R03 | word 2 is the total size | MEASURED | 8 recorded builds |
| R04 | u64 at 0x20 is the root offset, 0x380 in every build | MEASURED | 8 recorded builds |
| R05 | u64 at 0x40 is all ones | MEASURED | 8 recorded builds |
| R06 | u32 at 0x48 is 0x180 | MEASURED | 8 recorded builds |
| R07 | u32 at 0x58 is the geometry count | MEASURED | 8 recorded builds (always 1: one geometry passed); needs a two-geometry capture to confirm positively (P3) |
| R08 | u32 at 0x60 is the primitive count | MEASURED | 8 recorded builds (1, 2, 4, 8, 16, 32, 64) |
| R09 | u64 at 0xd0 is the structure's own address | MEASURED | 8 recorded builds |
| R10 | size law: 1..8 prims 2560 B, 16 prims 3584, 32 prims 3712, 64 prims 5632 | MEASURED | 8 recorded builds |
| R11 | u32 at 0x10: geometry section offset 0x400, then 0x800, 0x880, 0xa00 | MEASURED | 8 recorded builds |
| R12 | vertex 0 verbatim at the section start | MEASURED | the three v0 pokes |
| R13 | box maximum at section +0x90 | MEASURED | the v0x and v1x pokes |
| R20 | leaf packing of vertices 1 and 2, exponent at 0x47c | UNMEASURED | P2 |
| R21 | 8-byte child records at 0x380 | UNMEASURED | P2 |
| R22 | quantisation frame 0x180..0x37f | UNMEASURED | P2 |
| R23 | top level: header, instance record, transform, mask, flags | UNMEASURED | P3 |
| R24 | ray flags in the request other than the mask | UNMEASURED | P4 (not needed: the lowering classifies in the shader) |
| R25 | candidate reporting for non-opaque triangles and boxes | UNMEASURED | P4 (not needed: see above) |
| R26 | a query that starts at a bottom-level root | UNMEASURED | P4 (not needed) |
| R27 | tmin and tmax inclusive or exclusive | UNMEASURED | P5. The lowering never asks with tmin == a returned distance; the model assumes inclusive, the first ask uses the ray's own tmin |
| R28 | facing bit under a mirrored instance transform | UNMEASURED | P6 |
| R29 | whether the hardware reads the self address at 0xd0 | UNMEASURED | P7 |

Generation 1 writes, for the rows it has no measurement for: 2-child nodes of 128 bytes in the layout of
`nvk_hwrt_layout.h`, triangle leaves of 64 bytes holding the nine floats verbatim, and a top level of 128-byte instance
leaves. Row R04 is honoured (the root sits at 0x380 and is node 0), R03/R08/R11 are written, and all header words listed as
MEASURED are written with their measured values.

### 8-wide node option

`nvk/hwrt/hwrt_wide.c` is the 8-wide variant as a host reference: the binary tree is collapsed to up to eight children per 128-byte
node with 8-bit bounds in the node's own frame, rounded outward in the arithmetic the walk uses. The walk over it equals the model
bit for bit (0 of 9000 rays differ), it uses under two fifths of the binary tree's nodes, and boxes that do not contain their children
are caught. It models the shape rows R21/R22 suggest and is unmeasured like them. `NVK_HWRT_WIDE=1` (with `NVK_HWRT_QUERY=1`) selects it in the driver: `bvh/encode_wide.comp` runs after the 2-wide encode and overwrites the
node area in place (same bytes reserved, sparse slots, leaves and header untouched). Three kinds of pass: the depth word of every
intermediate box is cleared (the root gets 0), 64 propagation rounds give each box its parent's depth plus one (the traversal stack is 64
deep, so no tree here is deeper), and a box whose depth is a multiple of three becomes a wide node holding the descendants three levels
down (or the leaves met sooner), depth first. The passes are one text, `encode_wide_body.h`, compiled as GLSL for the driver and as C++ for
`hwrt_wide_shader.cpp`, which the host test runs on 40 random trees including 1 to 3 triangle ones: the node area it writes equals the
reference's byte for byte. The GPU's division may differ from the host's in the last bit, which can move a quantised bound by one step;
the outward widening keeps every box containing its child either way. P3 builds either shape on the card.

## Query region (what `check_ttu_region.py` judges)

Fifteen instructions, SM120, in this order. Offsets are in the 40..55 bit field of the low word; registers are aligned
64-bit pairs.

| # | opcode | fields |
|---|---|---|
| 0 | `0x3d0` open | bit 72 = 1 (fresh traversal) |
| 1 | `0x9d4` | length field = 12 |
| 2..6 | `0x3d1` store | offsets 0, 64, 80, 128, 512; source pair at 64..71, second pair (first + 2) at 32..39 |
| 7 | `0x3d3` go | |
| 8..13 | `0x3d2` load | offsets 896, 960, 976, 800, 784, 768; destination pair at 16..23, second pair (first + 2) at 64..71; type field 81..83 = 7; scoreboard i written (bits 110..112); bit 74 set on the last load only |
| 14 | `0x918` drain | waits on all six scoreboards |

## Measurement procedure (on a card)

Everything below uses our own driver and our own data. Nothing is read from, decompiled from, or compared with a vendor
binary. Run every GPU step through the bounded runner (own log directory, fault and WindowServer watch) and reboot after any
fault, as for every other GPU run. Only the process that sets `NVK_HWRT_QUERY` is affected; WindowServer never sees it.

**The probe.** `nvk/hwrt/probe/hwrt_probe.c` with `hwrt_probe.comp` builds a bottom and top level structure through the
driver under test, queries 256 rays under four flag sets (opaque, cull back, cull front, non-opaque with every candidate
confirmed) and compares each ray with a CPU reference; `--dump <dir>` writes `blas_<case>.bin` for `hwrt_layout_check.py`.
Cases `A`, `nNN` (NN triangles in a strip) and `big`. It compiles clean against the Vulkan headers and its shader compiles
with glslang; it has not been run on a card.
`glslangValidator -V --target-env vulkan1.2 hwrt_probe.comp -o hwrt_probe.spv`, then
`NVK_HWRT_QUERY=1 ./hwrt_probe <loader dylib> hwrt_probe.spv A`.

**P0 safety ladder.** One compute dispatch, one lane, `NVK_HWRT_QUERY=1`, a structure built by `encode_ttu.comp` for one
triangle (`hwrt_probe ... A`). Pass: no fault, and a miss or a hit is returned. A fault stops the session: record the first faulting address
against the structure (it names the field the hardware read) and fix the layout row, not the lowering.

**P1 fit the measured rows.** Capture bottom-level structures for the cases `A`, `B`, `n02` .. `n64` and the nine `poke_*`
cases (definitions in the header of `hwrt_layout_check.py`) with the vendor driver on the Linux boot, copy the dumps into
one directory and run `python3 nvk/hwrt/hwrt_layout_check.py <dir>`. Every row R01..R13 must PASS; the rows already pass on
the dumps recorded earlier.

**P2 pin the node and leaf rows (R20..R22).** For each unpinned field, change the generation-1 rule for that field only,
rebuild, and run the P4 ray set against our structure. The card is the oracle: a field is pinned when 32 of 32 rays agree
with the CPU reference for the whole scene set (one triangle, 8, 16, 64 on a strip, clustered, one triangle 100 times
larger). Bisect with the dumps from P1: the bytes our encoder writes and the bytes the vendor driver wrote for the same
scene differ in a short list of offsets; change them one at a time. Record each pinned field in the table above and in
`nvk_hwrt_layout.h`, and bump `NVK_HWRT_LAYOUT_PROVISIONAL` to the next generation.

**P3 top level (R23, R07).** Build a top level with 1, 2 and 16 instances, varied masks, a two-geometry bottom level, and
repeat P1/P2 for the instance record, the transform (3x4, row order), and the mask and flag bytes.

**P4 ray set.** The 32-ray corpus in `data/ray_cases_fresh.json` plus 1000 random rays per scene, queried through the
`hwrt_probe` (`NVK_HWRT_QUERY=1`) and through `NVK_HWRT_QUERY` unset on the same structure build. Pass: distances equal to
within 1 ulp, same instance, geometry and primitive, same facing; software and hardware agree on every ray that is not a
bit-identical tie. The ray flags and non-opaque cases of `hwrt_model_test.c` are the cases to add to the probe next.

**P5 tmin and tmax.** One triangle at a known distance; ask with tmin equal to, one step below and one step above that
distance, and with tmax likewise. Record the four answers in R27. If tmin is exclusive the lowering's one-step advance is
redundant but harmless; if tmax is exclusive nothing changes either, because the lowering asks with the committed distance.

**P6 mirrored transform.** The same triangle under +1 and -1 scale; the facing bit answers R28, and the lowering's XOR with
the instance flip flag is then either right as written or needs the transform determinant.

**P7 self address.** Copy a bottom level to a new address with and without the mode-1 rewrite; answers R29. If the hardware
never reads the word the relocation pass can go.

**P8 Cyberpunk.** Only after P0..P7: `NVMTL_HWRT=1`, one run of the benchmark, compare to the recorded software-traversal
number for the same settings with RT on. Flip the default only on that evidence.

## Checks that run without a card

```
bash nvk/hwrt/run_host_tests.sh                         # model, encode mapping, ABI, region judge, layout judge
bash nvk/hwrt/run_cpu_checks.sh <patched Mesa tree> build   # machine code of the query region and of the lowering (needs a built NVK tree)
cc -Iplugin -o /tmp/tap tools/test_app_profile.c && /tmp/tap   # the profile opt-in
```

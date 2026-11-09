# NAK changes on the speed-267 lane (applied to the speed267 Mesa build tree, in this order)

1. `patch_smemcap.py` (qmd.rs) - `NAK_SMEM_TARGET_CAP_KB`: cap the shared-memory carve-out target. Off by default.
   Measured neutral in GB7 (11-01); kept only as a diagnostic.
2. `patch_u2fmagic.py`, then `patch_u2fmagic2.py` (nak_nir.c) - u2f32 of an 8/16-bit value as
   fadd(as_float(0x4B000000 | x), -2^23), one PRMT per byte from unpack_32_4x8. Exact (M1-matched hashes);
   `NVMTL_NAK_NO_U2FMAGIC=1` turns it off. Measured neutral in GB7.
3. `nak_api_prepass_pick.diff` (nak/api.rs; first the NVMTL_NAK_NO_PREPASS gate, then `patch_prepass_pick.py`) -
   compute shaders are built with and without the prepass scheduler and keep the one with fewer static cycles at no
   occupancy or spill cost. Offline census over 96 GB7 kernels: 67 better, 0 worse, sum -8.9%.
   `NVMTL_NAK_PREPASS_PICK=0` = upstream behaviour; `NVMTL_NAK_NO_PREPASS=1` = never run the prepass.
4. `patch_pinwhole.py`, `patch_pinwhole2.py`, `patch_pinwhole3.py` (nak_nir_lower_non_uniform_ldcx.c; result =
   `nak_pinwhole.diff`) - `pin_uniform_addrs_section` also pins a 64-bit address that is uniform as a whole (a
   kernel-argument pointer read at a fixed offset), not only iadd(per-thread, uniform). Unpinned, legalize copied the
   base into a fresh GPR pair before every load inside the bounds check (raw_process: 84 of 664 instructions).
   Split uses weigh 2 in the 8-slot ranking, a whole-only base needs >= `NVMTL_NAK_PIN_WHOLE_MIN` uses (default 2),
   and no unpin is emitted when the section ends at the last block (it cost the `if (!in_bounds) exit` shortcut).
   Offline census (`census.sh`, `census_env.sh`), translated GB7 kernels: every one better or equal - raw_process
   -5.0% static cycles, quantize -3.2%, particle -1.9%. All 183: -1.1% instructions, -0.3% cycles; regressions only in
   native-Vulkan proxies (blend +3%, initialize_dye +6%). `NVMTL_NAK_NO_PIN_WHOLE=1` = previous behaviour
   (census: 0 of 183 kernels differ from the old compiler).
5. `patch_predload.py`, then `patch_predload_fwd.py`, `patch_predload_max.py`, `patch_predload_def32.py` (nak_nir.c;
   result = `nak_predload.diff`) - `nak_cg_predload`: a small divergent if whose one side is ALU + plain global loads
   (<= `NVMTL_NAK_PREDLOAD_MAX` instructions, default 32; <= 2 loads; not volatile/coherent) runs unconditionally, each load
   becoming `load_global_nv` predicated on the branch condition (hardware: no access, returns 0 when false); peephole
   then turns the phis into selects. `nak_cg_punroll` now also takes counters that step by a constant 2..4096
   (grid-stride loops) and runs again after predload. GB7 particle's inner loop: 79 -> ~50 issue cycles per trip,
   1 -> 4 loads in flight (`blkcyc.py`). Census: 35 kernels change, none fail; sort_keypoints -43%,
   pressure_solver_prep -43%, hysteresis -38%, Fluid's curl/vorticity/pressure/gradient -13..17%, gemm -12%,
   trace_bvh -9% static cycles. `NVMTL_NAK_NO_PREDLOAD=1` = previous behaviour (census: 0 kernels differ).
   Correctness + speed: `session5.sh` (probe forces hash old == new == new-with-switch-off, then GB7 three ways).
6. `patch_weakstore.py` (nak/from_nir.rs) - plain global stores are weak (STG.E), coherent/volatile stay strong.gpu;
   `NVMTL_NAK_STRONG_STORES=1` = previous behaviour. Found 11-01: GPU writes to Shared (system-RAM) buffers cost ~70-100 ns
   per threadgroup - a near-empty kernel 317 us (Shared) vs 5.6 us (Private, or Shared with NVMTL_SHARED_VRAM=1), GB7's own
   particle kernel 512 us vs 66 us (M1 Pro 188 us), outputs bit-identical. Timing of this patch pending (session23.sh).
7. `patch_small_arrays_def16.py` (nvmtl_raw_policy.h, nak_nir.c) - per-thread arrays of up to 16 elements go to
   registers (if-else trees) by default instead of local memory. The plugin's own fp32 conv kernels kept their tile
   accumulators and row tables in local memory (45 loads, 39 stores in the 64x32 kernel): the Metal front end leaves
   those fixed-count loops rolled, so the arrays reach NAK with a loop index. In GB7 the conv gate then took our kernel
   over MPS on 39 layers instead of 62, and one layer ran 852,768 vs 134,816 timestamp units. Limit 32 also takes
   trace_bvh's stack (36 -> 16 warps, Path Tracer -27%), so the default is 16. Census, 237 kernels: with
   `NVMTL_NO_SMALL_ARRAYS=1` 0 differ from the previous compiler; by default 0 differ from it plus NAK_B83_ARRAYS=16;
   11 change, every one loses local-memory traffic, none loses occupancy.

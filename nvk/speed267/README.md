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

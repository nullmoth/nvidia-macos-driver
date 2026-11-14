# AIR 2.7 - 2.9 / MSL 3.1 - 4.0 translator corpus

Small shaders under `src/` compiled at `-std=metal3.1`, `metal3.2` and `metal4.0`, for the default target (macOS 27,
AIR 2.9) and for `-target air64-apple-macos15.7` (AIR 2.7, or 2.8 at `metal4.0`), then run through the translator.

1. On a Mac with the Metal toolchain: `bash compile_corpus.sh <corpus>` writes `.air`, `.ll`, `.metallib` and
   `compile.tsv` (AIR version and triple per case). Tensor shaders compile only at `metal4.0`; `k_barriers_atomics`
   needs `metal3.2` or later (fences). Those combinations are recorded as `compile-fail`, which is expected.
2. On the build Mac: `bash build_host.sh <dir holding translator/> <build dir>` builds the translator for that machine,
   then `bash run_corpus.sh <corpus> <build dir>/release/libnvmtl_translate.dylib` calls `nvmtl_translate` on each `.ll`
   (the plugin's entry point), runs `spirv-val` on the output and writes `result.tsv`.

The run fails on any invalid SPIR-V, any crash, and any refusal of a shader not listed in `expected_refusals.txt`.
Mesh stages enter through `nvmtl_lower_mesh` and are reported as SKIPPED here.

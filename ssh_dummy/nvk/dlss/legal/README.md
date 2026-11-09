# Clean sources for the DLSS kernel-cache linker

The kernel-cache linker in `nvk/dlss/patch_dlss_cache.py` (edit 8) places a cubin in GPU memory, applies its relocations
and binds its constant banks. Every fact it uses comes from one of these sources, none of which is a disassembly of
NVIDIA's shipped files.

| fact | source |
|---|---|
| relocation kinds 56, 57, 58, 75 and the instruction bits each patches | LLVM `ELFRelocs/CUDA.def` (LLVM PR 133076): `R_CUDA_ABS32_LO_32`, `R_CUDA_ABS32_HI_32`, `R_CUDA_ABS47_34`, `R_CUDA_ABS55_16_34` |
| which kinds occur, and the unpatched call / MOV-immediate form | the cubins nvJitLink writes for our own `divk.ptx`, listed with `cubin_relocs.py` (an ELF reader) |
| `__assertfail` is an undefined symbol reached through a data relocation | the ELF symbol and relocation tables of the shipped cubins (ELF format, no disassembly) |
| EXIT and store words of the `__assertfail` stand-in | NAK's encoder, `sm70_encode.rs` (MIT), read at patch time by `nak_stub.py` |
| constant bank 0 layout (`0x358`, `0x37c`, `0x380`+) | `.nv.constant0` sizes of cubins compiled from our own PTX, and tinygrad's launcher (MIT) |

## Commands

On a Mac with NVIDIA's nvJitLink (the user's own download), through the DLSS ELF host (`ptxjit`, built from `dlss/`):

    ptxjit libnvJitLink.so.12 <sm> <outdir> divk.ptx          # sm: 75 89 90 100 120
    python3 -I cubin_relocs.py <outdir>/divk.sm*.cubin       # relocations, .nv.constant0 sizes

Tests:

    python3 -I stub_selftest.py <patched Mesa tree>           # stand-in words
    bash ../link_selftest.sh <patched Mesa tree> <cubin>...   # links each cubin, decodes every relocation independently

`link_selftest` on the 259 + 259 cubins of an RTX 50 and an RTX 40 kernel cache: 259/259 link, 2112 and 4452 code
relocations decoded with 0 mismatches. The previous placement of kind 75 (address bits 8..55 at bits 32..79) disagreed
with the definition for 119 of the sm_120 cubins (targets whose address bits 8 or 9 are set), and kind 58 was refused, so
54 of the 259 sm_89 cubins did not link.

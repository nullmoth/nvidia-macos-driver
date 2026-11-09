# DLSS 4 / 4.5 support files

The driver ships no NVIDIA code for DLSS. The installer downloads NVIDIA's own files from NVIDIA's public sources on
the user's Mac, checks their hashes, and builds what NVK needs from them locally.

- `elfhost.c`, `lnxlibc.c`: load an x86-64 Linux shared object (NVIDIA's DLSS runtimes, NGX core and nvJitLink) into a
  macOS process, written from the ELF and System V x86-64 ABI specifications. `lnxlibc.c` translates the glibc entry
  points whose ABI differs from macOS (structure layouts, flag values, thread objects, TLS).
- `dlss_cache.c`: at install time, compiles each DLSS fatbin's PTX for the user's GPU with NVIDIA's nvJitLink and
  `assertfail_trap.ptx`, into `/Library/NullMoth/dlss/kernels/<BLAKE3 of the PTX object>.sm<N>.cubin`. NVK reads an
  entry only when the fatbin has no code it can load itself (RTX 50 cards: DLSS ships sm_89 SASS and PTX).
- `assertfail_trap.ptx`: the device-side `__assertfail` the DLSS 4 transformer kernels import; it traps.
- `nvk_decide.c`: offline check of NVK's module decision (`nvk/dlss/patch_dlss_cache.py`) over captured fatbins.
- `blake3/`: BLAKE3 1.8.2 reference C (CC0-1.0 or Apache-2.0), the same hash NVK uses for the cache name.

Build (x86-64 macOS):

    clang -O2 -DBLAKE3_NO_SSE2 -DBLAKE3_NO_SSE41 -DBLAKE3_NO_AVX2 -DBLAKE3_NO_AVX512 -DBLAKE3_USE_NEON=0 \
      -o dlss_cache dlss_cache.c elfhost.c lnxlibc.c blake3/blake3.c blake3/blake3_dispatch.c blake3/blake3_portable.c \
      -liconv -lcompression

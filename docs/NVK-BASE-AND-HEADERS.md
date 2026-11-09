# NVK base revision and NVRM header provenance

## Mesa base

`nvk/MESA_BASE` is Mesa commit 90a959eb, the mesa-26.3.0-rc3 tag (2.9). 2.8 and 2.8.1 were built on 17ca6174 (2026-10-05, 26.3.0-devel); 32 nouveau commits separate the two and the whole patch chain applies to both without fuzz. The NAK features that matter for compute speed
are in that base, not behind a future update: the uniform base address on global loads (`uniform_addr`, `set_ureg_addr`,
`nak_nir_opt_uniform_address`, `nir_opt_offsets` for SM73 and newer), LDSM, and the cooperative-matrix lowering.
A rebase onto a newer Mesa therefore buys only the upstream commits made since. There are 62 of them touching
`src/nouveau` (about 930 added and 650 removed lines), and the whole 2.9 patch chain applies to the newer tree without a
reject. Moving the base would still re-base about 17,600 lines of patches that WindowServer composites through, with no
card available to validate, so the base stays. The upstream fixes that matter for current cards are carried instead as
`nvk/nvk-upstream-nak.patch` (eleven commits, MIT, listed in the patch header), applied only when
`NVK_EXTRA_PATCHES=nvk-upstream-nak` is set for `build/build_nvk.sh`:

- uniform `Ldcg` scoreboard on SM120 (RTX 50): a missing dependency, a correctness fix;
- `ready_cycle` in the prepass scheduler, SM120 IMMA/HMMA latencies, `opt_fall_through` always applied: scheduling;
- `IDP.4A` never encoded with an immediate, `OpBSSy` source type, input-attachment `texture_offset`.

Checked offline: the patch applies on top of the 2.9 chain with no reject; the build with it has the same 21 warnings and
exactly the same 2274 exports as the build without it. Not checkable offline: whether the changed scheduling is faster or
the same on a card. Default off until a Geekbench and compositor run on the target card shows no regression.

## NVRM headers

NVK is compiled against the 610.57.04 open kernel module headers with the NVOS change in `nvk/ogkm-nvos-620.patch`; the
kernel module is built from 620.70.06. `tools/nvk_header_abi.py` compares, between module versions, every struct and
constant in the 135 headers the NVRM backend preprocesses to, restricted to the 198 names the backend sources use.

| compared with 620.70.06 | structs differing | structs missing | defines differing | defines missing |
|---|---|---|---|---|
| 610.57.04 + NVOS change (shipped) | 0 | 0 | 0 | 0 |
| 615.78.08 (the headers 2.8 was built with) | 0 | 0 | 0 | 0 |

Control: the same tool without the restriction (`--all`) reports 13 structs differing, 7 missing, 20 constants differing and
126 missing between the shipped headers and 620.70.06, so the comparison does see changes when they exist; none of them
is a name the backend uses. The header list is produced by preprocessing the backend sources with `-MM` and keeping the
files that resolve into the module tree.

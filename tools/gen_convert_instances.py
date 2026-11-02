#!/usr/bin/env python3
"""Emit the SPIR-V for the GPU-side Metal -> Vulkan instance-descriptor conversion.

The CPU version is nvmtl_vk_convert_instances(); this must agree with it value for value.
Generated rather than hand-written so the 12 transform copies and the index arithmetic come
from the same expression the C does.

Push constants (48 B):
    0  u64 src     Metal descriptors, already offset, 4-byte aligned
    8  u64 dst     VkAccelerationStructureInstanceKHR array
    16 u64 uid     userID side table (u32 per instance)
    24 u64 astab   per-structure table: 4 words each - addr lo, addr hi, iftAdd, pad
    32 u32 stride  Metal descriptor stride in bytes, a multiple of 4
    36 u32 count
    40 u32 mtl_type   0 Default, 1 UserID, 3 Indirect
    44 u32 n_as
"""
import pathlib
import subprocess
import sys

L = []
def e(s=""):
    L.append(s)

e("OpCapability Shader")
e("OpCapability Int64")
e("OpCapability PhysicalStorageBufferAddresses")
e('OpExtension "SPV_KHR_physical_storage_buffer"')
e("OpMemoryModel PhysicalStorageBuffer64 GLSL450")
e('OpEntryPoint GLCompute %main "main" %push %gid')
e("OpExecutionMode %main LocalSize 64 1 1")
e("OpDecorate %arr_u32 ArrayStride 4")
e("OpMemberDecorate %U32 0 Offset 0")
e("OpDecorate %U32 Block")
for idx, off in enumerate((0, 8, 16, 24, 32, 36, 40, 44)):
    e("OpMemberDecorate %%Push %d Offset %d" % (idx, off))
e("OpDecorate %Push Block")
e("OpDecorate %gid BuiltIn GlobalInvocationId")

e("%void = OpTypeVoid")
e("%fnty = OpTypeFunction %void")
e("%bool = OpTypeBool")
e("%u32 = OpTypeInt 32 0")
e("%u64 = OpTypeInt 64 0")
e("%v3u = OpTypeVector %u32 3")
e("%arr_u32 = OpTypeRuntimeArray %u32")
e("%U32 = OpTypeStruct %arr_u32")
e("%pU32 = OpTypePointer PhysicalStorageBuffer %U32")
e("%pu32 = OpTypePointer PhysicalStorageBuffer %u32")
e("%Push = OpTypeStruct %u64 %u64 %u64 %u64 %u32 %u32 %u32 %u32")
e("%pcPush = OpTypePointer PushConstant %Push")
e("%pcU64 = OpTypePointer PushConstant %u64")
e("%pcU32 = OpTypePointer PushConstant %u32")
e("%inV3u = OpTypePointer Input %v3u")

CONSTS = sorted(set(range(0, 18)) | {12, 13, 14, 15, 16, 17, 24, 0xF, 0xFF, 0xFFFFFF, 0x7FFFFF, 0x800000})
for c in CONSTS:
    e("%%c%d = OpConstant %%u32 %d" % (c, c))

e("%push = OpVariable %pcPush PushConstant")
e("%gid = OpVariable %inV3u Input")

e("%main = OpFunction %void None %fnty")
e("%entry = OpLabel")
for name, idx, ty in (("src", 0, "u64"), ("dst", 1, "u64"), ("uid", 2, "u64"), ("astab", 3, "u64"),
                      ("stride", 4, "u32"), ("count", 5, "u32"), ("mtype", 6, "u32"), ("nas", 7, "u32")):
    e("%%p_%s = OpAccessChain %%pc%s %%push %%c%d" % (name, ty.upper(), idx))
    e("%%%s = OpLoad %%%s %%p_%s" % (name, ty, name))
e("%srcp = OpConvertUToPtr %pU32 %src")
e("%dstp = OpConvertUToPtr %pU32 %dst")
e("%uidp = OpConvertUToPtr %pU32 %uid")
e("%astabp = OpConvertUToPtr %pU32 %astab")
e("%gidv = OpLoad %v3u %gid")
e("%i = OpCompositeExtract %u32 %gidv 0")
e("%inrange = OpULessThan %bool %i %count")
e("OpSelectionMerge %done None")
e("OpBranchConditional %inrange %work %done")
e("%work = OpLabel")

# Word indices. The stride is asserted a multiple of 4 on the host.
e("%stridew = OpShiftRightLogical %u32 %stride %c2")
e("%sb = OpIMul %u32 %i %stridew")
e("%db = OpIMul %u32 %i %c16")


def rd(buf, idx, name):
    e("%%pr_%s = OpAccessChain %%pu32 %%%s %%c0 %%%s" % (name, buf, idx))
    e("%%%s = OpLoad %%u32 %%pr_%s Aligned 4" % (name, name))


def wr(buf, idx, val, name):
    e("%%pw_%s = OpAccessChain %%pu32 %%%s %%c0 %%%s" % (name, buf, idx))
    e("OpStore %%pw_%s %%%s Aligned 4" % (name, val))


def off(base, k, name):
    """A word index base + k, as its own id."""
    if k == 0:
        e("%%%s = OpCopyObject %%u32 %%%s" % (name, base))
    else:
        e("%%%s = OpIAdd %%u32 %%%s %%c%d" % (name, base, k))
    return name


# The transform is three rows of four in both layouts, so this is a straight copy. Copied as raw
# words - a bit pattern needs no conversion, and keeping them integers avoids a float type.
for r in range(3):
    for col in range(4):
        si, di = r * 4 + col, r * 4 + col
        off("sb", si, "si_%d_%d" % (r, col))
        off("db", di, "di_%d_%d" % (r, col))
        rd("srcp", "si_%d_%d" % (r, col), "t_%d_%d" % (r, col))
        wr("dstp", "di_%d_%d" % (r, col), "t_%d_%d" % (r, col), "t_%d_%d" % (r, col))

off("sb", 12, "i_options")
off("sb", 13, "i_mask")
off("sb", 14, "i_ift")
off("sb", 15, "i_w3")
rd("srcp", "i_options", "options")
rd("srcp", "i_mask", "mask")
rd("srcp", "i_ift", "ift")
rd("srcp", "i_w3", "w3")

# Words 16 and 17 exist only for the wider descriptor types. Rather than branch, the index is
# steered back onto word 15 when the type does not have them, so the read is always in bounds
# and its value is simply unused.
e("%is_t1 = OpIEqual %bool %mtype %c1")
e("%is_t3 = OpIEqual %bool %mtype %c3")
off("sb", 16, "i16_real")
off("sb", 17, "i17_real")
e("%i_w4 = OpSelect %u32 %is_t1 %i16_real %i_w3")
e("%i_reflo = OpSelect %u32 %is_t3 %i16_real %i_w3")
e("%i_refhi = OpSelect %u32 %is_t3 %i17_real %i_w3")
rd("srcp", "i_w4", "w4")
rd("srcp", "i_reflo", "reflo_raw")
rd("srcp", "i_refhi", "refhi_raw")

# user = type 1 ? w4 : type 3 ? w3 : 0
e("%user_t3 = OpSelect %u32 %is_t3 %w3 %c0")
e("%user = OpSelect %u32 %is_t1 %w4 %user_t3")

# The per-structure table is only consulted for the indexed types; Indirect names its structure
# inline, and its word 3 is a userID, not an index.
e("%idx_ok = OpULessThan %bool %w3 %nas")
e("%use_tab = OpLogicalNot %bool %is_t3")
e("%tab_ok = OpLogicalAnd %bool %use_tab %idx_ok")
e("%tab_base = OpIMul %u32 %w3 %c4")
e("%tab_safe = OpSelect %u32 %tab_ok %tab_base %c0")
off("tab_safe", 0, "i_tab_lo")
off("tab_safe", 1, "i_tab_hi")
off("tab_safe", 2, "i_tab_ift")
rd("astabp", "i_tab_lo", "tab_lo")
rd("astabp", "i_tab_hi", "tab_hi")
rd("astabp", "i_tab_ift", "tab_ift_raw")
e("%have_tab = OpLogicalAnd %bool %use_tab %idx_ok")
e("%add = OpSelect %u32 %have_tab %tab_ift_raw %c0")

# slot = ift + (add & 0x7FFFFF), computed wide so the overflow test matches the C exactly.
e("%add_low = OpBitwiseAnd %u32 %add %c8388607")
e("%ift64 = OpUConvert %u64 %ift")
e("%addlow64 = OpUConvert %u64 %add_low")
e("%slot64 = OpIAdd %u64 %ift64 %addlow64")
e("%lim64 = OpUConvert %u64 %c8388607")
e("%slot_big = OpUGreaterThan %bool %slot64 %lim64")
e("%slot_sum = OpUConvert %u32 %slot64")
e("%ift_clamped = OpBitwiseAnd %u32 %ift %c8388607")
e("%slot = OpSelect %u32 %slot_big %ift_clamped %slot_sum")
e("%add_hi = OpBitwiseAnd %u32 %add %c8388608")
e("%sbt = OpBitwiseOr %u32 %slot %add_hi")

# Word 12: instanceCustomIndex:24 | mask:8.  Word 13: sbtRecordOffset:24 | flags:8.
e("%cust = OpBitwiseAnd %u32 %user %c16777215")
e("%mask8 = OpBitwiseAnd %u32 %mask %c255")
e("%mask_sh = OpShiftLeftLogical %u32 %mask8 %c24")
e("%word12 = OpBitwiseOr %u32 %cust %mask_sh")
e("%sbt24 = OpBitwiseAnd %u32 %sbt %c16777215")
e("%flags4 = OpBitwiseAnd %u32 %options %c15")
e("%flags_sh = OpShiftLeftLogical %u32 %flags4 %c24")
e("%word13 = OpBitwiseOr %u32 %sbt24 %flags_sh")

# The structure reference: inline for Indirect, out of the table otherwise, 0 when unnamed.
e("%ref_lo_tab = OpSelect %u32 %have_tab %tab_lo %c0")
e("%ref_hi_tab = OpSelect %u32 %have_tab %tab_hi %c0")
e("%ref_lo = OpSelect %u32 %is_t3 %reflo_raw %ref_lo_tab")
e("%ref_hi = OpSelect %u32 %is_t3 %refhi_raw %ref_hi_tab")

off("db", 12, "o12")
off("db", 13, "o13")
off("db", 14, "o14")
off("db", 15, "o15")
wr("dstp", "o12", "word12", "o12")
wr("dstp", "o13", "word13", "o13")
wr("dstp", "o14", "ref_lo", "o14")
wr("dstp", "o15", "ref_hi", "o15")
wr("uidp", "i", "user", "uid")

e("OpBranch %done")
e("%done = OpLabel")
e("OpReturn")
e("OpFunctionEnd")

here = pathlib.Path(__file__).resolve().parent
asm = here / "nvmtl_convert_instances.spvasm"
asm.write_text("\n".join(L) + "\n")
spv = here / "nvmtl_convert_instances.spv"
subprocess.run(["spirv-as", "--target-env", "spv1.5", str(asm), "-o", str(spv)], check=True)
subprocess.run(["spirv-val", "--target-env", "vulkan1.2", str(spv)], check=True)
words = list(__import__("struct").unpack("<%dI" % (spv.stat().st_size // 4), spv.read_bytes()))
print("assembled + VALID: %d words (%d B)" % (len(words), spv.stat().st_size))

hdr = ["/*", " * NullMoth NVIDIA driver for macOS", " * Copyright (c) 2026 NullMoth Systems.",
       " * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0", " */", "",
       "static const uint32_t nvmtl_convert_instances_spv[] = {"]
for n in range(0, len(words), 8):
    hdr.append("    " + " ".join("0x%08xu," % w for w in words[n:n + 8]))
hdr.append("};")
out = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else here.parent / "plugin/nvmtl_convert_instances_spv.h"
out.write_text("\n".join(hdr) + "\n")
print("wrote", out)

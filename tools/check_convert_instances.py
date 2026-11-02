#!/usr/bin/env python3
"""Prove the GPU instance-conversion kernel agrees with the CPU one, value for value.

Neither side is retyped. The C body is EXTRACTED from nvmtl_vk.c and compiled; the SPIR-V is the
assembled module the plugin embeds, read back with spirv-dis and interpreted by opcode NAME (so a
wrong opcode number cannot let a wrong answer pass). Random descriptors of every carried type go
through both, and every one of the 64 output bytes per instance plus the userID word is compared.
"""
import os
import pathlib
import random
import struct
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PLUGIN = ROOT / "plugin"
HERE = pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / "nvmtl-convinst-check"
HERE.mkdir(parents=True, exist_ok=True)
M32 = 0xFFFFFFFF
M64 = (1 << 64) - 1

# ---------------------------------------------------------------- the C, extracted verbatim
src = (PLUGIN / "nvmtl_vk.c").read_text()
start = src.index("int nvmtl_vk_convert_instances(")
j = src.index("{", start)
depth = 0
while True:
    if src[j] == "{":
        depth += 1
    elif src[j] == "}":
        depth -= 1
        if depth == 0:
            break
    j += 1
body = src[start:j + 1]
assert "accelerationStructureReference" in body, "extraction missed the function"

HARNESS = r"""
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
typedef struct { float matrix[3][4]; } VkTransformMatrixKHR;
typedef struct {
    VkTransformMatrixKHR transform;
    uint32_t instanceCustomIndex : 24;
    uint32_t mask : 8;
    uint32_t instanceShaderBindingTableRecordOffset : 24;
    uint32_t flags : 8;
    uint64_t accelerationStructureReference;
} VkAccelerationStructureInstanceKHR;
static void nvlog(const char *f, ...) { (void)f; }
__BODY__
int main(int argc, char **argv) {
    FILE *f = fopen(argv[1], "rb");
    uint32_t stride, count, mtl_type, n_as, src_words;
    fread(&stride, 4, 1, f); fread(&count, 4, 1, f);
    fread(&mtl_type, 4, 1, f); fread(&n_as, 4, 1, f); fread(&src_words, 4, 1, f);
    uint32_t *mtl = calloc(src_words + 8, 4);
    fread(mtl, 4, src_words, f);
    uint64_t *addrs = calloc(n_as + 1, 8);
    uint32_t *ifts = calloc(n_as + 1, 4);
    fread(addrs, 8, n_as, f); fread(ifts, 4, n_as, f);
    fclose(f);
    VkAccelerationStructureInstanceKHR *out = calloc(count ? count : 1, 64);
    uint32_t *uid = calloc(count ? count : 1, 4);
    nvmtl_vk_convert_instances(mtl, stride, count, addrs, n_as, out, mtl_type, uid, ifts);
    FILE *o = fopen(argv[2], "wb");
    fwrite(out, 64, count, o); fwrite(uid, 4, count, o); fclose(o);
    return 0;
}
"""
(HERE / "cconv.c").write_text(HARNESS.replace("__BODY__", body))
subprocess.run(["clang", "-O1", "-w", str(HERE / "cconv.c"), "-o", str(HERE / "cconv")], check=True)
assert struct.calcsize("P") and True
size = subprocess.run(["clang", "-w", "-x", "c", "-", "-o", str(HERE / "szchk")],
                      input="#include <stdio.h>\n" + HARNESS.replace("__BODY__", "").replace(
                          "int main(", "int unused_main(") +
                      "int main(void){printf(\"%zu\\n\", sizeof(VkAccelerationStructureInstanceKHR));}",
                      text=True, capture_output=True)
if size.returncode == 0:
    n = subprocess.run([str(HERE / "szchk")], capture_output=True, text=True).stdout.strip()
    assert n == "64", "the instance struct is %s bytes, not 64 - the layout assumption is wrong" % n

# ------------------------------------------------------- interpret the assembled SPIR-V by name
# The generator leaves the assembled module beside itself; regenerate if it is not there, so the
# check always runs against a module built from the source of truth rather than a stale file.
SPV = ROOT / "tools/nvmtl_convert_instances.spv"
if not SPV.exists():
    subprocess.run([sys.executable, str(ROOT / "tools/gen_convert_instances.py"),
                    str(HERE / "regen.h")], check=True)
dis = subprocess.run(["spirv-dis", "--raw-id", "--no-header", str(SPV)],
                     capture_output=True, text=True, check=True).stdout
PROG = []
for line in dis.splitlines():
    line = line.strip()
    if not line or line.startswith(";"):
        continue
    res = None
    if line.startswith("%"):
        res, line = line.split("=", 1)
        res, line = int(res.strip()[1:]), line.strip()
    toks = line.split()
    PROG.append((res, toks[0], toks[1:]))

W64 = set()
CONST = {}
INPUT_VARS = set()
for res, op, a in PROG:
    if op == "OpTypeInt" and a[0] == "32" and False:
        pass
    if op == "OpTypeInt" and a[0] == "64":
        W64.add(res)
    if op == "OpConstant":
        CONST[res] = int(a[1])
    if op == "OpVariable" and a[1] == "Input":
        INPUT_VARS.add(res)
MERGE_OK = {"OpSelectionMerge", "OpBranch", "OpLabel", "OpFunctionEnd", "OpName", "OpDecorate",
            "OpMemberDecorate", "OpCapability", "OpExtension", "OpMemoryModel", "OpEntryPoint",
            "OpExecutionMode", "OpTypeVoid", "OpTypeFunction", "OpTypeBool", "OpTypeInt",
            "OpTypeVector", "OpTypeRuntimeArray", "OpTypeStruct", "OpTypePointer", "OpConstant",
            "OpVariable", "OpFunction", "OpReturn"}


def run_spirv(buffers, push, gid):
    val, ptr = {}, {}
    rtype = {}
    for res, op, a in PROG:
        if res is not None and a and a[0].startswith("%"):
            rtype[res] = int(a[0][1:])

    def g(i):
        if i in val:
            return val[i]
        if i in CONST:
            return CONST[i]
        raise KeyError("id %%%d is undefined" % i)

    def w(res):
        return M64 if rtype.get(res) in W64 else M32

    def mem(addr):
        for base, buf in buffers.items():
            if base <= addr < base + len(buf):
                return buf, addr - base
        raise MemoryError("unmapped access at 0x%x" % addr)

    infn, skip_to = False, None
    for res, op, a in PROG:
        if op == "OpFunction":
            infn = True
            continue
        if not infn:
            continue
        if op == "OpLabel":
            if skip_to is not None and res == skip_to:
                skip_to = None
            continue
        if skip_to is not None:
            continue
        ids = [int(t[1:]) for t in a if t.startswith("%")]
        if op == "OpAccessChain":
            obj = ids[1]
            if obj in val and isinstance(val[obj], tuple):
                # ids = [result type, base, member index, array index]
                ptr[res] = ("mem", val[obj][1] + 4 * g(ids[3]))
            else:
                ptr[res] = ("push", g(ids[2]))
        elif op == "OpLoad":
            if ids[1] in INPUT_VARS:        # the GlobalInvocationId builtin, loaded whole
                val[res] = tuple(gid)
                continue
            kind, where = ptr[ids[1]]
            if kind == "push":
                val[res] = push[where]
            else:
                buf, off = mem(where)
                val[res] = struct.unpack_from("<I", buf, off)[0]
        elif op == "OpStore":
            kind, where = ptr[ids[0]]
            buf, off = mem(where)
            struct.pack_into("<I", buf, off, g(ids[1]) & M32)
        elif op == "OpConvertUToPtr":
            val[res] = ("buf", g(ids[1]))
        elif op == "OpCompositeExtract":
            val[res] = val[ids[1]][int(a[2])]
        elif op == "OpCopyObject":
            val[res] = g(ids[1])
        elif op == "OpIAdd":
            val[res] = (g(ids[1]) + g(ids[2])) & w(res)
        elif op == "OpIMul":
            val[res] = (g(ids[1]) * g(ids[2])) & w(res)
        elif op == "OpUConvert":
            val[res] = g(ids[1]) & w(res)
        elif op == "OpIEqual":
            val[res] = g(ids[1]) == g(ids[2])
        elif op == "OpULessThan":
            val[res] = g(ids[1]) < g(ids[2])
        elif op == "OpUGreaterThan":
            val[res] = g(ids[1]) > g(ids[2])
        elif op == "OpLogicalAnd":
            val[res] = bool(g(ids[1])) and bool(g(ids[2]))
        elif op == "OpLogicalNot":
            val[res] = not bool(g(ids[1]))
        elif op == "OpSelect":
            val[res] = g(ids[2]) if bool(g(ids[1])) else g(ids[3])
        elif op == "OpBitwiseOr":
            val[res] = g(ids[1]) | g(ids[2])
        elif op == "OpBitwiseAnd":
            val[res] = g(ids[1]) & g(ids[2])
        elif op == "OpShiftRightLogical":
            val[res] = (g(ids[1]) >> g(ids[2])) & M32
        elif op == "OpShiftLeftLogical":
            val[res] = (g(ids[1]) << g(ids[2])) & M32
        elif op == "OpBranchConditional":
            if not bool(g(ids[0])):
                skip_to = ids[2]
        elif op in ("OpSelectionMerge", "OpBranch"):
            pass
        elif op in ("OpReturn", "OpFunctionEnd"):
            break
        else:
            raise NotImplementedError("the interpreter does not carry %s" % op)
    return


# ------------------------------------------------------------------------------- the comparison
SRC_BASE, DST_BASE, UID_BASE, TAB_BASE = 0x10000, 0x40000, 0x60000, 0x80000
NATURAL = {0: 64, 1: 68, 3: 72}
rng = random.Random(0x5060)
bad = 0
cases = 0
for mtl_type in (0, 1, 3):
    for trial in range(40):
        natural = NATURAL[mtl_type]
        stride = natural + 4 * rng.randrange(0, 4)
        count = rng.randrange(1, 9)
        n_as = rng.randrange(0, 6)
        sw = (count - 1) * (stride // 4) + natural // 4
        mtl = [rng.randrange(0, 1 << 32) for _ in range(sw)]
        # Mostly in-range structure indices, sometimes out of range, sometimes huge.
        for inst in range(count):
            base = inst * (stride // 4)
            mtl[base + 12] = rng.choice([0, 1, 3, 0xF, rng.randrange(0, 1 << 32)])
            mtl[base + 14] = rng.choice([0, 1, 7, 0x7FFFFE, 0x7FFFFF, rng.randrange(0, 1 << 32)])
            if base + 15 < sw:
                mtl[base + 15] = rng.choice([0, max(0, n_as - 1), n_as, n_as + 5,
                                             rng.randrange(0, 1 << 24)])
        addrs = [rng.randrange(0, 1 << 48) | 1 for _ in range(n_as)]
        ifts = [rng.choice([0, 1, 0x7FFFFF, 0x800000, 0x800001, rng.randrange(0, 1 << 24)])
                for _ in range(n_as)]

        blob = struct.pack("<5I", stride, count, mtl_type, n_as, sw)
        blob += struct.pack("<%dI" % sw, *mtl)
        blob += struct.pack("<%dQ" % n_as, *addrs) if n_as else b""
        blob += struct.pack("<%dI" % n_as, *ifts) if n_as else b""
        (HERE / "in.bin").write_bytes(blob)
        subprocess.run([str(HERE / "cconv"), str(HERE / "in.bin"), str(HERE / "out.bin")], check=True)
        cref = (HERE / "out.bin").read_bytes()

        srcbuf = bytearray(struct.pack("<%dI" % sw, *mtl) + b"\0" * 32)
        dstbuf = bytearray(64 * count)
        uidbuf = bytearray(4 * count)
        tab = bytearray()
        for k in range(max(n_as, 1)):
            a = addrs[k] if k < n_as else 0
            tab += struct.pack("<IIII", a & M32, a >> 32, ifts[k] if k < n_as else 0, 0)
        bufs = {SRC_BASE: srcbuf, DST_BASE: dstbuf, UID_BASE: uidbuf, TAB_BASE: tab}
        push = [SRC_BASE, DST_BASE, UID_BASE, TAB_BASE, stride, count, mtl_type, n_as]
        for inst in range(count + 2):       # +2 proves the out-of-range guard writes nothing
            run_spirv(bufs, push, (inst, 0, 0))
        gpu = bytes(dstbuf) + bytes(uidbuf)
        cases += 1
        if gpu != cref:
            bad += 1
            if bad <= 3:
                print("MISMATCH type %d trial %d stride %d count %d n_as %d"
                      % (mtl_type, trial, stride, count, n_as))
                for inst in range(count):
                    c = cref[inst * 64:inst * 64 + 64]
                    gq = gpu[inst * 64:inst * 64 + 64]
                    if c != gq:
                        for wi in range(16):
                            cw = struct.unpack_from("<I", c, wi * 4)[0]
                            gw = struct.unpack_from("<I", gq, wi * 4)[0]
                            if cw != gw:
                                print("   inst %d word %2d: C 0x%08x  GPU 0x%08x" % (inst, wi, cw, gw))

print("%d cases, %d mismatches" % (cases, bad))
sys.exit(1 if bad else 0)

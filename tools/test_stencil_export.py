#!/usr/bin/env python3
"""Test the production stencil-emulation detector without creating a GPU device."""
import ctypes
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / "plugin/nvmtl_vk.c").read_text()
start = source.index("static const uint32_t *nvmtl_spirv_type(")
end = source.index("uint32_t nvmtl_spirv_fetch_mask(", start)
detector = "#include <stdint.h>\n#include <stddef.h>\n#define NVMTL_STENCIL_EMU_OFFSET 160u\n" + source[start:end]


def instruction(opcode, *operands):
    return [((len(operands) + 1) << 16) | opcode, *operands]


def module(storage=9, offset=160, scalar_op=21, width=32, block=True, variable=True):
    # uint -> struct { uint } -> pointer -> variable. An unrelated uniform
    # struct also has Offset 160, so matching just that decoration must fail.
    result = [0x07230203, 0x00010400, 0, 20, 0]
    result += instruction(72, 8, 0, 35, 160)
    result += instruction(71, 8, 2)
    if block:
        result += instruction(71, 2, 2)
    result += instruction(72, 2, 0, 35, offset)
    result += instruction(scalar_op, 1, width, *([0] if scalar_op == 21 else []))
    result += instruction(30, 2, 1)
    result += instruction(30, 8, 1)
    result += instruction(32, 3, storage, 2)
    if variable:
        result += instruction(59, 3, 4, storage)
    return result


with tempfile.TemporaryDirectory(prefix="nvmtl-stencil-test-") as directory:
    directory = Path(directory)
    (directory / "detector.c").write_text(detector)
    subprocess.run(["xcrun", "clang", "-dynamiclib", "-O1", "-Wall", "-Wextra", "-Werror",
                    str(directory / "detector.c"), "-o", str(directory / "detector.dylib")], check=True)
    library = ctypes.CDLL(str(directory / "detector.dylib"))
    detect = library.nvmtl_spirv_stencil_emu
    detect.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    detect.restype = ctypes.c_uint32
    cases = [
        ("stencil push constant", module(), 1),
        ("uniform member at 160", module(storage=2), 0),
        ("storage buffer member at 160", module(storage=12), 0),
        ("other push constant plus uniform member at 160", module(offset=0), 0),
        ("float push constant", module(scalar_op=22), 0),
        ("64-bit push constant", module(width=64), 0),
        ("missing Block decoration", module(block=False), 0),
        ("uninstantiated pointer type", module(variable=False), 0),
        ("truncated instruction", module()[:-1], 0),
        ("zero-length instruction", module() + [0], 0),
    ]
    for name, words, expected in cases:
        buffer = (ctypes.c_uint32 * len(words))(*words)
        actual = detect(buffer, ctypes.sizeof(buffer))
        assert actual == expected, (name, actual, expected)
        print(f"PASS {name}")
    buffer = (ctypes.c_uint32 * len(module()))(*module())
    assert detect(buffer, ctypes.sizeof(buffer) - 1) == 0
    assert detect(buffer, 16) == 0
    print("PASS unaligned length and incomplete header")

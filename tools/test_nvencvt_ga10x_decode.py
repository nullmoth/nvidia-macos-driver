#!/usr/bin/env python3
"""Offline checks for the shared C7B0/C9B0 H.264 NVDEC interface."""

from __future__ import annotations

import re
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
NVENC = ROOT / "vendor/nvenc"


def check_source_contract() -> None:
    dec_c = (NVENC / "src/dec.c").read_text()
    dec_h = (NVENC / "src/dec.h").read_text()
    iface_h = (NVENC / "src/dec_iface.h").read_text()
    policy_h = (NVENC / "src/ga10x_policy.h").read_text()
    vt_dec = (NVENC / "vt/nvencvt_dec.c").read_text()

    surface_match = re.search(r"enum\s*\{\s*DEC_SURFACES\s*=\s*(\d+)\s*\}", dec_h)
    count_match = re.search(r"#define\s+NVDEC_IFACE_H264_SURFACE_COUNT\s+(\d+)", iface_h)
    if not surface_match or not count_match:
        raise AssertionError("could not read H.264 surface counts")
    surface_count = int(surface_match.group(1))
    if surface_count != int(count_match.group(1)):
        raise AssertionError("H.264 surface count differs from the asserted method range")

    for plane in ("LUMA", "CHROMA"):
        indices = {
            int(index)
            for index in re.findall(
                rf"NVDEC_IFACE_ASSERT_OFFSET\(SET_PICTURE_{plane}_OFFSET(\d+)\);", iface_h
            )
        }
        if indices != set(range(surface_count)):
            raise AssertionError(f"{plane.lower()} method assertions do not cover 0..{surface_count - 1}")

    create_pattern = re.compile(
        r"chan_create\s*\(\s*rm\s*,\s*&d->chan\s*,\s*NV2080_ENGINE_TYPE_NVDEC0\s*,"
        r"\s*rm_class\s*\(\s*rm\s*,\s*RM_CLS_NVDEC\s*\)",
        re.S,
    )
    set_object_pattern = re.compile(
        r"chan_mthd\s*\(\s*&d->chan\s*,\s*0\s*,\s*NVC56F_SET_OBJECT\s*,\s*1\s*,"
        r"\s*rm_class\s*\(\s*rm\s*,\s*RM_CLS_NVDEC\s*\)",
        re.S,
    )
    if not create_pattern.search(dec_c):
        raise AssertionError("H.264 channel creation must pass the runtime RM NVDEC class")
    if not set_object_pattern.search(dec_c):
        raise AssertionError("H.264 SET_OBJECT must use the runtime RM NVDEC class")
    for plane in ("LUMA", "CHROMA"):
        if not re.search(
            rf"NVC9B0_SET_PICTURE_{plane}_OFFSET0\s*\+\s*4\s*\*\s*\(NvU32\)i",
            dec_c,
        ):
            raise AssertionError(f"H.264 {plane.lower()} surface methods must advance by four bytes")

    if '#include "dec_iface.h"' not in dec_c or '#include "clc9b0.h"' in dec_c:
        raise AssertionError("H.264 decoder must include the shared C7B0/C9B0 interface contract")

    if "nvenc_ga10x_h264_decode_enabled" not in policy_h:
        raise AssertionError("the C7B0 H.264 registration policy helper is missing")
    if not re.search(
        r"const\s+bool\s+h264\s*=\s*!ampere\s*\|\|\s*"
        r"nvenc_ga10x_h264_decode_enabled\s*\(",
        vt_dec,
    ):
        raise AssertionError("GA10x H.264 registration must use the C7B0-only policy helper")
    if not re.search(r"\bh264\s*\?\s*reg\s*\(\s*kCMVideoCodecType_H264", vt_dec):
        raise AssertionError("H.264 registration must honor the H.264-specific policy")
    if not re.search(r"if\s*\(\s*h264\s*&&\s*!getenv\(\s*\"NVENCVT_NO_HEV1\"\s*\)\s*\)", vt_dec):
        raise AssertionError("avc3 registration must follow the H.264-specific policy")
    if not re.search(r"if\s*\(\s*hevcMpeg\s*&&\s*!getenv\(\s*\"NVENCVT_NO_HEVCDEC\"", vt_dec):
        raise AssertionError("HEVC registration must remain behind the separate HEVC/MPEG policy")
    if not re.search(r"if\s*\(\s*hevcMpeg\s*&&\s*!getenv\(\s*\"NVENCVT_NO_MPEG2DEC\"", vt_dec):
        raise AssertionError("MPEG registration must remain behind the separate HEVC/MPEG policy")


def check_compiled_contract() -> None:
    clang = shutil.which("clang")
    if not clang:
        raise RuntimeError("clang is required for this offline header check")

    include_dirs = [
        NVENC / "src",
        NVENC / "vendor/open-gpu-kernel-modules/src/common/sdk/nvidia/inc",
        NVENC / "vendor/open-gpu-kernel-modules/src/nvidia/arch/nvalloc/unix/include",
        NVENC / "vendor/open-gpu-doc/classes/video",
    ]
    source = """\
#include "dec.h"
#include "dec_iface.h"
#include "ga10x_policy.h"
_Static_assert(DEC_SURFACES == NVDEC_IFACE_H264_SURFACE_COUNT, "surface count mismatch");
int main(void)
{
    if (!nvenc_ga10x_h264_decode_enabled(NULL, 0xc7b0)) return 1;
    if (!nvenc_ga10x_h264_decode_enabled("1", 0xc7b0)) return 2;
    if (nvenc_ga10x_h264_decode_enabled("0", 0xc7b0)) return 3;
    if (nvenc_ga10x_h264_decode_enabled("invalid", 0xc7b0)) return 4;
    if (nvenc_ga10x_h264_decode_enabled("1", 0xc9b0)) return 5;
    if (nvenc_ga10x_h264_decode_enabled(NULL, 0)) return 6;
    return DEC_SURFACES != 17;
}
"""
    with tempfile.TemporaryDirectory(prefix="nvencvt-ga10x-decode-") as tmp:
        c_path = Path(tmp) / "interface_check.c"
        binary = Path(tmp) / "interface_check"
        c_path.write_text(source)
        command = [clang, "-std=c11", "-Wall", "-Wextra", "-Werror"]
        for directory in include_dirs:
            command.extend(["-I", str(directory)])
        command.extend([str(c_path), "-o", str(binary)])
        subprocess.run(command, check=True, cwd=NVENC)
        subprocess.run([str(binary)], check=True, cwd=NVENC)


def main() -> int:
    check_source_contract()
    check_compiled_contract()
    print("PASS: C7B0/C9B0 H.264 interface, C7B0 policy, and decoder registration gates")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

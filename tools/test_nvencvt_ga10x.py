#!/usr/bin/env python3
"""Offline regression checks for the local GA10x NVENC policy and interface."""
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
NVENC = ROOT / "vendor/nvenc"

POLICY_PROBE = r'''#include <stdio.h>
#include "enc_iface.h"
#include "ga10x_policy.h"

#define CHECK(condition, name) do { \
    if (!(condition)) { fprintf(stderr, "failed: %s\n", name); return __LINE__; } \
} while (0)

int main(void)
{
    CHECK(nvenc_ga10x_encode_enabled(NULL), "missing setting enables the already opt-in loaded plugin");
    CHECK(nvenc_ga10x_encode_enabled("1"), "explicit 1 enables GA10x encoding");
    CHECK(!nvenc_ga10x_encode_enabled("0"), "explicit 0 disables GA10x encoding");
    CHECK(!nvenc_ga10x_encode_enabled(""), "empty setting fails closed");
    CHECK(!nvenc_ga10x_encode_enabled("true"), "invalid setting fails closed");
    CHECK(!nvenc_ga10x_encode_enabled("2"), "unknown numeric setting fails closed");

    CHECK(nvenc_ga10x_engine(NULL) == 0, "default engine is NVENC0");
    CHECK(nvenc_ga10x_engine("0") == 0, "explicit engine 0 is accepted");
    CHECK(nvenc_ga10x_engine("1") < 0, "engine 1 is rejected on GA10x");
    CHECK(nvenc_ga10x_engine("2") < 0, "out-of-range engine is rejected");
    CHECK(nvenc_ga10x_engine("") < 0, "empty engine setting is rejected");
    CHECK(nvenc_ga10x_engine("invalid") < 0, "invalid engine setting is rejected");

    CHECK(nvenc_pic_setup_magic(NVC7B7_VIDEO_ENCODER, 0) == 0xc7b70006u,
          "C7B7 picture setup uses NVENC 7.3 magic");
    CHECK(nvenc_pic_setup_magic(NVC9B7_VIDEO_ENCODER, 0) == 0xc9b70006u,
          "Ada C9B7 magic is unchanged");
    CHECK(nvenc_pic_setup_magic(0xb4b7u, 0) == 0xb4b70006u,
          "TU117 B4B7 magic is unchanged");

    // C7B7 defines the 32-bit semaphore payload selector at bit 23. Its
    // 32-bit enum value, structure-size value, and release operation are all
    // zero, so the complete C7B7 release word is zero.
    CHECK(nvenc_semaphore32_word(NVC7B7_VIDEO_ENCODER) == 0u,
          "C7B7 32-bit semaphore release word matches the class definition");
    CHECK(nvenc_semaphore32_word(NVC7B7_VIDEO_ENCODER) ==
          nvenc_semaphore32_word(NVC9B7_VIDEO_ENCODER),
          "existing C9B7-derived 32-bit semaphore word remains unchanged");
    return 0;
}
'''


def c_function_body(source: str, signature: str) -> str:
    """Return a C function body, ignoring braces inside comments and literals."""
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"function signature not found: {signature}")
    opening = None
    depth = 0
    state = "code"
    index = start
    while index < len(source):
        char = source[index]
        next_char = source[index + 1] if index + 1 < len(source) else ""
        if state == "code":
            if char == "/" and next_char == "*":
                state = "block_comment"
                index += 2
                continue
            if char == "/" and next_char == "/":
                state = "line_comment"
                index += 2
                continue
            if char == '"':
                state = "string"
            elif char == "'":
                state = "char"
            elif char == "{":
                if opening is None:
                    opening = index
                depth += 1
            elif char == "}" and opening is not None:
                depth -= 1
                if depth == 0:
                    return source[opening + 1:index]
        elif state == "block_comment" and char == "*" and next_char == "/":
            state = "code"
            index += 2
            continue
        elif state == "line_comment" and char == "\n":
            state = "code"
        elif state in {"string", "char"}:
            if char == "\\":
                index += 2
                continue
            if state == "string" and char == '"' or state == "char" and char == "'":
                state = "code"
        index += 1
    raise AssertionError(f"unterminated function body: {signature}")


class NVEncVTGa10x(unittest.TestCase):
    def test_compiled_policy_and_class_helpers(self):
        clang = shutil.which("clang")
        self.assertIsNotNone(clang, "clang is required to compile the offline policy probe")
        with tempfile.TemporaryDirectory(prefix="nvencvt-ga10x-test-") as temp_name:
            temp = Path(temp_name)
            source = temp / "policy_probe.c"
            binary = temp / "policy_probe"
            source.write_text(POLICY_PROBE)
            includes = (
                NVENC / "src",
                NVENC / "vendor/open-gpu-doc/classes/video",
                NVENC / "vendor/open-gpu-kernel-modules/src/common/sdk/nvidia/inc",
            )
            command = [clang, "-std=gnu11", "-Wall", "-Werror"]
            for include in includes:
                command.extend(("-I", str(include)))
            command.extend((str(source), "-o", str(binary)))
            built = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stdout + built.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_ga10x_registration_requires_the_actual_rm_nvenc_class(self):
        source = (NVENC / "vt/nvencvt.c").read_text()
        kind = c_function_body(source, "static int gpu_nvenc_kind(void)")
        self.assertRegex(kind, r"rm_class\s*\(\s*&rm\s*,\s*RM_CLS_NVENC\s*\)")
        self.assertRegex(kind, r"cls\s*==\s*0xc7b7\s*\?\s*GPU_AMPERE")

        register = c_function_body(source, "NVEncVT_Register(int rating)")
        self.assertRegex(
            register,
            r"gpu\s*!=\s*GPU_AMPERE\s*\|\|\s*\(\s*"
            r"nvenc_ga10x_encode_enabled\s*\(\s*getenv\s*\(\s*"
            r"\"NVENCVT_AMPERE_ENC\"\s*\)\s*\)\s*&&\s*"
            r"gpu_nvenc_kind\s*\(\s*\)\s*==\s*GPU_AMPERE\s*\)",
            "GA10x registration must require opt-in and a real C7B7 RM class",
        )
        self.assertRegex(register, r"encoders\s*\?\s*register_one\s*\(\s*kCMVideoCodecType_H264")
        self.assertRegex(register, r"!encoders\s*\|\|\s*getenv\s*\(\s*\"NVENCVT_NO_HEVC\"")

    def test_ga10x_engine_path_uses_nvenc0_and_rejects_other_values(self):
        source = (NVENC / "vt/nvencvt.c").read_text()
        open_encoder = c_function_body(source, "static OSStatus open_encoder(State *s)")
        self.assertRegex(open_encoder, r"nvenc_ga10x_engine\s*\(\s*engine_setting\s*\)")
        self.assertRegex(open_encoder, r"gNvencvtGpu\s*==\s*GPU_AMPERE\s*&&\s*ga10x_engine\s*<\s*0")
        self.assertRegex(open_encoder, r"return\s+kVTParameterErr\s*;")
        self.assertRegex(open_encoder, r"c\.engine\s*=\s*gNvencvtGpu\s*==\s*GPU_AMPERE\s*\?\s*ga10x_engine")

    def test_av1_encoder_remains_ada_only_at_registration_and_submission(self):
        vt_source = (NVENC / "vt/nvencvt.c").read_text()
        register = c_function_body(vt_source, "NVEncVT_Register(int rating)")
        self.assertRegex(
            register,
            r"!encoders\s*\|\|\s*getenv\s*\(\s*\"NVENCVT_NO_AV1\"\s*\)\s*"
            r"\|\|\s*gpu\s*!=\s*GPU_ADA\s*\?\s*kVTCouldNotFindVideoEncoderErr\s*:",
        )

        enc_source = (NVENC / "src/enc.c").read_text()
        enc_open = c_function_body(enc_source, "int enc_open(Enc *e, const EncConfig *cfg)")
        self.assertRegex(
            enc_open,
            r"av1\s*&&\s*rm_class\s*\(\s*rm\s*,\s*RM_CLS_NVENC\s*\)\s*!=\s*0xc9b7",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)

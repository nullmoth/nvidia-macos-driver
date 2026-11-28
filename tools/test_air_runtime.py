#!/usr/bin/env python3
"""LLVM symbol-isolation regressions. Host fixtures include a failing unrestricted-library control.

AIR_RUNTIME=/path/to/air-runtime/libLLVM.dylib also checks the real reader/optimizer with Apple's LLVM loaded first
and last. AIR_RUNTIME_BASELINE can name the old runtime to prove that the real failing order is reproduced.
"""
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SYSTEM_LLVM = "/System/Library/PrivateFrameworks/GPUCompiler.framework/Versions/32023/Libraries/libLLVM.dylib"
RUNTIME = os.environ.get("AIR_RUNTIME")
BASELINE = os.environ.get("AIR_RUNTIME_BASELINE")
spec = importlib.util.spec_from_file_location("air_check", ROOT / "build/check_air_runtime.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


def optimize(library):
    """Each library owns its context, module, options, strings and disposal functions."""
    p = ctypes.c_void_p

    def api(name, result, *args):
        f = getattr(library, name)
        f.restype = result
        f.argtypes = list(args)
        return f

    context = api("LLVMContextCreate", p)()
    ir = b"define i32 @probe() { %v = add i32 40, 2\n ret i32 %v }\n"
    buffer = api("LLVMCreateMemoryBufferWithMemoryRangeCopy", p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p)(ir, len(ir), b"isolation")
    module, message = p(), p()
    assert api("LLVMParseIRInContext", ctypes.c_int, p, p, ctypes.POINTER(p), ctypes.POINTER(p))(context, buffer, ctypes.byref(module), ctypes.byref(message)) == 0
    options = api("LLVMCreatePassBuilderOptions", p)()
    error = api("LLVMRunPasses", p, p, ctypes.c_char_p, p, p)(module, b"always-inline,default<O1>", None, options)
    assert not error
    text = api("LLVMPrintModuleToString", p, p)(module)
    assert b"ret i32 42" in ctypes.string_at(text)
    api("LLVMDisposeMessage", None, p)(text)
    api("LLVMDisposePassBuilderOptions", None, p)(options)
    api("LLVMDisposeModule", None, p)(module)
    api("LLVMContextDispose", None, p)(context)


def child(mode, paths):
    libraries = []
    for path in paths:
        flags = os.RTLD_NOW | os.RTLD_LOCAL
        if mode == "fixture" and Path(path).name == "system.dylib":
            flags = os.RTLD_NOW | os.RTLD_GLOBAL
        libraries.append(ctypes.CDLL(path, mode=flags))
    if mode == "fixture":
        print(json.dumps([lib.LLVMProbe() for lib in libraries]), flush=True)
    else:
        for lib in libraries:
            optimize(lib)
        print("Both LLVM implementations optimized independently", flush=True)


def run_child(mode, *paths):
    env = {k: v for k, v in os.environ.items() if not k.startswith("DYLD_")}
    return subprocess.run([sys.executable, __file__, "--child", mode, *map(str, paths)], env=env, capture_output=True, text=True, timeout=30)


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("xcrun"), "needs macOS and Xcode tools")
class ExportBoundary(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="llvm-isolation-")
        cls.folder = Path(cls.temp.name)
        source = cls.folder / "probe.cpp"
        source.write_text('''namespace llvm {
__attribute__((weak,noinline,visibility("default"))) int shared_analysis() { return VALUE; }
}
extern "C" int LLVMProbe() { return llvm::shared_analysis(); }
''' + "".join(f'extern "C" void {name}() {{}}\n' for name in sorted(checker.required_api_names())))
        exports = cls.folder / "exports"
        exports.write_text("".join("_" + s + "\n" for s in (ROOT / "build/air_runtime.exports").read_text().splitlines()))
        for name, value, private in [("system", 10, False), ("unrestricted", 20, False), ("private", 20, True),
                                     ("incomplete", 20, True)]:
            command = ["xcrun", "clang++", "-dynamiclib", "-O0", "-DVALUE=" + str(value), str(source), "-o", str(cls.folder / (name + ".dylib"))]
            if name == "incomplete":
                command.append("-DLLVMGetNextGlobal=MissingEntry")
            if private:
                command.append("-Wl,-exported_symbols_list," + str(exports))
            subprocess.run(command, capture_output=True, text=True, check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def probe(self, first, second):
        r = run_child("fixture", self.folder / (first + ".dylib"), self.folder / (second + ".dylib"))
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def test_unrestricted_library_calls_the_other_implementation(self):
        self.assertEqual(self.probe("system", "unrestricted"), [10, 10])

    def test_system_first_keeps_both_implementations_independent(self):
        self.assertEqual(self.probe("system", "private"), [10, 20])

    def test_private_first_keeps_both_implementations_independent(self):
        self.assertEqual(self.probe("private", "system"), [20, 10])

    def test_gate_rejects_internal_exports(self):
        with self.assertRaisesRegex(ValueError, "internal symbols"):
            checker.check(self.folder / "unrestricted.dylib")

    def test_gate_accepts_private_library(self):
        checker.check(self.folder / "private.dylib")

    def test_gate_rejects_missing_walk_entry(self):
        with self.assertRaisesRegex(ValueError, "lacks required LLVM C entry points: _LLVMGetNextGlobal"):
            checker.check(self.folder / "incomplete.dylib")


@unittest.skipUnless(RUNTIME, "set AIR_RUNTIME for the built runtime")
class RealRuntime(unittest.TestCase):
    def test_exports_and_fixups(self):
        checker.check(RUNTIME)

    def test_both_load_orders(self):
        for paths in [(SYSTEM_LLVM, RUNTIME), (RUNTIME, SYSTEM_LLVM)]:
            with self.subTest(paths=paths):
                r = run_child("llvm", *paths)
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    @unittest.skipUnless(BASELINE, "set AIR_RUNTIME_BASELINE for the real negative control")
    def test_old_runtime_fails_with_system_first(self):
        r = run_child("llvm", SYSTEM_LLVM, BASELINE)
        self.assertLess(r.returncode, 0, r.stdout + r.stderr)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--child"]:
        child(sys.argv[2], sys.argv[3:])
    else:
        unittest.main()

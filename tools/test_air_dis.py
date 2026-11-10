#!/usr/bin/env python3
"""nvmtl-air-dis replaces the Metal toolchain's air-opt that packages 2.0 to 2.8.1 carried in /Library/GPUBundles/nvmtl.

That air-opt was Apple's own program (signed com.apple.air-opt, team 59GAB85EFG) and not ours to redistribute. The plugin
now runs nvmtl-air-dis from its own bundle: plugin/air_dis.c on the same upstream LLVM as the in-process reader. The
package gate refuses an Apple-signed program and a library that loads from outside the system and our folders.

usage: test_air_dis.py [air-runtime dir]   (arms that decode need an air-runtime/libLLVM.dylib and an AIR blob; the
       blob arms read AIR_DIS_BLOB, a bitcode wrapper carved from a system .metallib)"""
import os, shutil, subprocess, sys, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PKG = ROOT / "package"
RUNTIME = Path(sys.argv.pop(1)) if len(sys.argv) > 1 and not sys.argv[1].startswith("-") else None
LOAD = (ROOT / "plugin/NVMTLLibraryLoad.m").read_text()
MAC = shutil.which("clang") and shutil.which("codesign")


def sh(*a, **k):
    return subprocess.run([str(x) for x in a], capture_output=True, text=True, **k)


class PluginSource(unittest.TestCase):
    def test_the_plugin_no_longer_runs_anything_from_the_nvmtl_folder(self):
        self.assertNotIn('"/Library/GPUBundles/nvmtl/air-opt"', LOAD)
        self.assertNotIn("NVMTL_AIR_OPT", LOAD)

    def test_the_decoder_is_found_beside_the_plugin(self):
        self.assertIn('stringByAppendingPathComponent:@"nvmtl-air-dis"', LOAD)
        self.assertIn("dladdr((const void *)&nvmtl_air_dis_path", LOAD)

    def test_the_decoder_gets_an_empty_environment(self):
        i = LOAD.index("static BOOL nvmtl_run_air_opt")
        body = LOAD[i:LOAD.index("\n}\n", i)]
        self.assertIn("posix_spawn(&child, tool, &actions, NULL, argv, noenv)", body)
        self.assertNotIn("environ)", body)


@unittest.skipUnless(MAC, "needs a Mac with clang")
class Tool(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.t = Path(tempfile.mkdtemp())
        r = sh("bash", ROOT / "build/build_air_dis.sh", cls.t / "MacOS")
        assert r.returncode == 0, r.stdout + r.stderr
        cls.tool = cls.t / "MacOS/nvmtl-air-dis"
        if RUNTIME:
            os.symlink(RUNTIME, cls.t / "MacOS/air-runtime")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.t)

    def run_tool(self, *a):
        prefix = ["arch", "-x86_64"] if os.uname().machine == "arm64" else []
        return sh(*prefix, self.tool, *a)

    def test_it_builds_for_x86_64_without_warnings(self):
        self.assertIn("x86_64", sh("lipo", "-archs", self.tool).stdout)

    def test_any_other_arguments_are_refused(self):
        for args in ((), ("-S", "a"), ("-o", "a", "-S", "b"), ("-S", "a", "-o", "b", "extra")):
            self.assertEqual(self.run_tool(*args).returncode, 2, args)

    @unittest.skipUnless(RUNTIME and os.environ.get("AIR_DIS_BLOB"), "needs air-runtime and AIR_DIS_BLOB")
    def test_a_real_module_prints_as_air_assembly(self):
        out = self.t / "ok.ll"
        r = self.run_tool("-S", os.environ["AIR_DIS_BLOB"], "-o", out)
        self.assertEqual(r.returncode, 0, r.stderr)
        text = out.read_text()
        self.assertIn('target triple = "air64', text)
        self.assertIn("define ", text)

    @unittest.skipUnless(RUNTIME and os.environ.get("AIR_DIS_BLOB"), "needs air-runtime and AIR_DIS_BLOB")
    def test_a_damaged_module_fails_and_leaves_no_output(self):
        d = bytearray(Path(os.environ["AIR_DIS_BLOB"]).read_bytes())
        d[len(d) // 2:len(d) // 2 + 64] = b"\xff" * 64
        bad = self.t / "bad.air"; bad.write_bytes(d)
        out = self.t / "bad.ll"
        r = self.run_tool("-S", bad, "-o", out)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertFalse(out.exists())

    def test_without_air_runtime_it_stops_with_a_setup_error(self):
        lone = self.t / "lone"; lone.mkdir(exist_ok=True)
        shutil.copy(self.tool, lone / "nvmtl-air-dis")
        prefix = ["arch", "-x86_64"] if os.uname().machine == "arm64" else []
        r = sh(*prefix, lone / "nvmtl-air-dis", "-S", "/dev/null", "-o", self.t / "x.ll")
        self.assertEqual(r.returncode, 2, r.stderr)


@unittest.skipUnless(MAC, "needs a Mac with clang")
class Gate(unittest.TestCase):
    """check_package.sh on a tree that is complete except for the one thing each arm plants."""

    def setUp(self):
        self.t = t = Path(tempfile.mkdtemp()); self.addCleanup(shutil.rmtree, t)
        self.root = t / "pkgroot"
        self.macos = self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS"; self.macos.mkdir(parents=True)
        (t / "c.c").write_text("int NVEncVT_Register(int r){return r;} int NVEncVT_RegisterDecoders(int r){return r;} "
                               "int NVEncVT_CreateInstance(int a){return a;} int NVDecVT_CreateInstance(int a){return a;}\n")
        sh("clang", "-arch", "x86_64", "-dynamiclib", "-o", self.macos / "libnvencvt.dylib", t / "c.c")
        sh("codesign", "-f", "-s", "-", self.macos / "libnvencvt.dylib")
        (t / "m.c").write_text("int main(void){return 0;}\n")
        sh("clang", "-arch", "x86_64", "-o", self.macos / "nvmtl-air-dis", t / "m.c")
        sh("codesign", "-f", "-s", "-", self.macos / "nvmtl-air-dis")   # the gate requires every program and library signed
        k = t / "NMSMN.kext/Contents"; (k / "MacOS").mkdir(parents=True)
        import plistlib, hashlib
        (k / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.nullmoth.NMSMN"})); (k / "MacOS/NMSMN").write_bytes(b"k")
        (t / "ryzenadj").write_bytes(b"r")
        (self.root / "SHA256SUMS").write_text("")
        shutil.copy(PKG / "sums_complete.sh", self.root / "sums_complete.sh")
        sh("bash", PKG / "stage_nmsmn.sh", t / "NMSMN.kext", t / "ryzenadj", hashlib.sha256(b"r").hexdigest(), self.root)
        self.relist()

    def relist(self):
        files = sorted(str(p.relative_to(self.root)) for p in self.root.rglob("*") if p.is_file() and not p.is_symlink() and p.name != "SHA256SUMS")
        r = sh("bash", PKG / "sums_refresh.sh", self.root, *files)
        assert r.returncode == 0, r.stderr

    def gate(self):
        return sh("bash", PKG / "check_package.sh", self.root)

    def test_the_clean_tree_passes(self):
        r = self.gate()
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_an_apple_signed_program_is_refused(self):
        nv = self.root / "Library/GPUBundles/nvmtl"; nv.mkdir()
        shutil.copy("/usr/bin/true", nv / "air-opt")
        self.relist()
        r = self.gate()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Library/GPUBundles/nvmtl/air-opt is signed by Apple", r.stdout)

    def test_a_library_loading_from_homebrew_is_refused(self):
        lib = self.t / "dep.c"; lib.write_text("int dep(void){return 1;}\n")
        sh("clang", "-arch", "x86_64", "-dynamiclib", "-install_name", "/opt/homebrew/lib/libdep.dylib", "-o", self.t / "libdep.dylib", lib)
        use = self.t / "use.c"; use.write_text("int dep(void); int use(void){return dep();}\n")
        sh("clang", "-arch", "x86_64", "-dynamiclib", "-o", self.macos / "libuse.dylib", use, self.t / "libdep.dylib")
        sh("codesign", "-f", "-s", "-", self.macos / "libuse.dylib")   # the gate requires every program and library signed
        self.relist()
        r = self.gate()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("libuse.dylib loads /opt/homebrew/lib/libdep.dylib", r.stdout)

    def test_a_library_naming_itself_by_a_build_path_is_not_a_dependency(self):
        lib = self.t / "own.c"; lib.write_text("int own(void){return 1;}\n")
        sh("clang", "-arch", "x86_64", "-dynamiclib", "-install_name", "/Volumes/Build/libown.dylib", "-o", self.macos / "libown.dylib", lib)
        sh("codesign", "-f", "-s", "-", self.macos / "libown.dylib")   # the gate requires every program and library signed
        self.relist()
        self.assertEqual(self.gate().returncode, 0, self.gate().stdout)

    def test_the_decoder_is_required(self):
        (self.macos / "nvmtl-air-dis").unlink()
        self.relist()
        r = self.gate()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("lacks nvmtl-air-dis", r.stdout)


if __name__ == "__main__":
    unittest.main()

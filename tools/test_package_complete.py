#!/usr/bin/env python3
"""Every regular file in a driver package must be listed in its SHA256SUMS.

`shasum -c` reads only the files the list names, and install.sh copies whole folders as root, so a file the list left out
was installed unchecked. package/sums_complete.sh refuses such a tree; install.sh runs it right after `shasum -c`,
check_package.sh runs it at build time, and the staging scripts list what they add (package/sums_refresh.sh).
package/finish_package.sh runs the whole last stage in one order.

usage: test_package_complete.py   (the staging arms need a Mac with clang)"""
import hashlib, plistlib, shutil, subprocess, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PKG = ROOT / "package"
INSTALL = (PKG / "install.sh").read_text()


def run(*a):
    return subprocess.run(["bash", *map(str, a)], capture_output=True, text=True)


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def tree(t, files):
    root = t / "pkgroot"
    for rel, data in files.items():
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_bytes(data)
    return root


def listing(root, rels, form="./"):
    (root / "SHA256SUMS").write_text("".join(f"{sha(root / r)}  {form}{r}\n" for r in rels))


class SumsComplete(unittest.TestCase):
    def setUp(self):
        self.t = Path(tempfile.mkdtemp()); self.addCleanup(shutil.rmtree, self.t)

    def test_a_complete_list_passes_in_every_spelling(self):
        for form in ("./", "", "*"):
            root = tree(self.t / form.strip("./*") / "x", {"a": b"a", "Library/b": b"b"})
            if form == "*":
                (root / "SHA256SUMS").write_text("".join(f"{sha(root / r)} *{r}\n" for r in ("a", "Library/b")))
            else:
                listing(root, ["a", "Library/b"], form)
            r = run(PKG / "sums_complete.sh", root)
            self.assertEqual(r.returncode, 0, (form, r.stdout))

    def test_an_unlisted_file_is_refused_and_named(self):
        root = tree(self.t, {"a": b"a", "Library/NullMoth/kexts/planted.kext/Contents/MacOS/x": b"x"})
        listing(root, ["a"])
        r = run(PKG / "sums_complete.sh", root)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Library/NullMoth/kexts/planted.kext/Contents/MacOS/x", r.stdout)

    def test_a_tree_without_a_list_is_refused(self):
        root = tree(self.t, {"a": b"a"})
        self.assertNotEqual(run(PKG / "sums_complete.sh", root).returncode, 0)

    def test_a_listed_name_that_only_shares_a_prefix_does_not_cover_a_file(self):
        root = tree(self.t, {"ab": b"a", "a": b"b"})
        listing(root, ["ab"])
        self.assertNotEqual(run(PKG / "sums_complete.sh", root).returncode, 0)

    def test_an_internal_link_needs_no_line_of_its_own(self):
        root = tree(self.t, {"lib/libvulkan.1.dylib": b"v"})
        (root / "lib/libvulkan.dylib").symlink_to("libvulkan.1.dylib")
        listing(root, ["lib/libvulkan.1.dylib"])
        self.assertEqual(run(PKG / "sums_complete.sh", root).returncode, 0)

    def test_a_path_holding_a_newline_is_refused(self):
        root = tree(self.t, {"a": b"a", "odd\nname": b"n"})
        listing(root, ["a"])
        r = run(PKG / "sums_complete.sh", root)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("newline", r.stdout)


class SumsRefresh(unittest.TestCase):
    def setUp(self):
        self.t = Path(tempfile.mkdtemp()); self.addCleanup(shutil.rmtree, self.t)

    def test_it_replaces_both_spellings_and_keeps_the_rest(self):
        root = tree(self.t, {"x/f": b"new", "o": b"o"})
        (root / "SHA256SUMS").write_text(f"{sha(root / 'o')}  ./o\n{'1' * 64}  x/f\n{'2' * 64}  ./x/f\n")
        self.assertEqual(run(PKG / "sums_refresh.sh", root, "x/f").returncode, 0)
        lines = (root / "SHA256SUMS").read_text().splitlines()
        self.assertEqual([l for l in lines if l.endswith("x/f")], [f"{sha(root / 'x/f')}  ./x/f"])
        self.assertIn(f"{sha(root / 'o')}  ./o", lines)

    def test_it_refuses_a_link(self):
        root = tree(self.t, {"f": b"f"})
        (root / "l").symlink_to("f")
        listing(root, ["f"])
        self.assertNotEqual(run(PKG / "sums_refresh.sh", root, "l").returncode, 0)


class Installer(unittest.TestCase):
    def test_install_runs_the_completeness_check_right_after_shasum(self):
        c = INSTALL.find("shasum -a 256 --strict -c SHA256SUMS")
        s = INSTALL.find('bash "$HERE/sums_complete.sh" "$HERE" || die')
        self.assertGreater(c, 0)
        self.assertGreater(s, c)
        self.assertNotIn("sums_complete.sh", INSTALL[:c])

    def test_install_refuses_a_package_without_a_listed_checker(self):
        i = INSTALL.find('[ -f "$HERE/sums_complete.sh" ]')
        self.assertGreater(i, 0)
        self.assertIn('die "this package does not carry a verified sums_complete.sh', INSTALL[i:i + 300])

    def test_both_checks_come_before_the_install_changes_anything(self):
        s = INSTALL.find('bash "$HERE/sums_complete.sh" "$HERE" || die')
        first_change = min(x for x in (INSTALL.find("INSTALLING=1"), INSTALL.find('ditto "$HERE/Library/GPUBundles/$b"')) if x > 0)
        self.assertLess(s, first_change)


@unittest.skipUnless(shutil.which("clang") and shutil.which("codesign"), "needs a Mac with clang")
class Finish(unittest.TestCase):
    """finish_package.sh end to end on a minimal base tree."""

    def setUp(self):
        self.t = t = Path(tempfile.mkdtemp()); self.addCleanup(shutil.rmtree, t)
        (t / "c.c").write_text("int NVEncVT_Register(int r){return r;} int NVEncVT_RegisterDecoders(int r){return r;} "
                               "int NVEncVT_CreateInstance(int a){return a;} int NVDecVT_CreateInstance(int a){return a;}\n")
        subprocess.run(["clang", "-arch", "x86_64", "-dynamiclib", "-o", str(t / "nvencvt.dylib"), str(t / "c.c")], check=True, capture_output=True)
        (t / "b.c").write_text("int main(void){return 0;}\n")
        subprocess.run(["clang", "-arch", "x86_64", "-o", str(t / "nvmtl-boost"), str(t / "b.c")], check=True, capture_output=True)
        shutil.copy(t / "nvmtl-boost", t / "nvmtl-air-dis")
        k = t / "NMSMN.kext/Contents"; (k / "MacOS").mkdir(parents=True)
        (k / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.nullmoth.NMSMN"})); (k / "MacOS/NMSMN").write_bytes(b"k")
        (t / "ryzenadj").write_bytes(b"r")
        self.root = tree(t, {"install.sh": (PKG / "install.sh").read_bytes(),
                             "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist": b"<plist/>"})
        (self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS").mkdir()
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist"])

    def finish(self, *extra):
        t = self.t
        return run(PKG / "finish_package.sh", self.root, "--nvencvt", t / "nvencvt.dylib", "--air-dis", t / "nvmtl-air-dis",
                   "--nmsmn", t / "NMSMN.kext", t / "ryzenadj", hashlib.sha256(b"r").hexdigest(), *extra)

    def test_a_full_package_is_finished_and_passes_its_own_checks(self):
        r = self.finish("--boost", self.t / "nvmtl-boost")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue((self.root / "sums_complete.sh").exists())
        self.assertEqual(run(PKG / "sums_complete.sh", self.root).returncode, 0)
        sums = (self.root / "SHA256SUMS").read_text()
        self.assertIn(f"{sha(self.root / 'Library/NullMoth/boost/nvmtl-boost')}  ./Library/NullMoth/boost/nvmtl-boost", sums)
        ok = subprocess.run(["shasum", "-a", "256", "--strict", "-c", "SHA256SUMS", "--quiet"], cwd=self.root, capture_output=True)
        self.assertEqual(ok.returncode, 0, ok.stdout)

    def test_a_base_tree_with_an_unlisted_file_is_not_blessed(self):
        (self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/stray").write_bytes(b"s")
        r = self.finish()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("stray", r.stdout)

    def test_a_carried_linux_library_is_refused(self):
        # packages up to 2.8.1 carried NVIDIA's closed compiler libraries as ELF files; the Mach-O checks never saw them
        rel = "Library/NullMoth/kexts/libcarried.so"
        (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
        (self.root / rel).write_bytes(b"\x7fELF" + b"\0" * 60)
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist", rel])
        r = self.finish()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("libcarried.so is a Linux ELF binary", r.stdout)

    def test_the_vendor_compiler_bundle_is_refused(self):
        rel = "Library/GPUBundles/NVIDIAShared.bundle/Contents/PlugIns/air2nvvm.py"
        (self.root / rel).parent.mkdir(parents=True)
        (self.root / rel).write_bytes(b"# text\n")
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist", rel])
        r = self.finish()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("NVIDIAShared.bundle", r.stdout)

    def test_the_plugin_bundle_is_sealed_again_after_staging_and_a_later_change_is_refused(self):
        m = self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents"
        m.joinpath("Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.nullmoth.test", "CFBundleExecutable": "NVMTLDriver",
                                                             "CFBundlePackageType": "BNDL"}))
        shutil.copy(self.t / "nvmtl-boost", m / "MacOS/NVMTLDriver")
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist",
                            "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver"])
        r = self.finish()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        seal = subprocess.run(["codesign", "--verify", "--deep", "--strict", str(m.parent)], capture_output=True, text=True)
        self.assertEqual(seal.returncode, 0, seal.stderr)
        ok = subprocess.run(["shasum", "-a", "256", "--strict", "-c", "SHA256SUMS", "--quiet"], cwd=self.root, capture_output=True, text=True)
        self.assertEqual(ok.returncode, 0, ok.stdout)
        with open(m / "MacOS/libnvencvt.dylib", "ab") as fh: fh.write(b"x")      # changed after the seal
        bad = run(PKG / "check_package.sh", self.root)
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("seal does not verify", bad.stdout)

    def test_an_unsigned_program_is_refused(self):
        rel = "dlss/nm_unzstd"
        (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(self.t / "nvmtl-boost", self.root / rel)      # a linked program straight from the compiler: no signature
        subprocess.run(["codesign", "--remove-signature", str(self.root / rel)], capture_output=True)
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist", rel])
        r = self.finish()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("dlss/nm_unzstd is not signed", r.stdout)

    def test_a_stale_installer_is_refused(self):
        (self.root / "install.sh").write_text("#!/bin/bash\necho old\n")
        listing(self.root, ["install.sh", "Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist"])
        r = self.finish()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("is not package/install.sh", r.stderr)

    def test_the_ryzen_parts_are_required(self):
        r = run(PKG / "finish_package.sh", self.root, "--nvencvt", self.t / "nvencvt.dylib", "--air-dis", self.t / "nvmtl-air-dis")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("--nmsmn is required", r.stderr)

    def test_the_old_gate_alone_would_have_passed_the_stray_file(self):
        # Negative control: everything the gate checked before (codec library, Ryzen parts, their lines) is in order,
        # and only the completeness check stops this tree.
        self.assertEqual(self.finish().returncode, 0)
        (self.root / "Library/NullMoth/kexts/planted").write_bytes(b"p")
        r = run(PKG / "check_package.sh", self.root)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("does not list", r.stdout)
        self.assertNotIn("no matching line", r.stdout)


if __name__ == "__main__":
    unittest.main()

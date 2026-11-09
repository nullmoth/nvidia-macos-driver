#!/usr/bin/env python3
"""package/boost_daemon.sh: the opt-in GPU boost daemon is off by default, installs into /Library/NullMoth/boost only through
a root-owned chain of real folders, follows the setting and removes exactly what it made. Runs against a temp folder
(NULLMOTH_BOOST_ROOT) that stands for /, never the real system. The test's own uid plays root (NULLMOTH_BOOST_OWNER_UID).
  usage: test_boost_daemon.py [tree root]"""
import os, pathlib, subprocess, sys, tempfile

ROOT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "package/boost_daemon.sh"
bad = 0


def check(name, ok, detail=""):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name + (f"  [{detail}]" if detail else ""))


if not SCRIPT.exists():
    check("package/boost_daemon.sh exists", False)
    sys.exit(1)
inst = (ROOT / "package/install.sh").read_text()
unin = (ROOT / "package/uninstall.sh").read_text()
script = SCRIPT.read_text()
check("the installer runs it only for NULLMOTH_GPU_BOOST=1", 'NULLMOTH_GPU_BOOST:-0}" = 1' in inst)
check("the installer trusts it only when SHA256SUMS covers the script and the binary", "boost_daemon\\.sh$" in inst and "nvmtl-boost$" in inst)
check("the uninstaller removes it", "boost_daemon.sh" in unin and '" remove' in unin)
check("the daemon is not installed under /usr/local (user-writable on an Intel Mac)", 'BIN="$DIR/nvmtl-boost"' in script and 'DIR="$P/Library/NullMoth/boost"' in script)

ME = os.getuid()


def fresh(t, name):
    sysroot = t / name
    for d in ("Library/NullMoth", "Library/LaunchDaemons"):
        (sysroot / d).mkdir(parents=True)
    for d in (sysroot, sysroot / "Library", sysroot / "Library/NullMoth", sysroot / "Library/LaunchDaemons"):
        d.chmod(0o755)
    return sysroot


with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    pk = t / "pkg/Library/NullMoth/boost"; pk.mkdir(parents=True)
    (pk / "nvmtl-boost").write_text("#!/bin/sh\nexit 0\n")

    def run(sysroot, *a, owner=ME):
        env = dict(os.environ, NULLMOTH_BOOST_ROOT=str(sysroot), NULLMOTH_BOOST_NO_LAUNCHCTL="1", NULLMOTH_BOOST_OWNER_UID=str(owner))
        return subprocess.run(["bash", str(SCRIPT), *a], env=env, capture_output=True, text=True)

    def files(sysroot):
        return sorted(str(p.relative_to(sysroot)) for p in sysroot.rglob("*") if p.is_file() or p.is_symlink())

    s = fresh(t, "sys")
    conf = s / "Library/NullMoth/gpuboost"
    check("before install it says not installed", run(s, "status").stdout.strip() == "not installed")
    check("set does nothing when the daemon is not installed", run(s, "set", "on").returncode == 1 and not conf.exists())
    r = run(s, "install", str(t / "pkg"))
    plist = s / "Library/LaunchDaemons/com.nullmoth.nvmtl-boost.plist"
    check("install places the binary under /Library/NullMoth/boost, the plist and the setting",
          r.returncode == 0 and (s / "Library/NullMoth/boost/nvmtl-boost").exists() and plist.exists() and conf.read_text() == "gpuboost=1\n", r.stdout + r.stderr)
    check("the plist names only that path", "<string>/Library/NullMoth/boost/nvmtl-boost</string>" in plist.read_text() and plist.read_text().count("<string>/") == 1)
    check("no temporary file is left behind", not [f for f in files(s) if ".nullmoth-boost." in f], str(files(s)))
    check("the binary is executable; the plist and the setting cannot be written by others",
          os.access(s / "Library/NullMoth/boost/nvmtl-boost", os.X_OK) and (conf.stat().st_mode & 0o022) == 0 and (plist.stat().st_mode & 0o777) == 0o644)
    check("set off writes gpuboost=0, set on writes gpuboost=1", run(s, "set", "off").returncode == 0 and conf.read_text() == "gpuboost=0\n"
          and run(s, "set", "on").returncode == 0 and conf.read_text() == "gpuboost=1\n")
    run(s, "set", "off"); run(s, "install", str(t / "pkg"))
    check("installing again keeps the user's setting", conf.read_text() == "gpuboost=0\n")
    check("a bad set argument is refused", run(s, "set", "maybe").returncode == 2)
    check("status reports the setting", "gpuboost=0" in run(s, "status").stdout)
    legacy = s / "usr/local/libexec/nullmoth"; legacy.mkdir(parents=True); (legacy / "nvmtl-boost").write_text("old")
    r = run(s, "remove")
    check("remove takes away everything install made, and an old /usr/local copy", r.returncode == 0 and not files(s), str(files(s)))

    # refusals: each must leave the system untouched, and set/status must refuse too
    def refuses(name, mutate, owner=ME):
        sysroot = fresh(t, name)
        mutate(sysroot)
        before = files(sysroot)
        r = run(sysroot, "install", str(t / "pkg"), owner=owner)
        check(f"{name}: install is refused and nothing is written", r.returncode != 0 and "refused" in r.stdout and not [f for f in files(sysroot) if f not in before], r.stdout + r.stderr)
        return sysroot

    def symlinked(s):
        real = s / "elsewhere"; real.mkdir(); real.chmod(0o755)
        (s / "Library/NullMoth").rmdir(); (s / "Library/NullMoth").symlink_to(real)
    refuses("symlinked_parent", symlinked)
    refuses("user_writable_parent", lambda s: (s / "Library/NullMoth").chmod(0o777))
    refuses("group_writable_parent", lambda s: (s / "Library/NullMoth").chmod(0o775))
    refuses("writable_launchdaemons_folder", lambda s: (s / "Library/LaunchDaemons").chmod(0o777))
    refuses("symlinked_boost_folder", lambda s: (s / "Library/NullMoth/boost").symlink_to(s / "Library/LaunchDaemons"))
    refuses("not_root_owned", lambda s: None, owner=ME + 1)

    # an installed daemon whose folder is later made writable or swapped for a link: set and status refuse
    s = fresh(t, "later")
    run(s, "install", str(t / "pkg"))
    (s / "Library/NullMoth/boost").chmod(0o777)
    r = run(s, "set", "off")
    check("set refuses once the binary's folder can be written by others", r.returncode != 0 and "refused" in r.stdout and (s / "Library/NullMoth/gpuboost").read_text() == "gpuboost=1\n")
    check("status says the path is not trusted", run(s, "status").returncode != 0)
    (s / "Library/NullMoth/boost").chmod(0o755)
    b = s / "Library/NullMoth/boost/nvmtl-boost"; b.unlink(); b.symlink_to("/bin/sh")
    r = run(s, "set", "off")
    check("set refuses when the binary has been replaced by a link", r.returncode != 0 and "symbolic link" in r.stdout)
    empty = t / "empty"; empty.mkdir()
    s = fresh(t, "nodaemon")
    check("a package without the daemon installs nothing and does not fail", run(s, "install", str(empty)).returncode == 0 and not files(s))
sys.exit(1 if bad else 0)

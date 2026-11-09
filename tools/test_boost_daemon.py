#!/usr/bin/env python3
"""package/boost_daemon.sh: the opt-in GPU boost daemon is off by default, installs only what it names, follows the
setting and removes exactly what it made. Runs against a temp folder (NULLMOTH_BOOST_ROOT), never the real system."""
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
check("the installer runs it only for NULLMOTH_GPU_BOOST=1", 'NULLMOTH_GPU_BOOST:-0}" = 1' in inst)
check("the installer trusts it only when SHA256SUMS covers the script and the binary", "boost_daemon\\.sh$" in inst and "nvmtl-boost$" in inst)
check("the uninstaller removes it", "boost_daemon.sh" in unin and '" remove' in unin)

with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    pk = t / "pkg/Library/NullMoth/boost"; pk.mkdir(parents=True)
    (pk / "nvmtl-boost").write_text("#!/bin/sh\nexit 0\n")
    (pk / "com.nullmoth.nvmtl-boost.plist").write_text("<plist/>")
    env = dict(os.environ, NULLMOTH_BOOST_ROOT=str(t / "sys"), NULLMOTH_BOOST_NO_LAUNCHCTL="1")

    def run(*a):
        return subprocess.run(["bash", str(SCRIPT), *a], env=env, capture_output=True, text=True)

    check("before install it says not installed", run("status").stdout.strip() == "not installed")
    check("set does nothing when the daemon is not installed", run("set", "on").returncode == 1 and not (t / "sys/Library/NullMoth/gpuboost").exists())
    r = run("install", str(t / "pkg"))
    conf = t / "sys/Library/NullMoth/gpuboost"
    check("install places the binary, the plist and the setting", r.returncode == 0 and (t / "sys/usr/local/libexec/nullmoth/nvmtl-boost").exists()
          and (t / "sys/Library/LaunchDaemons/com.nullmoth.nvmtl-boost.plist").exists() and conf.read_text() == "gpuboost=1\n", r.stdout + r.stderr)
    check("the binary is executable and the setting cannot be written by others",
          os.access(t / "sys/usr/local/libexec/nullmoth/nvmtl-boost", os.X_OK) and (conf.stat().st_mode & 0o022) == 0)
    check("set off writes gpuboost=0", run("set", "off").returncode == 0 and conf.read_text() == "gpuboost=0\n")
    check("set on writes gpuboost=1", run("set", "on").returncode == 0 and conf.read_text() == "gpuboost=1\n")
    run("set", "off")
    run("install", str(t / "pkg"))
    check("installing again keeps the user's setting", conf.read_text() == "gpuboost=0\n")
    check("a bad set argument is refused", run("set", "maybe").returncode == 2)
    check("status reports the setting", "gpuboost=0" in run("status").stdout)
    r = run("remove")
    left = [p for p in (t / "sys").rglob("*") if p.is_file()]
    check("remove takes away everything install made", r.returncode == 0 and not left, str(left))
    empty = t / "empty"; empty.mkdir()
    check("a package without the daemon installs nothing and does not fail", run("install", str(empty)).returncode == 0 and not [p for p in (t / "sys").rglob("*") if p.is_file()])
sys.exit(1 if bad else 0)

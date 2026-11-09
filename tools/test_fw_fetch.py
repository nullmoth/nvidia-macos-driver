#!/usr/bin/env python3
"""package/fw_fetch.sh: the GSP firmware comes from NVIDIA's own driver package, verified by SHA-256, never from ours.

Arms (named sentences) run against a fake makeself .run whose pins are swapped in through FW_TEST_PINS (non-root only).
Needs Python 3.14 (compression.zstd) and clang (builds the Zstandard decoder the package ships)."""
import hashlib
import io
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from compression import zstd

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
fails = 0


def check(name, ok, extra=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (("  " + extra) if (extra and not ok) else ""))
    fails += 0 if ok else 1


def sha(b):
    return hashlib.sha256(b).hexdigest()


MEMBERS = ["gsp_tu10x.bin", "gsp_ga10x.bin", "ucodes_tu10x.bin", "ucodes_ga10x.bin"]
FILES = {m: ("fake firmware " + m).encode() * 50 for m in MEMBERS}


def make_run(path, files):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:
        for n, b in files.items():
            ti = tarfile.TarInfo("firmware/" + n)
            ti.size = len(b)
            t.addfile(ti, io.BytesIO(b))
        ti = tarfile.TarInfo("README")
        ti.size = 3
        t.addfile(ti, io.BytesIO(b"abc"))
    payload = zstd.compress(buf.getvalue())
    header = b"#!/bin/sh\n# fake makeself header\nskip=5\nexit 0\n"
    with open(path, "wb") as f:
        f.write(header + payload)


def run(pkg, env_extra, *args):
    env = dict(os.environ, **env_extra)
    return subprocess.run(["bash", pkg + "/fw_fetch.sh", *args], capture_output=True, text=True, env=env)


def dest_files(d):
    out = []
    for r, _, fs in os.walk(d):
        out += fs
    return sorted(out)


with tempfile.TemporaryDirectory() as td:
    pkg = td + "/pkg"
    os.makedirs(pkg + "/dlss")
    subprocess.check_call(["cp", ROOT + "/package/fw_fetch.sh", pkg + "/fw_fetch.sh"])
    z = ROOT + "/dlss/zstd"
    srcs = [ROOT + "/dlss/nm_unzstd.c"] + [os.path.join(z, d, f) for d in ("common", "decompress") for f in os.listdir(os.path.join(z, d)) if f.endswith(".c")]
    subprocess.check_call(["clang", "-O1", "-DZSTD_DISABLE_ASM", "-o", pkg + "/dlss/nm_unzstd"] + srcs, stderr=subprocess.DEVNULL)

    # the fake .run: header has skip=N where N is the line the payload starts on
    run_path = td + "/fake.run"
    make_run(run_path, FILES)
    raw = open(run_path, "rb").read()
    hdr = raw.index(b"exit 0\n") + len(b"exit 0\n")
    open(run_path, "wb").write(raw[:hdr].replace(b"skip=5", b"skip=%d" % (raw[:hdr].count(b"\n") + 1)) + raw[hdr:])
    run_sha = sha(open(run_path, "rb").read())

    def pins(path, run_sha_v, shas, url, proto="=file"):
        body = "URL_RUN='%s'\nSHA_RUN=%s\nCURL_PROTO='%s'\nfw_sha() { case \"$1\" in\n" % (url, run_sha_v, proto)
        for m, s in shas.items():
            body += "  %s) echo %s;;\n" % (m, s)
        body += "  *) echo none;;\nesac; }\n"
        open(path, "w").write(body)

    good = {m: sha(b) for m, b in FILES.items()}
    pins(td + "/good.pins", run_sha, good, "file://" + run_path)
    env = {"FW_TEST_PINS": td + "/good.pins"}

    d = td + "/d1"; os.makedirs(d)
    r = run(pkg, env, "--dest", d, "--run", run_path, "--installed", td + "/none")
    check("a driver package that matches its pin yields four verified firmware files", r.returncode == 0 and "FW_OK" in r.stdout and
          dest_files(d) == sorted(MEMBERS) and open(d + "/nvidia/620.70.06/gsp_tu10x.bin", "rb").read() == FILES["gsp_tu10x.bin"], r.stdout + r.stderr)

    d = td + "/d2"; os.makedirs(d)
    r = run(pkg, env, "--dest", d, "--installed", td + "/none")
    check("with no file given the package is downloaded from the pinned URL and verified", r.returncode == 0 and dest_files(d) == sorted(MEMBERS), r.stdout + r.stderr)

    inst = td + "/inst/nvidia/620.70.06"; os.makedirs(inst)
    for m, b in FILES.items():
        open(inst + "/" + m, "wb").write(b)
    pins(td + "/dead.pins", run_sha, good, "file://" + td + "/does-not-exist")
    d = td + "/d3"; os.makedirs(d)
    r = run(pkg, {"FW_TEST_PINS": td + "/dead.pins"}, "--dest", d, "--installed", td + "/inst")
    check("an earlier install that still matches is reused with no network at all", r.returncode == 0 and dest_files(d) == sorted(MEMBERS) and "already matches" in r.stdout, r.stdout + r.stderr)

    open(inst + "/gsp_ga10x.bin", "wb").write(b"tampered")
    d = td + "/d4"; os.makedirs(d)
    r = run(pkg, {"FW_TEST_PINS": td + "/dead.pins"}, "--dest", d, "--installed", td + "/inst")
    check("a changed installed file is not trusted and, with no network, the run stops with the download address",
          r.returncode == 1 and "To supply it by hand: download" in r.stderr, r.stdout + r.stderr)
    check("a failed run leaves the destination empty", dest_files(d) == [], str(dest_files(d)))

    bad = dict(good); bad["ucodes_ga10x.bin"] = "0" * 64
    pins(td + "/badm.pins", run_sha, bad, "file://" + run_path)
    d = td + "/d5"; os.makedirs(d)
    r = run(pkg, {"FW_TEST_PINS": td + "/badm.pins"}, "--dest", d, "--run", run_path, "--installed", td + "/none")
    check("a firmware file that differs from its pin is refused and nothing is left behind", r.returncode == 1 and dest_files(d) == [] and "ucodes_ga10x.bin" in r.stderr, r.stdout + r.stderr)

    pins(td + "/badr.pins", "1" * 64, good, "file://" + run_path)
    d = td + "/d6"; os.makedirs(d)
    r = run(pkg, {"FW_TEST_PINS": td + "/badr.pins"}, "--dest", d, "--run", run_path, "--installed", td + "/none")
    check("a driver package that differs from its pin is refused", r.returncode == 1 and dest_files(d) == [], r.stdout + r.stderr)

    short = {k: v for k, v in FILES.items() if k != "gsp_ga10x.bin"}
    make_run(td + "/short.run", short)
    raw = open(td + "/short.run", "rb").read()
    hdr = raw.index(b"exit 0\n") + len(b"exit 0\n")
    open(td + "/short.run", "wb").write(raw[:hdr].replace(b"skip=5", b"skip=%d" % (raw[:hdr].count(b"\n") + 1)) + raw[hdr:])
    pins(td + "/short.pins", sha(open(td + "/short.run", "rb").read()), good, "file://" + td + "/short.run")
    d = td + "/d7"; os.makedirs(d)
    r = run(pkg, {"FW_TEST_PINS": td + "/short.pins"}, "--dest", d, "--run", td + "/short.run", "--installed", td + "/none")
    check("a driver package without one of the files is refused", r.returncode == 1 and dest_files(d) == [], r.stdout + r.stderr)

# the real pins
fw = open(ROOT + "/package/fw_fetch.sh").read()
dl = open(ROOT + "/package/dlss_fetch.sh").read()
g = lambda s, k: re.search(r"^%s=(.*)$" % k, s, re.M).group(1)
check("the driver package URL and SHA-256 equal the ones dlss_fetch.sh pins", g(fw, "SHA_RUN") == g(dl, "SHA_RUN") and "NVIDIA-Linux-x86_64-620.70.06.run" in dl and
      "XFree86/Linux-x86_64/$VER/NVIDIA-Linux-x86_64-$VER.run" in fw and "XFree86/Linux-x86_64/620.70.06/NVIDIA-Linux-x86_64-620.70.06.run" in dl)
check("every firmware member the installer needs has a 64-hex pin",
      all(re.search(r"%s\)\s+echo [0-9a-f]{64};;" % re.escape(m), fw) for m in MEMBERS))
inst = open(ROOT + "/package/install.sh").read()
check("install.sh passes a driver package found by the 1401 apps (NULLMOTH_FW_RUN) to the fetcher", '${NULLMOTH_FW_RUN:+--run "$NULLMOTH_FW_RUN"}' in inst)
check("install.sh gets the firmware before it changes anything (before the backup directory is made)",
      inst.index('"$HERE/fw_fetch.sh"') < inst.index('BK=$(mktemp -d "/Library/NullMoth/backup-') and 'ditto "$FWSRC" "$FW"' in inst)
sys.exit(1 if fails else 0)

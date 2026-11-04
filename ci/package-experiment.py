#!/usr/bin/env python3
"""Package compiled kexts with the matching stock user-space driver; do not install."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import posixpath
import shutil
import struct
import tarfile
import urllib.request
from importlib.machinery import SourceFileLoader

STOCK_URL = "https://github.com/nullmoth/nvidia-macos-driver/releases/download/v2.7/nullmoth-nvidia-2.7.tar.gz"
STOCK_SHA256 = "63583c2bda20a36d31c35f9c5b0b52f33de9c4de2320e62c9dd681b5f686c523"


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for b in iter(lambda: f.read(1024 * 1024), b""):
            h.update(b)
    return h.hexdigest()


def package_filter(member, destination):
    # The stock installer places pkgroot/Library at /Library. Keep those link
    # relationships relative inside staging so extraction never escapes it.
    if member.issym() and PurePosixPath(member.linkname).is_absolute():
        target = PurePosixPath("pkgroot") / member.linkname.lstrip("/")
        if target.parts[:2] != ("pkgroot", "Library"):
            raise tarfile.FilterError(f"Unexpected absolute package link: {member.name}")
        member = member.replace(linkname=posixpath.relpath(str(target), posixpath.dirname(member.name)))
    return tarfile.data_filter(member, destination)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    out = args.output.resolve()
    if out == root or not out.is_relative_to(root):
        raise SystemExit("Output must be a build directory inside this checkout.")
    proof = json.loads((out / "build-proof.json").read_text())
    if proof["status"] != "KEXTS_COMPILED_HARDWARE_NOT_TESTED" or proof["experiment"] != "bar-preflight-v1":
        raise SystemExit("Both kext builds must succeed before packaging.")
    for name in ("NVRM", "NVRMFB"):
        p = out / "kexts" / (name + ".kext") / "Contents/MacOS" / name
        with p.open("rb") as f:
            magic, cpu, _, kind = struct.unpack("<4I", f.read(16))
        if (magic, cpu, kind) != (0xfeedfacf, 0x1000007, 11):
            raise SystemExit(f"Unexpected kext format: {name}")
    stock = out / "stock-2.7.tar.gz"
    with urllib.request.urlopen(STOCK_URL, timeout=120) as response, stock.open("wb") as f:
        shutil.copyfileobj(response, f)
    if digest(stock) != STOCK_SHA256:
        raise SystemExit("Stock release checksum mismatch.")
    stage = out / "package-stage"
    if stage.exists():
        raise SystemExit("Package staging directory must be fresh.")
    stage.mkdir()
    with tarfile.open(stock, "r:gz") as tf:
        tf.extractall(stage, filter=package_filter)
    package = stage / "pkgroot"
    stock_files = {p.relative_to(package).as_posix(): digest(p)
                   for p in package.rglob("*") if p.is_file() and not p.is_symlink()}
    verifier = SourceFileLoader("verify_kext_imports", str(root / "ci/verify-kext-imports.py")).load_module()
    verifier.verify_imports(out / "kexts", package / "Library/Extensions", out / "logs/import-validation.json")
    for name in ("NVRM",):
        target = (package / "Library/Extensions" / (name + ".kext")).resolve()
        if not target.is_relative_to(package.resolve()) or target.name != name + ".kext":
            raise SystemExit("Unexpected package kext destination.")
        shutil.rmtree(target)
        shutil.copytree(out / "kexts" / (name + ".kext"), target)
    proof["replaced_kexts"] = ["NVRM"]
    retained = {name: sha for name, sha in stock_files.items()
                if name != "SHA256SUMS" and not name.startswith("Library/Extensions/NVRM.kext/")}
    for name, sha in retained.items():
        if digest(package / name) != sha:
            raise SystemExit(f"Unexpected change to official package file: {name}")
    proof["unchanged_official_files"] = retained
    proof["stock_package_sha256"] = STOCK_SHA256
    proof["stock_components_retained"] = ["NVRMFB", "NVAccel", "NVRMAGDC", "all user-space and firmware files", "official install and uninstall scripts"]
    proof["package_note"] = "Personal small-BAR research build based on NullMoth 2.7. Not hardware validated."
    (package / "Library/NullMoth/bar-preflight-v1.json").parent.mkdir(parents=True, exist_ok=True)
    (package / "Library/NullMoth/bar-preflight-v1.json").write_text(json.dumps(proof, indent=2) + "\n")
    (package / "EXPERIMENTAL-BUILD.json").write_text(json.dumps(proof, indent=2) + "\n")
    checksum = package / "SHA256SUMS"
    lines = [f"{digest(p)}  {p.relative_to(package).as_posix()}" for p in sorted(package.rglob("*"))
             if p.is_file() and p != checksum and not p.is_symlink()]
    checksum.write_text("\n".join(lines) + "\n")
    suffix = "bar-preflight-v1"
    archive = out / f"nullmoth-2.7-rtx6000-{suffix}.tar.gz"
    with tarfile.open(archive, "w:gz") as tf:
        tf.add(package, arcname="pkgroot")
    (out / "SHA256SUMS.txt").write_text(f"{digest(archive)}  {archive.name}\n")
    print(f"Packaged {archive.name}; hardware validation remains pending.")


if __name__ == "__main__":
    main()

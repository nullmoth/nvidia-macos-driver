#!/usr/bin/env python3
"""Stage the locally revised NVEncVT build into an already assembled offline package.

This does not build, install, load, or execute the library. It validates the
build receipt, signs the library and containing bundle, copies the applicable
notices, then regenerates the package-wide SHA256SUMS. Changes are rolled back
if staging or final verification fails.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


EXPECTED = {
    "schema_version": 2,
    "artifact": "libnvencvt.dylib",
    "artifact_version": "v0.9.5-ga10x2",
    "local_revision": "ga10x-codecs-2",
    "arch": "x86_64",
    "min_os": "15.0",
    "upstream_repo": "mickys/nvenc",
    "upstream_tag": "v0.9.5",
    "upstream_commit": "1e91845d6f37d8bce671d62c14f4390324da3659",
}
BUILD_KEYS = set(EXPECTED) | {"sha256", "sdk"}
LIB_NAME = "libnvencvt.dylib"
BUILD_INFO = "nvencvt-build-info.json"
RESOURCE_BUILD_INFO = "nvencvt-build-info.json"
LICENSES = (
    ("vendor/nvenc/LICENSE", "NVEncVT-MIT-LICENSE.txt"),
    ("vendor/nvenc/THIRD_PARTY_NOTICES.md", "NVEncVT-THIRD_PARTY_NOTICES.md"),
    ("vendor/nvenc/vendor/open-gpu-kernel-modules/COPYING", "NVEncVT-NVIDIA-open-gpu-kernel-modules-COPYING.txt"),
    ("vendor/nvenc/vendor/open-gpu-doc/LICENSE.md", "NVEncVT-open-gpu-doc-LICENSE.md"),
)


class StageError(RuntimeError):
    pass


def say(message: str) -> None:
    print(message, file=sys.stderr)


def fail(message: str) -> None:
    raise StageError(message)


def require_plain_file(path: Path, label: str) -> None:
    if path.is_symlink() or not path.is_file():
        fail(f"{label} must be a regular file: {path}")


def require_plain_dir(path: Path, label: str) -> None:
    if path.is_symlink() or not path.is_dir():
        fail(f"{label} must be a real directory: {path}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def no_duplicate_json_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            fail(f"duplicate field in build metadata: {key}")
        result[key] = value
    return result


def load_build(build_dir: Path, repo: Path) -> tuple[Path, dict, bytes]:
    dylib = build_dir / LIB_NAME
    info_path = build_dir / BUILD_INFO
    require_plain_file(dylib, "build library")
    require_plain_file(info_path, "build metadata")
    try:
        info_bytes = info_path.read_bytes()
        info = json.loads(info_bytes.decode("utf-8"), object_pairs_hook=no_duplicate_json_keys)
    except (UnicodeError, json.JSONDecodeError) as exc:
        fail(f"cannot parse build metadata: {exc}")
    if not isinstance(info, dict) or set(info) != BUILD_KEYS:
        fail(f"build metadata fields do not match schema v2: {sorted(info) if isinstance(info, dict) else type(info).__name__}")
    for key, value in EXPECTED.items():
        if info.get(key) != value or key == "schema_version" and type(info.get(key)) is not int:
            fail(f"build metadata {key} must be {value!r}, got {info.get(key)!r}")
    if not isinstance(info.get("sdk"), str) or not info["sdk"] or len(info["sdk"]) > 32:
        fail("build metadata sdk must be a non-empty string of at most 32 characters")
    claimed = info.get("sha256")
    if not isinstance(claimed, str) or len(claimed) != 64 or any(c not in "0123456789abcdef" for c in claimed):
        fail("build metadata sha256 must be 64 lowercase hexadecimal characters")
    actual = sha256(dylib)
    if claimed != actual:
        fail(f"build library SHA-256 mismatch: metadata {claimed}, file {actual}")
    for rel, _ in LICENSES:
        require_plain_file(repo / rel, "NVEncVT license/notice")
    return dylib, info, info_bytes


def verify_manifest(pkgroot: Path) -> None:
    manifest = pkgroot / "SHA256SUMS"
    require_plain_file(manifest, "package SHA256SUMS")
    expected = set()
    for path in pkgroot.rglob("*"):
        relative = path.relative_to(pkgroot).as_posix()
        if "\n" in relative or "\r" in relative or "\\" in relative:
            fail(f"package path cannot be represented in SHA256SUMS: {relative!r}")
        if relative == "SHA256SUMS" or path.is_dir() and not path.is_symlink():
            continue
        if path.is_symlink():
            resolved = path.resolve(strict=True)
            if not resolved.is_file():
                continue
        elif not path.is_file():
            fail(f"unsupported package filesystem entry: {path.relative_to(pkgroot)!s}")
        expected.add(relative)
    listed = set()
    for line_number, line in enumerate(manifest.read_text(encoding="ascii").splitlines(), 1):
        if len(line) < 67 or line[64:66] != "  " or any(c not in "0123456789abcdef" for c in line[:64]):
            fail(f"malformed SHA256SUMS line {line_number}")
        name = line[66:]
        if not name or name.startswith("/") or "\\" in name or "\n" in name or "\r" in name:
            fail(f"unrepresentable path in SHA256SUMS line {line_number}")
        normalized = Path(name)
        if normalized.is_absolute() or ".." in normalized.parts or normalized.as_posix() != name:
            fail(f"unsafe path in SHA256SUMS line {line_number}: {name!r}")
        if name in listed:
            fail(f"duplicate path in SHA256SUMS line {line_number}: {name}")
        listed.add(name)
    if listed != expected:
        missing = sorted(expected - listed)
        extra = sorted(listed - expected)
        fail(f"SHA256SUMS does not cover this exact package tree (missing={missing[:5]}, extra={extra[:5]})")
    result = subprocess.run(
        ["shasum", "-a", "256", "--strict", "-c", "SHA256SUMS", "--quiet"],
        cwd=pkgroot,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if result.returncode:
        fail("package SHA256SUMS verification failed: " + result.stdout.strip())


def checked_package_paths(pkgroot: Path) -> tuple[Path, Path, Path]:
    bundle = pkgroot / "Library/GPUBundles/NVMTLDriver.bundle"
    require_plain_dir(bundle, "NVMTLDriver.bundle")
    executable = bundle / "Contents/MacOS/NVMTLDriver"
    require_plain_file(executable, "NVMTLDriver executable")
    macos = executable.parent
    require_plain_dir(macos, "NVMTLDriver Contents/MacOS")
    library = macos / LIB_NAME
    if library.exists() or library.is_symlink():
        require_plain_file(library, "existing NVEncVT library")
    resources = bundle / "Contents/Resources"
    if resources.exists() or resources.is_symlink():
        require_plain_dir(resources, "NVMTLDriver Contents/Resources")
    licenses = pkgroot / "licenses"
    if licenses.exists() or licenses.is_symlink():
        require_plain_dir(licenses, "package licenses directory")
    return bundle, library, resources


def check_symlinks(root: Path) -> None:
    """The installer accepts only relative links whose target remains in pkgroot."""
    for path in root.rglob("*"):
        if not path.is_symlink():
            continue
        relative = path.relative_to(root)
        if "\n" in str(relative) or "\r" in str(relative) or "\\" in str(relative):
            fail(f"package path cannot be represented in SHA256SUMS: {relative!s}")
        target = os.readlink(path)
        if os.path.isabs(target):
            fail(f"absolute package symlink is not allowed: {relative!s} -> {target}")
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError):
            fail(f"broken or cyclic package symlink: {relative!s}")
        if not resolved.is_relative_to(root):
            fail(f"package symlink leaves pkgroot: {relative!s} -> {target}")


def write_manifest(pkgroot: Path) -> None:
    entries: list[tuple[str, str]] = []
    for path in pkgroot.rglob("*"):
        rel = path.relative_to(pkgroot)
        rel_text = rel.as_posix()
        if rel_text == "SHA256SUMS" or path.is_dir() and not path.is_symlink():
            continue
        if "\n" in rel_text or "\r" in rel_text or "\\" in rel_text:
            fail(f"package path cannot be represented in SHA256SUMS: {rel_text!r}")
        if path.is_symlink():
            resolved = path.resolve(strict=True)
            if not resolved.is_file():
                continue
            digest_path = resolved
        elif path.is_file():
            digest_path = path
        else:
            fail(f"unsupported package filesystem entry: {rel_text}")
        entries.append((rel_text, sha256(digest_path)))
    entries.sort(key=lambda item: item[0])
    content = "".join(f"{digest}  {name}\n" for name, digest in entries)
    fd, temp_name = tempfile.mkstemp(prefix=".SHA256SUMS.", dir=pkgroot)
    temp_path = Path(temp_name)
    try:
        with os.fdopen(fd, "w", encoding="ascii", newline="\n") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temp_path, 0o644)
        os.replace(temp_path, pkgroot / "SHA256SUMS")
    finally:
        temp_path.unlink(missing_ok=True)


def copy_file(src: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)


def run(*args: str) -> None:
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        fail(f"command failed ({result.returncode}): {' '.join(args)}\n{result.stdout.strip()}")


def main(argv: list[str]) -> int:
    if len(argv) != 3 or argv[1] in {"-h", "--help"}:
        print(f"usage: {argv[0]} <pkgroot> <nvenc-build-output-dir>", file=sys.stderr)
        print("Stages the locally revised NVEncVT dylib into an assembled package; does not install or run it.", file=sys.stderr)
        return 2 if len(argv) != 2 else 0

    pkgroot = Path(argv[1]).resolve(strict=True)
    build_dir = Path(argv[2]).resolve(strict=True)
    repo = Path(__file__).resolve().parents[1]
    require_plain_dir(pkgroot, "pkgroot")
    require_plain_dir(build_dir, "NVEncVT build output")
    if pkgroot == build_dir or pkgroot in build_dir.parents or build_dir in pkgroot.parents:
        fail("pkgroot and build output must be separate directories")

    # Validate every input before taking a backup or changing the package.
    check_symlinks(pkgroot)
    verify_manifest(pkgroot)
    source_library, build_info, build_info_bytes = load_build(build_dir, repo)
    bundle, target_library, resources = checked_package_paths(pkgroot)
    license_dir = pkgroot / "licenses"

    with tempfile.TemporaryDirectory(prefix="nvencvt-stage-") as temp_name:
        temp = Path(temp_name)
        bundle_backup = temp / "NVMTLDriver.bundle"
        shutil.copytree(bundle, bundle_backup, symlinks=True, copy_function=shutil.copy2)
        resources_existed = resources.exists()
        licenses_existed = license_dir.exists()
        original_manifest = temp / "SHA256SUMS"
        shutil.copy2(pkgroot / "SHA256SUMS", original_manifest)
        file_backups: dict[Path, Path | None] = {}
        destinations = [resources / RESOURCE_BUILD_INFO]
        for _, packaged_name in LICENSES:
            destinations.extend((resources / packaged_name, license_dir / packaged_name))
        for index, dest in enumerate(destinations):
            if dest.exists() or dest.is_symlink():
                require_plain_file(dest, f"existing staged file {dest}")
                backup = temp / f"file-{index}"
                shutil.copy2(dest, backup)
                file_backups[dest] = backup
            else:
                file_backups[dest] = None
        changed = False

        def rollback() -> None:
            if not changed:
                return
            shutil.rmtree(bundle, ignore_errors=True)
            shutil.copytree(bundle_backup, bundle, symlinks=True, copy_function=shutil.copy2)
            for dest, backup in file_backups.items():
                if backup is None:
                    if dest.is_dir() and not dest.is_symlink():
                        shutil.rmtree(dest)
                    elif dest.exists() or dest.is_symlink():
                        dest.unlink(missing_ok=True)
                else:
                    copy_file(backup, dest)
            if not resources_existed and resources.is_dir() and not any(resources.iterdir()):
                resources.rmdir()
            if not licenses_existed and license_dir.is_dir() and not any(license_dir.iterdir()):
                license_dir.rmdir()
            shutil.copy2(original_manifest, pkgroot / "SHA256SUMS")

        try:
            changed = True
            copy_file(source_library, target_library)
            if sha256(target_library) != build_info["sha256"]:
                fail("staged dylib differs from the build whose receipt was verified")
            if shutil.which("xattr"):
                run("xattr", "-c", str(target_library))
            target_library.chmod(0o755)
            run("codesign", "--force", "--sign", "-", str(target_library))
            run("codesign", "--verify", "--strict", str(target_library))

            resources.mkdir(parents=True, exist_ok=True)
            (resources / RESOURCE_BUILD_INFO).write_bytes(build_info_bytes)
            (resources / RESOURCE_BUILD_INFO).chmod(0o644)
            license_dir.mkdir(parents=True, exist_ok=True)
            for source_rel, packaged_name in LICENSES:
                copy_file(repo / source_rel, license_dir / packaged_name)
                copy_file(repo / source_rel, resources / packaged_name)

            run("codesign", "--force", "--sign", "-", str(bundle))
            run("codesign", "--verify", "--strict", str(bundle))
            check_symlinks(pkgroot)
            write_manifest(pkgroot)
            verify_manifest(pkgroot)
            changed = False
        except BaseException:
            try:
                rollback()
            except BaseException as rollback_error:
                say(f"STOP: staging failed and rollback also failed: {rollback_error}")
                raise
            raise

    say(f"Staged {LIB_NAME} into {target_library}")
    say(f"Artifact: {build_info['artifact_version']} (local revision {build_info['local_revision']})")
    say(f"Upstream base: {build_info['upstream_repo']} {build_info['upstream_tag']} ({build_info['upstream_commit']})")
    say("Library and bundle signatures verified; package SHA256SUMS regenerated and verified.")
    say("No install, dlopen, VideoToolbox registration, or GPU work was performed.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv))
    except StageError as exc:
        print(f"STOP: {exc}", file=sys.stderr)
        raise SystemExit(1)
    except (OSError, UnicodeError) as exc:
        print(f"STOP: staging file operation failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("STOP: interrupted; package rollback attempted", file=sys.stderr)
        raise SystemExit(130)

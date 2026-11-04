#!/usr/bin/env python3
"""Build an x86_64 research driver on an Intel macOS runner. Never install it."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.request

NVIDIA_COMMIT = "1a82fbfda8f321b056eb3568850f9316c5e465c5"
STOCK_URL = "https://github.com/nullmoth/nvidia-macos-driver/releases/download/v2.5.2/nullmoth-nvidia-2.5.2.tar.gz"
STOCK_SHA256 = "b3c01bbdce788e6181636836c73653be19052e217442c87763923ed37296007d"


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def run(args, cwd=None, log=None):
    if log:
        log.parent.mkdir(parents=True, exist_ok=True)
        with log.open("w") as f:
            p = subprocess.run(args, cwd=cwd, stdout=f, stderr=subprocess.STDOUT)
        if p.returncode:
            contents = log.read_text(errors="replace")
            errors = [line for line in contents.splitlines()
                      if any(term in line.lower() for term in ("error:", "fatal", "unsupported", "undefined"))]
            print("\n".join(errors[:30]), flush=True)
            print(contents[-4000:], flush=True)
            raise RuntimeError(f"Command failed ({p.returncode}); log: {log}")
        return ""
    try:
        return subprocess.check_output(args, cwd=cwd, text=True, stderr=subprocess.STDOUT).strip()
    except subprocess.CalledProcessError as e:
        print(e.output[-9000:], flush=True)
        raise


def export_vendor(root, vendor, component, out):
    directory = vendor / "src" / component
    generated = directory / "_out/Darwin_x86_64"
    generated.mkdir(parents=True, exist_ok=True)
    cc = run(["xcrun", "--find", "clang"])
    cxx = run(["xcrun", "--find", "clang++"])
    run(["gmake", "-f", "Makefile", "-f", str(root / "ci/export-nvidia.mk"),
         "ci-export", "TARGET_OS=Darwin", "TARGET_ARCH=x86_64",
         "CC=" + cc, "CXX=" + cxx, "EXTRA_CFLAGS=-DNV_UNIX"], cwd=directory,
        log=out / "logs" / f"{component}-make-export.log")
    flags = shlex.split((generated / "ci-cflags.txt").read_text())
    # NVIDIA 615 adds Linux's external return thunk even for TARGET_OS=Darwin.
    # XNU does not export __x86_return_thunk. Use the compiler's native Darwin
    # returns, as the official kext does, rather than inventing a kernel import.
    flags = [flag for flag in flags if flag != "-mfunction-return=thunk-extern"]
    cc_flags = shlex.split((generated / "ci-cc-only-cflags.txt").read_text())
    cxx_flags = shlex.split((generated / "ci-cxx-only-cflags.txt").read_text())
    sources = shlex.split((generated / "ci-sources.txt").read_text())
    spdm_sources = set(shlex.split((generated / "ci-libspdm-sources.txt").read_text()))
    spdm_flags = shlex.split((generated / "ci-libspdm-cflags.txt").read_text())
    idname = "NV_RM_ID" if component == "nvidia" else "NV_KMS_ID"
    (generated / "g_nvid_string.c").write_text(
        f'const char {idname}[] = "nvidia id: NVIDIA UNIX Open Kernel Module for x86_64 615.78.08 Research Build (yuanjing-hash 2.7-bar-preflight-v1)";\n'
        f'const char *const p{idname} = {idname} + 11;\n')
    return directory, flags, sources, spdm_sources, spdm_flags, cc_flags, cxx_flags


def compile_one(source, directory, flags, out, label):
    source_path = (directory / source).resolve()
    if not source_path.is_file():
        raise RuntimeError(f"Missing source: {source_path}")
    key = hashlib.sha256(str(source_path).encode()).hexdigest()[:14]
    obj = out / "objects" / label / (source_path.stem + "-" + key + ".o")
    obj.parent.mkdir(parents=True, exist_ok=True)
    log = out / "logs" / label / (obj.stem + ".log")
    compiler = "clang++" if source_path.suffix in (".cpp", ".cc", ".cxx") else "clang"
    command = ["xcrun", compiler, *flags, "-c", str(source_path), "-o", str(obj)]
    run(command, cwd=directory, log=log)
    return obj


def build_archive(component, exported, common, out, jobs):
    directory, flags, sources, spdm_sources, spdm_flags, cc_flags, cxx_flags = exported
    objects = []
    commands = []
    for source in sources:
        extra = spdm_flags if source in spdm_sources else []
        language_flags = cxx_flags if Path(source).suffix in (".cpp", ".cc", ".cxx") else cc_flags
        commands.append((source, directory, [*flags, *language_flags, *common, *extra], out, component))
    print(f"Compiling {component}: {len(commands)} sources", flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {pool.submit(compile_one, *command): command[0] for command in commands}
        completed = 0
        errors = []
        for future in concurrent.futures.as_completed(futures):
            try:
                objects.append(future.result())
            except Exception as e:
                errors.append(str(e))
                for pending in futures:
                    pending.cancel()
                break
            completed += 1
            if completed % 50 == 0 or completed == len(commands):
                print(f"{component}: {completed}/{len(commands)} compiled", flush=True)
        if errors:
            raise RuntimeError("\n".join(errors))
    archive = out / f"lib{component}.a"
    run(["xcrun", "libtool", "-static", "-o", str(archive), *map(str, sorted(objects))],
        log=out / "logs" / f"{component}-archive.log")
    return archive


def native_flags(exported, sdk, headers, resource, root, vendor):
    directory, exported_flags = exported[:2]
    retained = []
    arguments = iter(exported_flags)
    for flag in arguments:
        if flag == "-I":
            retained.append("-I" + str((directory / next(arguments)).resolve()))
        elif flag.startswith("-I"):
            retained.append("-I" + str((directory / flag[2:]).resolve()))
        elif flag.startswith("-D") and flag not in ("-DNVRM", "-D_LANGUAGE_C", "-D__NO_CTYPE"):
            retained.append(flag)
    rm = vendor / "src/nvidia"
    kms = vendor / "src/nvidia-modeset"
    includes = [root / "kexts/NVRM", rm / "arch/nvalloc/unix/include", rm / "generated",
                kms / "os-interface/include", kms / "kapi/interface", kms / "kapi/include",
                kms / "interface", kms / "include", kms / "generated",
                vendor / "src/common/unix/nvidia-push/interface",
                vendor / "src/common/unix/common/inc", vendor / "src/common/modeset"]
    return ["-arch", "x86_64", "-isysroot", str(sdk), "-fapple-kext", "-mkernel", "-nostdinc",
            "-isystem", str(headers), "-isystem", str(resource / "include"),
            "-DKERNEL", "-DKERNEL_PRIVATE", "-DDRIVER_PRIVATE", "-DAPPLE", "-DNeXT",
            "-std=c++17", "-fno-rtti", "-fno-exceptions", "-fno-builtin", "-fno-common",
            "-Wall", "-Wno-unused-parameter", "-Wno-inconsistent-missing-override", "-O2",
            *retained, *["-I" + str(p) for p in includes]]


def link_kext(name, objects, archives, plist, sdk, out):
    bundle = out / "kexts" / (name + ".kext")
    binary = bundle / "Contents/MacOS" / name
    binary.parent.mkdir(parents=True, exist_ok=True)
    run(["xcrun", "clang++", "-arch", "x86_64", "-isysroot", str(sdk), "-fapple-kext",
         "-nostdlib", "-Xlinker", "-kext", *map(str, objects), *map(str, archives),
         "-lkmodc++", "-lkmod", "-lcc_kext", "-o", str(binary)],
        log=out / "logs" / (name + "-link.log"))
    shutil.copy2(plist, bundle / "Contents/Info.plist")
    plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
    run(["xcrun", "lipo", str(binary), "-verify_arch", "x86_64"])
    description = run(["file", str(binary)])
    if "kext bundle" not in description:
        raise RuntimeError(f"Unexpected binary format: {description}")
    return bundle


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--vendor", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("This build requires an Intel macOS runner.")
    root = Path(__file__).resolve().parents[1]
    vendor = args.vendor.resolve()
    out = args.output.resolve()
    if out == root or not out.is_relative_to(root):
        raise RuntimeError("Output must be a build directory inside this checkout")
    out.mkdir(parents=True, exist_ok=True)
    if run(["git", "rev-parse", "HEAD"], cwd=vendor) != NVIDIA_COMMIT:
        raise RuntimeError("NVIDIA source commit mismatch")
    sdk = Path(run(["xcrun", "--sdk", "macosx", "--show-sdk-path"]))
    headers = sdk / "System/Library/Frameworks/Kernel.framework/Headers"
    resource = Path(run(["xcrun", "clang", "-print-resource-dir"]))
    if not (headers / "IOKit/IOLib.h").is_file():
        raise RuntimeError(f"Kernel SDK headers unavailable: {headers}")
    proof = {"status": "BUILDING", "experiment": "bar-preflight-v1",
             "upstream": "v2.7", "nvidia_commit": NVIDIA_COMMIT,
             "fork_commit": run(["git", "rev-parse", "HEAD"], cwd=root),
             "sdk": str(sdk), "hardware_tested": False, "installed": False}
    (out / "build-proof.json").write_text(json.dumps(proof, indent=2))
    rm = export_vendor(root, vendor, "nvidia", out)
    kms = export_vendor(root, vendor, "nvidia-modeset", out)
    common = ["-arch", "x86_64", "-isysroot", str(sdk), "-mkernel", "-nostdinc",
              "-isystem", str(headers), "-isystem", str(resource / "include"),
              "-DKERNEL", "-DKERNEL_PRIVATE", "-DDRIVER_PRIVATE"]
    native = native_flags(rm, sdk, headers, resource, root, vendor)
    framebuffer = compile_one("kexts/NVRMFB/fb/nvrm-fb.cpp", root,
                              [*native, "-I" + str(root / "kexts/NVRMFB")], out, "NVRMFB")
    link_kext("NVRMFB", [framebuffer], [], root / "kexts/NVRMFB/fb/Info.plist", sdk, out)
    rm_archive = build_archive("nvidia", rm, common, out, args.jobs)
    kms_archive = build_archive("nvidia-modeset", kms, common, out, args.jobs)
    native_objects = []
    for name in ["NVRM.cpp", "os-xnu.cpp", "os-xnu2.cpp", "os-nvkms-xnu.cpp",
                 "nv_xnu_stubs.cpp", "nvrm_fbinfo.cpp", "nvrm_gpuva.cpp", "nvrm_surfshare.cpp",
                 "nvkms_shaders.cpp", "nv_xnu_power.cpp"]:
        native_objects.append(compile_one("kexts/NVRM/" + name, root, native, out, "NVRM"))
    rm_directory, rm_flags, _, _, _, rm_cc_flags, _ = rm
    native_objects.append(compile_one(str(root / "kexts/NVRM/nvrm_pageoff.c"), rm_directory,
                                      [*rm_flags, *rm_cc_flags, *common], out, "NVRM"))
    kms_directory, kms_flags, _, _, _, kms_cc_flags, _ = kms
    native_objects.append(compile_one(str(root / "kexts/NVRM/nvrm_kapi_fields.c"), kms_directory,
                                      [*kms_flags, *kms_cc_flags, *common], out, "NVRM"))
    link_kext("NVRM", native_objects, [kms_archive, rm_archive], root / "kexts/NVRM/Info.plist", sdk, out)
    proof["status"] = "KEXTS_COMPILED_HARDWARE_NOT_TESTED"
    proof["binaries"] = {p.parent.parent.parent.name: digest(p)
                         for p in (out / "kexts").glob("*.kext/Contents/MacOS/*")}
    (out / "build-proof.json").write_text(json.dumps(proof, indent=2))
    print("NVRM and NVRMFB compiled. Hardware acceptance remains pending.", flush=True)


if __name__ == "__main__":
    main()

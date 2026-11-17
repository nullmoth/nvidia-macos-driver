#!/usr/bin/env python3
"""Stage fork builds for Sequoia/Tahoe in a copy of an upstream package.

Requires macOS, Python 3, and Apple's command line tools. The upstream input
and build outputs remain unchanged; the destination must not already exist.
"""
import argparse
import hashlib
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
PLUGIN = Path("Library/GPUBundles/NVMTLDriver.bundle")
RUNTIME = Path("Library/GPUBundles/nvmtl")
TRANSLATOR = RUNTIME / "libnvmtl_translate.dylib"
BUNDLED_TRANSLATOR = PLUGIN / "Contents/MacOS/libnvmtl_translate.dylib"
FRAMEBUFFER = Path("Library/Extensions/NVRMFB.kext")
NVRM = Path("Library/Extensions/NVRM.kext")
AGDC = Path("Library/Extensions/NVRMAGDC.kext")
ACCEL = {v: Path("Library/NullMoth/kexts") / v / "NVAccel.kext" for v in ("15", "26")}
MINECRAFT = RUNTIME / "minecraft"
WINE = RUNTIME / "wine"
SEQUOIA_RUNTIME_MINIMUM = "15.5"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def inside(path, root):
    return path == root or root in path.parents


def run(*args, check=True):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True)
    if check and result.returncode:
        raise ValueError("{} failed:\n{}{}".format(args[0], result.stdout, result.stderr))
    return result


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(chunk)
    return checksum.hexdigest()


def validate_links(root):
    # Resolve installed absolute paths against the staged package root. This
    # accepts the package's /Library/... runtime links without following them
    # into the live system or the source package.
    for entry in root.rglob("*"):
        if entry.is_symlink():
            current = entry
            seen = set()
            while current.is_symlink():
                require(current not in seen, "package symlink loop: " + str(entry))
                seen.add(current)
                target = Path(os.readlink(current))
                require(".." not in target.parts, "package symlink contains parent traversal: " + str(entry))
                current = (root / target.relative_to("/")) if target.is_absolute() else (current.parent / target)
            require(inside(current.resolve(strict=True), root), "package symlink escapes its root: " + str(entry))


def regular(path):
    require(path.is_file() and not path.is_symlink(), "missing regular file: " + str(path))


def bundle(path, executable):
    require(path.is_dir() and not path.is_symlink(), "missing bundle: " + str(path))
    regular(path / "Contents/Info.plist")
    regular(path / "Contents/MacOS" / executable)
    with (path / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    require(info.get("CFBundleExecutable") == executable, "unexpected bundle executable: " + str(path))


def preserve_kext_release_metadata(target, original):
    """Retain fork personalities while carrying the matched release's identity."""
    target_plist = target / "Contents/Info.plist"
    original_plist = original / "Contents/Info.plist"
    current = plistlib.loads(target_plist.read_bytes())
    release = plistlib.loads(original_plist.read_bytes())
    require(current.get("CFBundleIdentifier") == release.get("CFBundleIdentifier"),
            "replacement kext bundle ID differs from release: " + str(target))
    merged = dict(current)
    for key in ("CFBundleVersion", "CFBundleShortVersionString", "OSBundleCompatibleVersion"):
        if key in release:
            merged[key] = release[key]
    if "OSBundleLibraries" in release:
        require(isinstance(release["OSBundleLibraries"], dict)
                and isinstance(current.get("OSBundleLibraries", {}), dict), "invalid kernel dependency metadata")
        dependencies = dict(current.get("OSBundleLibraries", {}))
        dependencies.update(release["OSBundleLibraries"])
        merged["OSBundleLibraries"] = dependencies
    if merged != current:
        target_plist.write_bytes(plistlib.dumps(merged, sort_keys=False))
        run("codesign", "--force", "--sign", "-", "--timestamp=none", target)
        run("codesign", "--verify", "--strict", target)


def manifest_name(name):
    if any(c in name for c in "\\\r\n"):
        return "\\", name.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r")
    return "", name


def decode_name(name):
    # shasum/coreutils escape file names containing backslashes or newlines.
    require(re.fullmatch(r"(?:[^\\]|\\[\\nr])*", name) is not None, "invalid checksum filename escape")
    return re.sub(r"\\([\\nr])", lambda m: {"\\": "\\", "n": "\n", "r": "\r"}[m[1]], name)


def verify_manifest(root, *, complete=True):
    manifest = root / "SHA256SUMS"
    regular(manifest)
    count = 0
    covered = set()
    for row in manifest.read_text(encoding="utf-8").splitlines():
        if not row or row.startswith("#"):
            continue
        escaped = row.startswith("\\")
        match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *](.+)", row[1:] if escaped else row)
        require(match is not None, "invalid checksum record in " + str(manifest))
        name = decode_name(match[2]) if escaped else match[2]
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts, "checksum path escapes package: " + name)
        target = root / relative
        require(inside(target.resolve(strict=True), root), "checksum target escapes package: " + name)
        require(digest(target) == match[1].lower(), "SHA256SUMS mismatch: " + name)
        covered.add(target.relative_to(root).as_posix())
        count += 1
    require(count > 0, "empty checksum manifest: " + str(manifest))
    if complete:
        present = {path.relative_to(root).as_posix() for path in root.rglob("*")
                   if path.is_file() and not path.is_symlink() and path != manifest}
        missing = present - covered
        require(not missing, "SHA256SUMS does not list package files: " + ", ".join(sorted(missing)[:20]))


def write_manifest(root):
    rows = []
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.is_symlink() or path == root / "SHA256SUMS":
            continue
        prefix, name = manifest_name("./" + path.relative_to(root).as_posix())
        rows.append(prefix + digest(path) + "  " + name + "\n")
    manifest = root / "SHA256SUMS"
    require(not manifest.is_symlink(), "checksum manifest must not be a symlink: " + str(manifest))
    with manifest.open("w", encoding="utf-8", newline="\n") as stream:
        stream.writelines(rows)
    manifest.chmod(0o644)
    verify_manifest(root)


def replace_file(source, destination):
    # Unlink first: never write through a packaged symlink or shared hard link.
    if destination.exists() or destination.is_symlink():
        destination.unlink()
    shutil.copy2(source, destination)


def validate_video_runtime(root):
    """A separately built, signed codec addon with pinned source provenance."""
    root = root.resolve(strict=True)
    regular(root / "NVENC-BUILD.json")
    fields = json.loads((root / "NVENC-BUILD.json").read_text())
    pin = json.loads((REPO / "video/nvenc-source.json").read_text())
    require(fields.get("schema_version") == 1 and fields.get("kind") == "nvenc-codecs", "invalid codec runtime schema")
    require(fields.get("registration_api") == {"version": 2, "symbol": "NVEncVT_RegisterCodecs",
                                               "decoder_scope": 1, "encoder_scope": 2},
            "codec runtime lacks the matching scoped registration API")
    require(fields.get("source") == pin["source"] and fields.get("dependencies") == pin["dependencies"],
            "codec runtime source/dependencies do not match this checkout")
    require(fields.get("overlay_sha256") == digest(REPO / "video" / pin["overlay"]), "codec source overlay mismatch")
    require(fields.get("target") == {"os": "macos", "arch": "x86_64", "minimum_macos": "15.0"}, "incompatible codec platform")
    require(fields.get("turing_defaults") == {"additional_decoders": ["vp08", "vp09"], "replace_stock_encoders": False, "av1": False},
            "incompatible Turing codec selection")
    require(fields.get("installation") == {"library": "/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib",
                                          "records": "/Library/GPUBundles/nvmtl/video"}, "unexpected codec installation paths")
    expected = {"libnvencvt.dylib", "source-changes.patch", "INSTALL.md", "LICENSES/nvenc-MIT.txt", "LICENSES/NVIDIA-MIT.txt", "LICENSES/WebKit-BSD.txt"}
    files = fields.get("files", {})
    require(set(files) == expected, "incomplete or unexpected codec payload")
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file() or p.is_symlink()}
    require(actual == expected | {"NVENC-BUILD.json"}, "unrecorded codec runtime files")
    for name in sorted(expected):
        path = root / name
        regular(path)
        require(inside(path.resolve(strict=True), root), "codec file escapes its runtime")
        require(re.fullmatch(r"[0-9a-f]{64}", files[name]) is not None and digest(path) == files[name], "codec runtime hash mismatch: " + name)
    require(digest(root / "source-changes.patch") == fields["overlay_sha256"], "packaged codec overlay mismatch")
    require(digest(root / "LICENSES/nvenc-MIT.txt") == digest(REPO / "video/nvenc-MIT.txt"), "codec source license mismatch")
    run("lipo", root / "libnvencvt.dylib", "-verify_arch", "x86_64")
    run("codesign", "--verify", "--strict", root / "libnvencvt.dylib")
    version = run("vtool", "-show-build", root / "libnvencvt.dylib").stdout
    require(re.search(r"\bminos 15\.0\b", version) is not None, "codec deployment target mismatch")
    return tuple(root / name for name in sorted(expected | {"NVENC-BUILD.json"}))


def video_default_config(text):
    # An explicit opt-out or process selection survives an update.
    if re.search(r"^\s*NVMTL_NVENCVT\s*=", text, re.M):
        return text
    return text.rstrip("\n") + "\n\n# Bundled additional NVIDIA video codecs; 0 disables the addon.\nNVMTL_NVENCVT=1\n"


def stage_video_runtime(root, video, files):
    video = video.resolve(strict=True)
    records = root / RUNTIME / "video"
    require(not records.is_symlink(), "codec record directory is a symlink")
    if records.exists():
        shutil.rmtree(records)
    for source in files:
        relative = source.relative_to(video)
        target = root / PLUGIN / "Contents/MacOS/libnvencvt.dylib" if relative == Path("libnvencvt.dylib") else records / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        replace_file(source, target)
    conf = root / RUNTIME / "nvrm610.conf"
    require(not conf.is_symlink(), "codec config is a symlink")
    conf.write_text(video_default_config(conf.read_text() if conf.exists() else ""))
    conf.chmod(0o644)
    run("codesign", "--force", "--sign", "-", "--timestamp=none", root / PLUGIN)
    run("codesign", "--verify", "--strict", root / PLUGIN)


def validate_minecraft_runtime(root, checker):
    require(root.is_dir() and not root.is_symlink(), "missing Minecraft runtime directory")
    validate_links(root.resolve())
    manifest = root / "nvk-bridge-runtime.properties"
    regular(manifest)
    fields = {}
    for line in manifest.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith(("#", "!")):
            continue
        key, separator, value = line.partition("=")
        key, value = key.strip(), value.strip()
        require(separator and key not in fields, "invalid or duplicate Minecraft runtime property")
        fields[key] = value
    expected = {"schema.version": "1", "target.os": "macos", "target.arch": "x86_64",
                "minecraft.version": "26.3", "presentation": "gpu-copy",
                "library.path": "lib/libnvk_minecraft.dylib"}
    require(all(fields.get(key) == value for key, value in expected.items()),
            "incompatible Minecraft runtime manifest")
    require(bool(fields.get("runtime.id")), "missing Minecraft runtime identity")
    require(re.fullmatch(r"[0-9a-fA-F]{64}", fields.get("library.sha256", "")) is not None,
            "invalid Minecraft runtime hash")
    entry = root / expected["library.path"]
    renderer = root / "lib/libnvk_renderer.dylib"
    for binary in (entry, renderer):
        regular(binary)
    require(digest(entry) == fields["library.sha256"].lower(), "Minecraft entry hash mismatch")
    regular(checker)
    for binary in (entry, renderer):
        run("lipo", binary, "-verify_arch", "x86_64")
        run("codesign", "--verify", "--strict", binary)
        run(checker, SEQUOIA_RUNTIME_MINIMUM, binary)
    files = (manifest, entry, renderer)
    icd = root / "nvk_bridge_icd.json"
    # Older verified Minecraft runtimes predate loader support. Preserve their
    # layout; newer runtimes must keep the frontend ICD through staging.
    if icd.exists() or icd.is_symlink():
        regular(icd)
        try:
            data = json.loads(icd.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            raise ValueError("invalid native bridge ICD JSON") from error
        require(isinstance(data, dict) and data.get("file_format_version") == "1.0.0",
                "incompatible native bridge ICD format")
        driver = data.get("ICD")
        require(isinstance(driver, dict)
                and driver.get("library_path") == "./lib/libnvk_minecraft.dylib"
                and driver.get("api_version") == "1.4.363",
                "native bridge ICD must select the matching presentation frontend")
        files += (icd,)
    return files


def validate_wine_frontend(root, record_name="FRONTEND-BUILD.json"):
    """Require the normal Wine build, including the retained hybrid repair."""
    require(record_name in ("FRONTEND-BUILD.json", "BUILD.json"), "unexpected frontend build record")
    record_path = root / record_name
    regular(record_path)
    record = json.loads(record_path.read_text())
    sources = ("nvk/native-macos/bridge.mm", "nvk/native-macos/present_worker.h",
               "plugin/nvmtl_fd_token.h", "plugin/nvmtl_rebind_import.h",
               "nvk/native-macos/present_hud.h", "nvk/wine-macos/build.py")
    require(record.get("schema_version") == 1, "incompatible Wine frontend build record")
    for name in sources:
        require(record.get("source_files", {}).get(name) == digest(REPO / name),
                "Wine frontend source mismatch; rebuild to retain hybrid GPU support: " + name)
    require(any(isinstance(command, list) and "-DNVKBRIDGE_WINE=1" in command
                for command in record.get("commands", [])), "frontend was not built for Wine")
    for key, relative in (("entry", "lib/libnvk_wine.dylib"),
                          ("renderer", "lib/libnvk_renderer.dylib")):
        regular(root / relative)
        expected = digest(root / relative)
        require(record.get(key, {}).get("path") == relative
                and record[key].get("sha256") == expected
                and record.get("files", {}).get(relative) == expected,
                "Wine frontend binary/build mismatch: " + relative)
    return record


def validate_wine_runtime(root, checker):
    require(root.is_dir() and not root.is_symlink(), "missing Wine/NVK provider directory")
    validate_links(root.resolve())
    manifest = root / "nvk-wine-runtime.json"
    regular(manifest)
    fields = json.loads(manifest.read_text(encoding="utf-8"))
    require(fields.get("schema_version") == 1 and fields.get("kind") == "provider"
            and "wine" not in fields, "stage a compact provider, not a bundled Wine installation")
    require(fields.get("platform") == {"os": "macos", "arch": "x86_64", "minimum_macos": "15.5"},
            "incompatible Wine/NVK provider platform")
    require(fields.get("dxvk", {}).get("version") == "3.1.1+nvk-shared-fallback",
            "Wine/NVK requires the repaired DXVK pair")
    require(fields.get("launch_defaults", {}).get("DXVK_CONFIG") == "dxvk.enableDescriptorBuffer = False",
            "Wine/NVK descriptor-set default is missing")
    required = {"lib/libnvk_wine.dylib", "lib/libnvk_renderer.dylib", "bin/nvk-runtime",
                "dxvk/x64/d3d11.dll", "dxvk/x64/dxgi.dll", "dxvk/source-changes.patch", "DXVK-BUILD.json",
                "LICENSES/frontend-PolyForm-Noncommercial-1.0.0.txt", "LICENSES/DXVK-zlib.txt",
                "LICENSES/Mesa-license.rst", "LICENSES/Wine-LGPL-2.1.txt"}
    if fields.get("vkd3d") is not None:
        require(isinstance(fields["vkd3d"], dict) and fields["vkd3d"].get("version") == "3.1",
                "unexpected vkd3d-proton version")
        required.update({"vkd3d/x64/d3d12.dll", "vkd3d/x64/d3d12core.dll", "VKD3D-BUILD.json",
                         "LICENSES/vkd3d-proton-LICENSE.txt", "LICENSES/vkd3d-proton-COPYING.txt",
                         "LICENSES/vkd3d-proton-AUTHORS.txt"})
        if fields["vkd3d"].get("modified"):
            require(fields["vkd3d"].get("fork_version") == "3.1-nvk.2",
                    "unexpected native-NVK vkd3d fork build")
            required.add("vkd3d/source-changes.patch")
    require(fields.get("frontend_build") == "FRONTEND-BUILD.json",
            "Wine provider needs current frontend provenance; rebuild to retain hybrid GPU support")
    required.add("FRONTEND-BUILD.json")
    if fields.get("dlss") is not None:
        require(isinstance(fields["dlss"], dict) and fields.get("vkd3d") is not None
                and fields.get("presentation", {}).get("wine_child_options") == 1,
                "DLSS needs the matched D3D12 component and Wine child option forwarding")
        required.update({"dlss/x64/nvapi64.dll", "dlss/x64/nvngx.dll", "dlss/x64/_nvngx.dll",
                         "DLSS-BUILD.json", "LICENSES/NVIDIA-NGX-driver-license.txt", "LICENSES/DXVK-NVAPI-MIT.txt"})
    files = fields.get("files", {})
    require(isinstance(files, dict) and required <= files.keys(), "incomplete Wine/NVK provider")
    for name, sha in files.items():
        relative = Path(name)
        require(name in required or (len(relative.parts) == 2 and relative.parts[0] == "LICENSES"),
                "unexpected Wine/NVK provider artifact: " + name)
        regular(root / relative)
        require(digest(root / relative) == sha, "Wine/NVK hash mismatch: " + name)
    require(fields.get("entry", {}).get("path") == "lib/libnvk_wine.dylib"
            and fields.get("renderer", {}).get("path") == "lib/libnvk_renderer.dylib",
            "unexpected Wine/NVK library paths")
    validate_wine_frontend(root)
    alias = root / "lib/libMoltenVK.dylib"
    require(alias.is_symlink() and os.readlink(alias) == "libnvk_wine.dylib", "incorrect Wine/NVK provider alias")
    regular(checker)
    for name in ("lib/libnvk_wine.dylib", "lib/libnvk_renderer.dylib", "bin/nvk-runtime"):
        binary = root / name
        run("lipo", binary, "-verify_arch", "x86_64")
        run("codesign", "--verify", "--strict", binary)
        run(checker, SEQUOIA_RUNTIME_MINIMUM, binary)
    run(root / "bin/nvk-runtime", "check", root)
    extra = {"nvk-wine-runtime.json", "README.md", "BUILD.json", "SHA256SUMS"}
    for name in extra:
        regular(root / name)
    # A provider working directory may also contain Wine/cache files. Only the
    # verified allowlist below is copied; never copy the directory wholesale.
    # The installable package itself always requires complete manifest coverage.
    verify_manifest(root, complete=False)
    return tuple(root / name for name in sorted(set(files) | extra))


def stage(args):
    require(sys.platform == "darwin", "run this helper on macOS")
    base = args.base.resolve(strict=True)
    output = args.output.absolute()
    require(not output.exists() and not output.is_symlink(), "output already exists: " + str(output))
    output = output.resolve()
    require(not inside(output, base), "output must be outside the upstream package")
    require(base not in (Path("/"), Path("/System/Volumes/Data")), "use an extracted upstream pkgroot")

    for name in ("install.sh", "uninstall.sh", "VERSION", "Library/GPUBundles/nvmtl-allow.txt"):
        regular(base / name)
    for name in ("Library/GPUBundles/NVIDIAShared.bundle", "Users/Shared/nvfw"):
        require((base / name).is_dir(), "incomplete upstream package: " + name)
    bundle(base / PLUGIN, "NVMTLDriver")
    for name in ("NVRM", "NVRMAGDC", "NVRMFB"):
        bundle(base / "Library/Extensions" / (name + ".kext"), name)
    for path in ACCEL.values():
        bundle(base / path, "NVAccel")
    # Older upstream package builds keep the translator only beside the
    # plugin executable. Accept that layout; staging creates the canonical
    # nvmtl copy and refreshes the plugin copy below.
    canonical_translator = base / TRANSLATOR
    bundled_translator = base / BUNDLED_TRANSLATOR
    require(
        (canonical_translator.is_file() and not canonical_translator.is_symlink())
        or bundled_translator.is_file(),
        "missing packaged translator: " + str(TRANSLATOR),
    )
    regular(base / RUNTIME / "libvulkan_nouveau.dylib")
    validate_links(base)
    verify_manifest(base)

    # Defaults name one exact build, never whichever output directory is newest.
    commit = run("git", "-C", REPO, "rev-parse", "--short", "HEAD").stdout.strip()
    plugin = args.plugin or REPO / ("out/plugin-" + commit + "-release/NVMTLDriver")
    translator = args.translator or REPO / "out/translator/x86_64-apple-darwin/release/libnvmtl_translate.dylib"
    nvrm = getattr(args, "nvrm", None)
    if nvrm:
        bundle(nvrm, "NVRM")
        validate_links(nvrm.resolve())
        run("lipo", nvrm / "Contents/MacOS/NVRM", "-verify_arch", "x86_64")
    framebuffer = getattr(args, "framebuffer", None)
    if framebuffer:
        bundle(framebuffer, "NVRMFB")
        validate_links(framebuffer.resolve())
        run("lipo", framebuffer / "Contents/MacOS/NVRMFB", "-verify_arch", "x86_64")
    agdc = getattr(args, "agdc", None)
    if agdc:
        bundle(agdc, "NVRMAGDC")
        validate_links(agdc.resolve())
        run("lipo", agdc / "Contents/MacOS/NVRMAGDC", "-verify_arch", "x86_64")
    plist = args.framebuffer_plist or (framebuffer / "Contents/Info.plist" if framebuffer else REPO / "kexts/NVRMFB/fb/Info.plist")
    for path in (plugin, translator, plist):
        regular(path)
    with plist.open("rb") as stream:
        require(plistlib.load(stream).get("CFBundleExecutable") == "NVRMFB", "not an NVRMFB Info.plist")
    replacements = [(plugin, PLUGIN / "Contents/MacOS/NVMTLDriver"), (translator, TRANSLATOR)]
    # The 2.9 loader's fallback is an open-source helper beside the executable.
    # Refuse an incomplete package instead of silently keeping the old air-opt.
    air_dis = getattr(args, "air_dis", None)
    needs_air_dis = b"nvmtl-air-dis" in plugin.read_bytes()
    if air_dis is None and needs_air_dis:
        air_dis = base / PLUGIN / "Contents/MacOS/nvmtl-air-dis"
    if air_dis is not None:
        regular(air_dis)
        run("lipo", air_dis, "-verify_arch", "x86_64")
        run("codesign", "--verify", "--strict", air_dis)
        replacements.append((air_dis, PLUGIN / "Contents/MacOS/nvmtl-air-dis"))
    accelerators = {major: getattr(args, "accel_" + major, None) for major in ACCEL}
    for accelerator in accelerators.values():
        if accelerator:
            bundle(accelerator, "NVAccel")
            validate_links(accelerator.resolve())
            run("lipo", accelerator / "Contents/MacOS/NVAccel", "-verify_arch", "x86_64")
    minecraft = getattr(args, "minecraft_runtime", None)
    minecraft_files = validate_minecraft_runtime(minecraft, base / "nullmoth-runtime-check") if minecraft else ()
    wine = getattr(args, "wine_runtime", None)
    wine_files = validate_wine_runtime(wine, base / "nullmoth-runtime-check") if wine else ()
    if not wine and (base / WINE).exists():
        validate_wine_frontend(base / WINE)
    video = getattr(args, "video_runtime", None)
    if video:
        video = video.resolve(strict=True)
    video_files = validate_video_runtime(video) if video else ()
    for source, _ in replacements:
        run("lipo", source, "-verify_arch", "x86_64")

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".nullmoth-stage-", dir=output.parent) as temporary:
        work = Path(temporary) / "pkgroot"
        shutil.copytree(base, work, symlinks=True)
        if nvrm:
            destination = work / NVRM
            shutil.rmtree(destination)
            shutil.copytree(nvrm, destination, symlinks=True)
        for major, accelerator in accelerators.items():
            if not accelerator:
                continue
            destination = work / ACCEL[major]
            shutil.rmtree(destination)
            shutil.copytree(accelerator, destination, symlinks=True)
        if accelerators["26"]:
            active = work / "Library/Extensions/NVAccel.kext"
            if active.exists():
                bundle(active, "NVAccel")
                shutil.rmtree(active)
                shutil.copytree(accelerators["26"], active, symlinks=True)
        if framebuffer:
            destination = work / FRAMEBUFFER
            shutil.rmtree(destination)
            shutil.copytree(framebuffer, destination, symlinks=True)
        if agdc:
            destination = work / AGDC
            shutil.rmtree(destination)
            shutil.copytree(agdc, destination, symlinks=True)
        replace_file(plist, work / FRAMEBUFFER / "Contents/Info.plist")
        # Build templates and reused bundles may carry older versions. Keep
        # per-OS release metadata without replacing the fork's personalities.
        for relative in (*ACCEL.values(), NVRM, FRAMEBUFFER, AGDC, Path("Library/Extensions/NVAccel.kext")):
            if (work / relative).is_dir() and (base / relative).is_dir():
                preserve_kext_release_metadata(work / relative, base / relative)
        for source, relative in replacements:
            replace_file(source, work / relative)
        if air_dis is not None:
            (work / RUNTIME / "air-opt").unlink(missing_ok=True)
        if minecraft:
            destination = work / MINECRAFT
            if destination.is_symlink() or destination.is_file():
                destination.unlink()
            elif destination.exists():
                shutil.rmtree(destination)
            (destination / "lib").mkdir(parents=True)
            for source in minecraft_files:
                replace_file(source, destination / source.relative_to(minecraft))
            replace_file(REPO / "nvk/native-macos/INSTALL.md", work / "MINECRAFT-NVK.md")
            replace_file(REPO / "nvk/native-macos/APPLICATIONS.md", work / "NVK-APPLICATIONS.md")
            validate_minecraft_runtime(destination, base / "nullmoth-runtime-check")
        if wine:
            destination = work / WINE
            if destination.is_symlink() or destination.is_file():
                destination.unlink()
            elif destination.exists():
                shutil.rmtree(destination)
            for source in wine_files:
                target = destination / source.relative_to(wine)
                target.parent.mkdir(parents=True, exist_ok=True)
                replace_file(source, target)
            (destination / "lib/libMoltenVK.dylib").symlink_to("libnvk_wine.dylib")
            replace_file(REPO / "nvk/wine-macos/INSTALL.md", work / "WINE-NVK.md")
            validate_wine_runtime(destination, base / "nullmoth-runtime-check")

        # Copy finalized binaries byte-for-byte, including the translator copy
        # which the plugin prefers over the canonical runtime directory.
        bundled = work / BUNDLED_TRANSLATOR
        if bundled.is_symlink():
            require(bundled.resolve(strict=True) == (work / TRANSLATOR).resolve(),
                    "bundled translator link must point to the canonical nvmtl translator")
        elif bundled.exists():
            replace_file(work / TRANSLATOR, bundled)
        if video:
            stage_video_runtime(work, video, video_files)
        if air_dis is not None:
            run("codesign", "--force", "--sign", "-", "--timestamp=none", work / PLUGIN)
            run("codesign", "--verify", "--strict", work / PLUGIN)
        # install.sh chooses the OS variant. Refresh only replaced variants.
        for major, accelerator in accelerators.items():
            if accelerator:
                write_manifest(work / ACCEL[major].parent)
        replace_file(REPO / "package/sums_complete.sh", work / "sums_complete.sh")
        for name in ("install.sh", "uninstall.sh"):
            (work / name).chmod(0o755)
        validate_links(work)
        write_manifest(work)
        require(not output.exists() and not output.is_symlink(), "output appeared while staging; refusing to replace it")
        work.rename(output)
    print("Package ready: " + str(output))
    print("Updated: NVMTLDriver, translator, " + ("NVRMFB" if framebuffer else "NVRMFB Info.plist")
          + (", NVRM" if nvrm else "")
          + (", NVRMAGDC" if agdc else "")
          + "".join(", macOS " + major + " NVAccel" for major, source in accelerators.items() if source)
          + (", Minecraft native NVK runtime" if minecraft else "")
          + (", Wine native NVK provider and DXVK" if wine else ""))
    if video:
        print("Included: pinned additional NVIDIA video codecs; existing explicit codec settings preserved.")
    print("SHA256SUMS verified. Install on Sequoia or Tahoe using install.sh.")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", type=Path, help="unmodified extracted upstream pkgroot")
    parser.add_argument("output", type=Path, help="new package directory outside the input")
    parser.add_argument("--plugin", type=Path, help="NVMTLDriver executable (default: current-commit release output)")
    parser.add_argument("--translator", type=Path, help="libnvmtl_translate.dylib (default: out/translator Intel release)")
    parser.add_argument("--air-dis", type=Path, help="matched signed nvmtl-air-dis fallback; required by the 2.9 loader unless already in the base")
    parser.add_argument("--nvrm", type=Path, help="optional complete rebuilt NVRM.kext with matching NVIDIA core and firmware")
    parser.add_argument("--framebuffer-plist", type=Path, help="NVRMFB Info.plist (default: this checkout's PR #131 plist)")
    parser.add_argument("--framebuffer", type=Path, help="optional complete rebuilt NVRMFB.kext; uses its plist unless overridden")
    parser.add_argument("--agdc", type=Path, help="optional complete rebuilt NVRMAGDC.kext")
    parser.add_argument("--accel-26", type=Path, help="optional complete rebuilt Tahoe NVAccel.kext")
    parser.add_argument("--accel-15", type=Path, help="optional complete rebuilt Sequoia NVAccel.kext")
    parser.add_argument("--minecraft-runtime", type=Path, help="optional native Minecraft runtime built by nvk/native-macos/build.py")
    parser.add_argument("--wine-runtime", type=Path, help="optional compact Wine/NVK provider built by nvk/wine-macos/provider.py")
    parser.add_argument("--video-runtime", type=Path, help="signed codec runtime built by build/build_nvenc.py")
    try:
        stage(parser.parse_args())
    except (OSError, ValueError, RuntimeError, plistlib.InvalidFileException) as error:
        print("STOP: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

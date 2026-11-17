#!/usr/bin/env python3
"""Extract a compact Wine/CrossOver provider from a validated Wine/NVK bundle.

Wine stays an application dependency. Only the matched Vulkan/DXVK libraries,
optional vkd3d-proton pair, setup helper, license notices and provenance are
included in the driver package.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "build"))
from stage_package import validate_wine_frontend
CRITICAL = (
    "lib/libnvk_wine.dylib", "lib/libnvk_renderer.dylib",
    "bin/nvk-runtime", "dxvk/x64/d3d11.dll", "dxvk/x64/dxgi.dll",
)
VKD3D_VERSION = "3.1"
VKD3D_COMMIT = "4522cc1a8b1347f1ab1b0f7b14a23ed9444d95d1"
VKD3D_ARCHIVE_SHA256 = "b85ea8219b71093a3924913dd7aae6172333dc0c069983ed510816d67f02120b"
VKD3D_NVK_VERSION = "3.1-nvk.2"
VKD3D_NVK_COMMIT = "495c43e17029bd12b70be0b76d09ef8529bf8264"
VKD3D_NVK_ARCHIVE_SHA256 = "b2943e7391a0174ff68a73338577b8f943e2810fbb64fa74adf5bd16153b0f5d"
NVAPI_COMMIT = "f9fdf268e888d17ae9875a4f01908164ff05f740"
NVAPI_ARCHIVE_SHA256 = "60c284223530d643c446c263f1e1a96c6de7b5ff21796219646da734d97a70d6"
NGX_ARCHIVE_SHA256 = "c210d0f81b2cc80be65192a56135ce22ee3cb46ab2eb22eb520ee78e6ad9c13f"
NGX_ARCHIVE_URL = "https://developer.download.nvidia.com/compute/nvidia-driver/redist/nvidia_driver/linux-x86_64/nvidia_driver-linux-x86_64-610.57.04-archive.tar.xz"
DLSS_FILES = {
    "nvapi64.dll": "1bcf9b68a53f35d94e9e7dfdb68244d74c6300f3979906009478c320a780e12e",
    "nvngx.dll": "52a68acc4e477eeff598b5ca01f26215dc5aa487c8f150b02c27ad099c11de0b",
    "_nvngx.dll": "bdad96fdca36050a94a221bb89eab4fc5db6e53ef799494a68957a2e2b28e283",
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate_frontend(frontend, base_renderer, renderer_source=None):
    """Accept a new backend only when it derives from the explicitly selected renderer."""
    record = json.loads((frontend / "BUILD.json").read_text())
    if record["entry"]["path"] != CRITICAL[0] or record["renderer"]["path"] != CRITICAL[1]:
        raise ValueError("Unexpected frontend library layout")
    for key in ("entry", "renderer"):
        binary = frontend / record[key]["path"]
        if binary.is_symlink() or not binary.is_file() or digest(binary) != record[key]["sha256"]:
            raise ValueError("Frontend " + key + " does not match its build record")
        subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
        subprocess.run(["lipo", str(binary), "-verify_arch", "x86_64"], check=True)
    if renderer_source is None:
        if record["renderer"]["sha256"] != digest(base_renderer):
            raise ValueError("Frontend renderer must match the base provider; select --renderer-source to upgrade it")
        return record, None
    renderer_source = renderer_source.resolve(strict=True)
    subprocess.run(["codesign", "--verify", "--strict", str(renderer_source)], check=True)
    subprocess.run(["lipo", str(renderer_source), "-verify_arch", "x86_64"], check=True)
    with tempfile.TemporaryDirectory(prefix="nvk-renderer-identity-") as temporary:
        private = Path(temporary) / "libnvk_renderer.dylib"
        shutil.copyfile(renderer_source, private)
        private.chmod(0o755)
        commands = [
            ["install_name_tool", "-id", "@loader_path/libnvk_renderer.dylib", str(private)],
            ["codesign", "--force", "--sign", "-", "--timestamp=none", str(private)],
        ]
        for command in commands:
            subprocess.run(command, check=True)
        if digest(private) != record["renderer"]["sha256"]:
            raise ValueError("Frontend renderer does not derive from --renderer-source")
    return record, {"source_path": str(renderer_source), "source_sha256": digest(renderer_source),
                    "private_sha256": record["renderer"]["sha256"], "commands": commands}


def add_vkd3d(root, archive, source):
    if digest(archive) == VKD3D_NVK_ARCHIVE_SHA256:
        return add_vkd3d_nvk(root, archive, source)
    if digest(archive) != VKD3D_ARCHIVE_SHA256:
        raise ValueError("Unrecognized vkd3d-proton release archive")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    if revision != VKD3D_COMMIT or subprocess.check_output(
            ["git", "status", "--porcelain", "--untracked-files=no"], cwd=source, text=True).strip():
        raise ValueError("Use the clean vkd3d-proton 3.1 source for license/provenance records")
    paths = []
    for name in ("d3d12.dll", "d3d12core.dll"):
        relative = "vkd3d/x64/" + name
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as package:
            with package.extractfile("vkd3d-proton-3.1/x64/" + name) as stream:
                target.write_bytes(stream.read())
        target.chmod(0o755)
        paths.append(relative)
    for name in ("LICENSE", "COPYING", "AUTHORS"):
        relative = "LICENSES/vkd3d-proton-" + name + ".txt"
        shutil.copyfile(source / name, root / relative)
        paths.append(relative)
    record = {
        "version": VKD3D_VERSION, "source_revision": revision, "modified": False,
        "source_url": "https://github.com/HansKristian-Work/vkd3d-proton/tree/" + revision,
        "archive_url": "https://github.com/HansKristian-Work/vkd3d-proton/releases/download/v3.1/vkd3d-proton-3.1.tar.zst",
        "archive_sha256": VKD3D_ARCHIVE_SHA256,
        "files": {path: digest(root / path) for path in paths},
        "validation": "Packaging verifies identity and architecture; execution must be tested separately",
    }
    (root / "VKD3D-BUILD.json").write_text(json.dumps(record, indent=2) + "\n")
    return {"version": VKD3D_VERSION, "source_revision": revision, "modified": False,
            "archive_sha256": VKD3D_ARCHIVE_SHA256, "build_record": "VKD3D-BUILD.json"}, paths + ["VKD3D-BUILD.json"]


def add_vkd3d_nvk(root, archive, source):
    """Accept only the pinned matched fork build, including its runtime patch."""
    if digest(archive) != VKD3D_NVK_ARCHIVE_SHA256:
        raise ValueError("Unrecognized native-NVK vkd3d archive")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=source, text=True).strip():
        raise ValueError("Use the clean matching vkd3d-nvk source")
    prefix = "vkd3d-nvk-" + VKD3D_NVK_VERSION + "/"
    with tarfile.open(archive) as package:
        def read(name):
            member = package.getmember(prefix + name)
            if not member.isfile() or member.size > 64 * 1024 * 1024:
                raise ValueError("Invalid vkd3d-nvk archive entry: " + name)
            with package.extractfile(member) as stream:
                return stream.read()
        metadata = json.loads(read("BUILD.json"))
        if (metadata.get("version") != VKD3D_NVK_VERSION
                or metadata.get("packaging_revision") != revision
                or metadata.get("runtime_source_revision") != VKD3D_NVK_COMMIT
                or metadata.get("upstream_revision") != VKD3D_COMMIT
                or metadata.get("runtime_modified") is not True):
            raise ValueError("Native-NVK vkd3d source/build identity mismatch")
        files = {}
        entries = {"x64/" + name: "vkd3d/x64/" + name for name in ("d3d12.dll", "d3d12core.dll")}
        entries.update({name: "LICENSES/vkd3d-proton-" + name + ".txt"
                        for name in ("LICENSE", "COPYING", "AUTHORS")})
        entries["source-changes.patch"] = "vkd3d/source-changes.patch"
        for original, relative in entries.items():
            data = read(original)
            if hashlib.sha256(data).hexdigest() != metadata["files"].get(original):
                raise ValueError("Native-NVK vkd3d artifact hash mismatch: " + original)
            if original in ("LICENSE", "COPYING", "AUTHORS") and data != (source / original).read_bytes():
                raise ValueError("Native-NVK vkd3d license/source mismatch")
            if original.endswith(".dll"):
                offset = int.from_bytes(data[60:64], "little")
                if data[:2] != b"MZ" or data[offset:offset+6] != b"PE\0\0\x64\x86":
                    raise ValueError("Expected an x64 vkd3d DLL")
                if metadata["build"]["files"].get(original) != hashlib.sha256(data).hexdigest():
                    raise ValueError("Native-NVK vkd3d compiler output mismatch")
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            target.chmod(0o755 if original.endswith(".dll") else 0o644)
            files[relative] = digest(target)
    patch = subprocess.check_output(["git", "diff", "--binary", VKD3D_COMMIT,
        VKD3D_NVK_COMMIT, "--", "libs", "include", "khronos", "subprojects", "meson.build", "meson_options.txt"], cwd=source)
    if hashlib.sha256(patch).hexdigest() != files["vkd3d/source-changes.patch"]:
        raise ValueError("Native-NVK vkd3d runtime patch differs from source")
    record = {"version": VKD3D_VERSION, "fork_version": VKD3D_NVK_VERSION,
              "source_revision": VKD3D_NVK_COMMIT, "upstream_revision": VKD3D_COMMIT,
              "modified": True, "source_url": "https://github.com/HighDelay/vkd3d-nvk/tree/" + VKD3D_NVK_COMMIT,
              "archive_sha256": VKD3D_NVK_ARCHIVE_SHA256, "files": files, "build": metadata["build"],
              "binding_policy": "Descriptor sets for NVK on Wine Darwin; other drivers/hosts unchanged"}
    (root / "VKD3D-BUILD.json").write_text(json.dumps(record, indent=2) + "\n")
    return {key: record[key] for key in ("version", "fork_version", "source_revision", "upstream_revision", "modified", "archive_sha256", "binding_policy")} | {
        "build_record": "VKD3D-BUILD.json"}, list(files) + ["VKD3D-BUILD.json"]


def add_dlss(root, nvapi_archive, nvapi_source, ngx_archive):
    if digest(nvapi_archive) != NVAPI_ARCHIVE_SHA256 or digest(ngx_archive) != NGX_ARCHIVE_SHA256:
        raise ValueError("Unrecognized DXVK-NVAPI or NVIDIA NGX archive")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=nvapi_source, text=True).strip()
    if revision != NVAPI_COMMIT or subprocess.check_output(
            ["git", "status", "--porcelain", "--untracked-files=no"], cwd=nvapi_source, text=True).strip():
        raise ValueError("Use the clean DXVK-NVAPI 0.9.2 source for license/provenance records")
    paths = []
    license_path = "LICENSES/NVIDIA-NGX-driver-license.txt"
    archive_root = "nvidia_driver-linux-x86_64-610.57.04-archive/"
    wanted = {archive_root + "wine/" + name: "dlss/x64/" + name for name in ("nvngx.dll", "_nvngx.dll")}
    wanted[archive_root + "LICENSE"] = license_path
    # Stream the large NVIDIA archive once; random tar member lookup repeatedly
    # decompresses hundreds of megabytes of unrelated driver files.
    for package, selection in ((nvapi_archive, {"./x64/nvapi64.dll": "dlss/x64/nvapi64.dll"}),
                               (ngx_archive, wanted)):
      pending = dict(selection)
      with tarfile.open(package, "r|*") as archive:
        for info in archive:
            if info.name not in pending:
                continue
            relative = pending.pop(info.name)
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if not info.isfile() or info.size > 20 * 1024 * 1024:
                raise ValueError("Unexpected DLSS component archive entry")
            with archive.extractfile(info) as stream:
                target.write_bytes(stream.read())
            target.chmod(0o755 if target.suffix == ".dll" else 0o644)
            if target.suffix == ".dll" and digest(target) != DLSS_FILES[target.name]:
                raise ValueError("Unexpected DLSS component DLL: " + target.name)
            paths.append(relative)
            if not pending:
                break
      if pending:
          raise ValueError("Missing DLSS component archive entries: " + ", ".join(pending))
    nvapi_license = "LICENSES/DXVK-NVAPI-MIT.txt"
    shutil.copyfile(nvapi_source / "LICENSE", root / nvapi_license)
    paths.append(nvapi_license)
    metadata = {
        "super_resolution": "experimental", "frame_generation": "unqualified; normal hardware checks",
        "game_feature_library": "Use the game's original nvngx_dlss.dll; not redistributed here",
        "nvapi": {"version": "0.9.2", "source_revision": revision,
                  "source_url": "https://github.com/jp7677/dxvk-nvapi/tree/" + revision,
                  "archive_sha256": NVAPI_ARCHIVE_SHA256},
        "ngx": {"version": "610.57.04", "archive_url": NGX_ARCHIVE_URL,
                "archive_sha256": NGX_ARCHIVE_SHA256, "modified": False,
                "license": license_path},
        "files": {path: digest(root / path) for path in paths},
    }
    (root / "DLSS-BUILD.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return {"nvapi_version": "0.9.2", "ngx_version": "610.57.04", "build_record": "DLSS-BUILD.json"}, paths + ["DLSS-BUILD.json"]


def build(base, output, vkd3d_archive=None, vkd3d_source=None, frontend=None,
          nvapi_archive=None, nvapi_source=None, ngx_archive=None, renderer_source=None):
    base, output = base.resolve(strict=True), output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError("Output exists; choose a new directory")
    output = output.resolve()
    if base == output or base in output.parents:
        raise ValueError("Output must be outside the base runtime")
    if bool(vkd3d_archive) != bool(vkd3d_source):
        raise ValueError("Supply both --vkd3d-archive and --vkd3d-source")
    if renderer_source and not frontend:
        raise ValueError("--renderer-source requires --frontend")
    dlss_inputs = [nvapi_archive, nvapi_source, ngx_archive]
    if any(dlss_inputs) and not all(dlss_inputs):
        raise ValueError("Supply --nvapi-archive, --nvapi-source and --ngx-archive together")
    subprocess.run([str(base / "bin/nvk-runtime"), "check", str(base)], check=True)
    source = json.loads((base / "nvk-wine-runtime.json").read_text())
    if source["dxvk"]["version"] != "3.1.1+nvk-shared-fallback":
        raise ValueError("Use the repaired DXVK 3.1.1 pair, not the stock release")
    if frontend:
        validate_wine_frontend(frontend.resolve(strict=True), "BUILD.json")
    else:
        if source.get("frontend_build") != "FRONTEND-BUILD.json":
            raise ValueError("Rebuild the Wine frontend to retain hybrid GPU support")
        validate_wine_frontend(base)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".wine-nvk-", dir=output.parent) as temporary:
        root = Path(temporary) / "provider"
        root.mkdir()
        paths = [*CRITICAL, "dxvk/source-changes.patch", "DXVK-BUILD.json"]
        if source.get("frontend_build"):
            if source["frontend_build"] != "FRONTEND-BUILD.json":
                raise ValueError("Unknown frontend build record")
            paths.append("FRONTEND-BUILD.json")
        if source.get("vkd3d") and not vkd3d_archive:
            paths.extend(path for path in source["files"] if path.startswith("vkd3d/") or path == "VKD3D-BUILD.json")
        if source.get("dlss") and not all(dlss_inputs):
            paths.extend(path for path in source["files"] if path.startswith("dlss/") or path == "DLSS-BUILD.json")
        licenses = base / "LICENSES"
        if not licenses.is_dir() or licenses.is_symlink():
            raise ValueError("The dependency license records are required")
        for name in ("frontend-PolyForm-Noncommercial-1.0.0.txt", "DXVK-zlib.txt", "Mesa-license.rst", "Wine-LGPL-2.1.txt"):
            if not (licenses / name).is_file():
                raise ValueError("Missing license: " + name)
        paths.extend(p.relative_to(base).as_posix() for p in sorted(licenses.glob("*")))
        for relative in paths:
            path = base / relative
            if not path.is_file() or path.is_symlink():
                raise ValueError("Missing regular runtime artifact: " + relative)
            if base not in path.resolve(strict=True).parents:
                raise ValueError("Runtime artifact resolves outside its root: " + relative)
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            # Installed inputs may carry protected macOS metadata. Deliver the
            # verified bytes and executable mode, without copying system xattrs.
            shutil.copyfile(path, target)
            target.chmod(0o755 if path.stat().st_mode & 0o111 else 0o644)
        presentation = source["presentation"]
        frontend_record = source.get("frontend_build")
        renderer_update = None
        if frontend:
            frontend = frontend.resolve(strict=True)
            record, renderer_update = validate_frontend(frontend, root / CRITICAL[1], renderer_source)
            for relative in CRITICAL[:2]:
                shutil.copyfile(frontend / relative, root / relative)
                (root / relative).chmod(0o755)
            shutil.copyfile(frontend / "BUILD.json", root / "FRONTEND-BUILD.json")
            paths.append("FRONTEND-BUILD.json")
            frontend_record = "FRONTEND-BUILD.json"
            presentation = {**presentation, **record["presentation"]}
        vkd3d = source.get("vkd3d")
        if vkd3d_archive:
            vkd3d, additions = add_vkd3d(root, vkd3d_archive.resolve(strict=True), vkd3d_source.resolve(strict=True))
            paths.extend(additions)
        dlss = source.get("dlss")
        if all(dlss_inputs):
            if not vkd3d:
                raise ValueError("DLSS requires the matched vkd3d-proton component")
            if presentation.get("wine_child_options") != 1:
                raise ValueError("DLSS needs a Wine frontend with child-process option forwarding")
            dlss, additions = add_dlss(root, nvapi_archive.resolve(strict=True), nvapi_source.resolve(strict=True), ngx_archive.resolve(strict=True))
            paths.extend(additions)
        if dlss and (not vkd3d or presentation.get("wine_child_options") != 1):
            raise ValueError("DLSS requires D3D12 and Wine child-process option forwarding")
        paths = sorted(set(paths))
        # Provider-only manifests do not require a vendored Wine installation.
        # Build this helper rather than shipping an older bundle-only helper.
        helper = root / "bin/nvk-runtime"
        commands = [
            ["xcrun", "clang++", "-arch", "x86_64", "-mmacosx-version-min=15.0",
             "-std=c++17", "-fobjc-arc", "-O2", "-Wall", "-Wextra", "-Werror",
             str(REPO / "nvk/wine-macos/setup.mm"), "-framework", "Foundation", "-o", str(helper)],
            ["codesign", "--force", "--sign", "-", "--timestamp=none", str(helper)],
        ]
        for command in commands:
            subprocess.run(command, check=True)
        (root / "lib/libMoltenVK.dylib").symlink_to("libnvk_wine.dylib")
        shutil.copyfile(REPO / "nvk/wine-macos/INSTALL.md", root / "README.md")
        files = {p: digest(root / p) for p in paths}
        identity = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:12]
        manifest = {
            "schema_version": 1, "kind": "provider", "runtime_id": "nvk-wine-provider-" + identity,
            "source_runtime_id": source["runtime_id"], "platform": source["platform"],
            "entry": {"path": CRITICAL[0], "sha256": files[CRITICAL[0]]},
            "renderer": {"path": CRITICAL[1], "sha256": files[CRITICAL[1]]},
            "files": files, "dxvk": source["dxvk"], "presentation": presentation,
            "launch_defaults": {"DXVK_CONFIG": "dxvk.enableDescriptorBuffer = False",
                                "DXVK_HUD": "devinfo,fps,frametimes,scale=1.25",
                                "WINEDLLOVERRIDES": "d3d11,dxgi=n"},
            "validation": {"inherited_binaries": ("DXVK/D3D12/NGX unchanged; refreshed Vulkan frontend/backend recorded in FRONTEND-BUILD.json and BUILD.json"
                                                   if renderer_update else
                                                   "Renderer/DXVK/D3D12/NGX unchanged; refreshed Vulkan frontend recorded in FRONTEND-BUILD.json"
                                                   if frontend else "Vulkan and DXVK binaries copied byte-for-byte from source_runtime_id"),
                           "helper": "Rebuilt; run the setup/rollback tests on this output",
                           "external_wine_crossover": "Not qualified by packaging"},
        }
        if vkd3d:
            manifest["vkd3d"] = vkd3d
        if dlss:
            manifest["dlss"] = dlss
        if frontend_record:
            manifest["frontend_build"] = frontend_record
        (root / "nvk-wine-runtime.json").write_text(json.dumps(manifest, indent=2) + "\n")
        record = {
            "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
            "source_runtime_id": source["runtime_id"], "source_manifest_sha256": digest(base / "nvk-wine-runtime.json"),
            "source_build_sha256": digest(base / "BUILD.json"),
            "source_files": {name: digest(REPO / name) for name in
                             ("nvk/wine-macos/setup.mm", "nvk/wine-macos/provider.py", "nvk/wine-macos/INSTALL.md")},
            "commands": [[part.replace(str(REPO), "$SOURCE").replace(str(root), "$PROVIDER")
                          for part in command] for command in commands], "files": files,
        }
        if renderer_update:
            record["renderer_update"] = renderer_update
        (root / "BUILD.json").write_text(json.dumps(record, indent=2) + "\n")
        with (root / "SHA256SUMS").open("w") as stream:
            for path in sorted(root.rglob("*")):
                if path.is_file() and not path.is_symlink() and path != root / "SHA256SUMS":
                    stream.write(digest(path) + "  " + path.relative_to(root).as_posix() + "\n")
        subprocess.run([str(helper), "check", str(root)], check=True)
        if output.exists() or output.is_symlink():
            raise ValueError("Output appeared while building; refusing to replace it")
        root.rename(output)
    return {"root": str(output), "runtime_id": manifest["runtime_id"], "bundled_wine": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True, help="validated full Wine/NVK runtime with repaired DXVK")
    parser.add_argument("--output", type=Path, required=True, help="new compact provider directory")
    parser.add_argument("--vkd3d-archive", type=Path, help="optional pinned upstream or native-NVK vkd3d release archive")
    parser.add_argument("--vkd3d-source", type=Path, help="matching clean source for license/provenance records")
    parser.add_argument("--frontend", type=Path, help="optional Wine frontend build using the same renderer")
    parser.add_argument("--renderer-source", type=Path, help="explicit verified upstream renderer for a frontend/backend upgrade")
    parser.add_argument("--nvapi-archive", type=Path, help="optional pinned DXVK-NVAPI 0.9.2 archive")
    parser.add_argument("--nvapi-source", type=Path, help="matching clean DXVK-NVAPI source for its MIT license")
    parser.add_argument("--ngx-archive", type=Path, help="pinned NVIDIA 610.57.04 archive; only the unmodified NGX pair and license are copied")
    args = parser.parse_args()
    try:
        print(json.dumps(build(args.base, args.output, args.vkd3d_archive, args.vkd3d_source, args.frontend,
                               args.nvapi_archive, args.nvapi_source, args.ngx_archive, args.renderer_source), indent=2))
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print("STOP: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

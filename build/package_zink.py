#!/usr/bin/env python3
"""Build one complete driver archive with Zink and native CGL adapters."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tarfile
import tempfile

import stage_package as base_tools
from record_zink_runtime import COMPONENTS
from prepare_zink import MESA_COMMIT
from package_layout import organize
from package_webkit import stage as stage_webkit

ROOT = Path(__file__).resolve().parents[1]
BASE_RUNTIME = Path('Library/GPUBundles/nvmtl')
RUNTIME = BASE_RUNTIME / 'zink'
ZINK_FILES = ('libnvk_zink_vulkan.dylib', 'libnvk_renderer.dylib', 'NATIVE-BUILD.json', 'libnvk_zink.dylib', 'libnvk_cgl.dylib', 'nvk_zink.h', 'nvk_cgl.h',
              'zink_probe', 'zink_sync_probe', 'zink_native_present_probe', 'cgl_renderer_probe', 'cgl_zink_probe', 'cgl_zink_legacy_probe', 'cgl_iosurface_probe', 'cgl_iosurface_interop_probe', 'obs_zink_policy_probe', 'cgl_zink_window_probe', 'cgl_zink_front_probe', 'cgl-os-build.txt', 'cgl-dispatch-map.json', 'cgl-api-requirements.json', 'ZINK-BUILD.json')


def run(*args):
    return subprocess.check_output([str(a) for a in args], text=True, stderr=subprocess.STDOUT)


def copy_bundle(source, dest):
    base_tools.validate_links(source.resolve())
    if dest.exists():
        shutil.rmtree(dest)
    # Bundled shader seeds are signed resources, even when their directory is
    # named "sass". Only strip transient caches outside bundles/frameworks.
    def ignored(directory, names):
        parts = (source.name, *Path(directory).relative_to(source).parts)
        sealed = any(part.endswith(('.bundle', '.framework')) for part in parts)
        return [name for name in names if name == '.DS_Store' or name.startswith('._')
                or (not sealed and name in ('sass', 'aircache', 'spvcache'))]

    # Copy framework directory symlinks as links, and keep installation flags
    # out of user-owned output.
    shutil.copytree(source, dest, symlinks=True, copy_function=copy_file,
                    ignore=ignored)



def copy_file(source, destination):
    source, destination = Path(source), Path(destination)
    if destination.exists() or destination.is_symlink(): destination.unlink()
    shutil.copyfile(source, destination)
    destination.chmod(source.stat().st_mode & 0o777)


def stage_optional_audio(payload):
    """Keep the verified upstream audio payload outside default extensions."""
    active = payload / 'Library/Extensions/NMXAudio.kext'
    optional = payload / 'Optional/NMXAudio.kext'
    if not active.exists() and not optional.exists():
        return None
    if active.exists() and optional.exists():
        raise ValueError('duplicate audio payloads; select one verified NMXAudio build')
    source = active if active.exists() else optional
    base_tools.bundle(source, 'NMXAudio')
    base_tools.validate_links(source.resolve())
    info = plistlib.loads((source/'Contents/Info.plist').read_bytes())
    if info.get('CFBundleIdentifier') != 'com.nullmoth.NMXAudio':
        raise ValueError('unexpected optional audio bundle identity')
    run('lipo', source/'Contents/MacOS/NMXAudio', '-verify_arch', 'x86_64')
    binary_hash = base_tools.digest(source/'Contents/MacOS/NMXAudio')
    if active.exists():
        optional.parent.mkdir(exist_ok=True)
        active.rename(optional)
    shutil.copy2(ROOT/'docs/OPTIONAL-AUDIO.md', optional.parent/'Audio.md')
    return {'bundle_version': info.get('CFBundleVersion'), 'binary_sha256': binary_hash,
            'default_install': False, 'minimum_macos_major': 26,
            'provenance': 'unchanged audio payload from the checksum-verified base package'}


def stage(base, zink, output, components=None, native_gld=False, records=None, webkit_fixup_build=None):
    if components is not None:
        raise ValueError("Stage matched driver components with build/stage_package.py first; arbitrary Library overlays are unsupported")
    base = base.resolve(strict=True)
    zink = zink.resolve(strict=True)
    output = output.absolute()
    if output.exists() or output.is_symlink() or base_tools.inside(output.resolve(), base):
        raise ValueError('Output must be new and outside the base package')
    records = Path(records).absolute() if records is not None else output.with_name(output.name + '-build-records')
    if records.exists() or records.is_symlink() or base_tools.inside(records.resolve(), output.resolve()) or base_tools.inside(records.resolve(), base):
        raise ValueError('Build records must be new and outside the input/output package')
    base_tools.verify_manifest(base)
    base_tools.validate_links(base)
    for name in ('install.sh', 'uninstall.sh', 'nullmoth-runtime-check.sh', 'nullmoth-runtime-check', 'VERSION'):
        base_tools.regular(base / name)
    for name in ('NVRM', 'NVRMFB', 'NVRMAGDC', 'NVAccel'):
        base_tools.bundle(base / 'Library/Extensions' / (name + '.kext'), name)
    for major in ('15', '26'):
        base_tools.bundle(base / 'Library/NullMoth/kexts' / major / 'NVAccel.kext', 'NVAccel')
    for name in ('NVMTLDriver', 'NVIDIAShared'):
        base_tools.bundle(base / 'Library/GPUBundles' / (name + '.bundle'), name)
    for name in ('libvulkan_nouveau.dylib', 'libvulkan.1.4.362.dylib', 'nvk_icd.json'):
        base_tools.regular(base / BASE_RUNTIME / name)
    base_tools.validate_wine_frontend(base / BASE_RUNTIME / 'wine')
    for name in ZINK_FILES:
        base_tools.regular(zink / name)
    if native_gld:
        base_tools.bundle(zink / 'NVKZinkGLDriver.bundle', 'NVKZinkGLDriver')
        info = plistlib.loads((zink / 'NVKZinkGLDriver.bundle/Contents/Info.plist').read_bytes())
        build = (zink / 'cgl-os-build.txt').read_text().strip()
        if info.get('NVKZinkDiagnosticOnly') or not info.get('NVKZinkExperimentalGLD'):
            raise ValueError('A diagnostic GLD cannot be used for native installation')
        if not build or info.get('NVKZinkBuildOS') != build:
            raise ValueError('GLD and CGL build provenance differ')
        base_tools.regular(zink / 'gld_bundle_probe')
    provenance = json.loads((zink/'NATIVE-BUILD.json').read_text())
    if provenance.get('renderer_original_sha256') != base_tools.digest(base/BASE_RUNTIME/'libvulkan_nouveau.dylib'):
        raise ValueError('Private Zink renderer does not match the verified driver renderer')
    if provenance.get('frontend_source_sha256') != base_tools.digest(ROOT/'nvk/native-macos/bridge.mm'):
        raise ValueError('Private Zink Vulkan frontend was not built from this source')
    for name in ('plugin/nvmtl_fd_token.h','plugin/nvmtl_rebind_import.h','nvk/native-macos/present_worker.h'):
        if provenance.get('frontend_headers',{}).get(name) != base_tools.digest(ROOT/name):
            raise ValueError('Private Zink Vulkan frontend header changed since building: '+name)
    for name in ('libnvk_renderer.dylib','libnvk_zink_vulkan.dylib'):
        if provenance.get('files',{}).get(name) != base_tools.digest(zink/name):
            raise ValueError('Private Zink frontend/renderer hash mismatch: '+name)
    components_record = json.loads((zink/'ZINK-BUILD.json').read_text())
    mesa = components_record.get('components',{}).get('zink',{}).get('mesa_source',{})
    if mesa.get('commit') != MESA_COMMIT or mesa.get('overlay_sha256') != base_tools.digest(ROOT/'zink/mesa-zink-nvk.patch'):
        raise ValueError('Zink requires the pinned Mesa source and matching native overlay')
    for component in ['zink','cgl'] + (['gld'] if native_gld else []):
        item = components_record.get('components',{}).get(component,{})
        files,sources = COMPONENTS[component]
        for name in files:
            if item.get('files',{}).get(name) != base_tools.digest(zink/name):
                raise ValueError('Zink component hash mismatch: '+name)
        for name in sources:
            if item.get('source_files',{}).get(name) != base_tools.digest(ROOT/name):
                raise ValueError('Zink component source changed since building: '+name)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.zink-package-', dir=output.parent) as temp:
        payload = Path(temp) / 'pkgroot'
        copy_bundle(base, payload)
        shutil.copy2(ROOT / 'docs/GPU-UTILIZATION.md', payload / 'GPU-UTILIZATION.md')
        audio_identity = stage_optional_audio(payload)
        webkit_identity = stage_webkit(payload, webkit_fixup_build, run)
        # Always bundle the real framebuffer, outside Library/Extensions. Only
        # an explicit installer choice copies it into the system repositories.
        panel_build = Path(temp) / 'intel-panel'
        run('/bin/bash', ROOT/'build/build_intel_panel.sh', panel_build)
        panel = payload/'Optional/NMIntelFB.kext'
        copy_bundle(panel_build/'NMIntelFB.kext', panel)
        run('codesign', '--verify', '--strict', panel)
        matched_accelerators = {}
        for major in ('15', '26'):
            executable = payload/'Library/NullMoth/kexts'/major/'NVAccel.kext/Contents/MacOS/NVAccel'
            if b'nmintelfb-frame-abi:2\0' not in executable.read_bytes():
                raise ValueError('Intel panel requires the matched frame-ABI-2 NVAccel for macOS '+major)
            matched_accelerators[major] = base_tools.digest(executable)
        if b'nmintelfb-frame-abi:2\0' not in (payload/'Library/Extensions/NVAccel.kext/Contents/MacOS/NVAccel').read_bytes():
            raise ValueError('Intel panel requires the matched selected NVAccel')
        panel_identity = {
            'source_commit': run('git', '-C', ROOT, 'rev-parse', 'HEAD').strip(),
            'source_sha256': {str(p.relative_to(ROOT)): base_tools.digest(p) for p in
                [ROOT/'build/build_intel_panel.sh', ROOT/'tools/intel_panel_control.sh',
                 ROOT/'kexts/NVRM/nvrm_gpuva_abi.h', ROOT/'kexts/NVRM/agdc/AppleGraphicsDeviceControl_re.h',
                 ROOT/'kexts/NVRM/agdc/nvrm-agdc-k5.h', *sorted((ROOT/'kexts/NMIntelFB').glob('*'))]
                if p.is_file()},
            'files': {str(p.relative_to(panel)): base_tools.digest(p) for p in panel.rglob('*') if p.is_file()},
            'installation': 'optional installer prompt; default no; retain an existing opt-in',
            'nvidia_intel_panel_acceleration_enabled_by_default': False,
            'selected_panel_auto_activation': 'ordinary copies; nullmoth-intelfb-auto=0 disables it',
            'experimental_ram_scanout_enabled_by_default': False,
            'frame_bridge_abi': 2,
            'matched_accelerators_sha256': matched_accelerators,
            'nvkms_headers': json.loads((panel_build/'NVKMS-HEADERS.json').read_text()),
        }
        shutil.copy2(ROOT/'docs/INTEL-PANEL.md', payload/'Optional/README.md')
        shutil.copy2(ROOT/'tools/intel_panel_control.sh', payload/'Optional/Intel-panel.command')
        (payload/'Optional/Intel-panel.command').chmod(0o755)
        replacements = {}
        # Verify the firmware version from the actual NVRM image, not stale text
        # in release manifests. Every referenced mandatory firmware must exist.
        nvrm_strings = run('strings', payload / 'Library/Extensions/NVRM.kext/Contents/MacOS/NVRM')
        firmware_paths = set(re.findall(r'/Library/NullMoth/nvfw/(nvidia/[^\s]+/(?:gsp|ucodes)_[^\s]+\.bin)', nvrm_strings))
        firmware_paths = {p for p in firmware_paths if '/gsp_log_' not in p}
        if not firmware_paths:
            raise ValueError('Cannot establish NVRM firmware requirements')
        for path in firmware_paths:
            base_tools.regular(payload / 'Users/Shared/nvfw' / path)
        runtime = payload / RUNTIME
        runtime.mkdir(exist_ok=True)
        for name in ZINK_FILES:
            shutil.copy2(zink / name, runtime / name)
        if native_gld:
            bundle = payload / 'Library/GPUBundles/NVKZinkGLDriver.bundle'
            copy_bundle(zink / bundle.name, bundle)
            binary = bundle / 'Contents/MacOS/NVKZinkGLDriver'
            run('lipo', binary, '-verify_arch', 'x86_64')
            commands = run('otool', '-l', binary)
            for path in re.findall(r'cmd LC_RPATH\n.*?path (.+?) \(offset', commands, re.S):
                run('install_name_tool', '-delete_rpath', path, binary)
            run('install_name_tool', '-add_rpath', '@loader_path/../../../nvmtl/zink', binary)
            run('codesign', '--force', '--sign', '-', bundle)
            run('codesign', '--verify', '--strict', bundle)
            shutil.copy2(zink / 'gld_bundle_probe', runtime / 'gld_bundle_probe')
            for relative in ('Library/Extensions/NVAccel.kext', 'Library/NullMoth/kexts/15/NVAccel.kext',
                             'Library/NullMoth/kexts/26/NVAccel.kext'):
                plist = payload / relative / 'Contents/Info.plist'
                data = plistlib.loads(plist.read_bytes())
                data.setdefault('IOKitPersonalities', {}).setdefault('NVAccel', {})['IOGLBundleName'] = 'NVKZinkGLDriver'
                plist.write_bytes(plistlib.dumps(data))
                run('codesign', '--force', '--sign', '-', payload / relative)
                run('codesign', '--verify', '--strict', payload / relative)
        # Signatures include the selected GLD plist. Record the final paired
        # accelerators after that transformation, not their pre-signing bytes.
        panel_identity['matched_accelerators_sha256'] = {
            major: base_tools.digest(payload/'Library/NullMoth/kexts'/major/'NVAccel.kext/Contents/MacOS/NVAccel')
            for major in ('15','26')}
        panel_identity['selected_accelerator_sha256'] = base_tools.digest(
            payload/'Library/Extensions/NVAccel.kext/Contents/MacOS/NVAccel')
        (payload/'Optional/INTEL-PANEL-BUILD.json').write_text(json.dumps(panel_identity, indent=2)+'\n')
        # Only the test utility consumes this ICD path. Production Zink opens
        # the private frontend directly, with no process-global loader override.
        icd = {'file_format_version':'1.0.0','ICD':{'library_path':'./libnvk_renderer.dylib','api_version':'1.4.363'}}
        (runtime/'nvk_icd.json').write_text(json.dumps(icd,indent=2)+'\n')
        for name in ('libnvk_zink.dylib', 'libnvk_cgl.dylib', 'libnvk_zink_vulkan.dylib', 'libnvk_renderer.dylib'):
            binary = runtime / name
            run('lipo', binary, '-verify_arch', 'x86_64')
            # Keep dlopen resolution within the packaged runtime. A successful
            # probe must not accidentally load the installed Vulkan loader.
            commands = run('otool', '-l', binary)
            paths = re.findall(r'cmd LC_RPATH\n.*?path (.+?) \(offset', commands, re.S)
            for path in paths:
                run('install_name_tool', '-delete_rpath', path, binary)
            run('install_name_tool', '-add_rpath', '@loader_path', binary)
            run('codesign', '--force', '--sign', '-', binary)
            run('codesign', '--verify', '--strict', binary)
        run('codesign', '--verify', '--strict', payload / 'Library/GPUBundles/NVMTLDriver.bundle')
        # Upstream NVIDIAShared seals ELF/scripts as nested code with signatures
        # in xattrs, which a portable tar does not retain. Check its executable
        # and Mach-O children; the complete package manifest covers ELF/scripts.
        shared = payload / 'Library/GPUBundles/NVIDIAShared.bundle'
        run('codesign', '--verify', '--strict', '--ignore-resources', shared)
        for name in ('libLLVM.dylib', 'libzstd.1.dylib', 'nvvmdrive'):
            run('codesign', '--verify', '--strict', shared / 'Contents/PlugIns' / name)
        shutil.copy2(ROOT / 'docs/ZINK-OPENGL.md', runtime / 'ZINK-OPENGL.md')
        shutil.copy2(ROOT / 'docs/ZINK-SEQUOIA-ABI.json', runtime / 'CGL-ABI-EVIDENCE.json')
        shutil.copy2(ROOT / 'docs/ZINK-SEQUOIA-TESTS.json', runtime / 'CGL-SEQUOIA-TESTS.json')
        for name in ('ZINK-PR2-REVIEW.md','COMBINED-INSTALL.md',
                     'ZINK-BUFFER-MAPPING.md','ZINK-BUFFER-MAPPING-TESTS.json',
                     'ZINK-PERSISTENT-MAPPING-TESTS.json','ZINK-MUTABLE-MAPPING-TESTS.json'):
            shutil.copy2(ROOT / 'docs' / name,runtime / name)
        source_dir = runtime / 'zink-source'
        if source_dir.exists(): shutil.rmtree(source_dir)
        source_dir.mkdir()
        for name in ('nvk_zink.c', 'nvk_zink.h', 'nvk_zink_private.h', 'nvk_iosurface.c', 'nvk_cgl.c', 'nvk_cgl.h', 'appkit.m', 'nvk_cgl_gld.h', 'nvk_present.c', 'nvk_present.h', 'native_drawable.m', 'native_drawable.h',
                     'gld_bridge.c', 'meson.build', 'mesa-zink-nvk.patch'):
            shutil.copy2(ROOT / 'zink' / name, source_dir / name)
        for name in ('build_zink.sh', 'prepare_zink.py', 'opengl_api.py', 'build_cgl_zink.sh', 'generate_cgl_dispatch.py',
                     'build_gld_bridge.sh', 'generate_gld_forward.py', 'build_zink_vulkan.py', 'record_zink_runtime.py'):
            shutil.copy2(ROOT / 'build' / name, source_dir / name)
        for name in ('Install.command', 'Uninstall.command', 'Verify.command'):
            shutil.copy2(ROOT / 'package' / name, payload / name)
            (payload / name).chmod(0o755)
        # Retire the one-off Sequoia qualification launcher from older bases.
        (payload/'Test-Zink.command').unlink(missing_ok=True)
        test_dir = runtime / 'zink-tests'
        if test_dir.exists(): shutil.rmtree(test_dir)
        # Use this checkout's installer fixes, preserving its private payload,
        # preflight, backup, rollback and OS accelerator-selection behavior.
        for name in ('install.sh', 'uninstall.sh', 'sums_complete.sh'):
            shutil.copy2(ROOT / 'package' / name, payload / name)
            (payload / name).chmod(0o755)
        native_dlss = None
        if (payload / 'dlss_fetch.sh').is_file():
            run('/bin/bash', ROOT / 'build/build_dlss_cache.sh', payload)
            shutil.copy2(ROOT / 'docs/METALFX-DLSS.md', payload / 'METALFX-DLSS.md')
            tool_files = ['dlss_fetch.sh', 'dlss/dlss_cache', 'dlss/nm_unzstd', 'dlss/assertfail_trap.ptx',
                          'dlss/nm_unzstd.LICENSE', 'dlss/BLAKE3-NOTICE.txt', 'dlss/BLAKE3-LICENSE-CC0.txt']
            for tool in ('dlss_cache', 'nm_unzstd'):
                run('lipo', payload / 'dlss' / tool, '-verify_arch', 'x86_64')
                run('codesign', '--verify', '--strict', payload / 'dlss' / tool)
            native_dlss = {'installation': 'optional, NULLMOTH_DLSS=1; pinned NVIDIA downloads on the target Mac',
                           'files': {name: base_tools.digest(payload / name) for name in tool_files},
                           'source_sha256': {p.relative_to(ROOT).as_posix(): base_tools.digest(p)
                               for p in [ROOT / 'build/build_dlss_cache.sh', ROOT / 'package/dlss_fetch.sh',
                                         *sorted((ROOT / 'dlss').rglob('*'))] if p.is_file()}}
        licenses = payload / 'licenses'
        licenses.mkdir(exist_ok=True)
        for source, name in ((ROOT / 'LICENSE', 'NullMoth-LICENSE-PolyForm-Noncommercial-1.0.0.txt'),
                             (ROOT / 'NOTICE', 'NOTICE'), (ROOT / 'translator/LICENSE', 'translator-LICENSE'),
                             (ROOT / 'translator/COPYING', 'translator-COPYING')):
            shutil.copy2(source, licenses / name)
        for source in (ROOT / 'licenses').iterdir():
            if source.is_file(): shutil.copy2(source, licenses / source.name)
        (licenses / 'Zink-MIT.txt').write_text(
            'Copyright (c) 2026 Dino Zink contributors\n\n' +
            'Permission is hereby granted, free of charge, to any person obtaining a copy of this software '
            'and associated documentation files (the "Software"), to deal in the Software without restriction, '
            'including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, '
            'and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, '
            'subject to the following conditions:\n\n'
            'The above copyright notice and this permission notice shall be included in all copies or substantial '
            'portions of the Software.\n\nTHE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS '
            'OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR '
            'PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY '
            'CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, '
            'OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n')
        version = (payload / 'VERSION').read_text().strip()
        identity = {'schema_version': 1, 'repository': 'https://github.com/HighDelay/nvidia-macos-driver-nvk',
                    'package_version': version, 'release_tag': 'v' + version + '-nvk', 'runtime_minimum_macos': '15.5',
                    'base': str(base), 'base_manifest_sha256': base_tools.digest(base / 'SHA256SUMS'),
                    'source_commit': run('git', '-C', ROOT, 'rev-parse', 'HEAD').strip(),
                    'source_dirty': bool(run('git', '-C', ROOT, 'status', '--porcelain', '--untracked-files=no').strip()),
                    'component_replacements': replacements, 'firmware': sorted(firmware_paths),
                    'optional_intel_panel': panel_identity,
                    'optional_audio': audio_identity,
                    'optional_webkit_fixup': webkit_identity,
                    'native_dlss_support': native_dlss,
                    'zink_sha256': base_tools.digest(runtime / 'libnvk_zink.dylib'),
                    'cgl_sha256': base_tools.digest(runtime / 'libnvk_cgl.dylib'),
                    'gld_sha256': (base_tools.digest(payload / 'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/MacOS/NVKZinkGLDriver')
                                   if native_gld else None),
                    'zink_source_sha256': {p.name: base_tools.digest(p) for p in sorted(source_dir.iterdir()) if p.is_file()},
                    'native_cgl': ('automatic GLD proxy for Intel Sequoia 15.5+ and Tahoe; Minecraft 26.3 gameplay confirmed on Sequoia 15.8.1 / RTX 2070 SUPER / 256 MiB BAR1'
                                   if native_gld else 'explicit application attachment; GLD bundle is not included'),
                    'native_cgl_macos_majors': ['15','26'] if native_gld else [],
                    'native_cgl_abi_evidence_sha256': base_tools.digest(ROOT/'docs/ZINK-SEQUOIA-ABI.json'),
                    'private_vulkan_build': provenance,
                    'component_builds': components_record,
                    'native_cgl_os_build': (runtime / 'cgl-os-build.txt').read_text().strip()}
        # Keep detailed source/validation records outside the installable tree.
        organized = Path(temp) / 'build-records'
        organize(payload, organized, identity)
        for major in ('15', '26'):
            plist = plistlib.loads((payload / 'Library/NullMoth/kexts' / major / 'NVAccel.kext/Contents/Info.plist').read_bytes())
            if plist.get('IOKitPersonalities', {}).get('NVAccel', {}).get('IOGLBundleName') == 'NVKZinkGLDriver' and not native_gld:
                raise ValueError('The incomplete diagnostic GLD must not be activated')
            base_tools.write_manifest(payload / 'Library/NullMoth/kexts' / major)
        base_tools.validate_links(payload)
        base_tools.write_manifest(payload)
        run('/bin/bash', payload / 'nullmoth-runtime-check.sh', payload, run('sw_vers', '-productVersion').strip())
        records.parent.mkdir(parents=True, exist_ok=True)
        os.rename(organized, records)
        os.rename(payload, output)
    return output


def archive_identity(info):
    info.uid = info.gid = 0
    info.uname = info.gname = 'root'
    info.pax_headers = {}
    return info


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--base', required=True, type=Path)
    ap.add_argument('--zink', required=True, type=Path)
    ap.add_argument('--components', type=Path, help='Deprecated; use build/stage_package.py to prepare matched components first')
    ap.add_argument('--stage', required=True, type=Path)
    ap.add_argument('--archive', required=True, type=Path)
    ap.add_argument('--build-records', type=Path, help='New external build-record directory (default: <stage>-build-records)')
    ap.add_argument('--webkit-fixup-build', required=True, type=Path, help='Matched optional NVWebKitFixup build directory, including BUILD.json')
    ap.add_argument('--native-gld', action='store_true', default=True, help='Select native Zink OpenGL for applications (default; Intel Sequoia/Tahoe accelerators)')
    ap.add_argument('--explicit-cgl-only', action='store_false', dest='native_gld', help='Developer-only package: do not select a system GLD')
    args = ap.parse_args()
    if args.archive.exists(): raise SystemExit('Archive already exists')
    payload = stage(args.base, args.zink, args.stage, args.components, args.native_gld, args.build_records, args.webkit_fixup_build)
    args.archive.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.archive.with_name('.' + args.archive.name + '.tmp')
    try:
        with tarfile.open(temporary, 'w:gz', compresslevel=6) as archive:
            archive.add(payload, arcname='pkgroot', filter=archive_identity)
        os.rename(temporary, args.archive)
    finally:
        temporary.unlink(missing_ok=True)
    checksum = base_tools.digest(args.archive)
    args.archive.with_name(args.archive.name + '.sha256').write_text(checksum + '  ' + args.archive.name + '\n')
    print(args.archive)
    print(checksum)


if __name__ == '__main__':
    main()

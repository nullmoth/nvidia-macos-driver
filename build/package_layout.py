#!/usr/bin/env python3
"""Keep installable files in pkgroot and development records beside the build."""
import json
from pathlib import Path
import re
import shutil
from urllib.parse import quote

import stage_package as integrity

ROOT = Path(__file__).resolve().parents[1]
GITHUB_FILES = 'https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/'
ZINK = Path('Library/GPUBundles/nvmtl/zink')
GUIDES = {
    Path('MINECRAFT-NVK.md'): 'Minecraft.md',
    Path('NVK-APPLICATIONS.md'): 'Vulkan-Applications.md',
    Path('WINE-NVK.md'): 'Wine-CrossOver.md',
    Path('VIDEO-CODECS.md'): 'Video-codecs.md',
    Path('METALFX-DLSS.md'): 'MetalFX-DLSS.md',
    Path('GPU-UTILIZATION.md'): 'GPU-Utilization.md',
    ZINK / 'ZINK-OPENGL.md': 'Zink-OpenGL.md',
}
GUIDE_SOURCES = {
    Path('MINECRAFT-NVK.md'): Path('nvk/native-macos/INSTALL.md'),
    Path('NVK-APPLICATIONS.md'): Path('nvk/native-macos/APPLICATIONS.md'),
    Path('WINE-NVK.md'): Path('nvk/wine-macos/INSTALL.md'),
    Path('VIDEO-CODECS.md'): Path('video/INSTALL.md'),
    Path('METALFX-DLSS.md'): Path('docs/METALFX-DLSS.md'),
    Path('GPU-UTILIZATION.md'): Path('docs/GPU-UTILIZATION.md'),
    ZINK / 'ZINK-OPENGL.md': Path('docs/ZINK-OPENGL.md'),
}
ZINK_REPORTS = (
    'CGL-ABI-EVIDENCE.json', 'CGL-SEQUOIA-TESTS.json',
    'ZINK-BUFFER-MAPPING-TESTS.json', 'ZINK-PERSISTENT-MAPPING-TESTS.json',
    'ZINK-MUTABLE-MAPPING-TESTS.json', 'ZINK-PR2-REVIEW.md',
    'COMBINED-INSTALL.md', 'ZINK-BUFFER-MAPPING.md',
)
IDENTITY_FIELDS = (
    'repository', 'source_commit', 'source_dirty', 'binary_source_commit',
    'package_version', 'release_tag', 'upstream_commit', 'runtime_minimum_macos',
    'native_cgl_macos_majors',
)


def organize(payload, records, identity=None):
    """Finalize a private staging tree before writing its final checksums.

    records must be a new directory outside payload. Runtime manifests and all
    bundle resources stay in place; only root metadata and named reports move.
    """
    payload = Path(payload).resolve(strict=True)
    records = Path(records).absolute()
    integrity.require(not records.exists() and not records.is_symlink(), 'build records already exist')
    integrity.require(not integrity.inside(records.resolve(), payload), 'build records must be outside pkgroot')
    integrity.validate_links(payload)
    current = payload / 'PACKAGE-IDENTITY.json'
    if identity is None:
        integrity.regular(current)
    previous = json.loads(current.read_text()) if current.exists() else None
    identity = dict(previous if identity is None else identity)
    records.mkdir(parents=True)

    def move(relative, destination=None):
        source = payload / relative
        if not source.exists() and not source.is_symlink():
            return
        integrity.require(not source.is_symlink(), 'metadata must not be a symlink: ' + str(relative))
        target = (records / relative) if destination is None else destination
        integrity.require(not target.exists() and not target.is_symlink(), 'metadata destination already exists: ' + str(target))
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(source), str(target))

    if current.exists():
        move(Path('PACKAGE-IDENTITY.json'), records / 'inherited/PACKAGE-IDENTITY.json')
    for relative, name in GUIDES.items():
        guide = payload / 'Guides' / name
        # A verified base may already use this layout. Keep its old guide with
        # the build records before installing the component's refreshed guide.
        if (payload / relative).exists() and (guide.exists() or guide.is_symlink()):
            integrity.regular(guide)
            move(guide.relative_to(payload), records / 'inherited/Guides' / name)
        move(relative, guide)
        if guide.is_file():
            # Source-tree links to reviews/build tools remain useful after the
            # guide moves into an archive that does not contain the repository.
            def link(match):
                target, separator, anchor = match[2].partition('#')
                if not target or ':' in target or target.startswith('/'):
                    return match[0]
                repo_file = (ROOT / GUIDE_SOURCES[relative].parent / target).resolve()
                if not repo_file.is_file() or not integrity.inside(repo_file, ROOT):
                    return match[0]
                url = GITHUB_FILES + quote(repo_file.relative_to(ROOT).as_posix())
                return match[1] + url + (separator + anchor if separator else '') + ')'
            guide.write_text(re.sub(r'(\[[^\]]*\]\()([^\s)]+)\)', link, guide.read_text()))
    for entry in sorted(payload.iterdir()):
        if entry.is_file() and entry.suffix.lower() in ('.json', '.md') and entry.name != 'READ-ME.md':
            move(Path(entry.name))
    move(Path('MANIFEST.txt'))
    move(Path('provenance'))
    move(Path('Optional/INTEL-PANEL-BUILD.json'))
    for name in ZINK_REPORTS:
        move(ZINK / name)

    # Bind the small installable identity to the complete external build record.
    detail = records / 'PACKAGE-IDENTITY.json'
    detail.write_text(json.dumps(identity, indent=2) + '\n')
    compact = {key: identity[key] for key in IDENTITY_FIELDS if key in identity}
    compact.update(schema_version=1, package_version=identity.get('package_version', (payload / 'VERSION').read_text().strip()),
                   runtime_minimum_macos=identity.get('runtime_minimum_macos', '15.5'),
                   detailed_build_record_sha256=integrity.digest(detail))
    panel = payload / 'Optional/NMIntelFB.kext/Contents/MacOS/NMIntelFB'
    compact['optional_intel_panel'] = {'bundled': panel.is_file(), 'default': 'No', 'existing_opt_ins_retained': True}
    audio = payload / 'Optional/NMXAudio.kext/Contents/MacOS/NMXAudio'
    compact['optional_audio'] = {'bundled': audio.is_file(), 'default': 'No', 'minimum_macos_major': 26, 'existing_opt_ins_retained': True}
    webkit = payload/'Optional/NVWebKitFixup/NVWebKitFixup.kext/Contents/MacOS/NVWebKitFixup'
    compact['optional_webkit_fixup'] = {'bundled': webkit.is_file(), 'default': 'Manual instructions',
                                       'installation': 'OpenCore EFI only; Automatic detects a unique compatible EFI or asks for a choice; explicit target overrides detection'}
    # SHA256SUMS covers every file. These hashes identify the primary components.
    paths = [Path('Library/Extensions') / (name + '.kext/Contents/MacOS/' + name)
             for name in ('NVRM', 'NVRMFB', 'NVAccel', 'NVRMAGDC')]
    paths += [Path('Library/NullMoth/kexts') / major / 'NVAccel.kext/Contents/MacOS/NVAccel' for major in ('15', '26')]
    paths += [Path('Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver'), panel.relative_to(payload), audio.relative_to(payload), webkit.relative_to(payload)]
    compact['components_sha256'] = {str(path): integrity.digest(payload / path) for path in paths if (payload / path).is_file()}
    current.write_text(json.dumps(compact, indent=2) + '\n')
    current.chmod(0o644)
    readme = (ROOT / 'docs/COMBINED-INSTALL.md').read_text()
    for relative, name in GUIDES.items():
        if (payload / 'Guides' / name).is_file():
            readme = readme.replace(GITHUB_FILES + GUIDE_SOURCES[relative].as_posix(), 'Guides/' + name)
    readme = readme.replace(GITHUB_FILES + 'docs/INTEL-PANEL.md', 'Optional/README.md')
    readme = readme.replace(GITHUB_FILES + 'docs/OPTIONAL-AUDIO.md', 'Optional/Audio.md')
    readme = readme.replace(GITHUB_FILES + 'docs/SAFARI-WEBGL.md', 'Optional/NVWebKitFixup/README.md')
    (payload / 'READ-ME.md').write_text(readme)
    (payload / 'READ-ME.md').chmod(0o644)
    integrity.validate_links(payload)
    return records

#!/usr/bin/env python3
"""Package the one-time 3.0.1 Wine bridge supplement; never build a full driver."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

REPO = Path(__file__).resolve().parents[1]
UPDATE_ID = '3.0.1-nvk-hybrid-1'
ARCHIVE = 'nvidia-nvk-bridge-update-3.0.1-nvk.tar.gz'
BASE_MANIFEST = '03bb393c948fc6946f9d4de1dbb8cdb907dc07c431a32435e9f12db6c84d5080'
ENTRY = 'lib/libnvk_wine.dylib'
CHANGED = (ENTRY, 'FRONTEND-BUILD.json', 'BUILD.json', 'nvk-wine-runtime.json', 'SHA256SUMS')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def run(command, log):
    result = subprocess.run(list(map(str, command)), cwd=REPO, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, check=False)
    log.write_text(result.stdout)
    if result.returncode:
        raise RuntimeError(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-provider', type=Path, required=True)
    parser.add_argument('--frontend', type=Path, required=True,
                        help='Output from nvk/wine-macos/build.py')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    base, frontend = args.base_provider.resolve(), args.frontend.resolve()
    assert sha(base / 'nvk-wine-runtime.json') == BASE_MANIFEST, 'Not the published 3.0.1 provider'
    old_manifest = json.loads((base / 'nvk-wine-runtime.json').read_text())
    for name, digest in old_manifest['files'].items():
        assert sha(base / name) == digest, f'Modified base provider: {name}'
    for line in (base / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        assert sha(base / name) == digest, f'Base provider checksum mismatch: {name}'
    before = {str(p.relative_to(base)): sha(p) for p in sorted(base.rglob('*'))
              if p.is_file() and not p.is_symlink()}
    assert not any('.DS_Store' in n or any(c.startswith('._') for c in Path(n).parts) for n in before)
    links = {str(p.relative_to(base)): str(p.readlink()) for p in base.rglob('*') if p.is_symlink()}
    assert links == {'lib/libMoltenVK.dylib': 'libnvk_wine.dylib'}
    build = json.loads((frontend / 'BUILD.json').read_text())
    for name, digest in build['source_files'].items():
        assert sha(REPO / name) == digest, f'Frontend source changed since build: {name}'
    assert sha(frontend / ENTRY) == build['files'][ENTRY]
    assert build['renderer']['sha256'] == old_manifest['renderer']['sha256']
    assert sha(frontend / 'lib/libnvk_renderer.dylib') == old_manifest['renderer']['sha256']
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    package = out / 'NVK-Bridge-Update'
    payload = package / 'payload'
    (payload / 'lib').mkdir(parents=True)
    (package / 'bin').mkdir()
    records = out / 'build-records'
    records.mkdir()
    assets = out / 'assets'
    assets.mkdir()
    shutil.copy2(frontend / ENTRY, payload / ENTRY)
    source_commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=REPO, text=True).strip()
    # Build commands and raw diff/logs remain outside the distributable package.
    portable_build = {k: v for k, v in build.items() if k not in ('commands', 'source_diff')}
    portable_build['validated_source_commit'] = source_commit
    portable_build['source_uncommitted'] = bool(subprocess.check_output(
        ['git', 'diff', 'HEAD', '--', *build['source_files']], cwd=REPO, text=True))
    for key in ('entry', 'renderer'):
        for field in ('dependencies', 'deployment'):
            value = portable_build[key].get(field)
            if value:
                portable_build[key][field] = value.replace(str(frontend), '$FRONTEND').replace(str(REPO), '$SOURCE')
    write_json(payload / 'FRONTEND-BUILD.json', portable_build)
    manifest = copy.deepcopy(old_manifest)
    manifest['source_runtime_id'] = manifest['runtime_id']
    manifest['entry']['sha256'] = sha(payload / ENTRY)
    for name in (ENTRY, 'FRONTEND-BUILD.json'):
        manifest['files'][name] = sha(payload / name)
    manifest['runtime_id'] = 'nvk-wine-provider-' + hashlib.sha256(
        json.dumps(manifest['files'], sort_keys=True).encode()).hexdigest()[:12]
    manifest['bridge_update'] = {'id': UPDATE_ID, 'base_release': 'v3.0.1-nvk',
                                 'scope': 'Initial Wine Metal layer selection on hybrid graphics'}
    manifest['validation']['inherited_binaries'] = (
        'All runtime components except the Wine frontend retained byte-for-byte from source_runtime_id')
    manifest['validation']['helper'] = 'Retained unchanged from the base provider'
    write_json(payload / 'nvk-wine-runtime.json', manifest)
    provider_build = json.loads((base / 'BUILD.json').read_text())
    provider_build['files'] = manifest['files']
    provider_build['bridge_update'] = {'id': UPDATE_ID, 'source_commit': source_commit,
                                      'frontend_build': 'FRONTEND-BUILD.json'}
    write_json(payload / 'BUILD.json', provider_build)
    after = dict(before)
    for name in CHANGED[:-1]:
        after[name] = sha(payload / name)
    # Same provider layout and checksum contract as the original installation.
    (payload / 'SHA256SUMS').write_text(''.join(f'{after[name]}  {name}\n'
        for name in sorted(after) if name != 'SHA256SUMS'))
    after['SHA256SUMS'] = sha(payload / 'SHA256SUMS')
    helper = package / 'bin/nvk-bridge-update'
    command = ['xcrun', 'clang++', '-arch', 'x86_64', '-mmacosx-version-min=15.5',
               '-std=c++17', '-fobjc-arc', '-O2', '-Wall', '-Wextra', '-Werror',
               '-Wno-deprecated-declarations', REPO / 'tools/bridge-update/update.mm',
               '-framework', 'Foundation', '-framework', 'Security', '-o', helper]
    run(command, records / 'build-helper.log')
    run(['codesign', '--force', '--sign', '-', '--timestamp=none', helper], records / 'sign-helper.log')
    for binary in (helper, payload / ENTRY):
        run(['codesign', '--verify', '--strict', binary], records / (binary.name + '-signature.log'))
        run(['lipo', binary, '-verify_arch', 'x86_64'], records / (binary.name + '-architecture.log'))
    for title, action in (('Check', 'check'), ('Install', 'install'), ('Rollback', 'rollback')):
        script = package / (title + '.command')
        script.write_text('#!/bin/zsh\nset -eu\n'
            'package_dir="$(cd -- "$(dirname -- "$0")" && pwd -P)"\n'
            'cd -- "$package_dir"\n'
            '/usr/bin/shasum -a 256 -c SHA256SUMS\n'
            '/usr/bin/codesign --verify --strict "$package_dir/bin/nvk-bridge-update"\n'
            + ('' if action == 'check' else '/usr/bin/sudo ') +
            f'"$package_dir/bin/nvk-bridge-update" {action} --package "$package_dir"\n'
            'printf "\\nPress Return to close. "\nread -r response\n')
        script.chmod(0o755)
    shutil.copy2(REPO / 'tools/bridge-update/README.md', package / 'README.md')
    shutil.copy2(base / 'LICENSES/frontend-PolyForm-Noncommercial-1.0.0.txt', package / 'LICENSE.txt')
    record = {'schema_version': 1, 'update_id': UPDATE_ID, 'source_commit': source_commit,
              'source_files': {name: sha(REPO / name) for name in (
                  'tools/bridge-update/update.mm', 'tools/bridge-update/README.md',
                  'tools/bridge-update/test_update.py', 'build/package_bridge_update.py',
                  'nvk/native-macos/bridge.mm', 'nvk/native-macos/surface_device_test.mm')},
              'base_release': 'v3.0.1-nvk', 'base_provider_manifest_sha256': BASE_MANIFEST,
              'renderer_sha256': old_manifest['renderer']['sha256'],
              'entry_sha256': after[ENTRY],
              'validation_limits': 'Hybrid color probes passed with explicit NVIDIA binding; patched game testing pending.'}
    write_json(package / 'BUILD-IDENTITY.json', record)
    package_files = {str(p.relative_to(package)): sha(p) for p in sorted(package.rglob('*')) if p.is_file()}
    spec = {'schema_version': 1, 'update_id': UPDATE_ID, 'base_release': 'v3.0.1-nvk',
            'before': before, 'after': after, 'symlinks': links, 'package_files': package_files}
    write_json(package / 'UPDATE.json', spec)
    (package / 'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(package)}\n'
        for p in sorted(package.rglob('*')) if p.is_file() and p != package / 'SHA256SUMS'))
    archive = assets / ARCHIVE
    # Explicit file list avoids AppleDouble, Finder metadata and resource forks.
    with tarfile.open(archive, 'w:gz', format=tarfile.PAX_FORMAT) as tar:
        for path in [package, *sorted(package.rglob('*'))]:
            assert path.name != '.DS_Store' and not path.name.startswith('._')
            info = tar.gettarinfo(str(path), arcname='NVK-Bridge-Update' +
                                  ('/' + str(path.relative_to(package)) if path != package else ''))
            info.uid = info.gid = 0
            info.uname = info.gname = ''
            info.pax_headers = {}
            if path.is_file():
                with path.open('rb') as stream:
                    tar.addfile(info, stream)
            else:
                tar.addfile(info)
    (assets / (ARCHIVE + '.sha256')).write_text(f'{sha(archive)}  {ARCHIVE}\n')
    write_json(records / 'asset-identity.json', {'file': ARCHIVE, 'sha256': sha(archive),
                                               'bytes': archive.stat().st_size})
    print(json.dumps({'archive': str(archive), 'sha256': sha(archive), 'provider_id': manifest['runtime_id']}, indent=2))


if __name__ == '__main__':
    main()

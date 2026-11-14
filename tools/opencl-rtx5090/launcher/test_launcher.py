#!/usr/bin/env python3
"""CPU-only contracts using synthetic packages; no app or GPU execution."""
import copy
import json
import os
from pathlib import Path
import shlex
import tempfile
import types
from unittest.mock import patch

import launch


def rejected(call):
    try:
        call()
    except RuntimeError:
        return
    raise AssertionError('Expected rejection')


example = json.loads(Path(__file__).with_name('package.example.json').read_text())
for uid, euid in ((0, 0), (501, 0), (501, 502)):
    with patch.object(os, 'getuid', return_value=uid), patch.object(os, 'geteuid', return_value=euid):
        rejected(launch.require_normal_user)
with patch.object(os, 'getuid', return_value=501), patch.object(os, 'geteuid', return_value=501):
    launch.require_normal_user()

with tempfile.TemporaryDirectory() as directory:
    temporary = Path(directory).resolve()
    root = temporary / "package space 'quote' $HOME; sentinel"
    root.mkdir()
    manifest = copy.deepcopy(example)
    manifest['artifacts'] = {}
    for relative in example['artifacts']:
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(relative)
        manifest['artifacts'][relative] = launch.sha(target)
    external = temporary / 'loader'
    external.write_text('synthetic external dependency')
    alias = temporary / 'loader-alias'
    alias.symlink_to(external)
    manifest.update(loader=str(external), loader_alias=str(alias),
                    external_pins={str(external): launch.sha(external)})
    manifest['app']['bundle'] = str(temporary / 'Genuine App.app')

    def signed(argv):
        if '-dv' in argv:
            mode = 'cli' if argv[-1].endswith('geekbench_avx2') else 'gui'
            identity = manifest['app']['executables'][mode]
            return types.SimpleNamespace(stderr='\n'.join(
                key + '=' + identity[key] for key in ('CDHash', 'Identifier', 'TeamIdentifier')))
        return types.SimpleNamespace(stdout='', stderr='')

    with patch.object(launch, 'command', side_effect=signed):
        launch.verify(root, manifest)
        (root / 'bin/clspv').write_text('changed')
        rejected(lambda: launch.verify(root, manifest))
        (root / 'bin/clspv').write_text('bin/clspv')
        external.write_text('changed')
        rejected(lambda: launch.verify(root, manifest))
        external.write_text('synthetic external dependency')
        alias.unlink()
        alias.symlink_to(temporary / 'different-loader')
        rejected(lambda: launch.verify(root, manifest))
        alias.unlink()
        alias.symlink_to(external)
    with patch.object(launch, 'command', return_value=types.SimpleNamespace(stderr='CDHash=wrong')):
        rejected(lambda: launch.verify(root, manifest))

    rejected(lambda: launch.checked_path(root, '../outside'))
    (root / 'escape').symlink_to(temporary)
    rejected(lambda: launch.checked_path(root, 'escape/file'))
    assert launch.checked_path(root, 'bin/clspv') == root / 'bin/clspv'

    inherited = {'PATH': '/usr/bin', 'LANG': 'C',
                 **{prefix + 'UNTRUSTED': 'bad' for prefix in launch.PREFIXES}}
    env = launch.environment(root, manifest, temporary / 'run', temporary / 'cache', inherited)
    assert not any(key.endswith('UNTRUSTED') for key in env)
    assert env['PATH'] == '/usr/bin' and env['LANG'] == 'C'
    assert all(env[key] == value for key, value in example['experiment_config'].items())
    assert shlex.split(env['CLVK_CLSPV_PATH']) == [str(root / 'bin/clspv')]
    assert env['CLVK_IGNORE_OUT_OF_ORDER_EXECUTION'] == '0'
    assert env['CLVK_QUEUE_PROFILING_USE_TIMESTAMP_QUERIES'] == '0'
    assert env['VK_DRIVER_FILES'] == env['VK_ICD_FILENAMES']
    # The compiler executable is quoted; clvk appends generated paths unquoted.
    for component in ('with space', "single'quote", 'double"quote', '$HOME',
                      '$(touch sentinel)', '`touch sentinel`', 'semi;colon',
                      'new\nline', 'tab\tname', 'pipe|name', 'amp&name',
                      'in<name', 'out>name', 'star*name', 'glob?name', '[glob]'):
        rejected(lambda: launch.environment(root, manifest, temporary / component,
                                             temporary / 'cache', inherited))
    rejected(lambda: launch.environment(root, manifest, Path('relative'),
                                         temporary / 'cache', inherited))
    # Validate the final environment, including overrides of launcher-owned keys.
    for key in ('CLVK_COMPILER_TEMP_DIR', 'TMPDIR'):
        for value in ('relative', str(temporary / 'own key;unsafe')):
            changed = copy.deepcopy(manifest)
            changed['experiment_config'][key] = value
            rejected(lambda: launch.environment(root, changed, temporary / 'run',
                                                 temporary / 'cache', inherited))
    identity = launch.cache_identity(manifest)
    reordered = copy.deepcopy(manifest)
    reordered['experiment_config'] = dict(reversed(list(manifest['experiment_config'].items())))
    assert launch.cache_identity(reordered) == identity
    for category, key in [('artifacts', 'bin/clspv'), ('experiment_config', 'NVRM_610_BOOST')]:
        changed = copy.deepcopy(manifest)
        changed[category][key] = 'changed'
        assert launch.cache_identity(changed) != identity
    assert launch.argv_for('gui', manifest) == [str(Path(manifest['app']['bundle']) / 'Contents/MacOS/Geekbench 7')]
    assert launch.argv_for('cli', manifest)[1:] == [
        '--gpu', 'OpenCL', '--gpu-platform-id', '0', '--gpu-device-id', '0']

print('PASS: privilege, payload/dependency/identity, containment, environment, quoting, temporary-path guards, cache, argv')

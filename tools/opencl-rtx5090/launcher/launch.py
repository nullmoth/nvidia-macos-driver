#!/usr/bin/env python3
"""Launch the unchanged Geekbench app with this package's process-local OpenCL."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

PREFIXES = ('CLVK', 'NVMTL', 'NVK', 'NAK', 'NVRM', 'VK_', 'MESA', 'DYLD',
            'NM_TEST', 'CLTRACE', 'NIR_', 'RADV_', 'ACO_', 'METAL2VULKAN_', 'ELFHOST_')
GPU_NAMES = r'(?:geekbench[^/]*|LeagueofLegends|League of Legends|SimMetalHost|SimRenderServer|api_tests|numeric_oracle|fp32_oracle|readback_oracle|native_gemm_oracle|convert_oracle|workgroup_oracle|cl_info_only|query_memory_properties|transfer_contract_gpu|simple_test)'


def sha(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def checked_path(root, relative):
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()):
        raise RuntimeError('Package path escapes its version directory: ' + relative)
    return path


def command(argv):
    return subprocess.run(argv, capture_output=True, text=True, timeout=30, check=True)


def verify(root, manifest):
    if manifest['schema'] != 1 or manifest['kind'] != 'private-opencl-launch-package':
        raise RuntimeError('Unexpected package manifest')
    for relative, digest in manifest['artifacts'].items():
        if sha(checked_path(root, relative)) != digest:
            raise RuntimeError('Package changed: ' + relative)
    for path, digest in manifest['external_pins'].items():
        if sha(path) != digest:
            raise RuntimeError('Installed dependency changed: ' + path)
    for relative in ('Frameworks/OpenCL.framework', 'lib/libvulkan_nouveau.dylib'):
        command(['/usr/bin/codesign', '--verify', '--strict', str(root / relative)])
    if Path(manifest['loader_alias']).resolve() != Path(manifest['loader']).resolve():
        raise RuntimeError('Installed Vulkan loader alias changed')
    app = Path(manifest['app']['bundle'])
    command(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)])
    for mode, identity in manifest['app']['executables'].items():
        result = command(['/usr/bin/codesign', '-dv', '--verbose=4', '--arch', 'x86_64', str(app / identity['relative'])])
        lines = dict(line.split('=', 1) for line in result.stderr.splitlines() if '=' in line)
        if any(lines.get(key) != identity[key] for key in ('CDHash', 'TeamIdentifier', 'Identifier')):
            raise RuntimeError('Original Geekbench identity changed: ' + mode)


def cache_identity(manifest):
    values = {'binaries': {key: manifest['artifacts'][key] for key in (
        'Frameworks/OpenCL.framework/Versions/A/OpenCL', 'bin/clspv', 'lib/libvulkan_nouveau.dylib')},
        'config': manifest['experiment_config']}
    return hashlib.sha256(json.dumps(values, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def environment(root, manifest, run, cache, inherited):
    env = {key: value for key, value in inherited.items() if not key.startswith(PREFIXES)}
    env.update(DYLD_FRAMEWORK_PATH=str(root / 'Frameworks'), DYLD_PRINT_LIBRARIES='1',
               VK_DRIVER_FILES=str(run / 'nvk_icd.json'), VK_ICD_FILENAMES=str(run / 'nvk_icd.json'),
               CLVK_CLSPV_PATH=shlex.quote(str(root / 'bin/clspv')), CLVK_CACHE_DIR=str(cache / 'clvk'),
               MESA_SHADER_CACHE_DIR=str(cache / 'mesa'), CLVK_COMPILER_TEMP_DIR=str(run / 'tmp'),
               TMPDIR=str(run / 'tmp'), CLVK_LOG='1', CLVK_LOG_DEST='file:' + str(run / 'clvk-errors.log'),
               CLVK_IGNORE_OUT_OF_ORDER_EXECUTION='0', CLVK_QUEUE_PROFILING_USE_TIMESTAMP_QUERIES='0')
    env.update(manifest['experiment_config'])
    # shortcut: clvk shells out; reject unquoted temp paths until it uses argv.
    for key in ('CLVK_COMPILER_TEMP_DIR', 'TMPDIR'):
        path = env[key]
        if not Path(path).is_absolute() or shlex.quote(path) != path:
            raise RuntimeError('Compiler temporary path must be absolute and shell-safe: ' + key)
    return env


def argv_for(mode, manifest):
    path = str(Path(manifest['app']['bundle']) / manifest['app']['executables'][mode]['relative'])
    return [path] if mode == 'gui' else [path, '--gpu', 'OpenCL', '--gpu-platform-id', '0', '--gpu-device-id', '0']


def conflicts(own_group=None):
    rows = command(['/bin/ps', '-axo', 'pid=,pgid=,comm=']).stdout
    return [int(pid) for pid, group, path in re.findall(r'^\s*(\d+)\s+(\d+)\s+(.+)$', rows, re.M)
            if int(group) != own_group and re.fullmatch(GPU_NAMES, path.rsplit('/', 1)[-1], re.I)]


def require_normal_user():
    if os.geteuid() == 0 or os.getuid() != os.geteuid():
        raise RuntimeError('Use this launcher as your normal macOS user; sudo/setuid is unsupported.')


def main():
    require_normal_user()
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--gui', action='store_true')
    mode.add_argument('--cli-exec', action='store_true', help='exec fixed Geekbench OpenCL CLI; caller owns timeout/qualification')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    manifest = json.loads((root / 'package.json').read_text())
    verify(root, manifest)
    if conflicts():
        raise RuntimeError('Another GPU workload is already running; close it before using this launcher.')
    home = Path.home()
    logs = home / 'Library/Logs/NullMoth-OpenCL'
    logs.mkdir(parents=True, exist_ok=True, mode=0o700)
    run = Path(tempfile.mkdtemp(prefix='gui-' if args.gui else 'cli-', dir=logs))
    (run / 'tmp').mkdir(mode=0o700)
    cache = home / 'Library/Caches/NullMoth-OpenCL' / cache_identity(manifest)
    for name in ('clvk', 'mesa'):
        (cache / name).mkdir(parents=True, exist_ok=True, mode=0o700)
    write_json(run / 'nvk_icd.json', {'file_format_version': '1.0.0', 'ICD': {
        'library_path': str(root / 'lib/libvulkan_nouveau.dylib'), 'api_version': '1.3.0'}})
    env = environment(root, manifest, run, cache, os.environ)
    mode = 'gui' if args.gui else 'cli'
    argv = argv_for(mode, manifest)
    launch = {'mode': mode, 'argv': argv, 'package': str(root), 'cache': str(cache),
              'experiment_config': manifest['experiment_config'], 'started_epoch': time.time(),
              'pid': os.getpid(),
              'env': {key: value for key, value in env.items() if key.startswith(PREFIXES) or key == 'TMPDIR'}}
    write_json(run / 'launch.json', launch)
    if args.gui:
        print('Log directory: ' + str(run), flush=True)
        for number, name in ((1, 'stdout'), (2, 'stderr')):
            fd = os.open(str(run / name), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.dup2(fd, number)
            os.close(fd)
    else:
        # The external verifier owns this same PID/process group and its watchdog.
        print('PACKAGE_RUN_DIR=' + str(run), file=sys.stderr, flush=True)
    os.execve(argv[0], argv, env)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print('Launch stopped: ' + str(error), file=sys.stderr)
        sys.exit(1)

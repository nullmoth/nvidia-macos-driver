#!/usr/bin/env python3
"""Exercise resource reads through the production translator ABI, without a GPU.

Pass a built translator; optionally pass --baseline with the pre-fix translator.
The object-index and nullable-field regressions must fail against that baseline;
the ordinary loop fixture is a compatibility control.
"""
import argparse
import ctypes as c
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'translator/wrapper/tests/fixtures'


def translate(library, source, stage):
    lib = c.CDLL(str(library.resolve()))
    lib.nvmtl_translate.argtypes = [c.c_char_p, c.c_char_p, c.POINTER(c.c_void_p),
                                   c.POINTER(c.c_size_t), c.c_void_p, c.c_size_t]
    lib.nvmtl_translate.restype = c.c_int
    lib.nvmtl_translate_free.argtypes = [c.c_void_p, c.c_size_t]
    out, size, error = c.c_void_p(), c.c_size_t(), c.create_string_buffer(8192)
    rc = lib.nvmtl_translate(source.read_bytes(), stage.encode(), c.byref(out), c.byref(size), error, len(error))
    if rc:
        raise RuntimeError(error.value.decode(errors='replace'))
    try:
        return c.string_at(out, size.value)
    finally:
        lib.nvmtl_translate_free(out, size)


def instructions(data):
    words = struct.unpack('<' + 'I' * (len(data) // 4), data)
    offset = 5
    while offset < len(words):
        count, op = words[offset] >> 16, words[offset] & 65535
        assert count and offset + count <= len(words)
        yield op, words[offset + 1:offset + count]
        offset += count


def checks(library, plugin=None, nvk_directory=None):
    results = {}
    with tempfile.TemporaryDirectory(prefix='nvmtl-resource-reads-') as tmp:
        oracle = Path(tmp) / 'resource-reads'
        if plugin:
            subprocess.run(['cc', '-O2', '-I' + str(ROOT / 'plugin'),
                            str(ROOT / 'tools/oracle_resource_reads.c'), '-o', str(oracle)], check=True)
        for fixture, stage in [('object_info_index.ll', 'vertex'), ('local_resource_phi.ll', 'kernel'), ('loop_resource_phi.ll', 'kernel')]:
            data = translate(library, FIXTURES / fixture, stage)
            path = Path(tmp) / 'test.spv'; path.write_bytes(data)
            subprocess.run(['spirv-val', '--target-env', 'vulkan1.2', '--scalar-block-layout', str(path)], check=True, capture_output=True)
            ins = list(instructions(data))
            # OpTypePointer's storage class 12 is StorageBuffer.
            buffer_types = {a[0] for op, a in ins if op == 32 and a[1] == 12}
            # Access chains, pointer copies, parameters and variables have type/result first.
            buffer_ptrs = {a[1] for op, a in ins if op in (55, 59, 65, 66, 67, 83, 245) and a[0] in buffer_types}
            if stage == 'kernel':
                # This fixture has one input buffer and a write-only output: the data must be loaded.
                results[fixture] = any(op == 61 and a[2] in buffer_ptrs for op, a in ins)
                if plugin:
                    env = {k: v for k, v in os.environ.items()
                           if not k.startswith(('NVMTL_', 'NVK_', 'NAK_', 'DYLD_'))}
                    env['NVMTL_PROCESS_ENV'] = '1'
                    env['NVMTL_LOG_PATH'] = str(Path(tmp) / 'driver.log')
                    if nvk_directory:
                        env['DYLD_LIBRARY_PATH'] = str(nvk_directory.resolve())
                    subprocess.run([str(oracle), str(plugin.resolve()), str(path)], env=env, check=True)
            else:
                converted = {a[1] for op, a in ins if op == 113}  # OpUConvert of the instance index
                stride = any(op == 71 and a[1:] == (6, 64) for op, a in ins)
                path_uses_index = any(op in (65, 66) and a[0] in buffer_types and any(v in converted for v in a[3:]) for op, a in ins)
                results[fixture] = stride and path_uses_index
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--plugin', type=Path, help='run actual GPU reads through this isolated plugin')
    parser.add_argument('--nvk-directory', type=Path, help='directory containing an isolated NVK dylib')
    args = parser.parse_args()
    result = checks(args.library, args.plugin, args.nvk_directory)
    if args.json:
        print(json.dumps(result)); return
    assert all(result.values()), result
    print('fixed: dynamic object index and live resource loads preserved')
    if args.baseline:
        # Separate processes prevent dyld from conflating equal install names.
        p = subprocess.run([sys.executable, __file__, str(args.baseline), '--json'], check=True, capture_output=True, text=True)
        before = json.loads(p.stdout)
        assert before == {'object_info_index.ll': False, 'local_resource_phi.ll': False,
                          'loop_resource_phi.ll': True}, before
        print('baseline: both silent miscompilations reproduced; ordinary loop control retained')


if __name__ == '__main__':
    main()

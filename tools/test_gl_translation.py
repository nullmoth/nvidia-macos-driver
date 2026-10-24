#!/usr/bin/env python3
"""Check GL translation through the built C ABI, without loading a GPU driver.

Usage: python3 tools/test_gl_translation.py /path/to/libnvmtl_translate.dylib
       --spirv-val /path/to/spirv-val --spirv-as /path/to/spirv-as
The validator must support Vulkan 1.2 and --scalar-block-layout.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]


def shader_variants():
    source = (ROOT / 'translator/translator/tests/fixtures/gl/'
              'sampler2d_param_chosen_by_constant.ll').read_text()
    before = '  store i32 0, ptr %6, align 4'
    assert source.count(before) == 1
    dynamic = source.replace(before,
        '  %mode_float = extractelement <2 x float> %0, i32 0\n'
        '  %mode_int = fptosi float %mode_float to i32\n'
        '  store i32 %mode_int, ptr %6, align 4')
    shadow = dynamic.replace(
        '{ <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>,',
        '{ float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>,')
    shadow = shadow.replace(
        '%12 = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %10, ptr addrspace(2) %11, <2 x float> %2,',
        '%12 = call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %10, ptr addrspace(2) %11, i32 1, <2 x float> %2, float 0.5,')
    shadow = shadow.replace('%13 = extractvalue { <4 x float>, i8 } %12, 0',
        '%depth = extractvalue { float, i8 } %12, 0\n'
        '  %13 = insertelement <4 x float> zeroinitializer, float %depth, i32 0')
    shadow = shadow.replace('texture2d<float, sample>', 'depth2d<float, sample>')
    # Exercise A8 output rewriting on the same retry, using the regular sample.
    fragment = dynamic.replace('!air.vertex =', '!air.fragment =').replace(
        '!2 = !{!"air.position",', '!2 = !{!"air.render_target", i32 0,')
    fragment = fragment.replace('!3 = !{!"air.vertex_output", !"user(slot8)",',
        '!3 = !{!"air.render_target", i32 1,')
    fragment = fragment.replace('!"air.vertex_input", !"air.location_index", i32 0, i32 1, !"attrib_0",',
        '!"air.fragment_input", !"user(slot0)", !"air.center", !"air.perspective",')
    return [('constant', source, 'vertex', False, 0),
            ('dynamic', dynamic, 'vertex', False, 0),
            ('shadow', shadow, 'vertex', True, 0),
            ('a8_fragment', fragment, 'fragment', False, 3)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('--spirv-val', default='spirv-val')
    parser.add_argument('--spirv-as', default='spirv-as')
    args = parser.parse_args()
    library = ctypes.CDLL(str(args.library.resolve()))
    pointer = ctypes.POINTER
    translate = library.nvmtl_translate
    translate.argtypes = [ctypes.c_char_p, ctypes.c_char_p, pointer(ctypes.c_void_p),
                          pointer(ctypes.c_size_t), ctypes.c_void_p, ctypes.c_size_t]
    translate.restype = ctypes.c_int
    specialize = library.nvmtl_translate_samplers
    specialize.argtypes = translate.argtypes[:2] + [ctypes.c_char_p] + translate.argtypes[2:]
    specialize.restype = ctypes.c_int
    promote = library.nvmtl_promote_gl_uniform
    promote.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_uint32,
                       pointer(ctypes.c_void_p), pointer(ctypes.c_size_t), pointer(ctypes.c_uint32),
                       ctypes.c_void_p, ctypes.c_size_t]
    promote.restype = ctypes.c_int
    free = library.nvmtl_translate_free
    free.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    free.restype = None

    with tempfile.TemporaryDirectory(prefix='nvmtl-gl-translation-') as directory:
        directory = Path(directory)

        def validate(data):
            path = directory / 'module.spv'
            path.write_bytes(data)
            subprocess.run([args.spirv_val, '--target-env', 'vulkan1.2',
                            '--scalar-block-layout', str(path)], check=True)

        def run(ll, stage, payload=None):
            output, size = ctypes.c_void_p(), ctypes.c_size_t()
            error = ctypes.create_string_buffer(8192)
            params = [ll.encode(), stage.encode()]
            if payload is not None:
                params.append(json.dumps(payload).encode())
            params.extend([ctypes.byref(output), ctypes.byref(size), error, len(error)])
            rc = (translate if payload is None else specialize)(*params)
            try:
                assert rc == 0, error.value.decode(errors='replace')
                data = ctypes.string_at(output, size.value)
            finally:
                if output.value:
                    free(output, size.value)
            validate(data)
            return data

        old_dump = os.environ.get('NVMTL_DUMP_RETRY_LL')
        try:
            for name, ll, stage, shadow, a8 in shader_variants():
                dump = directory / (name + '.ll')
                os.environ['NVMTL_DUMP_RETRY_LL'] = str(dump)
                run(ll, stage)
                if name != 'constant':
                    assert dump.exists(), 'fixture must exercise the pointer retry'
                repaired = dump.read_text() if dump.exists() else ll
                # Different runtime states make losing specialization detectable.
                samplers = {}
                for index, filter_name in [(0, 'Nearest'), (1, 'Linear')]:
                    samplers[str(index)] = {
                        'min_filter': filter_name, 'mag_filter': filter_name, 'mip_filter': 'None',
                        'address_mode_s': 'ClampToEdge', 'address_mode_t': 'ClampToEdge',
                        'address_mode_r': 'ClampToEdge', 'coordinates': 'Pixel',
                        'compare_function': 'Less' if shadow else 'None', 'max_anisotropy': 1,
                        'lod_min_clamp': 0.0, 'lod_max_clamp': 1000.0,
                        'border_color': 'TransparentBlack', 'reduction': 'WeightedAverage', 'lod_bias': 0.0}
                payload = {'samplers': samplers, 'constants': {}, 'a8_targets': a8}
                dump.unlink(missing_ok=True)
                actual = run(ll, stage, payload)
                expected = run(repaired, stage, payload)
                assert actual == expected, 'retry must preserve all runtime options'
                if name != 'constant':
                    assert dump.exists(), 'specialized path must use the shared retry'
                if a8:
                    plain = run(repaired, stage, dict(payload, a8_targets=0))
                    assert actual != plain, 'A8 output rewriting must survive the retry'
                    a8_only = dict(payload, samplers={})
                    assert run(ll, stage, a8_only) == run(repaired, stage, a8_only), \
                        'the A8-only path must retry without requiring runtime samplers'
                print(f'PASS: {name}: normal and specialized translation validate; options retained')
        finally:
            if old_dump is None:
                os.environ.pop('NVMTL_DUMP_RETRY_LL', None)
            else:
                os.environ['NVMTL_DUMP_RETRY_LL'] = old_dump

        fixture = ROOT / 'translator/wrapper/tests/fixtures/gl_uniform_global_initializer.spvasm'
        original = directory / 'global-initializer.spv'
        subprocess.run([args.spirv_as, '--target-env', 'vulkan1.2', str(fixture), '-o', str(original)], check=True)
        data = original.read_bytes()
        validate(data)
        input_buffer = ctypes.create_string_buffer(data)
        output, size, extent = ctypes.c_void_p(), ctypes.c_size_t(), ctypes.c_uint32()
        error = ctypes.create_string_buffer(8192)
        rc = promote(input_buffer, len(data), 7, 65536, ctypes.byref(output), ctypes.byref(size),
                     ctypes.byref(extent), error, len(error))
        try:
            assert rc != 0 and b'pointer escapes through Variable' in error.value, error.value
            assert not output.value and size.value == 0 and extent.value == 0
            assert input_buffer.raw[:len(data)] == data, 'failed promotion must leave the input intact'
        finally:
            if output.value:
                free(output, size.value)
        # The unaffected scalar-block fixture still promotes to a valid module.
        # Remove the global alias with a fixture variant rather than mutating IDs.
        source = fixture.read_text().replace(' %saved\n', '\n')
        source = '\n'.join(line for line in source.splitlines() if not any(
            line.startswith(prefix) for prefix in ['%private_pointer', '%saved', '%copy', '%indirect', '%another'])) + '\n'
        clean = directory / 'ordinary.spvasm'
        clean.write_text(source)
        subprocess.run([args.spirv_as, '--target-env', 'vulkan1.2', str(clean), '-o', str(original)], check=True)
        data = original.read_bytes()
        validate(data)
        input_buffer = ctypes.create_string_buffer(data)
        rc = promote(input_buffer, len(data), 7, 65536, ctypes.byref(output), ctypes.byref(size),
                     ctypes.byref(extent), error, len(error))
        try:
            assert rc == 0 and extent.value == 16, error.value
            validate(ctypes.string_at(output, size.value))
        finally:
            if output.value:
                free(output, size.value)
        print('PASS: valid global pointer capture refused; ordinary uniform promotion validates')


if __name__ == '__main__':
    main()

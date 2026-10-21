#!/usr/bin/env python3
'''Compile and exercise the production all-black gamma table check.'''
import pathlib, subprocess, sys, tempfile
here = pathlib.Path(__file__).resolve()
roots = [pathlib.Path(sys.argv[1]).resolve()] if len(sys.argv) > 1 else [here.parent.parent]
root = next((p for p in roots if (p / 'kexts/NVRMFB/fb/nvrm-fb.cpp').exists() or (p / 'source.cpp').exists()), None)
if root is None: raise SystemExit('kexts/NVRMFB/fb/nvrm-fb.cpp or source.cpp not found')
srcpath = root / 'kexts/NVRMFB/fb/nvrm-fb.cpp'
if not srcpath.exists(): srcpath = root / 'source.cpp'
src = srcpath.read_text(encoding='utf-8')
start = src.index('static bool nvrmLutIsBlack(')
body = src[start:src.index('\n}', start) + 2]
assert 'NVKMS' not in body and 'FBLOG' not in body
assert src.index('nvrmLutIsBlack(channels, count, width, data)') > src.index('gamma: refused a table')
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
using UInt8 = uint8_t; using UInt16 = uint16_t; using UInt32 = uint32_t;
'''
checks = r'''
int main() {
    UInt8 b8[3 * 256]; UInt16 b16[3 * 1024];
    std::memset(b8, 0, sizeof b8); std::memset(b16, 0, sizeof b16);
    assert(nvrmLutIsBlack(1, 256, 8, b8));
    b8[255] = 1; assert(!nvrmLutIsBlack(1, 256, 8, b8));
    std::memset(b8, 0, sizeof b8);
    assert(nvrmLutIsBlack(3, 256, 8, b8));
    b8[256 + 7] = 9; assert(!nvrmLutIsBlack(3, 256, 8, b8));
    std::memset(b16, 0, sizeof b16);
    assert(nvrmLutIsBlack(3, 1024, 16, b16));
    assert(nvrmLutIsBlack(1, 1024, 16, b16));
    b16[2 * 1024 + 1023] = 1; assert(!nvrmLutIsBlack(3, 1024, 16, b16));
    std::memset(b16, 0, sizeof b16);
    b16[1023] = 0xffff; assert(!nvrmLutIsBlack(1, 1024, 16, b16));
    puts("PASS: all-zero tables are black for 1 and 3 channels, 8 and 16 bits; one nonzero entry clears the guard");
}
'''
with tempfile.TemporaryDirectory() as td:
    temp = pathlib.Path(td); cpp = temp / 'check.cpp'
    cpp.write_text(fixture + '\n' + body + '\n' + checks, encoding='utf-8')
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Wextra', '-Werror', str(cpp), '-o', str(temp / 'check')], check=True)
    subprocess.run([str(temp / 'check')], check=True)
print('PASS: gamma guard rejects only fully black tables and runs before the LUT is stored')

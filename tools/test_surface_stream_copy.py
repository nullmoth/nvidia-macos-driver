#!/usr/bin/env python3
"""Check production surface-copy bounds with guard pages and varied alignment."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'plugin/nvmtl_vk.c').read_text()
start = source.index('#if defined(__x86_64__)\n#include <immintrin.h>')
# the copy helpers end where the image writers begin (the imported-pages writer comes first)
end = min(i for i in (source.find('int nvmtl_vk_image_write_host(', start), source.index('int nvmtl_vk_image_write(', start)) if i >= 0)
helpers = source[start:end]
fixture = r'''
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
'''
checks = r'''
int main(void) {
    size_t page=(size_t)sysconf(_SC_PAGESIZE), span=page*8;
    unsigned char *mapping=mmap(NULL,span+page,PROT_READ|PROT_WRITE,MAP_ANON|MAP_PRIVATE,-1,0);
    assert(mapping!=MAP_FAILED && !mprotect(mapping+span,page,PROT_NONE));
    unsigned char *output=malloc(span+64); assert(output);
    for(size_t i=0;i<span;i++) mapping[i]=(unsigned char)(i*37+19);
    const size_t lengths[]={0,1,15,16,63,64,65,4095,4096,4097,8191,8192};
    for(unsigned offset=0;offset<32;offset++) for(unsigned dst=0;dst<32;dst++)
        for(unsigned k=0;k<sizeof lengths/sizeof lengths[0];k++) {
            size_t n=lengths[k]; memset(output,0xcd,span+64);
            nvmtl_copy_from_surface(output+32+dst,mapping+offset,n);
            assert(!memcmp(output+32+dst,mapping+offset,n));
            for(size_t i=0;i<32+dst;i++) assert(output[i]==0xcd);
            for(size_t i=32+dst+n;i<span+64;i++) assert(output[i]==0xcd);
        }
    for(size_t tail=0;tail<128;tail++) {
        size_t n=8192+tail; unsigned char *src=mapping+span-n;
        nvmtl_copy_from_surface(output,src,n); assert(!memcmp(output,src,n));
    }
    munmap(mapping,span+page);free(output);
    puts("PASS: 12288 alignment/length cases, unchanged destination guards, 128 read-boundary cases");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.c').write_text(fixture + helpers + checks)
    subprocess.run(['clang', '-O2', str(path / 'test.c'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

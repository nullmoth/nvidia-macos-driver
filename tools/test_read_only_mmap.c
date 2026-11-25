#include <assert.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static int host_range_readonly(const void *ptr, size_t len) {
    if (!ptr || !len) return 0;
    mach_vm_address_t addr = (mach_vm_address_t)(uintptr_t)ptr;
    const mach_vm_address_t end = addr + (mach_vm_size_t)len;
    if (end < addr) return 0;
    while (addr < end) {
        mach_vm_address_t cur = addr;
        mach_vm_size_t size = 0;
        vm_region_basic_info_data_64_t info;
        mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t obj = MACH_PORT_NULL;
        kern_return_t kr = mach_vm_region(mach_task_self(), &cur, &size, VM_REGION_BASIC_INFO_64,
                                          (vm_region_info_t)&info, &count, &obj);
        if (obj != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), obj);
        if (kr != KERN_SUCCESS || cur > addr || !size) return 0;
        if (!(info.protection & VM_PROT_WRITE)) return 1;
        if (size >= end - cur) break;
        addr = cur + size;
    }
    return 0;
}

int main(void) {
    const size_t pg = (size_t)getpagesize();
    void *rw = mmap(NULL, 2 * pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    assert(rw != MAP_FAILED);
    assert(host_range_readonly(rw, 2 * pg) == 0);
    assert(mprotect((char *)rw + pg, pg, PROT_READ) == 0);
    assert(host_range_readonly(rw, pg) == 0);
    assert(host_range_readonly(rw, 2 * pg) == 1);
    assert(host_range_readonly((char *)rw + pg, pg) == 1);
    munmap(rw, 2 * pg);

    puts("test_read_only_mmap: OK");
    return 0;
}

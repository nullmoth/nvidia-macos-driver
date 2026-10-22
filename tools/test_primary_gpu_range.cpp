#include "../kexts/NVPrimarySelector/nvrm_gpu_select_policy.h"
#include <cassert>
#include <cstdint>
#include <cstdio>
int main(){
    using NvPrimaryGpu::contains;
    constexpr uint64_t base=0x10024000000ULL,bytes=35389440;
    assert(contains(base,67108864,base,bytes));
    assert(!contains(0x1002c000000ULL,67108864,base,bytes));
    assert(!contains(0x1805c000000ULL,67108864,base,bytes));
    assert(!contains(0x18054000000ULL,67108864,base,bytes));
    assert(contains(base,bytes,base,bytes));
    assert(!contains(base,bytes-1,base,bytes));
    assert(!contains(base,bytes,base-1,bytes));
    assert(!contains(base,bytes,base+1,bytes));
    assert(!contains(base,bytes,base,0));
    assert(!contains(0,bytes,base,bytes));
    assert(!contains(UINT64_MAX-15,16,8,8));
    puts("PASS unique console range from four-card report, exact boundaries and overflow-safe refusal");
}

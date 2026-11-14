#!/usr/bin/env python3
"""Compile the actual calibration functions against CPU-only Vulkan mocks."""
import argparse
import tempfile
import hashlib
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--clvk-tree', type=Path, required=True)
args = parser.parse_args()
output = tempfile.TemporaryDirectory(prefix='clvk-calibration-cpu-')
ROOT = Path(output.name)
SOURCE = args.clvk_tree / 'src/device.cpp'


def extract(text, name):
    start = text.index('cl_int cvk_device::'+name+'(')
    brace = text.index('{', start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


PREFIX = r'''
#include <cstdint>
#include <cstdio>
#include <limits>
#include <mutex>
#include <vector>
using cl_ulong = uint64_t;
using cl_int = int;
constexpr int VK_SUCCESS=0, CL_SUCCESS=0, CL_OUT_OF_RESOURCES=-5;
constexpr int VK_STRUCTURE_TYPE_CALIBRATED_TIMESTAMP_INFO_EXT=1;
constexpr int VK_TIME_DOMAIN_CLOCK_MONOTONIC_EXT=2, VK_TIME_DOMAIN_DEVICE_EXT=3;
struct VkCalibratedTimestampInfoEXT { int type; const void* next; int domain; };
struct Sample { uint64_t host,device,deviation; int result; };
static std::vector<Sample> samples;
static std::vector<uint32_t> requests;
static unsigned calls;
static int query(uintptr_t,uint32_t n,const VkCalibratedTimestampInfoEXT*,uint64_t* t,uint64_t* d) {
    requests.push_back(n);
    if(calls>=samples.size()) { ++calls; return -88; }
    Sample s=samples[calls++];t[0]=s.host;if(n==2)t[1]=s.device;*d=s.deviation;return s.result;
}
#define cvk_error_fn(...) ((void)0)
struct cvk_device {
    struct { decltype(&query) vkGetCalibratedTimestampsEXT=&query; } m_vkfns;
    uintptr_t vulkan_device() const { return 0; }
    uint64_t timestamp_to_ns(uint64_t ticks) const { return ticks*2; }
    cl_int get_device_host_timer(cl_ulong*,cl_ulong*,uint64_t* = nullptr) const;
    cl_int update_device_host_timer_no_lock();
    cl_int device_timer_to_host(cl_ulong,cl_ulong&,uint64_t* = nullptr);
    std::mutex m_sync_mutex;
    cl_ulong m_sync_host=0,m_sync_dev=0;
    uint64_t m_sync_deviation=0;
};
static void reset(const std::vector<uint64_t>& deviations,int fail=-1) {
    samples.clear();requests.clear();calls=0;
    for(unsigned i=0;i<deviations.size();i++)
        samples.push_back({100000+i,10000+i,deviations[i],static_cast<int>(i)==fail?-77:0});
}
#define REQUIRE(x) do { if(!(x)) { std::fprintf(stderr,"CPU_TEST_FAIL line=%d %s\n",__LINE__,#x); return 1; } } while(0)
'''

TESTS = r'''
int main() {
    cvk_device d;
    const auto max=std::numeric_limits<uint64_t>::max();
    const std::vector<std::vector<uint64_t>> cases={
        {max,max,max,max,max,max,max,max},
        {13550552,25,80,90,40,60,70,50},
        {10,20,30,40,50,60,70,13550552},
        {100,90,80,70,60,50,40,1},
        {7,7,7,7,7,7,7,7},
        {max,max,2,2,3,4,5,6}};
    const unsigned winners[]={0,1,0,7,0,2};
    for(unsigned c=0;c<cases.size();c++) {
        reset(cases[c]);cl_ulong dev=11,host=22;uint64_t deviation=33;
        REQUIRE(d.get_device_host_timer(&dev,&host,&deviation)==CL_SUCCESS);
        REQUIRE(calls==8);for(auto n:requests) REQUIRE(n==2);
        REQUIRE(host==100000+winners[c]);REQUIRE(dev==2*(10000+winners[c]));
        REQUIRE(deviation==cases[c][winners[c]]);
    }
    reset({987,1,1,1,1,1,1,1});cl_ulong host=22;uint64_t deviation=33;
    REQUIRE(d.get_device_host_timer(nullptr,&host,&deviation)==CL_SUCCESS);
    REQUIRE(calls==1 && requests[0]==1 && host==100000 && deviation==987);
    for(int failure=0;failure<8;failure++) {
        reset({8,7,6,5,4,3,2,1},failure);cl_ulong dev=11;host=22;deviation=33;
        REQUIRE(d.get_device_host_timer(&dev,&host,&deviation)==CL_OUT_OF_RESOURCES);
        REQUIRE(calls==static_cast<unsigned>(failure+1));
        REQUIRE(dev==11 && host==22 && deviation==33);
    }
    reset({1},0);host=22;deviation=33;
    REQUIRE(d.get_device_host_timer(nullptr,&host,&deviation)==CL_OUT_OF_RESOURCES);
    REQUIRE(calls==1 && host==22 && deviation==33);
    reset({8,7,6,5,4,3,2,1});cl_ulong dev=0;host=0;
    REQUIRE(d.get_device_host_timer(&dev,&host)==CL_SUCCESS);
    REQUIRE(calls==8 && dev==20014 && host==100007);
    reset({9});d.m_sync_dev=1000;d.m_sync_host=5000;d.m_sync_deviation=17;
    host=22;deviation=33;
    REQUIRE(d.device_timer_to_host(900,host,&deviation)==CL_SUCCESS);
    REQUIRE(calls==0 && host==4900 && deviation==17);
    d.m_sync_dev=200;d.m_sync_host=100;host=22;
    REQUIRE(d.device_timer_to_host(150,host,&deviation)==CL_SUCCESS);
    REQUIRE(calls==0 && host==50 && deviation==17);
    reset({80,60,40,8,30,20,10,9});d.m_sync_dev=0;d.m_sync_host=10;
    REQUIRE(d.device_timer_to_host(19000,host,&deviation)==CL_SUCCESS);
    REQUIRE(calls==8 && host==98997 && deviation==8);
    REQUIRE(d.m_sync_dev==20006 && d.m_sync_host==100003 && d.m_sync_deviation==8);
    reset({8,7,6,5,4,3,2,1},4);d.m_sync_dev=0;d.m_sync_host=999;d.m_sync_deviation=888;
    host=22;deviation=33;
    REQUIRE(d.device_timer_to_host(19000,host,&deviation)==CL_OUT_OF_RESOURCES);
    REQUIRE(calls==5 && host==22 && deviation==33);
    REQUIRE(d.m_sync_dev==0 && d.m_sync_host==999 && d.m_sync_deviation==888);
    std::puts("CPU calibration tests PASS: max/ties/outliers/pairing/host-only/errors/mapping metadata");
    return 0;
}
'''


def main():
    text = SOURCE.read_text()
    extracted = '\n\n'.join(extract(text, name) for name in (
        'get_device_host_timer', 'update_device_host_timer_no_lock', 'device_timer_to_host'))
    results = {}
    for name, functions, expected in (
        ('actual', extracted, 0),
        ('missing_first_winner', extracted.replace('attempt == 0 || ', ''), 1),
    ):
        cpp=ROOT/(name+'.cpp'); binary=ROOT/name
        cpp.write_text(PREFIX+'\n'+functions+'\n'+TESTS)
        subprocess.run(['nice','-n','19','xcrun','clang++','-std=c++17','-O2','-Wall','-Wextra','-Werror',str(cpp),'-o',str(binary)],check=True)
        result=subprocess.run([str(binary)],capture_output=True,text=True)
        if result.returncode != expected:
            raise RuntimeError(name+': '+result.stdout+result.stderr)
        results[name]={'returncode':result.returncode,'stdout':result.stdout.strip(),'stderr':result.stderr.strip()}
    report={'status':'PASS','gpu_calls':False,'source':str(SOURCE),'source_sha256':hashlib.sha256(SOURCE.read_bytes()).hexdigest(),'tests':results}
    (ROOT/'result.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))


if __name__ == '__main__':
    main()

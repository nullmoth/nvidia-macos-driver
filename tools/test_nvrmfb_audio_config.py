from pathlib import Path
root=Path(__file__).resolve().parents[1]
source=(root/'kexts/NVRMFB/fb/nvrm-audio-auto.inc').read_text(encoding='utf-8')
validate=source[source.index('static bool audioCodecValid'):source.index('void NVRMNVDAFramebuffer::publish')]
selection=source[source.index('        OSData *explicitCodec'):source.index('        const NMHDATopology *topology')]
stub=r'''
#include <cstdint>
#include <cstring>
#include <vector>
#include <map>
#include <string>
#include <cassert>
#include <cstdio>
#include "kexts/NVRMFB/nvrm_audio_match.h"
struct OSObject { virtual ~OSObject() = default; };
#define OSDynamicCast(T,x) dynamic_cast<T *>(x)
struct OSData : OSObject {
    std::vector<uint8_t> data;
    OSData(std::initializer_list<uint8_t> d):data(d){}
    unsigned getLength(){return data.size();}
    const void *getBytesNoCopy(){return data.data();}
    bool isEqualTo(OSData *other){return data == other->data;}
};
struct OSNumber : OSObject {
    uint64_t value; OSNumber(uint64_t v):value(v){}
    uint64_t unsigned64BitValue(){return value;}
};
struct OSDictionary : OSObject {
    std::map<std::string,OSObject *> values;
    OSObject *getObject(const char *key){auto it=values.find(key);return it==values.end()?nullptr:it->second;}
    OSObject *copyProperty(const char *key){return getObject(key);}
};
struct OSArray : OSObject {
    std::vector<OSObject *> values;
    unsigned getCount(){return values.size();}
    OSObject *getObject(unsigned i){return values.at(i);}
};
struct Result { bool valid,error; uint8_t pin; const char *source; };
'''
test=r'''
int main(){
    OSData p5{0,1,5,0},p6{0,1,6,0},bad{0,1};
    OSNumber zero{0},one{1};
    OSDictionary entry,other,props;
    entry.values={{"PhysicalIndex",&zero},{"CodecInfo",&p6}};
    other.values={{"PhysicalIndex",&one},{"CodecInfo",&p5}};
    OSArray entries; entries.values={&entry,&other};
    props.values={{"NVRMFBDisplayAudioOverrides",&entries},{"NVRMFBDisplayAudioCodecInfo",&p5}};
    auto r=resolve(&props,1,0,true); assert(r.valid&&!r.error&&r.pin==6);
    r=resolve(&props,2,1,true); assert(r.valid&&r.pin==5);
    props.values.erase("NVRMFBDisplayAudioOverrides");
    r=resolve(&props,1,0,true); assert(r.valid&&r.pin==5);
    r=resolve(&props,2,0,true); assert(!r.valid&&!r.error);
    props.values["NVRMFBDisplayAudioCodecInfo"]=&bad;
    r=resolve(&props,1,0,true); assert(!r.valid&&r.error);
    props.values.clear(); props.values["NVRMFBDisplayAudioOverrides"]=&entries;
    other.values["PhysicalIndex"]=&zero;
    r=resolve(&props,2,0,true); assert(!r.valid&&r.error);
    other.values["PhysicalIndex"]=&one; other.values["CodecInfo"]=&p6;
    r=resolve(&props,2,0,true); assert(!r.valid&&r.error);
    other.values["CodecInfo"]=&p5;
    r=resolve(&props,2,0,false); assert(!r.valid&&r.error);
    puts("PASS: extracted production configuration selection: precedence, legacy single/multi, malformed, duplicate physical interface, conflicting pin, unknown connector");
}
'''
import subprocess, tempfile, os
with tempfile.TemporaryDirectory() as directory:
    path=Path(directory)/'test-config.cpp'
    path.write_text(stub+validate+'''
Result resolve(OSDictionary *me,unsigned external,uint32_t physicalIndex,bool haveConnector){
struct {uint32_t physicalIndex;} connector{physicalIndex};
'''+selection+'return {valid,error,chosen[2],source};\n}\n'+test,encoding='utf-8')
    binary=Path(directory)/'test-config'
    subprocess.run([os.environ.get('CXX','clang++'),'-std=c++17','-Wall','-Wextra','-Werror',
        '-fsanitize=address,undefined','-I',str(root),str(path),'-o',str(binary)],check=True)
    subprocess.run([str(binary)],check=True)

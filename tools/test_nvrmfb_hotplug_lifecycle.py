#!/usr/bin/env python3
'''Compile and exercise production NVRMFB connection and DDC methods.'''
import pathlib, subprocess, sys, tempfile
here = pathlib.Path(__file__).resolve()
roots = [pathlib.Path(sys.argv[1]).resolve()] if len(sys.argv) > 1 else [here.parent.parent, here.parent.parent.parent]
root = next((p for p in roots if (p / 'kexts/NVRMFB/fb/nvrm-fb.cpp').exists() or (p / 'source.cpp').exists()), None)
if root is None: raise SystemExit('source.cpp or kexts/NVRMFB/fb/nvrm-fb.cpp not found')
srcpath = root / 'kexts/NVRMFB/fb/nvrm-fb.cpp'
if not srcpath.exists(): srcpath = root / 'source.cpp'
src = srcpath.read_text(encoding='utf-8')
def extract(text, marker):
    start = text.index(marker); opening = text.index('{', start); depth = 0
    for pos in range(opening, len(text)):
        if text[pos] == '{': depth += 1
        elif text[pos] == '}':
            depth -= 1
            if not depth: return text[start:pos + 1]
    raise AssertionError(f'unclosed production function: {marker}')
attrs = extract(src, 'IOReturn NVRMNVDAFramebuffer::getAttributeForConnection(')
ddc = extract(src, 'IOReturn NVRMNVDAFramebuffer::getDDCBlock(')
notify = extract(src, 'void NVRMNVDAFramebuffer::notifyConnectionChanged(')
callback = extract(src, 'void NVRMNVDAFramebuffer::connectNotifyFire(')
assert 'scheduleCall(fConnectNotify, deadline)' in notify and 'thread_call_enter' not in notify and 'startLock()' not in notify
assert 'NVRMCallReference callReference(me)' in callback and 'me->fConnect.proc(me->fConnect.target, me->fConnect.ref)' in callback
assert '&fConnectNotify' in extract(src, 'void NVRMNVDAFramebuffer::stop(')
assert src.index('fConnectNotify = nvrmfbCall(connectNotifyFire, this)') < src.index('super::start(provider)')
assert 'return !fDpyLost && fEdidSize >= 128;' in src
hotplug = extract(src, 'void NVRMNVDAFramebuffer::hotplugFire(')
missing = hotplug[hotplug.index('for (int k = 0; k < gNDpy; k++'):]
missing = missing[:missing.index('if (!added) return;')]
assert 'if (!me->fKms->getDynamicDisplayInfo(me->fDev, dd) || dd->connected) continue;' in missing
assert 'fb->fDpyLost = true;' in missing and 'fb->fEdidSize = 0;' in missing and 'fb->clearDisplayAudioProperties();' in missing
assert hotplug.index('if (sl) IORecursiveLockUnlock(sl);') < hotplug.index('notify[f]->notifyConnectionChanged(500);')
assert hotplug.index('if (sl) IORecursiveLockUnlock(sl);') < hotplug.index('fb->reflip()')
assert 'fb->fDpyLost = false;' in hotplug and 'fb->notifyConnectionChanged(500);' in hotplug
assert 'fb->fSurf =' not in hotplug and 'fb->fSize =' not in hotplug
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
using IOIndex=unsigned; using IOSelect=unsigned; using IOByteCount=unsigned long; using IOOptionBits=unsigned;
using UInt32=unsigned; using UInt8=unsigned char; using IOReturn=int; using thread_call_param_t=void*; using thread_call_t=void*;
enum { kIOReturnSuccess=0,kIOReturnUnsupported=-1,kIOReturnBadArgument=-2,kConnectionEnable=1,kConnectionFlags=2,
kConnectionColorModesSupported=3,kConnectionColorDepthsSupported=4,kConnectionControllerColorDepth=5,
kConnectionControllerDepthsSupported=6,kConnectionColorMode=7,kConnectionCheckEnable=8,kConnectionDisplayFlags=9,
kConnectionSupportsHLDDCSense=10,kConnectionSupportsAppleSense=11,kConnectionSupportsLLDDCSense=12,
kConnectionDisplayParameterCount=13,kConnectionDisplayParameters=14,kIOConnectionBuiltIn=1,kIODDCBlockTypeEDID=1 };
#define FBSEL(a) char _s[5]={(char)((a)>>24),(char)((a)>>16),(char)((a)>>8),(char)(a),0}
#define FBLOG(...) ((void)0)
static void nvu_copy(void*d,const void*s,size_t n){std::memcpy(d,s,n);}
static void clock_interval_to_deadline(unsigned ms,int,std::uint64_t*d){*d=ms;} static int kMillisecondScale=1;
struct OSObject{int refs=1;void retain(){++refs;}void release(){--refs;}};
class Base{public:IOReturn getAttributeForConnection(IOIndex,IOSelect,std::uintptr_t*){return 77;}};
struct NVRMNVDAFramebuffer:OSObject,Base{
using super=Base; bool fDpyLost=false,fInternal=false,fConnectFired=false; unsigned fEdidSize=0,fIndex=0; UInt8 fEdid[256]={};
volatile int fStopping=0; thread_call_t fConnectNotify=reinterpret_cast<void*>(1);
struct{void(*proc)(void*,void*);void*target;void*ref;}fConnect{}; unsigned scheduled=0;std::uint64_t deadline=0;
void scheduleCall(thread_call_t,std::uint64_t d){++scheduled;deadline=d;retain();}
IOReturn getAttributeForConnection(IOIndex,IOSelect,std::uintptr_t*); IOReturn getDDCBlock(IOIndex,UInt32,IOSelect,IOOptionBits,UInt8*,IOByteCount*);
void notifyConnectionChanged(unsigned); static void connectNotifyFire(thread_call_param_t,thread_call_param_t);
};
class NVRMCallReference{OSObject*p;public:explicit NVRMCallReference(OSObject*v):p(v){}~NVRMCallReference(){p->release();}};
static int OSAddAtomic(int,volatile int*v){return *v;}
'''
checks = r'''
int main(){NVRMNVDAFramebuffer f;std::uintptr_t v=0;f.fEdidSize=128;
assert(f.getAttributeForConnection(0,kConnectionEnable,&v)==kIOReturnSuccess&&v==1);
assert(f.getAttributeForConnection(0,kConnectionCheckEnable,&v)==kIOReturnSuccess&&v==1);
assert(f.getAttributeForConnection(0,99,&v)==77);f.fDpyLost=true;f.fEdidSize=0;
assert(f.getAttributeForConnection(0,kConnectionEnable,&v)==kIOReturnSuccess&&v==0);
assert(f.getAttributeForConnection(0,kConnectionCheckEnable,&v)==kIOReturnSuccess&&v==0);
assert(f.getAttributeForConnection(0,kConnectionSupportsHLDDCSense,&v)==kIOReturnUnsupported);
UInt8 out[128]={};IOByteCount n=128;assert(f.getDDCBlock(0,1,kIODDCBlockTypeEDID,0,out,&n)==kIOReturnUnsupported);
f.fDpyLost=false;f.fEdidSize=128;for(unsigned i=0;i<128;i++)f.fEdid[i]=(UInt8)i;n=128;
assert(f.getDDCBlock(0,1,kIODDCBlockTypeEDID,0,out,&n)==kIOReturnSuccess&&n==128&&out[127]==127);
n=127;assert(f.getDDCBlock(0,1,kIODDCBlockTypeEDID,0,out,&n)==kIOReturnBadArgument);
assert(f.getDDCBlock(0,0,kIODDCBlockTypeEDID,0,out,&n)==kIOReturnUnsupported);
assert(f.getDDCBlock(0,1,99,0,out,&n)==kIOReturnUnsupported);f.notifyConnectionChanged(500);
assert(f.scheduled==1&&f.deadline==500&&f.refs==2);unsigned calls=0;
f.fConnect.proc=[](void*t,void*r){++*static_cast<unsigned*>(t);assert(r==reinterpret_cast<void*>(7));};
f.fConnect.target=&calls;f.fConnect.ref=reinterpret_cast<void*>(7);NVRMNVDAFramebuffer::connectNotifyFire(&f,nullptr);
assert(calls==1&&f.fConnectFired&&f.refs==1);puts("PASS: extracted production connection, DDC, scheduling, callback methods");}
'''
with tempfile.TemporaryDirectory() as td:
    temp=pathlib.Path(td); cpp=temp/'check.cpp'; cpp.write_text(fixture+'\n'+attrs+'\n'+ddc+'\n'+notify+'\n'+callback+'\n'+checks,encoding='utf-8')
    subprocess.run(['clang++','-std=c++17','-Wall','-Wextra',str(cpp),'-o',str(temp/'check')],check=True)
    subprocess.run([str(temp/'check')],check=True)
print('PASS: hotplug transition path; omitted handles require successful offline query; callback and relight run outside display gate; scanout stays allocated')

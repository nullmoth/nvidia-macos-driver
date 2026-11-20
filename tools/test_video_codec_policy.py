#!/usr/bin/env python3
"""Execute patched codec-family selection and VP8 header parsing under sanitizers."""
import argparse
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'build'))
sys.path.insert(0, str(ROOT / 'tools/rotation'))
from build_nvenc import prepare
from test_support import method

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('source', type=Path)
args = parser.parse_args()
prepare(args.source.resolve())
source = (args.source / 'vt/nvencvt.c').read_text()
enum = re.search(r'enum \{ GPU_NONE[^;]+;', source).group(0)
policy = method(source, 'static int pci_codec_family(') + '\n' + method(source, 'static bool codec_external_encoder(')
vp8 = (args.source / 'src/vp8dec.c').read_text()
header = (args.source / 'src/vp8dec.h').read_text()
structure = header[header.index('typedef struct {'):header.index('} Vp8Header;') + len('} Vp8Header;')]
parsing = vp8[vp8.index('typedef struct { const uint8_t'):vp8.index('int vp8dec_open(')]
fixture = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
@@ENUM@@
@@POLICY@@
@@STRUCTURE@@
@@PARSING@@
int main(void) {
 assert(pci_codec_family(0x10de,0x1f06,0x030000)==GPU_TU10X);
 assert(pci_codec_family(0x10de,0x1e84,0x030000)==GPU_TU10X);
 assert(pci_codec_family(0x10de,0x2184,0x030000)==GPU_TU10X);
 assert(pci_codec_family(0x10de,0x1f82,0x030000)==GPU_TU117);
 assert(pci_codec_family(0x10de,0x1f97,0x030000)==GPU_NONE);
 assert(pci_codec_family(0x10de,0x1f98,0x030000)==GPU_NONE);
 assert(pci_codec_family(0x10de,0x1f9c,0x030000)==GPU_NONE);
 assert(pci_codec_family(0x10de,0x1f99,0x030000)==GPU_TU117); // Adjacent GTX1650 retains codecs.
 assert(pci_codec_family(0x10de,0x1f9f,0x030000)==GPU_TU117); // MX550 has media engines.
 assert(pci_codec_family(0x10de,0x2489,0x030000)==GPU_AMPERE);
 assert(pci_codec_family(0x10de,0x2704,0x030200)==GPU_ADA);
 assert(pci_codec_family(0x10de,0x228e,0x040300)==GPU_NONE); // HDMI audio is not a GPU
 assert(pci_codec_family(0x1002,0x1f06,0x030000)==GPU_NONE);
 assert(pci_codec_family(0x10de,0x1f06,0)==GPU_NONE);
 assert(pci_codec_family(0x10de,0x2b85,0x030000)==GPU_NONE); // no unqualified Blackwell codec takeover
 unsetenv("NVENCVT_AMPERE_ENC");
 assert(codec_external_encoder(GPU_ADA) && codec_external_encoder(GPU_TU117));
 assert(!codec_external_encoder(GPU_TU10X) && !codec_external_encoder(GPU_AMPERE) && !codec_external_encoder(GPU_NONE));
 setenv("NVENCVT_AMPERE_ENC","1",1);
 assert(codec_external_encoder(GPU_AMPERE) && !codec_external_encoder(GPU_TU10X));
 uint8_t frame[11]={0x30,0,0,0x9d,1,0x2a,16,0,16,0,0};Vp8Header h;
 assert(!vp8_parse_header(frame,sizeof frame,&h) && h.key && h.width==16 && h.height==16);
 frame[0]=0x10;assert(vp8_parse_header(frame,sizeof frame,&h)==-1); // empty first partition
 frame[0]=0x38;assert(vp8_parse_header(frame,sizeof frame,&h)==-1); // reserved version
 assert(vp8_parse_header(frame,2,&h)==-1);
 uint32_t random=1;uint8_t bytes[128];
 for(unsigned i=0;i<50000;i++) {
  for(unsigned n=0;n<sizeof bytes;n++){random=random*1664525u+1013904223u;bytes[n]=random>>24;}
  (void)vp8_parse_header(bytes,i%sizeof bytes,&h);
 }
 puts("PASS: GPU families, display/audio filtering, Turing encoder preservation, malformed/truncated VP8 and 50000 bounded random headers");
}
'''.replace('@@ENUM@@', enum).replace('@@POLICY@@', policy).replace('@@STRUCTURE@@', structure).replace('@@PARSING@@', parsing)
with tempfile.TemporaryDirectory(prefix='nvenc-policy-') as directory:
    p = Path(directory)
    (p / 'test.c').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-O1', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-fno-sanitize-recover=all', str(p / 'test.c'), '-o', str(p / 'test')], check=True)
    subprocess.run([str(p / 'test')], check=True, timeout=30)

# Run the complete production registrars with only OS codec-registration calls
# replaced. This checks Ada/Ampere/TU117 retention without running their engine
# commands on the Turing test machine.
decoder = (args.source / 'vt/nvencvt_dec.c').read_text()
definitions = '\n'.join(re.findall(r'^#define (?:ENCODER_ID|DECODER_ID)[^\n]+', source + '\n' + decoder, re.M))
codec_enums = '\n'.join(re.findall(r'^enum \{ CODEC_[^;]+;', decoder, re.M))
registrars = method(source, '__attribute__((visibility("default"))) OSStatus NVEncVT_RegisterCodecs(')
registrars += '\n' + method(source, '__attribute__((visibility("default"))) OSStatus NVEncVT_Register(')
registrars += '\n' + method(decoder, 'OSStatus NVDecVT_Register(')
registration_fixture = r'''
#include <VideoToolbox/VideoToolbox.h>
#include <CoreFoundation/CoreFoundation.h>
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dlfcn.h>
#include <execinfo.h>
typedef uint32_t NvU32;
@@ENUM@@
@@CODEC_ENUMS@@
@@DEFINITIONS@@
#define NVENC_VERSION "policy-test"
static void ignored_log(const char *format,...){(void)format;}
#define LOG ignored_log
#define DLOG ignored_log
static int selected,gNvencvtGpu;
static unsigned encoders,decoders;
static bool gDLog;
static FILE *gDLogF;
static char gCardName[]="policy-test";
static int pci_gpu_kind(void){return selected;}
static int gpu_nvenc_kind(void){return selected;}
@@ENCODER_POLICY@@
static unsigned bit(FourCharCode codec){
 switch(codec){case kCMVideoCodecType_H264:return 1;case kCMVideoCodecType_HEVC:return 2;case CODEC_AV1:return 4;
 case CODEC_VP8:return 8;case CODEC_VP9:return 16;case kCMVideoCodecType_MPEG1Video:return 32;
 case kCMVideoCodecType_MPEG2Video:return 64;case kCMVideoCodecType_MPEG4Video:return 128;
 case CODEC_AVC3:return 256;case CODEC_HEV1:return 512;default:assert(0);return 0;}
}
static OSStatus register_one(FourCharCode codec,CFStringRef id,CFStringRef name,int rating){
 (void)id;(void)name;assert(rating==1100);encoders|=bit(codec);return noErr;
}
static void rename_encoders(const char **ids,const char **names,const char **codecs,int n){(void)ids;(void)names;(void)codecs;(void)n;}
static NvU32 nvdec_class(void){return selected==GPU_ADA?0xc9b0:selected==GPU_AMPERE?0xc7b0:0xc4b0;}
static OSStatus NVDecVT_CreateInstance(FourCharCode c,CFAllocatorRef a,void **o){(void)c;(void)a;(void)o;assert(0);return -1;}
static OSStatus record_decoder(FourCharCode c,CFDictionaryRef info,void *factory){assert(info&&factory);decoders|=bit(c);return noErr;}
static void supplemental(CMVideoCodecType c){assert(c==CODEC_VP9||c==CODEC_AV1);}
static void *find_symbol(void *handle,const char *name){
 (void)handle;
 if(!strcmp(name,"VTRegisterVideoDecoderWithInfo"))return (void*)record_decoder;
 if(!strcmp(name,"VTRegisterSupplementalVideoDecoderIfAvailable"))return (void*)supplemental;
 return NULL;
}
static int no_log_file(const char *name,int mode){(void)name;(void)mode;return -1;}
#define dlsym find_symbol
#define access no_log_file
@@REGISTRARS@@
int main(void){
 const int families[]={GPU_ADA,GPU_TU117,GPU_AMPERE,GPU_TU10X,GPU_NONE};
 const unsigned expectedEncoder[]={7,3,0,0,0};
 const unsigned legacy=1|2|16|32|64|128|256|512;
 const unsigned expectedDecoder[]={legacy|4,legacy,1|2|4|16|256,8|16,0};
 const char *settings[]={"NVENCVT_ANY_GPU","NVENCVT_AMPERE_ENC","NVENCVT_AMPERE_ALL_DEC","NVENCVT_TURING_ALL_DEC",
 "NVENCVT_AMPERE_H264_DEC","NVMTL_NO_NVDEC_HEVC",
 "NVENCVT_NO_DEC","NVENCVT_NO_HEVC","NVENCVT_NO_AV1","NVENCVT_NO_HEVCDEC","NVENCVT_NO_HEV1","NVENCVT_NO_VP9DEC",
 "NVENCVT_NO_VP8DEC","NVENCVT_NO_AV1DEC","NVENCVT_NO_MPEG2DEC","NVENCVT_NO_MPEG4DEC","NVENCVT_LOG"};
 for(unsigned n=0;n<sizeof settings/sizeof *settings;n++)unsetenv(settings[n]);
 for(unsigned n=0;n<sizeof families/sizeof *families;n++){
  selected=families[n];encoders=decoders=0;OSStatus status=NVEncVT_Register(1100);
  assert((status==noErr)==(selected!=GPU_NONE));assert(encoders==expectedEncoder[n]);assert(decoders==expectedDecoder[n]);
  for(unsigned scope=1;scope<=3;scope++){
   encoders=decoders=0;status=NVEncVT_RegisterCodecs(1100,scope);
   unsigned ee=(scope&2)?expectedEncoder[n]:0,dd=(scope&1)?expectedDecoder[n]:0;
   assert(encoders==ee&&decoders==dd);
   assert((status==noErr)==(ee!=0||dd!=0));
  }
 }
 encoders=decoders=0;
 assert(NVEncVT_RegisterCodecs(1100,0)!=noErr&&NVEncVT_RegisterCodecs(1100,4)!=noErr);
 assert(!encoders&&!decoders);
 selected=GPU_AMPERE;encoders=decoders=0;setenv("NVENCVT_AMPERE_ALL_DEC","1",1);
 assert(NVEncVT_RegisterCodecs(1100,1)==noErr&&encoders==0&&decoders==(legacy|4));
 unsetenv("NVENCVT_AMPERE_ALL_DEC");setprogname("VTDecoderXPCService");encoders=decoders=0;
 assert(NVEncVT_RegisterCodecs(1100,1)==noErr&&encoders==0&&decoders==(4|16));
 setenv("NVENCVT_AMPERE_H264_DEC","1",1);encoders=decoders=0;
 assert(NVEncVT_RegisterCodecs(1100,1)==noErr&&encoders==0&&decoders==(1|4|16|256));
 setprogname("codec-policy-test");unsetenv("NVENCVT_AMPERE_H264_DEC");
 setenv("NVMTL_NO_NVDEC_HEVC","1",1);encoders=decoders=0;
 assert(NVEncVT_RegisterCodecs(1100,1)==noErr&&encoders==0&&decoders==(1|4|16|256));
 unsetenv("NVMTL_NO_NVDEC_HEVC");setenv("NVENCVT_AMPERE_H264_DEC","0",1);encoders=decoders=0;
 assert(NVEncVT_RegisterCodecs(1100,1)==noErr&&encoders==0&&decoders==(2|4|16));
 unsetenv("NVENCVT_AMPERE_H264_DEC");
 selected=GPU_TU10X;encoders=decoders=0;setenv("NVENCVT_TURING_ALL_DEC","1",1);
 assert(NVEncVT_Register(1100)==noErr&&encoders==0&&decoders==(legacy|8));
 puts("PASS: complete registrar sets retain Ada/TU117/Ampere/TU10x codecs; decoder-only, encoder-only, combined and invalid scopes");
}
'''.replace('@@ENUM@@', enum).replace('@@CODEC_ENUMS@@', codec_enums).replace('@@DEFINITIONS@@', definitions)
registration_fixture = registration_fixture.replace('@@ENCODER_POLICY@@', method(source, 'static bool codec_external_encoder(')).replace('@@REGISTRARS@@', registrars)
with tempfile.TemporaryDirectory(prefix='nvenc-registration-') as directory:
    p = Path(directory)
    (p / 'test.c').write_text(registration_fixture)
    subprocess.run(['xcrun', 'clang', '-O1', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-fno-sanitize-recover=all', str(p / 'test.c'), '-framework', 'CoreFoundation', '-o', str(p / 'test')], check=True)
    subprocess.run([str(p / 'test')], check=True, timeout=30)

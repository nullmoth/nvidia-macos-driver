#!/usr/bin/env python3
"""Exercise the patched NVIDIA DVI validation and modeset conversion with sanitizers."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--ogkm', type=Path, default=os.environ.get('OGKM'), required=not os.environ.get('OGKM'),
               help='Pristine NVIDIA 620.70.06 source tree')
a = p.parse_args()
source = (a.ogkm / 'src/nvidia-modeset/kapi/src/nvkms-kapi.c').read_text()


def function(text, name):
    match = re.search(r'static\s+(?:void|int|NvBool)\s+' + name + r'\s*\(', text)
    assert match, name
    start = text.index('{', match.start())
    depth = 1
    end = start + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[match.start():end] + '\n'


fixture = r'''
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nvkms-kapi-internal.h"
#undef nvKmsKapiLogDeviceDebug
#define nvKmsKapiLogDeviceDebug(...) ((void)0)
#define nvEvoLog(...) ((void)0)
#define nvkms_memset memset
#define nvkms_memcpy memcpy
static NvKmsConnectorType connectorType=NVKMS_CONNECTOR_TYPE_DVI_D;
static NvKmsConnectorSignalFormat signalFormat=NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS;
static enum NvYuv420Mode disposition=NV_YUV420_MODE_NONE;
static NvBool staticOk=NV_TRUE,connectorOk=NV_TRUE,queryOk=NV_TRUE,modeOk=NV_TRUE;
static unsigned validationCalls,policyCalls;
static NvBool GetStaticDisplayInfo(struct NvKmsKapiDevice *d,NvKmsKapiDisplay display,
                                  struct NvKmsKapiStaticDisplayInfo *out) {
    if(!d || !staticOk) return NV_FALSE;
    out->connectorHandle=display;return NV_TRUE;
}
static NvBool GetConnectorInfo(struct NvKmsKapiDevice *d,NvKmsKapiConnector connector,
                              struct NvKmsKapiConnectorInfo *out) {
    if(!d || !connectorOk) return NV_FALSE;
    out->signalFormat=signalFormat;
    out->type=connector==0x100?connectorType:NVKMS_CONNECTOR_TYPE_HDMI;
    return NV_TRUE;
}
static NvBool isDvi(void) {
    return staticOk && connectorOk && signalFormat==NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS &&
        (connectorType==NVKMS_CONNECTOR_TYPE_DVI_D || connectorType==NVKMS_CONNECTOR_TYPE_DVI_I);
}
static void checkColor(const struct NvKmsDpyOutputColorParams *c) {
    ++policyCalls;
    if(isDvi()) {
        assert(c->formatSpecified && c->format==NV_KMS_DPY_ATTRIBUTE_REQUESTED_COLOR_FORMAT_RGB);
        assert(c->bpcSpecified && c->bpc==NV_KMS_DPY_ATTRIBUTE_CURRENT_COLOR_BPC_8);
    } else {
        const struct NvKmsDpyOutputColorParams zero={};
        assert(!memcmp(c,&zero,sizeof zero));
    }
}
static NvBool fakeIoctl(struct nvkms_per_open *open,NvU32 cmd,void *data,size_t size) {
    (void)open;++validationCalls;
    if(cmd==NVKMS_IOCTL_VALIDATE_MODE_INDEX) {
        assert(size==sizeof(struct NvKmsValidateModeIndexParams));
        struct NvKmsValidateModeIndexParams *v=data;checkColor(&v->request.dpyOutputColor);
        assert(v->request.modeValidation.overrides==NVKMS_MODE_VALIDATION_NO_RRX1K_CHECK);
        if(!queryOk)return NV_FALSE;
        v->reply.end=v->request.modeIndex>=2;v->reply.valid=modeOk;
        v->reply.yuv420Mode=disposition;v->reply.preferredMode=NV_TRUE;
        v->reply.mode.timings.hVisible=1920;v->reply.mode.timings.vVisible=1080;
        v->reply.mode.timings.pixelClockHz=148500000;
        return NV_TRUE;
    }
    assert(cmd==NVKMS_IOCTL_VALIDATE_MODE && size==sizeof(struct NvKmsValidateModeParams));
    struct NvKmsValidateModeParams *v=data;checkColor(&v->request.dpyOutputColor);
    if(!queryOk)return NV_FALSE;
    v->reply.valid=modeOk;v->reply.yuv420Mode=disposition;return NV_TRUE;
}
#define nvkms_ioctl_from_kapi fakeIoctl
static void NvKmsKapiCursorConfigToKms(const struct NvKmsKapiCursorRequestedConfig *c,
                                     struct NvKmsFlipCommonParams *p,NvBool full) {
    (void)c;(void)p;(void)full;
}
static NvBool NvKmsKapiLayerConfigToKms(struct NvKmsKapiDevice *d,
       const struct NvKmsKapiLayerRequestedConfig *c,NvU32 layer,NvU32 head,
       struct NvKmsFlipCommonParams *p,NvBool commit,NvBool full) {
    (void)d;(void)c;(void)layer;(void)head;(void)p;(void)commit;(void)full;return NV_TRUE;
}
static NvBool GetOutputTransferFunction(const struct NvKmsKapiHeadRequestedConfig *c,
                                       enum NvKmsOutputTf *tf) {
    (void)c;*tf=NVKMS_OUTPUT_TF_NONE;return NV_TRUE;
}
@@FUNCTIONS@@
int main(void) {
    struct NvKmsKapiDevice d={};d.hKmsDevice=1;d.hKmsDisp=2;
    struct NvKmsKapiDisplayMode m={};
    for(unsigned type=0;type<6;type++) {
        const NvKmsConnectorType types[]={NVKMS_CONNECTOR_TYPE_DVI_D,NVKMS_CONNECTOR_TYPE_DVI_I,
            NVKMS_CONNECTOR_TYPE_HDMI,NVKMS_CONNECTOR_TYPE_DP,NVKMS_CONNECTOR_TYPE_LVDS,NVKMS_CONNECTOR_TYPE_VGA};
        connectorType=types[type];
        for(unsigned signal=0;signal<2;signal++) {
            signalFormat=signal?NVKMS_CONNECTOR_SIGNAL_FORMAT_VGA:NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS;
            for(unsigned yuv=0;yuv<3;yuv++) {
                disposition=(enum NvYuv420Mode)yuv;
                for(unsigned bad=0;bad<3;bad++) {
                    staticOk=bad!=1;connectorOk=bad!=2;
                    const NvBool expected=!isDvi() || disposition==NV_YUV420_MODE_NONE;
                    NvBool valid=NV_FALSE,preferred=NV_FALSE;
                    assert(GetDisplayMode(&d,0x100,0,&m,&valid,&preferred)==1);
                    assert(valid==expected && preferred && m.timings.hVisible==1920 && m.timings.vVisible==1080);
                    assert(ValidateDisplayMode(&d,0x100,&m)==expected);
                }
            }
        }
    }
    staticOk=connectorOk=NV_TRUE;connectorType=NVKMS_CONNECTOR_TYPE_DVI_D;
    signalFormat=NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS;disposition=NV_YUV420_MODE_NONE;
    NvBool valid=NV_TRUE,preferred=NV_FALSE;
    assert(GetDisplayMode(&d,0x100,2,&m,&valid,&preferred)==0);
    assert(GetDisplayMode(&d,0x100,0,NULL,NULL,NULL)==1);
    modeOk=NV_FALSE;
    assert(GetDisplayMode(&d,0x100,0,&m,&valid,&preferred)==1 && !valid);
    assert(!ValidateDisplayMode(&d,0x100,&m));modeOk=NV_TRUE;
    queryOk=NV_FALSE;valid=NV_TRUE;
    assert(GetDisplayMode(&d,0x100,0,&m,&valid,&preferred)==-1 && valid);
    assert(!ValidateDisplayMode(&d,0x100,&m));queryOk=NV_TRUE;
    unsigned calls=validationCalls;
    assert(GetDisplayMode(NULL,0x100,0,&m,&valid,&preferred)==-1);
    assert(!ValidateDisplayMode(NULL,0x100,&m) && calls==validationCalls);

    // The real full modeset converter: DVI is RGB8 on both test and commit,
    // and the HDMI head's automatic format/depth remains byte-for-byte intact.
    struct NvKmsKapiRequestedModeSetConfig *c=calloc(1,sizeof *c);
    struct NvKmsSetModeParams *out=malloc(sizeof *out);
    assert(c && out);c->headsMask=3;
    for(unsigned h=0;h<2;h++) {
        struct NvKmsKapiHeadRequestedConfig *hr=&c->headRequestedConfig[h];
        hr->modeSetConfig.bActive=NV_TRUE;hr->modeSetConfig.numDisplays=1;
        hr->modeSetConfig.displays[0]=0x100u<<h;hr->modeSetConfig.mode=m;
        hr->modeSetConfig.colorFormat=NV_KMS_DPY_ATTRIBUTE_REQUESTED_COLOR_FORMAT_UNKNOWN;
    }
    for(unsigned commit=0;commit<2;commit++) {
        assert(NvKmsKapiRequestedModeSetConfigToKms(&d,c,out,commit));
        assert(out->request.disp[0].requestedHeadsBitMask==3);
        const struct NvKmsSetModeOneHeadRequest *dvi=&out->request.disp[0].head[0];
        const struct NvKmsSetModeOneHeadRequest *hdmi=&out->request.disp[0].head[1];
        assert(dvi->colorFormatSpecified && dvi->colorFormat==NV_KMS_DPY_ATTRIBUTE_REQUESTED_COLOR_FORMAT_RGB);
        assert(dvi->colorBpcSpecified && dvi->colorBpc==NV_KMS_DPY_ATTRIBUTE_CURRENT_COLOR_BPC_8);
        assert(hdmi->colorFormatSpecified && hdmi->colorFormat==NV_KMS_DPY_ATTRIBUTE_REQUESTED_COLOR_FORMAT_UNKNOWN);
        assert(!hdmi->colorBpcSpecified && hdmi->colorBpc==NV_KMS_DPY_ATTRIBUTE_CURRENT_COLOR_BPC_UNKNOWN);
    }
    free(c);free(out);assert(policyCalls>=216);
    puts("PASS: production indexed/explicit validation and full modeset; DVI RGB8 and YUV disposition; HDMI/DP/LVDS/VGA/analog isolation; query errors; null guards");
}
'''
names = ['NvKmsModeToKapi', 'InitNvKmsModeValidationParams', 'KapiDviRgb8',
         'GetDisplayMode', 'NvKmsKapiDisplayModeToKapi', 'ValidateDisplayMode',
         'NvKmsKapiRequestedModeSetConfigToKms']
with tempfile.TemporaryDirectory(prefix='nv-kapi-dvi-') as directory:
    temp = Path(directory)
    file = temp / 'kapi/src/nvkms-kapi.c'
    file.parent.mkdir(parents=True)
    file.write_text(source)
    for patch in sorted((ROOT / 'kexts/NVRM/patches').glob('*.patch')):
        if 'nvkms-kapi.c' not in patch.read_text():
            continue
        # Completion uses paths from the NVIDIA root; the other KAPI patches
        # are relative to src/nvidia-modeset.
        strip = 3 if 'a/src/nvidia-modeset/' in patch.read_text() else 1
        subprocess.run(['patch', '--batch', '--fuzz=0', '--no-backup-if-mismatch',
                        '-p'+str(strip), '-i', str(patch)], cwd=temp, check=True,
                       stdout=subprocess.DEVNULL)
    patched = file.read_text()
    functions = '\n'.join(function(patched, name) for name in names)
    text = fixture.replace('@@FUNCTIONS@@', functions)
    includes = [a.ogkm / item for item in [
        'src/nvidia-modeset/kapi/interface', 'src/nvidia-modeset/kapi/include',
        'src/nvidia-modeset/interface', 'src/nvidia-modeset/include',
        'src/nvidia-modeset/os-interface/include', 'src/nvidia/arch/nvalloc/unix/include',
        'src/common/sdk/nvidia/inc', 'src/common/inc', 'src/common/shared/inc',
        'src/common/unix/common/inc', 'src/common/unix/common/utils/interface',
        'src/common/unix/nvidia-push/interface', 'src/common/modeset']]
    variants = {'candidate': text,
                'without_disposition_filter': text.replace(
                    '(!dviRgb8 || paramsValidate.reply.yuv420Mode == NV_YUV420_MODE_NONE)', 'NV_TRUE'),
                'without_modeset_policy': text.replace(
                    'if (KapiDviRgb8(device, headModeSetConfig->displays[0], &color))', 'if (0)')}
    for name, code in variants.items():
        src = temp / (name+'.c');exe = temp / name
        src.write_text(code)
        command = ['clang', '-std=gnu11', '-g', '-O1', '-fsanitize=address,undefined',
                   '-fno-omit-frame-pointer', '-Werror=implicit-function-declaration']
        command += ['-I'+str(path) for path in includes]
        subprocess.run(command+[str(src), '-o', str(exe)], check=True)
        result = subprocess.run([str(exe)], capture_output=True, text=True)
        if name == 'candidate':
            assert result.returncode == 0, result.stderr
            print(result.stdout.strip())
        else:
            assert result.returncode != 0, 'Negative control passed: '+name
            print('PASS: negative control rejected: '+name)

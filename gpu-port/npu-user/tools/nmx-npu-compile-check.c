/*
 * nmx-npu-compile-check: compiles a small network for Intel NPUs through the same driver-compiler library Intel's NPU
 * user-mode driver loads (libopenvino_intel_npu_compiler_loader.dylib, beside libze_intel_npu), and reports the
 * compiled blob. It needs no NPU: the compiler is pure software, so this separates "the compiler works on this Mac"
 * from "the NPU runs what it compiled" in a report.
 *
 *   nmx-npu-compile-check [loader dylib] [platform...]     platforms: 3720 (Meteor/Arrow Lake), 4000 (Lunar Lake)
 *   NMX_MODEL_XML=<model.xml> [NMX_MODEL_BIN=<weights.bin>] [NMX_MODEL_OPTIONS=<io options>]: compile that OpenVINO IR
 *   instead (the options name its inputs and outputs, as --inputs_precisions="name:FP32" ...)
 *
 * The network is Parameter[1,3,16,16] f32 -> ReLU -> Result, serialized the way OpenVINO's NPU plugin hands a model to
 * the compiler: compiler version, input count (2), IR XML size + bytes, weights size + bytes.
 * Exit 0 when every platform compiled to a non-empty ELF blob.
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "npu_driver_compiler.h"

static const char model_xml[] =
    "<?xml version=\"1.0\"?>\n"
    "<net name=\"nmx_relu\" version=\"11\">\n"
    "  <layers>\n"
    "    <layer id=\"0\" name=\"data\" type=\"Parameter\" version=\"opset1\">\n"
    "      <data shape=\"1,3,16,16\" element_type=\"f32\"/>\n"
    "      <output><port id=\"0\" precision=\"FP32\" names=\"data\"><dim>1</dim><dim>3</dim><dim>16</dim><dim>16</dim>"
    "</port></output>\n"
    "    </layer>\n"
    "    <layer id=\"1\" name=\"relu\" type=\"ReLU\" version=\"opset1\">\n"
    "      <input><port id=\"0\" precision=\"FP32\"><dim>1</dim><dim>3</dim><dim>16</dim><dim>16</dim></port></input>\n"
    "      <output><port id=\"1\" precision=\"FP32\" names=\"relu\"><dim>1</dim><dim>3</dim><dim>16</dim><dim>16</dim>"
    "</port></output>\n"
    "    </layer>\n"
    "    <layer id=\"2\" name=\"out\" type=\"Result\" version=\"opset1\">\n"
    "      <input><port id=\"0\" precision=\"FP32\"><dim>1</dim><dim>3</dim><dim>16</dim><dim>16</dim></port></input>\n"
    "    </layer>\n"
    "  </layers>\n"
    "  <edges>\n"
    "    <edge from-layer=\"0\" from-port=\"0\" to-layer=\"1\" to-port=\"0\"/>\n"
    "    <edge from-layer=\"1\" from-port=\"1\" to-layer=\"2\" to-port=\"0\"/>\n"
    "  </edges>\n"
    "</net>\n";

/* the compiler tests' descriptors (vpux_driver_compiler/test/functional/vcl_tests_common.cpp) */
static int device_for(const char *platform, vcl_device_desc_t *d)
{
    memset(d, 0, sizeof(*d));
    d->size = sizeof(*d);
    if (strcmp(platform, "3720") == 0) {
        d->deviceID = 0x7D1D, d->revision = 3, d->tileCount = 2;
        return 0;
    }
    if (strcmp(platform, "4000") == 0) {
        d->deviceID = 0x643E, d->revision = 3, d->tileCount = 6;
        return 0;
    }
    return -1;
}

static uint8_t *read_file(const char *path, size_t *len)
{
    FILE *f = fopen(path, "rb");
    if (f == NULL)
        return NULL;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *b = n >= 0 ? malloc((size_t)n + 1) : NULL;
    if (b != NULL && fread(b, 1, (size_t)n, f) != (size_t)n) {
        free(b);
        b = NULL;
    }
    fclose(f);
    if (b != NULL)
        *len = (size_t)n;
    return b;
}

#define SYM(h, name) name##_fn name##_p = (name##_fn)dlsym(h, #name)
typedef vcl_result_t (*vclGetVersion_fn)(vcl_version_info_t *, vcl_version_info_t *);
typedef vcl_result_t (*vclCompilerCreate_fn)(vcl_compiler_desc_t *, vcl_device_desc_t *, vcl_compiler_handle_t *,
                                             vcl_log_handle_t *);
typedef vcl_result_t (*vclCompilerDestroy_fn)(vcl_compiler_handle_t);
typedef vcl_result_t (*vclCompilerGetProperties_fn)(vcl_compiler_handle_t, vcl_compiler_properties_t *);
typedef vcl_result_t (*vclExecutableCreate_fn)(vcl_compiler_handle_t, vcl_executable_desc_t, vcl_executable_handle_t *);
typedef vcl_result_t (*vclExecutableGetSerializableBlob_fn)(vcl_executable_handle_t, uint8_t *, uint64_t *);
typedef vcl_result_t (*vclExecutableDestroy_fn)(vcl_executable_handle_t);
typedef vcl_result_t (*vclLogHandleGetString_fn)(vcl_log_handle_t, size_t *, char *);

int main(int argc, char **argv)
{
    const char *path = argc > 1 ? argv[1] : "/Library/NullMoth/npu/lib/libopenvino_intel_npu_compiler_loader.dylib";
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (h == NULL) {
        printf("FAIL load %s: %s\n", path, dlerror());
        return 1;
    }
    SYM(h, vclGetVersion);
    SYM(h, vclCompilerCreate);
    SYM(h, vclCompilerDestroy);
    SYM(h, vclCompilerGetProperties);
    SYM(h, vclExecutableCreate);
    SYM(h, vclExecutableGetSerializableBlob);
    SYM(h, vclExecutableDestroy);
    SYM(h, vclLogHandleGetString);
    if (!vclGetVersion_p || !vclCompilerCreate_p || !vclCompilerDestroy_p || !vclCompilerGetProperties_p ||
        !vclExecutableCreate_p || !vclExecutableGetSerializableBlob_p || !vclExecutableDestroy_p) {
        printf("FAIL %s does not export the vcl API\n", path);
        return 1;
    }
    vcl_version_info_t cv = { 0 }, pv = { 0 };
    vclGetVersion_p(&cv, &pv);
    printf("compiler API %u.%u, profiling %u.%u\n", cv.major, cv.minor, pv.major, pv.minor);

    /* the model: the built-in ReLU network, or an IR from files */
    const char *xml_text = model_xml;
    uint64_t xml_size = sizeof(model_xml) - 1, weights_size = 0;
    uint8_t *weights = NULL;
    const char *mx = getenv("NMX_MODEL_XML"), *mb = getenv("NMX_MODEL_BIN");
    if (mx != NULL && *mx != '\0') {
        size_t n;
        char *x = (char *)read_file(mx, &n);
        if (x == NULL) {
            printf("FAIL cannot read %s\n", mx);
            return 1;
        }
        xml_text = x, xml_size = n;
        if (mb != NULL && *mb != '\0') {
            weights = read_file(mb, &n);
            if (weights == NULL) {
                printf("FAIL cannot read %s\n", mb);
                return 1;
            }
            weights_size = n;
        }
    }
    /* the serialized model: version, count, xml, weights */
    const uint32_t count = 2;
    const uint64_t ir_size = sizeof(cv) + sizeof(count) + sizeof(xml_size) + xml_size + sizeof(weights_size) + weights_size;
    uint8_t *ir = calloc(1, ir_size);
    if (ir == NULL)
        return 1;
    uint8_t *w = ir;
    memcpy(w, &cv, sizeof(cv)), w += sizeof(cv);
    memcpy(w, &count, sizeof(count)), w += sizeof(count);
    memcpy(w, &xml_size, sizeof(xml_size)), w += sizeof(xml_size);
    memcpy(w, xml_text, xml_size), w += xml_size;
    memcpy(w, &weights_size, sizeof(weights_size)), w += sizeof(weights_size);
    if (weights_size)
        memcpy(w, weights, weights_size);

    const char *defaults[] = { "3720", "4000" };
    const char **platforms = argc > 2 ? (const char **)argv + 2 : defaults;
    int nplat = argc > 2 ? argc - 2 : 2, failed = 0;
    for (int i = 0; i < nplat; i++) {
        vcl_device_desc_t dev;
        if (device_for(platforms[i], &dev) != 0) {
            printf("FAIL %s: unknown platform\n", platforms[i]);
            failed++;
            continue;
        }
        vcl_compiler_desc_t cd = { .version = { VCL_COMPILER_VERSION_MAJOR, VCL_COMPILER_VERSION_MINOR },
                                   .debugLevel = VCL_LOG_ERROR };
        vcl_compiler_handle_t c = NULL;
        vcl_log_handle_t log = NULL;
        vcl_result_t r = vclCompilerCreate_p(&cd, &dev, &c, &log);
        if (r != VCL_RESULT_SUCCESS) {
            printf("FAIL %s: vclCompilerCreate 0x%x\n", platforms[i], (unsigned)r);
            failed++;
            continue;
        }
        vcl_compiler_properties_t prop = { 0 };
        if (vclCompilerGetProperties_p(c, &prop) == VCL_RESULT_SUCCESS) {
            printf("%s: compiler %s (IR version %u.%u), opsets %u\n", platforms[i], prop.id ? prop.id : "?",
                   prop.version.major, prop.version.minor, prop.supportedOpsets);
            /* the IR carries the compiler's own version (OpenVINO's compiler_impl.cpp stamps _compilerProperties.version,
               not vclGetVersion's API version); the deserializer refuses any other */
            memcpy(ir, &prop.version, sizeof(prop.version));
        }
        char options[1024];
        const char *io = getenv("NMX_MODEL_OPTIONS");
        snprintf(options, sizeof(options), "%s --config NPU_PLATFORM=\"%s\"",
                 io != NULL && *io != '\0' ? io
                 : "--inputs_precisions=\"data:FP32\" --inputs_layouts=\"data:NCHW\" --outputs_precisions=\"relu:FP32\" "
                   "--outputs_layouts=\"relu:NCHW\"", platforms[i]);
        vcl_executable_desc_t ed = { .modelIRData = ir, .modelIRSize = ir_size, .options = options,
                                     .optionsSize = strlen(options) + 1 };
        vcl_executable_handle_t e = NULL;
        r = vclExecutableCreate_p(c, ed, &e);
        uint64_t blob_size = 0;
        if (r == VCL_RESULT_SUCCESS)
            r = vclExecutableGetSerializableBlob_p(e, NULL, &blob_size);
        uint8_t *blob = r == VCL_RESULT_SUCCESS && blob_size ? malloc(blob_size) : NULL;
        if (blob != NULL)
            r = vclExecutableGetSerializableBlob_p(e, blob, &blob_size);
        if (r == VCL_RESULT_SUCCESS && blob != NULL && blob_size >= 4 && memcmp(blob, "\x7f" "ELF", 4) == 0) {
            printf("OK   %s: compiled to a %llu-byte ELF blob\n", platforms[i], (unsigned long long)blob_size);
            /* NMX_SAVE_BLOB_DIR: keep the blob (nmx_relu_<platform>.blob), e.g. as test input for the NPU driver */
            const char *dir = getenv("NMX_SAVE_BLOB_DIR");
            if (dir != NULL && *dir != '\0') {
                char out[1024];
                snprintf(out, sizeof(out), "%s/nmx_relu_%s.blob", dir, platforms[i]);
                FILE *f = fopen(out, "wb");
                if (f == NULL || fwrite(blob, 1, blob_size, f) != blob_size) {
                    printf("FAIL %s: cannot write %s\n", platforms[i], out);
                    failed++;
                }
                if (f != NULL)
                    fclose(f);
            }
        } else {
            printf("FAIL %s: vclExecutableCreate/GetSerializableBlob 0x%x, %llu bytes\n", platforms[i], (unsigned)r,
                   (unsigned long long)blob_size);
            failed++;
            size_t n = 0;
            if (log != NULL && vclLogHandleGetString_p && vclLogHandleGetString_p(log, &n, NULL) == VCL_RESULT_SUCCESS &&
                n > 0) {
                char *s = malloc(n + 1);
                if (s != NULL && vclLogHandleGetString_p(log, &n, s) == VCL_RESULT_SUCCESS) {
                    s[n] = '\0';
                    printf("compiler log:\n%s\n", s);
                }
                free(s);
            }
        }
        free(blob);
        if (e != NULL)
            vclExecutableDestroy_p(e);
        vclCompilerDestroy_p(c);
    }
    free(ir);
    return failed ? 1 : 0;
}

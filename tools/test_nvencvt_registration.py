#!/usr/bin/env python3
"""Exercise the production NVMTL_NVENCVT selector and check its guarded loader wiring.

The selector is extracted verbatim from NVMTLDevice.m and compiled as a tiny host-only C fixture.
The remaining checks read the production loader/config/decode-service functions; this test never
loads the VideoToolbox dylib, opens an NVRM client, or requires a GPU.
"""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
dev = (root / "plugin/NVMTLDevice.m").read_text()
objects = (root / "plugin/NVMTLObjects.m").read_text()
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


def function_source(text, signature):
    """Return one C/ObjC function, counting braces while ignoring comments and literals."""
    start = text.find(signature)
    if start < 0:
        return ""
    opening = text.find("{", start + len(signature))
    if opening < 0:
        return ""
    depth = 0
    i = opening
    state = "code"
    while i < len(text):
        c = text[i]
        n = text[i + 1] if i + 1 < len(text) else ""
        if state == "line_comment":
            if c == "\n":
                state = "code"
        elif state == "block_comment":
            if c == "*" and n == "/":
                state = "code"
                i += 1
        elif state in ("string", "char"):
            if c == "\\":
                i += 1
            elif (state == "string" and c == '"') or (state == "char" and c == "'"):
                state = "code"
        elif c == "/" and n == "/":
            state = "line_comment"
            i += 1
        elif c == "/" and n == "*":
            state = "block_comment"
            i += 1
        elif c == '"':
            state = "string"
        elif c == "'":
            state = "char"
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[start:i + 1]
        i += 1
    return ""


selector_sig = "static bool nvmtl_nvencvt_selected_for_process(const char *selector, const char *process)"
selector = function_source(dev, selector_sig)
check("the production selector helper is present for the extracted fixture", bool(selector))

if selector:
    fixture = r'''#define _POSIX_C_SOURCE 200809L
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdio.h>
''' + selector + r'''
struct selector_case { const char *selector, *process; bool want; };
int main(void) {
    const struct selector_case cases[] = {
        { NULL, "VTDecoderXPCService", false },
        { "", "VTDecoderXPCService", false },
        { "0", "VTDecoderXPCService", false },
        { "1", "VTDecoderXPCService", true },
        { "1", "WindowServer", true },
        { "VTDecoderXPCService", "VTDecoderXPCService", true },
        { "OBS", "VTDecoderXPCService", false },
        { "OBS,VTDecoderXPCService", "VTDecoderXPCService", true },
        { "OBS VTDecoderXPCService", "VTDecoderXPCService", true },
        { "OBS:VTDecoderXPCService", "VTDecoderXPCService", true },
        { "VTDecoderXPCServiceHelper", "VTDecoderXPCService", false },
        { "org.example.app", "ExampleApp", false },
        { "ExampleApp", NULL, false },
    };
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        bool got = nvmtl_nvencvt_selected_for_process(cases[i].selector, cases[i].process);
        if (got != cases[i].want) {
            fprintf(stderr, "case %zu failed: selector=%s process=%s got=%d want=%d\n", i,
                cases[i].selector ? cases[i].selector : "(null)",
                cases[i].process ? cases[i].process : "(null)", got, cases[i].want);
            return 1;
        }
    }
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="nvencvt-registration-") as td:
        source = pathlib.Path(td) / "selector.c"
        binary = pathlib.Path(td) / "selector"
        source.write_text(fixture)
        try:
            build = subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", str(source), "-o", str(binary)],
                                   capture_output=True, text=True)
        except FileNotFoundError:
            check("host C compiler available for the extracted selector fixture", False, "cc not found")
        else:
            check("the extracted production selector compiles with warnings as errors",
                  build.returncode == 0, build.stderr.strip())
            if build.returncode == 0:
                run = subprocess.run([str(binary)], capture_output=True, text=True)
                check("default-off, global opt-in, exact process list, and delimiters behave as configured",
                      run.returncode == 0, run.stderr.strip())

loader = function_source(dev, "static void nvmtl_nvencvt_register(void)")
guard = function_source(dev, "static bool nvmtl_loadable_root_only(const char *path)")
check("the loader consumes the selected process through the production helper",
      bool(loader) and 'getenv("NVMTL_NVENCVT")' in loader
      and "nvmtl_nvencvt_selected_for_process(sel, me)" in loader)
check("WindowServer remains excluded after opt-in selection",
      bool(loader) and 'strcmp(me, "WindowServer")' in loader)
check("the dylib still passes the root-owned file and parent-directory gate before dlopen",
      bool(loader) and bool(guard) and loader.index("nvmtl_loadable_root_only(path)") < loader.index("dlopen(path")
      and guard.count("lstat(") == 2 and "S_ISREG" in guard and "S_ISDIR" in guard
      and "st.st_uid != 0" in guard and "ds.st_uid == 0" in guard
      and guard.count("S_IWGRP | S_IWOTH") == 2)
check("the production loader resolves its sibling dylib and the documented registrar symbol",
      bool(loader) and '"%s/libnvencvt.dylib"' in loader
      and 'dlsym(h, "NVEncVT_Register")' in loader and "RTLD_NOW | RTLD_LOCAL" in loader)
check("the log describes the registration call without claiming every codec registered",
      bool(loader) and '"nvencvt: external codec registration call' in loader
      and '"nvencvt: external encoder %s' not in loader)

# Release builds discard inherited NVMTL_* switches before reading the root-managed config. Keep
# that policy: this opt-in is configured in nvrm610.conf, not opened to every process environment.
conf = function_source(objects, "__attribute__((constructor)) static void nvmtl_conf_knobs(void)")
scrub = conf.split("#endif", 1)[0] if conf else ""
check("release builds still scrub inherited NVMTL_* environment switches",
      bool(conf) and "#if NVMTL_RELEASE" in conf and 'strncmp(*e, "NVMTL_", 6)' in scrub
      and "NVMTL_NVENCVT=" not in scrub and "NVMTL_NVENCVT_RATING=" not in scrub)
check("the root-managed config continues to accept the NVMTL_NVENCVT selector",
      bool(conf) and 'fopen("/Library/GPUBundles/nvmtl/nvrm610.conf"' in conf
      and 'strncmp(key, "NVMTL_", 6)' in conf)

check("the production conf reader can hold the full 511-byte selector line",
      bool(conf) and "char line[1024];" in conf)
if conf:
    # Call the exact production parser body, but route its fixed system path to a temporary fixture.
    # Strip only the constructor attribute so loading the test executable cannot run the parser early.
    conf_function = conf.replace("__attribute__((constructor)) ", "", 1)
    conf_fixture = r'''#define _POSIX_C_SOURCE 200809L
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static const char *test_config_path;
static void nvlog(const char *format, ...) { (void)format; }
static FILE *route_fopen(const char *path, const char *mode) {
    if (!strcmp(path, "/Library/GPUBundles/nvmtl/nvrm610.conf"))
        return fopen(test_config_path, mode);
    return fopen(path, mode);
}
#define fopen route_fopen
''' + conf_function + r'''
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    test_config_path = argv[1];
    setenv("NVMTL_NVENCVT", "from-environment", 1);
    setenv("NVMTL_FAKE", "from-environment", 1);
    unsetenv("NVMTL_EXPECTED");
    unsetenv("NVMTL_CONF_SET");
    nvmtl_conf_knobs();

    char expected[512];
    memset(expected, 'A', 497);
    const char tail[] = "  NVMTL_FAKE=1";
    memcpy(expected + 497, tail, sizeof tail - 1);
    expected[511] = '\0';
    const char *value = getenv("NVMTL_NVENCVT");
    if (!value || strlen(value) != 511 || memcmp(value, expected, sizeof expected)) {
        fprintf(stderr, "selector not read whole: got %zu bytes\n", value ? strlen(value) : 0);
        return 3;
    }
    if (getenv("NVMTL_FAKE")) {
        fprintf(stderr, "a truncated continuation became a pseudo config key\n");
        return 4;
    }
    const char *next = getenv("NVMTL_EXPECTED");
    if (!next || strcmp(next, "ok")) {
        fprintf(stderr, "the config line after the long selector was not read\n");
        return 5;
    }
    return 0;
}
'''
    selector_value = "A" * 497 + "  NVMTL_FAKE=1"
    if len(selector_value) != 511:
        check("conf boundary fixture uses the selector maximum", False, str(len(selector_value)))
    with tempfile.TemporaryDirectory(prefix="nvencvt-conf-") as td:
        source = pathlib.Path(td) / "conf.c"
        binary = pathlib.Path(td) / "conf"
        config = pathlib.Path(td) / "nvrm610.conf"
        config.write_text(f"NVMTL_NVENCVT={selector_value}\nNVMTL_EXPECTED=ok\n", encoding="ascii")
        source.write_text(conf_fixture)
        try:
            build = subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-DNVMTL_RELEASE=1",
                                    str(source), "-o", str(binary)], capture_output=True, text=True)
        except FileNotFoundError:
            check("host C compiler available for the extracted conf fixture", False, "cc not found")
        else:
            check("the extracted production conf parser compiles with warnings as errors",
                  build.returncode == 0, build.stderr.strip())
            if build.returncode == 0:
                run = subprocess.run([str(binary), str(config)], capture_output=True, text=True)
                check("511-byte selector survives newline and cannot create a pseudo config key",
                      run.returncode == 0, run.stderr.strip())

# Match the shipped overlay boundary: the selector is reached after an NVIDIA Metal device is
# initialized, while the VTDecoderXPCService warmup only registers the built-in H.264/HEVC decoders.
device_init_sig = "- (instancetype)initWithAcceleratorPort:(mach_port_t)port"
device_init_start = dev.rfind(device_init_sig)
device_init = function_source(dev[device_init_start:], device_init_sig) if device_init_start >= 0 else ""
decode_service = function_source(dev, "__attribute__((constructor)) static void nvmtl_nvdec_in_decode_service(void)")
check("the external library is invoked from the NVIDIA Metal-device registration path",
      bool(device_init) and "nvmtl_nvencvt_register();" in device_init
      and device_init.index("nvmtl_nvdec_hevc_register();") < device_init.index("nvmtl_nvencvt_register();"))
check("decode-service warmup does not broaden the overlay's external-loader scope",
      bool(decode_service) and "dispatch_async(" in decode_service
      and "nvmtl_nvdec_register();" in decode_service
      and "nvmtl_nvdec_hevc_register();" in decode_service
      and "nvmtl_nvencvt_register();" not in decode_service)

print("FAILED" if fails else "PASS")
sys.exit(1 if fails else 0)

#!/usr/bin/env python3
"""The ANGLE Metal rule is on by default for Chrome and Chromium, and opt-in for everything else.

The constructor read the environment and the process name; lift it into a fixture with stubs so the
matching and the NVMTL_ANGLE_METAL overrides are checked without a GPU or a browser.
"""
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
source = (ROOT / "plugin/NVMTLDevice.m").read_text()

start = source.index("static bool nvmtl_angle_chrome_family(void)")
end = source.index("static bool nvmtl_process_is_allowed(void)")
region = source[start:end].replace("__attribute__((constructor)) ", "")

fixture = r"""
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>

#define LOG_NOTICE 5
// the app path the bundle check reads; "" = a process outside any .app
static const char *g_path = "";
int proc_pidpath(int, void *buf, unsigned size) {
    if (!g_path[0]) return 0;
    strncpy((char *)buf, g_path, size - 1); ((char *)buf)[size - 1] = 0;
    return (int)strlen((char *)buf);
}
static const char *g_name = "";
static const char *g_opt = nullptr;
static char g_angle[1024] = {0};
static int g_logs = 0;

const char *getprogname(void) { return g_name; }
char *getenv(const char *k) {
    if (!strcmp(k, "NVMTL_ANGLE_METAL")) return (char *)g_opt;
    if (!strcmp(k, "ANGLE_FEATURE_OVERRIDES_DISABLED")) return g_angle[0] ? g_angle : nullptr;
    return nullptr;
}
int setenv(const char *k, const char *v, int) {
    if (!strcmp(k, "ANGLE_FEATURE_OVERRIDES_DISABLED")) {
        strncpy(g_angle, v, sizeof g_angle - 1);
        g_angle[sizeof g_angle - 1] = 0;
    }
    return 0;
}
static void syslog(int, const char *, ...) { g_logs++; }

""" + region + r"""

static void run(const char *name, const char *opt, const char *pre, const char *want, const char *path = "") {
    g_name = name;
    g_path = path;
    g_opt = opt;
    if (pre) {
        strncpy(g_angle, pre, sizeof g_angle - 1);
        g_angle[sizeof g_angle - 1] = 0;
    } else {
        g_angle[0] = 0;
    }
    g_logs = 0;
    nvmtl_angle_allow_nvidia();
    if (strcmp(g_angle, want) != 0) {
        fprintf(stderr, "%s opt=%s pre=%s -> '%s', want '%s'\n",
                name, opt ? opt : "-", pre ? pre : "-", g_angle, want);
        exit(1);
    }
}

static const char *g_app_exe = "", *g_plain_exe = "";
int main(int argc, char **argv) {
    if (argc > 2) { g_app_exe = argv[1]; g_plain_exe = argv[2]; }
    // Chrome and Chromium, including their helper processes, are on by default.
    run("Google Chrome", nullptr, nullptr, "disableMetalOnNvidia");
    run("Google Chrome Helper (Aperitif)", nullptr, nullptr, "disableMetalOnNvidia");
    run("Chromium", nullptr, nullptr, "disableMetalOnNvidia");
    run("Chromium Helper (GPU)", nullptr, nullptr, "disableMetalOnNvidia");
    // Everything else stays opt-in.
    run("Safari", nullptr, nullptr, "");
    run("Slack Helper (Renderer)", nullptr, nullptr, "");
    run("QQ Helper", nullptr, nullptr, "");
    // PR #404: Edge, Brave, Electron apps and Discord are on by default too.
    run("Microsoft Edge Helper (GPU)", nullptr, nullptr, "disableMetalOnNvidia");
    run("Brave Browser Helper", nullptr, nullptr, "disableMetalOnNvidia");
    run("Discord Helper (Renderer)", nullptr, nullptr, "disableMetalOnNvidia");
    // ...and any app whose bundle carries ANGLE or a Chromium framework (path given by the test on argv[1])
    run("Slack Helper (Renderer)", nullptr, nullptr, "disableMetalOnNvidia", g_app_exe);
    run("Slack Helper (Renderer)", nullptr, nullptr, "", g_plain_exe);
    // The environment variable still overrides both ways.
    run("Safari", "1", nullptr, "disableMetalOnNvidia");
    run("Google Chrome", "0", nullptr, "");
    // An existing ANGLE override is kept, without duplicating the rule.
    run("Google Chrome", nullptr, "foo", "foo:disableMetalOnNvidia");
    run("Google Chrome", nullptr, "disableMetalOnNvidia", "disableMetalOnNvidia");
    printf("ok\n");
    return 0;
}
"""

with tempfile.TemporaryDirectory() as tmp:
    exe = pathlib.Path(tmp) / "test_angle"
    subprocess.run(
        ["xcrun", "clang++", "-std=c++17", "-O0", "-x", "c++", "-o", str(exe), "-"],
        input=fixture.encode(), check=True,
    )
    app = pathlib.Path(tmp) / "Slack.app"
    (app / "Contents/Frameworks/Electron Framework.framework").mkdir(parents=True)
    (app / "Contents/MacOS").mkdir(parents=True)
    result = subprocess.run([str(exe), str(app / "Contents/MacOS/Slack Helper"), str(pathlib.Path(tmp) / "plain/bin/tool")],
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "ok", result.stdout

print("ok: ANGLE Chrome/Chromium/Edge/Brave/Electron/Discord matching, app-bundle framework check, opt-in for other apps, NVMTL_ANGLE_METAL overrides")

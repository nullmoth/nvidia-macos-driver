/* nvmtl_osver.m - libnvmtl_osver.dylib, installed at /Library/GPUBundles/nvmtl/libnvmtl_osver.dylib.

   Some applications decide what to offer from the operating system version they read at start-up, before Metal is
   used. Cyberpunk 2077 reads it during device set-up and offers MetalFX frame generation only above macOS 26; the
   Metal plugin is loaded later than that read, so an answer installed by the plugin is never seen. This library is
   loaded by dyld with the program, and its initializer runs before any of the program's own code.

   Who loads it: only a program that names it in a load command. The Intel build of Cyberpunk 2077 is relinked by us and
   carries it as a weak load command, so the game still starts when the driver is not installed. Nothing system-wide
   loads it. Even when loaded it answers only for a process that the application profile matches (executable name and
   bundle identifier, nvmtl_app_profile.h) and only when the real system is older than the profile's version, or for a
   process that asked for a version itself.

   Settings, in order: NVMTL_OS_VERSION from this process's own environment (kept only with NVMTL_PROCESS_ENV=1, as the
   release plugin keeps knobs), then the last NVMTL_OS_VERSION line of nvrm610.conf, then the profile. "0" turns the
   answer off. NVMTL_APP_PROFILE=0 (environment or conf) turns the profile off.

   Build: build/build_osver.sh. */
#import <Foundation/Foundation.h>
#include <crt_externs.h>
#include <os/log.h>
#include <stdio.h>
#include "nvmtl_app_profile.h"
#include "nvmtl_env_knobs.h"
#include "nvmtl_osver_install.h"

/* Set once the answer is installed; the plugin looks it up and does not install it a second time. */
__attribute__((visibility("default"))) int nvmtl_osver_installed;

static const char *kConf = "/Library/GPUBundles/nvmtl/nvrm610.conf";

/* The value of key for this process: its kept environment first, then the conf file's last line for it. */
static int setting(const char *key, int kept, char *out, size_t cap)
{
    const char *e = kept ? getenv(key) : NULL;
    if (e) { snprintf(out, cap, "%s", e); return 1; }
    FILE *f = fopen(kConf, "r");
    if (!f) return 0;
    char line[512], v[128]; int found = 0;
    while (fgets(line, sizeof line, f)) if (nvmtl_conf_line_value(line, key, v, sizeof v)) { snprintf(out, cap, "%s", v); found = 1; }
    fclose(f);
    return found;
}

__attribute__((constructor)) static void nvmtl_osver_init(void)
{
    @autoreleasepool {
        const int kept = nvmtl_env_knobs_kept(*_NSGetEnviron());
        char ver[128], off[16];
        const char *env_ver = setting("NVMTL_OS_VERSION", kept, ver, sizeof ver) ? ver : NULL;
        const char *prof_off = setting("NVMTL_APP_PROFILE", kept, off, sizeof off) ? off : NULL;
        char bid[256] = "";
        CFBundleRef mb = CFBundleGetMainBundle(); CFStringRef s = mb ? CFBundleGetIdentifier(mb) : NULL;
        if (s && !CFStringGetCString(s, bid, sizeof bid, kCFStringEncodingUTF8)) bid[0] = 0;
        const NSOperatingSystemVersion r = [[NSProcessInfo processInfo] operatingSystemVersion];
        long real[3] = { (long)r.majorVersion, (long)r.minorVersion, (long)r.patchVersion };
#ifdef NVMTL_OSVER_TEST
        // tools/test_osver.sh only (never in the shipped build): pretend the system is older, so the profile's answer
        // can be proven end to end on a Mac that already runs the newer version.
        { const char *t = getenv("NVMTL_OSVER_TEST_REAL"); long tv[3]; if (t && nvmtl_os_version_parse(t, tv)) memcpy(real, tv, sizeof real); }
#endif
        long v[3];
        if (!nvmtl_os_version_answer(getprogname(), bid, prof_off, env_ver, real, v)) return;
        const NSOperatingSystemVersion fake = { v[0], v[1], v[2] };
        const int n = nvmtl_osver_install(fake);
        if (n) nvmtl_osver_installed = 1;
        os_log(OS_LOG_DEFAULT, "nvmtl osver: %{public}s (%{public}s) is told macOS %ld.%ld.%ld instead of %ld.%ld.%ld (%{public}s)%{public}s",
               getprogname(), bid, v[0], v[1], v[2], real[0], real[1], real[2], env_ver ? "NVMTL_OS_VERSION" : "application profile",
               n ? "" : " - FAILED: NSProcessInfo has no operatingSystemVersion to replace");
    }
}

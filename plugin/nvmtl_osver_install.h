/* nvmtl_osver_install.h - tells this process a newer operating system version through NSProcessInfo.
   Shared by libnvmtl_osver.dylib (nvmtl_osver.m), which runs before the application's own code, and the plugin, which
   runs when Metal loads it. Which version, and for which process, is decided by nvmtl_os_version_answer
   (nvmtl_app_profile.h); this file only installs the answer.

   +[NSProcessInfo processInfo] is an instance of a private subclass on current macOS (_NSSwiftProcessInfo), and that
   subclass implements operatingSystemVersion itself, so replacing the method on NSProcessInfo alone changes nothing a
   caller sees. The answer is installed on the class of the real instance and on NSProcessInfo. */
#ifndef NVMTL_OSVER_INSTALL_H
#define NVMTL_OSVER_INSTALL_H
#import <Foundation/Foundation.h>
#import <objc/runtime.h>

static inline BOOL nvmtl_osver_at_least(NSOperatingSystemVersion have, NSOperatingSystemVersion want)
{
    if (have.majorVersion != want.majorVersion) return have.majorVersion > want.majorVersion;
    if (have.minorVersion != want.minorVersion) return have.minorVersion > want.minorVersion;
    return have.patchVersion >= want.patchVersion;
}
/* Installs the answer on every class that implements it for this process; returns how many classes now answer it
   (0 means nothing could be installed and the caller should say so). */
static inline int nvmtl_osver_install(NSOperatingSystemVersion fake)
{
    Class classes[2] = { object_getClass([NSProcessInfo processInfo]), [NSProcessInfo class] };
    int n = 0;
    for (int i = 0; i < 2; i++) {
        Class c = classes[i];
        if (!c || (i == 1 && c == classes[0])) continue;
        Method v = class_getInstanceMethod(c, @selector(operatingSystemVersion));
        Method a = class_getInstanceMethod(c, @selector(isOperatingSystemAtLeastVersion:));
        if (!v || !a) continue;
        // class_replaceMethod adds the method to the class itself when it only inherits it, so a subclass that is
        // asked first answers the same as its superclass.
        class_replaceMethod(c, @selector(operatingSystemVersion),
                            imp_implementationWithBlock(^NSOperatingSystemVersion(id s) { (void)s; return fake; }),
                            method_getTypeEncoding(v));
        class_replaceMethod(c, @selector(isOperatingSystemAtLeastVersion:),
                            imp_implementationWithBlock(^BOOL(id s, NSOperatingSystemVersion want) { (void)s; return nvmtl_osver_at_least(fake, want); }),
                            method_getTypeEncoding(a));
        n++;
    }
    return n;
}
#endif

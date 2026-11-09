/* test_osver_probe.m - the program tools/test_osver.sh runs as a stand-in for the game. It is linked with a weak load
   command for libnvmtl_osver.dylib, as the game's build is, and prints what its own code sees on its FIRST read, in
   main(), before anything else runs: the version, two isOperatingSystemAtLeastVersion: answers and the class of the
   process-info instance. `base-only` first replaces the method on NSProcessInfo alone, the way the plugin used to. */
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <dlfcn.h>
int main(int argc, char **argv)
{
    @autoreleasepool {
        if (argc > 1 && !strcmp(argv[1], "base-only")) {
            Method m = class_getInstanceMethod([NSProcessInfo class], @selector(operatingSystemVersion));
            method_setImplementation(m, imp_implementationWithBlock(^NSOperatingSystemVersion(id s) { (void)s; return (NSOperatingSystemVersion){ 99, 0, 0 }; }));
        }
        NSProcessInfo *pi = [NSProcessInfo processInfo];
        NSOperatingSystemVersion v = [pi operatingSystemVersion];
        int *inst = dlsym(RTLD_DEFAULT, "nvmtl_osver_installed");
        printf("version %ld.%ld.%ld atleast27 %d atleast99 %d class %s lib %s\n", (long)v.majorVersion, (long)v.minorVersion,
               (long)v.patchVersion, [pi isOperatingSystemAtLeastVersion:(NSOperatingSystemVersion){ 27, 0, 0 }],
               [pi isOperatingSystemAtLeastVersion:(NSOperatingSystemVersion){ 99, 0, 0 }], class_getName(object_getClass(pi)),
               inst ? (*inst ? "answered" : "loaded") : "absent");
    }
    return 0;
}

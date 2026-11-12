// objc_probe: print, as JSON, the Objective-C protocols named MTL* of the running system's Metal framework with their
// required and optional instance methods, and the instance methods and size of the plugin base classes.
// It only loads the framework and reads class data; it creates no Metal device and draws nothing.
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <dlfcn.h>
static NSArray *methods(Protocol *p, BOOL req) {
    unsigned n = 0; struct objc_method_description *d = protocol_copyMethodDescriptionList(p, req, YES, &n);
    NSMutableArray *a = [NSMutableArray new];
    for (unsigned i = 0; i < n; i++) [a addObject:NSStringFromSelector(d[i].name)];
    free(d); return a;
}
int main(void) { @autoreleasepool {
    if (!dlopen("/System/Library/Frameworks/Metal.framework/Metal", RTLD_NOW)) { fprintf(stderr, "no Metal\n"); return 1; }
    NSMutableDictionary *P = [NSMutableDictionary new], *C = [NSMutableDictionary new];
    unsigned n = 0; Protocol * __unsafe_unretained *pl = objc_copyProtocolList(&n);
    for (unsigned i = 0; i < n; i++) {
        NSString *nm = @(protocol_getName(pl[i]));
        if (![nm hasPrefix:@"MTL"]) continue;
        unsigned k = 0; Protocol * __unsafe_unretained *par = protocol_copyProtocolList(pl[i], &k);
        NSMutableArray *ps = [NSMutableArray new];
        for (unsigned j = 0; j < k; j++) [ps addObject:@(protocol_getName(par[j]))];
        free(par);
        P[nm] = @{@"imeth": methods(pl[i], YES), @"oimeth": methods(pl[i], NO), @"protocols": ps};
    }
    for (NSString *cn in @[@"MTLIOAccelDevice", @"_MTLDevice"]) {
        Class c = NSClassFromString(cn); if (!c) continue;
        unsigned k = 0; Method *ml = class_copyMethodList(c, &k); NSMutableDictionary *im = [NSMutableDictionary new];
        for (unsigned j = 0; j < k; j++) im[NSStringFromSelector(method_getName(ml[j]))] = @"";
        free(ml);
        unsigned iv = 0; Ivar *il = class_copyIvarList(c, &iv); NSMutableArray *ivs = [NSMutableArray new];
        for (unsigned j = 0; j < iv; j++) [ivs addObject:@{@"name": @(ivar_getName(il[j]) ?: ""), @"offset": @(ivar_getOffset(il[j])), @"type": @(ivar_getTypeEncoding(il[j]) ?: "")}];
        free(il);
        Class s = class_getSuperclass(c);
        C[cn] = @{@"super": s ? NSStringFromClass(s) : @"", @"size": @(class_getInstanceSize(c)), @"start": @0, @"imeth": im, @"cmeth": @{}, @"ivars": ivs, @"protocols": @[]};
    }
    NSDictionary *out = @{@"classes": C, @"protocols": P, @"categories": @[], @"system": NSProcessInfo.processInfo.operatingSystemVersionString};
    NSData *d = [NSJSONSerialization dataWithJSONObject:out options:0 error:nil];
    fwrite(d.bytes, 1, d.length, stdout);
} return 0; }

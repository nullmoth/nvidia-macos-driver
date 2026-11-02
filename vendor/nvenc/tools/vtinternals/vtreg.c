// vtreg.c: list VideoToolbox's own video encoder registry (the FigRegistry its encoder list and selection use), found
// from the `mov <global>(%rip), %rdi` in the code that adds the plug-in search paths (VTCopySupportedPropertyForVideoEncoderID
// +1652 +0x223 on macOS 26.7.1), and optionally run VideoToolbox's registry filter over it.
#include <CoreFoundation/CoreFoundation.h>
#include <VideoToolbox/VideoToolbox.h>
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

extern OSStatus FigRegistryCopyFilteredItemList(void *reg, void *filter, void *ctx, CFArrayRef *out, int *count);
extern OSStatus FigRegistryAddSearchPathWithOptions(void *reg, CFURLRef dir, int options);
extern OSStatus FigRegistryRescan(void *reg);

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    CFArrayRef l = NULL; VTCopyVideoEncoderList(NULL, &l);   // initialise VideoToolbox's registry
    unsigned char *f = (unsigned char *)dlsym(RTLD_DEFAULT, "VTCopySupportedPropertyForVideoEncoderID") + 1652;
    unsigned char *ins = f + 0x223;                          // 48 8b 3d disp32: mov disp(%rip), %rdi
    if (ins[0] != 0x48 || ins[1] != 0x8b || ins[2] != 0x3d) { printf("unexpected code at +0x223: %02x %02x %02x\n", ins[0], ins[1], ins[2]); return 1; }
    void **glob = (void **)(ins + 7 + *(int *)(ins + 3));
    void *reg = *glob;
    printf("registry global %p -> %p\n", (void *)glob, reg);
    for (int k = 2; k <= 3; k++) {   // the FigRegistry object's fields (iOS decompile: +16 class, +24 path set, +32 cache array, +40 item set, +56 dict)
        CFTypeRef o = ((CFTypeRef *)reg)[k];
        if (!o) { printf("  field %d: NULL\n", k); continue; }
        CFStringRef d = CFCopyDescription(o); char b[4096]; CFStringGetCString(d, b, sizeof b, kCFStringEncodingUTF8);
        printf("  field %d: %.600s\n", k, b); CFRelease(d);
    }
    CFArrayRef caches = ((CFArrayRef *)reg)[4];
    for (CFIndex i = 0; caches && i < CFArrayGetCount(caches); i++) {
        void **ce = (void **)CFArrayGetValueAtIndex(caches, i);   // cache entry: [0] class -> set of items, [1] mutex, +16 options, +20 scans
        CFDictionaryRef byClass = ce[0];
        printf("cache %ld: options %d scans %d, classes %ld\n", (long)i, *(int *)((char *)ce + 16), *(int *)((char *)ce + 20), byClass ? (long)CFDictionaryGetCount(byClass) : -1L);
        if (byClass) { CFStringRef d = CFCopyDescription(byClass); char b[8192]; CFStringGetCString(d, b, sizeof b, kCFStringEncodingUTF8);
            for (char *c = strstr(b, "Bundle URL: "); c; c = strstr(c + 1, "Bundle URL: ")) { char t[160]; sscanf(c + 12, "%159s", t); printf("    %s\n", t); }
            CFRelease(d); }
    }
    if (getenv("ADD_PATH")) {
        const char *p = getenv("ADD_PATH");
        CFURLRef u = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)p, (CFIndex)strlen(p), true);
        printf("add %s -> %d, rescan -> %d\n", p, (int)FigRegistryAddSearchPathWithOptions(reg, u, 0), (int)FigRegistryRescan(reg));
    }
    CFArrayRef items = NULL; int cnt = 0;
    OSStatus st = FigRegistryCopyFilteredItemList(reg, NULL, NULL, &items, &cnt);
    printf("items -> %d, %ld\n", (int)st, items ? (long)CFArrayGetCount(items) : -1L);
    for (CFIndex i = 0; items && i < CFArrayGetCount(items); i++) {
        CFStringRef d = CFCopyDescription(CFArrayGetValueAtIndex(items, i));
        char b[2048]; CFStringGetCString(d, b, sizeof b, kCFStringEncodingUTF8);
        char *u = strstr(b, "Bundle URL: "), *id = strstr(b, "Class Implementation ID: ");
        if (u) { char *e = strchr(u, '\n'); if (e) *e = 0; }
        if (id) { char *e = strchr(id, '\n'); if (e) *e = 0; }
        printf("  %s | %s\n", u ? u + 12 : "?", id ? id + 25 : "?");
        if (id && strstr(id, "org.nvenc.videoencoder.avc")) {
            extern OSStatus FigRegistryItemCopyDescription(const void *item, CFDictionaryRef *out);
            CFTypeRef item = CFArrayGetValueAtIndex(items, i);
            CFDictionaryRef dd = NULL; OSStatus ds = FigRegistryItemCopyDescription(item, &dd);
            CFStringRef t = dd ? CFCopyDescription(dd) : NULL; char tb[4096] = ""; if (t) CFStringGetCString(t, tb, sizeof tb, kCFStringEncodingUTF8);
            printf("    ItemCopyDescription -> %d: %s\n", (int)ds, tb);
            CFTypeRef raw = ((CFTypeRef *)item)[2];
            printf("    raw field 2: type %lu (dict %lu), same object as copy: %s\n", (unsigned long)CFGetTypeID(raw), (unsigned long)CFDictionaryGetTypeID(), raw == (CFTypeRef)dd ? "yes" : "no");
            if (getenv("RENAME")) { CFDictionarySetValue((CFMutableDictionaryRef)raw, CFSTR("VTEncoderName"), CFSTR("nvenc H.264 (NVIDIA RTX 40)")); printf("    renamed\n"); }
        }
        CFRelease(d);
    }
    CFArrayRef l2 = NULL; VTCopyVideoEncoderList(NULL, &l2);
    for (CFIndex i = 0; l2 && i < CFArrayGetCount(l2); i++) {
        CFDictionaryRef d = CFArrayGetValueAtIndex(l2, i);
        CFStringRef id = CFDictionaryGetValue(d, CFSTR("EncoderID")), nm = CFDictionaryGetValue(d, CFSTR("EncoderName"));
        char a[256] = "", b[256] = ""; if (id) CFStringGetCString(id, a, sizeof a, kCFStringEncodingUTF8); if (nm) CFStringGetCString(nm, b, sizeof b, kCFStringEncodingUTF8);
        CFStringRef dn = CFDictionaryGetValue(d, CFSTR("DisplayName")), cn = CFDictionaryGetValue(d, CFSTR("CodecName")); char c3[128] = "", c4[128] = "";
        if (dn) CFStringGetCString(dn, c3, sizeof c3, kCFStringEncodingUTF8); if (cn) CFStringGetCString(cn, c4, sizeof c4, kCFStringEncodingUTF8);
        if (strstr(a, "nvenc")) printf("LIST: %s = %s | DisplayName %s | CodecName %s\n", a, b, c3, c4);
    }
    return 0;
}

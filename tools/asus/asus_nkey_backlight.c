/* asus_nkey_backlight.c - set the keyboard backlight of an ASUS ROG / TUF laptop whose keyboard is the USB "N-KEY" (Aura) device,
 * from user space. The layout is the one the public Linux driver hid-asus documents (drivers/hid/hid-asus.c, GPL-2.0), written
 * again here:
 *   vendor 0x0b05; the devices that driver gives a keyboard backlight: 0x1854 0x1837 0x1869 0x1866 0x19b6 0x18c6 0x1a30 0x1abe 0x1b4c
 *   handshake, once per feature report id the device has among 0x5a 0x5d 0x5e (16 bytes):
 *       { id, 'A','S','U','S',' ','T','e','c','h','.','I','n','c','.', 0 }
 *   brightness, feature report id 0x5a, 64 bytes:  { 0x5a, 0xba, 0xc5, 0xc4, level, 0 ... }  with level 0 (off) to 3
 *
 * usage: asus_nkey_backlight [--dry-run] [--force] <level 0..3>
 * --dry-run prints the reports as hex and sends nothing. It refuses to send unless the boot-args carry nmkbl=1 (the switch that
 * turns the keyboard-backlight work on) or --force is given. Build: cc -o asus_nkey_backlight asus_nkey_backlight.c
 * -framework IOKit -framework CoreFoundation. macOS asks for Input Monitoring permission the first time. Never run on an ASUS. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#ifdef __APPLE__
#include <IOKit/hid/IOHIDManager.h>
#include <sys/sysctl.h>
#endif

#define ASUS_VENDOR_ID      0x0b05
#define ASUS_KBD_REPORT_ID  0x5a
#define ASUS_KBD_REPORT_SIZE 64
#define ASUS_HANDSHAKE_SIZE 16
#define ASUS_MAX_LEVEL      3

static const uint16_t kAsusPids[] = { 0x1854, 0x1837, 0x1869, 0x1866, 0x19b6, 0x18c6, 0x1a30, 0x1abe, 0x1b4c };
static const uint8_t kAsusInitIds[] = { 0x5a, 0x5d, 0x5e };

static int asus_pid_has_backlight(uint16_t pid)
{
    for (unsigned i = 0; i < sizeof kAsusPids / sizeof kAsusPids[0]; i++) if (kAsusPids[i] == pid) return 1;
    return 0;
}

static void asus_handshake_report(uint8_t out[ASUS_HANDSHAKE_SIZE], uint8_t report_id)
{
    static const char tag[] = "ASUS Tech.Inc.";          /* 14 characters */
    out[0] = report_id;
    memcpy(out + 1, tag, 14);
    out[15] = 0;
}

static void asus_brightness_report(uint8_t out[ASUS_KBD_REPORT_SIZE], int level)
{
    memset(out, 0, ASUS_KBD_REPORT_SIZE);
    if (level < 0) level = 0;
    if (level > ASUS_MAX_LEVEL) level = ASUS_MAX_LEVEL;
    out[0] = ASUS_KBD_REPORT_ID; out[1] = 0xba; out[2] = 0xc5; out[3] = 0xc4; out[4] = (uint8_t)level;
}

#ifdef __APPLE__
static int device_has_feature_report(IOHIDDeviceRef d, uint8_t id)
{
    CFArrayRef els = IOHIDDeviceCopyMatchingElements(d, NULL, kIOHIDOptionsTypeNone);
    int has = 0;
    if (els) {
        for (CFIndex i = 0; i < CFArrayGetCount(els) && !has; i++) {
            IOHIDElementRef e = (IOHIDElementRef)CFArrayGetValueAtIndex(els, i);
            if (IOHIDElementGetType(e) == kIOHIDElementTypeFeature && IOHIDElementGetReportID(e) == id) has = 1;
        }
        CFRelease(els);
    }
    return has;
}

static int send_all(int level)
{
    IOHIDManagerRef m = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
    int vid = ASUS_VENDOR_ID;
    CFNumberRef v = CFNumberCreate(NULL, kCFNumberIntType, &vid);
    CFMutableDictionaryRef match = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(match, CFSTR(kIOHIDVendorIDKey), v);
    IOHIDManagerSetDeviceMatching(m, match);
    CFRelease(match); CFRelease(v);
    if (IOHIDManagerOpen(m, kIOHIDOptionsTypeNone) != kIOReturnSuccess) { fprintf(stderr, "could not open the HID manager (Input Monitoring permission?)\n"); return 2; }
    CFSetRef devs = IOHIDManagerCopyDevices(m);
    int sent = 0;
    if (devs) {
        CFIndex n = CFSetGetCount(devs);
        const void **list = calloc((size_t)n, sizeof *list);
        CFSetGetValues(devs, list);
        for (CFIndex i = 0; i < n; i++) {
            IOHIDDeviceRef d = (IOHIDDeviceRef)list[i];
            int pid = 0; CFNumberRef p = IOHIDDeviceGetProperty(d, CFSTR(kIOHIDProductIDKey));
            if (p) CFNumberGetValue(p, kCFNumberIntType, &pid);
            if (!asus_pid_has_backlight((uint16_t)pid) || !device_has_feature_report(d, ASUS_KBD_REPORT_ID)) continue;
            for (unsigned k = 0; k < sizeof kAsusInitIds; k++) {
                if (!device_has_feature_report(d, kAsusInitIds[k])) continue;
                uint8_t hs[ASUS_HANDSHAKE_SIZE]; asus_handshake_report(hs, kAsusInitIds[k]);
                IOHIDDeviceSetReport(d, kIOHIDReportTypeFeature, kAsusInitIds[k], hs, sizeof hs);   /* the driver tolerates a failed handshake */
            }
            uint8_t rep[ASUS_KBD_REPORT_SIZE]; asus_brightness_report(rep, level);
            if (IOHIDDeviceSetReport(d, kIOHIDReportTypeFeature, ASUS_KBD_REPORT_ID, rep, sizeof rep) == kIOReturnSuccess) sent++;
        }
        free(list); CFRelease(devs);
    }
    CFRelease(m);
    printf("set the backlight on %d N-KEY interface(s)\n", sent);
    return sent ? 0 : 1;
}

static int nmkbl_enabled(void)
{
    char args[1024]; size_t n = sizeof args;
    if (sysctlbyname("kern.bootargs", args, &n, NULL, 0) != 0) return 0;
    return strstr(args, "nmkbl=1") != NULL;
}
#endif

static void hex(const uint8_t *b, int n) { for (int i = 0; i < n; i++) printf("%02x", b[i]); printf("\n"); }

#ifndef ASUS_NKEY_NO_MAIN
int main(int argc, char **argv)
{
    int dry = 0, force = 0, level = -1;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--dry-run")) dry = 1;
        else if (!strcmp(argv[i], "--force")) force = 1;
        else { char *end = 0; long v = strtol(argv[i], &end, 10); level = (end && end != argv[i] && !*end) ? (int)v : -1; }
    }
    if (level < 0 || level > ASUS_MAX_LEVEL) { fprintf(stderr, "usage: %s [--dry-run] [--force] <level 0..3>\n", argv[0]); return 2; }
    if (dry) {
        for (unsigned k = 0; k < sizeof kAsusInitIds; k++) { uint8_t hs[ASUS_HANDSHAKE_SIZE]; asus_handshake_report(hs, kAsusInitIds[k]); hex(hs, sizeof hs); }
        uint8_t rep[ASUS_KBD_REPORT_SIZE]; asus_brightness_report(rep, level); hex(rep, sizeof rep);
        return 0;
    }
#ifdef __APPLE__
    if (!force && !nmkbl_enabled()) { fprintf(stderr, "off: boot-args do not carry nmkbl=1 (use --force to send anyway)\n"); return 3; }
    return send_all(level);
#else
    (void)force; fprintf(stderr, "needs macOS\n"); return 2;
#endif
}
#endif

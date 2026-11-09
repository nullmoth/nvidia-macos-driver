/* razer_kbd_backlight.c - set the keyboard backlight of a Razer laptop (Blade) from user space.
 *
 * A Razer keyboard is a USB HID device that takes a 90-byte feature report. The layout and the checksum are documented by
 * the public OpenRazer driver (razercommon.h, razerchromacommon.c) and written again here:
 *   byte 0 status (0), 1 transaction id, 2..3 remaining packets (big-endian, 0), 4 protocol type (0), 5 data size,
 *   6 command class, 7 command id, 8..87 arguments, 88 checksum, 89 reserved (0).
 *   The checksum is the XOR of bytes 2 through 87.
 *   Set brightness, standard devices:  class 0x03, id 0x03, size 3, args { storage, led, brightness }
 *   Set brightness, extended devices:  class 0x0F, id 0x04, size 3, args { storage, led, brightness }
 *   The backlight led is 0x05; storage 0x01 keeps the value over a power cycle, 0x00 does not.
 *
 * usage: razer_kbd_backlight [--dry-run] [--extended] [--txn 0x1f] <brightness 0..255>
 * --dry-run prints the report as hex and sends nothing. Build: cc -o razer_kbd_backlight razer_kbd_backlight.c
 * -framework IOKit -framework CoreFoundation. It opens every Razer HID interface and sends the report to each one that accepts a
 * feature report; macOS asks the user for Input Monitoring permission the first time. Off unless the user runs it. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#ifdef __APPLE__
#include <IOKit/hid/IOHIDManager.h>
#endif

#define RAZER_REPORT_BYTES 90
#define RAZER_VENDOR_ID    0x1532
#define RAZER_BACKLIGHT_LED 0x05

static void razer_set_brightness_report(uint8_t out[RAZER_REPORT_BYTES], int extended, uint8_t txn, uint8_t storage, uint8_t brightness)
{
    memset(out, 0, RAZER_REPORT_BYTES);
    out[1] = txn;
    out[5] = 3;                                  /* data size */
    out[6] = extended ? 0x0F : 0x03;             /* command class */
    out[7] = extended ? 0x04 : 0x03;             /* command id */
    out[8] = storage;
    out[9] = RAZER_BACKLIGHT_LED;
    out[10] = brightness;
    uint8_t crc = 0;
    for (int i = 2; i < 88; i++) crc ^= out[i];
    out[88] = crc;
}

#ifdef __APPLE__
static int send_to_all(const uint8_t *rep)
{
    IOHIDManagerRef m = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
    int vid = RAZER_VENDOR_ID;
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
        for (CFIndex i = 0; i < n; i++)
            if (IOHIDDeviceSetReport((IOHIDDeviceRef)list[i], kIOHIDReportTypeFeature, 0, rep, RAZER_REPORT_BYTES) == kIOReturnSuccess) sent++;
        free(list); CFRelease(devs);
    }
    CFRelease(m);
    printf("sent to %d Razer HID interface(s)\n", sent);
    return sent ? 0 : 1;
}
#endif

int main(int argc, char **argv)
{
    int dry = 0, ext = 0, level = -1; uint8_t txn = 0xFF;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--dry-run")) dry = 1;
        else if (!strcmp(argv[i], "--extended")) ext = 1;
        else if (!strcmp(argv[i], "--txn") && i + 1 < argc) txn = (uint8_t)strtoul(argv[++i], 0, 0);
        else level = atoi(argv[i]);
    }
    if (level < 0 || level > 255) { fprintf(stderr, "usage: %s [--dry-run] [--extended] [--txn 0x1f] <brightness 0..255>\n", argv[0]); return 2; }
    uint8_t rep[RAZER_REPORT_BYTES];
    razer_set_brightness_report(rep, ext, txn, 1, (uint8_t)level);
    if (dry) { for (int i = 0; i < RAZER_REPORT_BYTES; i++) printf("%02x", rep[i]); printf("\n"); return 0; }
#ifdef __APPLE__
    return send_to_all(rep);
#else
    fprintf(stderr, "needs macOS\n"); return 2;
#endif
}

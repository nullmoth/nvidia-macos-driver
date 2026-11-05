// nvfb-edid.c: EDID overrides for NVRMFB (fork, feature/change-resolution), a C port of tools/nvfb-edid.py that needs no
// Python (macOS has none without the developer tools). Build: clang -O2 -Wall -o nvfb-edid nvfb-edid.c -framework IOKit
// -framework CoreFoundation
//
//   nvfb-edid list                                   connected displays: EDID id, detailed timings, override state
//   sudo nvfb-edid add [-d VVVV-PPPP] WxH@HZ ...     write the display's override with up to six extra timings
//   sudo nvfb-edid remove [-d VVVV-PPPP | --all]     delete the override (the display's own EDID again)
//   nvfb-edid make <EDID .bin> <out .bin> WxH@HZ ... offline: the same override from an EDID file
//   nvfb-edid dump <out .bin> [-d VVVV-PPPP]         the display's current EDID to a file
//
// An override is the display's own EDID plus one CTA-861 extension block of detailed timings (VESA CVT reduced blanking v2,
// with the vertical blanking excess in the back porch so that high rates fit a detailed timing). NVRMFB hands
// /Library/NullMoth/edid-override/<vendor>-<product>.bin to NVKMS with every display query; NVKMS validates the timings
// against the link (and refuses those it cannot carry) and macOS lists them. Takes effect at the next restart.
// While an override is in use the registry shows it, so `add` keeps the display's own EDID in .../original/<id>.bin and
// starts from that. Only add timings the display can show: a real monitor may stay black on one it does not support
// (restart in safe mode, or remove the file from another account / over SSH, to recover).
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define DIR "/Library/NullMoth/edid-override"
#define ORIG DIR "/original"

typedef struct { unsigned char b[1024]; int n; } Edid;

static int edid_ok(const Edid *e)
{
    static const unsigned char hdr[8] = { 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0 };
    if (e->n < 128 || e->n % 128 || memcmp(e->b, hdr, 8) || (e->b[126] + 1) * 128 != e->n) return 0;
    for (int o = 0; o < e->n; o += 128) { unsigned char s = 0; for (int i = 0; i < 128; i++) s += e->b[o + i]; if (s) return 0; }
    return 1;
}
static void edid_id(const Edid *e, char id[10]) { snprintf(id, 10, "%02X%02X-%02X%02X", e->b[8], e->b[9], e->b[11], e->b[10]); }
static void checksum(unsigned char *blk) { unsigned char s = 0; for (int i = 0; i < 127; i++) s += blk[i]; blk[127] = (unsigned char)-s; }

static int read_file(const char *path, Edid *e)
{
    FILE *f = fopen(path, "rb"); if (!f) return -1;
    e->n = (int)fread(e->b, 1, sizeof e->b, f); fclose(f);
    return edid_ok(e) ? 0 : -2;
}
static int write_file(const char *path, const Edid *e, mode_t mode)
{
    char tmp[300]; snprintf(tmp, sizeof tmp, "%s.new", path);
    FILE *f = fopen(tmp, "wb"); if (!f) { perror(tmp); return -1; }
    if (fwrite(e->b, 1, (size_t)e->n, f) != (size_t)e->n || fclose(f)) { perror(tmp); unlink(tmp); return -1; }
    if (geteuid() == 0 && chown(tmp, 0, 0)) { perror(tmp); unlink(tmp); return -1; }
    chmod(tmp, mode);
    if (rename(tmp, path)) { perror(path); unlink(tmp); return -1; }
    return 0;
}

// every distinct IODisplayEDID in the registry (NVRMFB publishes one per connected display)
static int displays(Edid *out, int cap)
{
    io_iterator_t it; int n = 0;
    if (IORegistryCreateIterator(kIOMainPortDefault, kIOServicePlane, kIORegistryIterateRecursively, &it) != KERN_SUCCESS) return 0;
    io_registry_entry_t r;
    while ((r = IOIteratorNext(it))) {
        CFTypeRef p = IORegistryEntryCreateCFProperty(r, CFSTR("IODisplayEDID"), kCFAllocatorDefault, 0);
        if (p && CFGetTypeID(p) == CFDataGetTypeID() && n < cap) {
            Edid e = { .n = (int)CFDataGetLength(p) };
            if (e.n <= (int)sizeof e.b) {
                memcpy(e.b, CFDataGetBytePtr(p), (size_t)e.n);
                int dup = 0; for (int i = 0; i < n; i++) dup |= out[i].n == e.n && !memcmp(out[i].b, e.b, (size_t)e.n);
                if (!dup && edid_ok(&e)) out[n++] = e;
            }
        }
        if (p) CFRelease(p);
        IOObjectRelease(r);
    }
    IOObjectRelease(it);
    return n;
}

static void print_dtds(const Edid *e)
{
    for (int blk = 0; blk < e->n / 128; blk++) {
        const unsigned char *b = e->b + blk * 128; int start, end;
        if (blk == 0) { start = 54; end = 126; }
        else if (b[0] == 0x02 && b[2] >= 4) { start = b[2]; end = 127; }
        else continue;
        for (int o = start; o + 18 <= end; o += 18) {
            const unsigned char *d = b + o; const unsigned p = d[0] | d[1] << 8;
            if (!p) continue;
            const unsigned w = d[2] | (d[4] >> 4) << 8, hb = d[3] | (d[4] & 15) << 8, h = d[5] | (d[7] >> 4) << 8, vb = d[6] | (d[7] & 15) << 8;
            printf("      %ux%u @ %.2f Hz (pixel clock %.2f MHz)%s\n", w, h, p * 10000.0 / ((w + hb) * (double)(h + vb)), p / 100.0, blk ? "  [extension]" : "");
        }
    }
}

static int pick(const char *want, Edid *e)
{
    Edid d[8]; const int n = displays(d, 8); char id[10];
    if (!n) { fprintf(stderr, "no display with an EDID in the registry\n"); return -1; }
    for (int i = 0; i < n; i++) { edid_id(&d[i], id); if (!want || !strcmp(want, id)) { if (!want && n > 1) break; *e = d[i]; return 0; } }
    if (!want) fprintf(stderr, "%d displays: choose one with -d (see nvfb-edid list)\n", n);
    else fprintf(stderr, "no display %s (see nvfb-edid list)\n", want);
    return -1;
}

typedef struct { int w, h, hz; } Mode;
static int parse_modes(int argc, char **argv, Mode *m)
{
    if (argc < 1 || argc > 6) { fprintf(stderr, "give 1 to 6 modes as WxH@HZ\n"); return -1; }
    for (int i = 0; i < argc; i++) {
        char extra;
        if (sscanf(argv[i], "%dx%d@%d%c", &m[i].w, &m[i].h, &m[i].hz, &extra) != 3 || m[i].w < 320 || m[i].h < 200 || m[i].hz < 20 || m[i].hz > 500 || m[i].w > 4095 || m[i].h > 4095) {
            fprintf(stderr, "bad mode %s (want WxH@HZ, e.g. 1920x1080@144)\n", argv[i]); return -1;
        }
    }
    return argc;
}

// CVT 1.2 reduced blanking v2 (as nvfb-edid.py): fixed 80-pixel horizontal blanking, >= 460 us vertical blanking laid out
// as a 3-line front porch, an 8-line sync and the rest as back porch
static int dtd(const Mode *m, unsigned char *b)
{
    const int hfront = 8, hsync = 32, hblank = 80, vsync = 8, vback = 6, vfront_min = 1, vfront = 3;
    const double hp = (1.0 / m->hz - 460e-6) / m->h;
    int vbi = (int)floor(460e-6 / hp) + 1; if (vbi < vfront_min + vsync + vback) vbi = vfront_min + vsync + vback;
    const long pk = lround(m->hz * (double)(m->w + hblank) * (m->h + vbi) / 1000.0), p = lround(pk / 10.0);
    if (p <= 0 || p >= 65536) { fprintf(stderr, "%dx%d@%d: pixel clock %ld kHz does not fit a detailed timing\n", m->w, m->h, m->hz, p * 10); return -1; }
    memset(b, 0, 18);
    b[0] = p & 255; b[1] = (unsigned char)(p >> 8);
    b[2] = m->w & 255; b[3] = hblank & 255; b[4] = (unsigned char)((m->w >> 8) << 4 | hblank >> 8);
    b[5] = m->h & 255; b[6] = vbi & 255; b[7] = (unsigned char)((m->h >> 8) << 4 | vbi >> 8);
    b[8] = hfront; b[9] = hsync; b[10] = (unsigned char)((vfront & 15) << 4 | (vsync & 15));
    b[17] = 0x18 | 0x02;   // digital separate sync, horizontal positive, vertical negative
    printf("  %dx%d@%d: total %dx%d, pixel clock %.2f MHz (CVT-RB2)\n", m->w, m->h, m->hz, m->w + hblank, m->h + vbi, p / 100.0);
    return 0;
}

static int make(const Edid *in, const Mode *m, int nm, Edid *out)
{
    if (in->n / 128 >= 4) { fprintf(stderr, "the EDID already has 3 extension blocks\n"); return -1; }
    *out = *in;
    unsigned char *ext = out->b + out->n; memset(ext, 0, 128);
    ext[0] = 0x02; ext[1] = 0x03; ext[2] = 4;   // CTA-861 rev 3, detailed timings from byte 4, no data blocks
    for (int i = 0; i < nm; i++) if (dtd(&m[i], ext + 4 + 18 * i)) return -1;
    checksum(ext);
    out->b[126]++; checksum(out->b); out->n += 128;
    return 0;
}

static void usage(void)
{
    fprintf(stderr, "usage: nvfb-edid list\n"
                    "       sudo nvfb-edid add [-d VVVV-PPPP] WxH@HZ ...   (up to 6, e.g. 1920x1080@144 2560x1440@120)\n"
                    "       sudo nvfb-edid remove [-d VVVV-PPPP | --all]\n"
                    "       nvfb-edid make <EDID .bin> <out .bin> WxH@HZ ...\n"
                    "       nvfb-edid dump <out .bin> [-d VVVV-PPPP]\n"
                    "Overrides take effect at the next restart. Add only timings the display can show.\n");
    exit(2);
}

int main(int argc, char **argv)
{
    if (argc < 2) usage();
    const char *cmd = argv[1], *want = NULL; int a = 2;
    if (argc > a + 1 && !strcmp(argv[a], "-d")) { want = argv[a + 1]; a += 2; }
    if (!strcmp(cmd, "list")) {
        Edid d[8]; const int n = displays(d, 8); char id[10], path[300];
        if (!n) { printf("no display with an EDID in the registry\n"); return 0; }
        for (int i = 0; i < n; i++) {
            edid_id(&d[i], id); snprintf(path, sizeof path, DIR "/%s.bin", id);
            Edid ov; const int has = read_file(path, &ov) == 0;
            const int active = has && ov.n == d[i].n && !memcmp(ov.b, d[i].b, (size_t)d[i].n);
            printf("display %s: EDID %d bytes, %s\n", id, d[i].n, active ? "override in use" : has ? "override installed (restart to use it)" : "no override");
            print_dtds(&d[i]);
        }
        return 0;
    }
    if (!strcmp(cmd, "make")) {
        if (argc - a < 3) usage();
        Edid in, out; Mode m[6];
        const int r = read_file(argv[a], &in);
        if (r) { fprintf(stderr, "%s: %s\n", argv[a], r == -1 ? strerror(errno) : "not an EDID"); return 1; }
        const int nm = parse_modes(argc - a - 2, argv + a + 2, m);
        if (nm < 0 || make(&in, m, nm, &out) || write_file(argv[a + 1], &out, 0644)) return 1;
        char id[10]; edid_id(&out, id);
        printf("%s: %d bytes; install as " DIR "/%s.bin (root:wheel 0644)\n", argv[a + 1], out.n, id);
        return 0;
    }
    if (!strcmp(cmd, "dump")) {
        if (argc - a < 1) usage();
        const char *outp = argv[a];
        if (argc - a >= 3 && !strcmp(argv[a + 1], "-d")) want = argv[a + 2];
        Edid e; if (pick(want, &e) || write_file(outp, &e, 0644)) return 1;
        printf("%s: %d bytes\n", outp, e.n);
        return 0;
    }
    if (!strcmp(cmd, "add") || !strcmp(cmd, "remove")) {
        if (geteuid() != 0) { fprintf(stderr, "run with sudo\n"); return 1; }
        const int all = !strcmp(cmd, "remove") && argc > a && !strcmp(argv[a], "--all");
        if (all) {
            if (system("rm -f " DIR "/*.bin")) return 1;
            printf("removed every override; restart to use the displays' own EDIDs\n");
            return 0;
        }
        Edid cur; if (pick(want, &cur)) return 1;
        char id[10], path[300], opath[300]; edid_id(&cur, id);
        snprintf(path, sizeof path, DIR "/%s.bin", id); snprintf(opath, sizeof opath, ORIG "/%s.bin", id);
        if (!strcmp(cmd, "remove")) {
            if (unlink(path) && errno != ENOENT) { perror(path); return 1; }
            printf("removed the override for %s; restart to use its own EDID\n", id);
            return 0;
        }
        Mode m[6]; const int nm = parse_modes(argc - a, argv + a, m);
        if (nm < 0) return 1;
        Edid base, ov;
        if (read_file(opath, &base) == 0) printf("starting from %s's own EDID (%s)\n", id, opath);
        else if (read_file(path, &ov) == 0 && ov.n == cur.n && !memcmp(ov.b, cur.b, (size_t)cur.n)) {
            fprintf(stderr, "%s shows its override and its own EDID was not kept: sudo nvfb-edid remove, restart, then add again\n", id); return 1;
        } else {
            base = cur;
            mkdir(DIR, 0755); mkdir(ORIG, 0755);
            if (write_file(opath, &base, 0644)) return 1;
            printf("kept %s's own EDID in %s\n", id, opath);
        }
        mkdir(DIR, 0755);
        if (make(&base, m, nm, &ov) || write_file(path, &ov, 0644)) return 1;
        printf("wrote %s (%d bytes); restart, then pick the new modes in System Settings > Displays\n", path, ov.n);
        return 0;
    }
    usage();
}

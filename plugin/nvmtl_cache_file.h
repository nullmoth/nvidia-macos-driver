/* nvmtl_cache_file.h - shader-cache files that prove they are whole.
   A cache entry is read back by a later process and trusted: a shader that is short, torn or from a half-finished write
   is a wrong shader, not a missing one. Every entry is written as <payload><footer>, where the footer holds a magic,
   the payload length and a 64-bit hash of the payload. A reader accepts the file only when all three agree and otherwise
   deletes it, so the next lookup translates again. The write goes to a temporary file in the same folder, is flushed to
   the disk, and only then renamed over the final name, so no process ever sees a partial file under the final name and a
   crash cannot leave a renamed file whose data never reached the disk.
   An entry without a footer (written by an older driver) is not trusted: it reads as missing and is removed.
   Pure C over file descriptors, so tools/test_cache_file.c proves it without the plugin. */
#ifndef NVMTL_CACHE_FILE_H
#define NVMTL_CACHE_FILE_H
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <pthread.h>
#include <sys/stat.h>

#define NVMTL_CF_FOOTER 16u
#define NVMTL_CF_MAGIC 0x31464356u   /* "VCF1" little-endian */

static inline uint64_t nvmtl_cf_hash(const uint8_t *p, size_t n)
{
    uint64_t h = 1469598103934665603ull;   /* FNV-1a, 64-bit */
    for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 1099511628211ull; }
    return h;
}

/* Why a file was refused, for the log. */
typedef enum { NVMTL_CF_OK = 0, NVMTL_CF_MISSING, NVMTL_CF_SHORT, NVMTL_CF_NO_FOOTER, NVMTL_CF_LENGTH, NVMTL_CF_HASH } nvmtl_cf_status;
static inline const char *nvmtl_cf_why(nvmtl_cf_status s)
{
    switch (s) { case NVMTL_CF_OK: return "ok"; case NVMTL_CF_MISSING: return "missing"; case NVMTL_CF_SHORT: return "shorter than a footer";
                 case NVMTL_CF_NO_FOOTER: return "no footer (written by an older driver, or cut short)";
                 case NVMTL_CF_LENGTH: return "payload length does not match the file"; default: return "payload hash does not match"; }
}

/* Checks a file image; on NVMTL_CF_OK *payload_len is the payload's length (the payload is the file's first bytes). */
static inline nvmtl_cf_status nvmtl_cf_check(const uint8_t *file, size_t size, size_t *payload_len)
{
    if (size < NVMTL_CF_FOOTER) return NVMTL_CF_SHORT;
    const uint8_t *f = file + size - NVMTL_CF_FOOTER;
    uint32_t magic, len; uint64_t hash;
    memcpy(&magic, f, 4); memcpy(&len, f + 4, 4); memcpy(&hash, f + 8, 8);
    if (magic != NVMTL_CF_MAGIC) return NVMTL_CF_NO_FOOTER;
    if ((size_t)len != size - NVMTL_CF_FOOTER) return NVMTL_CF_LENGTH;
    if (nvmtl_cf_hash(file, len) != hash) return NVMTL_CF_HASH;
    if (payload_len) *payload_len = len;
    return NVMTL_CF_OK;
}

/* Reads and verifies `path`. Returns a malloc'd payload (caller frees) or NULL with *status set. A file that fails the
   check is removed so the next lookup translates it again; a file that simply is not there is left alone. */
static inline uint8_t *nvmtl_cf_read(const char *path, size_t *payload_len, nvmtl_cf_status *status)
{
    nvmtl_cf_status st = NVMTL_CF_MISSING; uint8_t *buf = NULL; size_t len = 0;
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) { if (status) *status = NVMTL_CF_MISSING; return NULL; }
    struct stat sb;
    if (!fstat(fd, &sb) && sb.st_size >= 0) {
        len = (size_t)sb.st_size; buf = malloc(len ? len : 1);
        size_t got = 0;
        while (buf && got < len) { ssize_t r = read(fd, buf + got, len - got); if (r > 0) got += (size_t)r; else if (r < 0 && errno == EINTR) continue; else break; }
        if (buf && got == len) st = nvmtl_cf_check(buf, len, payload_len);
        else st = NVMTL_CF_SHORT;
    }
    close(fd);
    if (st != NVMTL_CF_OK) { free(buf); buf = NULL; unlink(path); }
    if (status) *status = st;
    return buf;
}

/* Writes payload+footer to a temporary file beside `path`, flushes it, renames it into place. Returns 0, or an errno. */
static inline int nvmtl_cf_write(const char *path, const void *payload, size_t n)
{
    if (n > 0xffffffffu) return EFBIG;
    char tmp[1100];
    static unsigned seq;
    int k = snprintf(tmp, sizeof tmp, "%s.tmp-%d-%u", path, (int)getpid(), __atomic_fetch_add(&seq, 1, __ATOMIC_RELAXED));
    if (k <= 0 || (size_t)k >= sizeof tmp) return ENAMETOOLONG;
    int fd = open(tmp, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0644);
    if (fd < 0) return errno;
    uint8_t foot[NVMTL_CF_FOOTER]; uint32_t magic = NVMTL_CF_MAGIC, len = (uint32_t)n; uint64_t h = nvmtl_cf_hash(payload, n);
    memcpy(foot, &magic, 4); memcpy(foot + 4, &len, 4); memcpy(foot + 8, &h, 8);
    int err = 0;
    const uint8_t *parts[2] = { payload, foot }; const size_t sizes[2] = { n, NVMTL_CF_FOOTER };
    for (int p = 0; p < 2 && !err; p++)
        for (size_t off = 0; off < sizes[p];) {
            ssize_t w = write(fd, parts[p] + off, sizes[p] - off);
            if (w > 0) off += (size_t)w; else if (w < 0 && errno == EINTR) continue; else { err = errno ? errno : EIO; break; }
        }
#ifdef F_FULLFSYNC
    if (!err && fcntl(fd, F_FULLFSYNC) != 0 && fsync(fd) != 0) err = errno;
#else
    if (!err && fsync(fd) != 0) err = errno;
#endif
    if (close(fd) != 0 && !err) err = errno;
    if (!err && rename(tmp, path) != 0) err = errno;
    if (err) unlink(tmp);
    return err;
}
#endif

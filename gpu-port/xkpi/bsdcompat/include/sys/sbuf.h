/*
 * FreeBSD sbuf(9) (safe, growable string buffers) for the port. xnu's <sys/sbuf.h> declares the structure but no
 * functions for kexts, so the port carries its own implementation (src/nmx_sbuf.c) under FreeBSD's names.
 */
#ifndef NMXKPI_SYS_SBUF_H
#define NMXKPI_SYS_SBUF_H
#ifndef _SYS_SBUF_H_
#define _SYS_SBUF_H_    /* xnu's guard */
#endif
#include <sys/types.h>
#include <stdarg.h>
struct sbuf {
    char *s_buf;
    void *s_unused;
    ssize_t s_size;
    ssize_t s_len;
    int s_flags;
    int s_error;
};
#define SBUF_FIXEDLEN 0x00000000
#define SBUF_AUTOEXTEND 0x00000001
#define SBUF_INCLUDENUL 0x00000002
#define SBUF_DRAINTOEOR 0x00000004
#define SBUF_NOWAIT 0x00000008
#define SBUF_DYNAMIC 0x00010000
#define SBUF_FINISHED 0x00020000
#define SBUF_DYNSTRUCT 0x00080000
#define sbuf_new nmx_sbuf_new
#define sbuf_clear nmx_sbuf_clear
#define sbuf_cat nmx_sbuf_cat
#define sbuf_cpy nmx_sbuf_cpy
#define sbuf_bcat nmx_sbuf_bcat
#define sbuf_printf nmx_sbuf_printf
#define sbuf_vprintf nmx_sbuf_vprintf
#define sbuf_putc nmx_sbuf_putc
#define sbuf_finish nmx_sbuf_finish
#define sbuf_data nmx_sbuf_data
#define sbuf_len nmx_sbuf_len
#define sbuf_done nmx_sbuf_done
#define sbuf_delete nmx_sbuf_delete
#define sbuf_error nmx_sbuf_error
#define sbuf_trim nmx_sbuf_trim
#define sbuf_bcpy nmx_sbuf_bcpy
#define sbuf_setpos nmx_sbuf_setpos
struct sbuf *sbuf_new(struct sbuf *s, char *buf, int length, int flags);
void sbuf_clear(struct sbuf *s);
int sbuf_cat(struct sbuf *s, const char *str);
int sbuf_cpy(struct sbuf *s, const char *str);
int sbuf_bcat(struct sbuf *s, const void *buf, size_t len);
int sbuf_printf(struct sbuf *s, const char *fmt, ...) __attribute__((__format__(__printf__, 2, 3)));
int sbuf_vprintf(struct sbuf *s, const char *fmt, va_list ap);
int sbuf_putc(struct sbuf *s, int c);
int sbuf_trim(struct sbuf *s);
int sbuf_bcpy(struct sbuf *s, const void *buf, size_t len);
int sbuf_setpos(struct sbuf *s, ssize_t pos);
int sbuf_error(const struct sbuf *s);
int sbuf_finish(struct sbuf *s);
char *sbuf_data(struct sbuf *s);
ssize_t sbuf_len(struct sbuf *s);
int sbuf_done(const struct sbuf *s);
void sbuf_delete(struct sbuf *s);
#define sbuf_new_auto() sbuf_new(NULL, NULL, 0, SBUF_AUTOEXTEND)
#endif

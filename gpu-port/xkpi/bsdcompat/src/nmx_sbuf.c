/* FreeBSD sbuf(9) for the port: a string buffer that is either fixed-size or grows on demand (SBUF_AUTOEXTEND). */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/sbuf.h>

MALLOC_DEFINE(M_SBUF, "sbuf", "string buffers");

#define SBUF_MINSIZE 64

static int sbuf_extend(struct sbuf *s, size_t need)
{
    if (!(s->s_flags & SBUF_AUTOEXTEND))
        return -1;
    size_t ns = s->s_size ? (size_t)s->s_size : SBUF_MINSIZE;
    while (ns < (size_t)s->s_len + need + 1)
        ns *= 2;
    char *nb = malloc(ns, M_SBUF, M_WAITOK);
    if (nb == NULL)
        return -1;
    if (s->s_buf) {
        memcpy(nb, s->s_buf, (size_t)s->s_len);
        if (s->s_flags & SBUF_DYNAMIC)
            free(s->s_buf, M_SBUF);
    }
    s->s_buf = nb;
    s->s_size = (ssize_t)ns;
    s->s_flags |= SBUF_DYNAMIC;
    return 0;
}

struct sbuf *sbuf_new(struct sbuf *s, char *buf, int length, int flags)
{
    int dynstruct = 0;
    if (s == NULL) {
        s = malloc(sizeof(*s), M_SBUF, M_WAITOK | M_ZERO);
        if (s == NULL)
            return NULL;
        dynstruct = SBUF_DYNSTRUCT;
    } else {
        bzero(s, sizeof(*s));
    }
    s->s_flags = (flags & (SBUF_AUTOEXTEND | SBUF_INCLUDENUL)) | dynstruct;
    s->s_buf = buf;
    s->s_size = length;
    if (buf == NULL && length > 0) {
        s->s_buf = malloc((size_t)length, M_SBUF, M_WAITOK);
        s->s_flags |= SBUF_DYNAMIC;
    } else if (buf == NULL && (flags & SBUF_AUTOEXTEND)) {
        (void)sbuf_extend(s, SBUF_MINSIZE);
    }
    if (s->s_buf == NULL && s->s_size != 0) {
        if (dynstruct)
            free(s, M_SBUF);
        return NULL;
    }
    return s;
}

void sbuf_clear(struct sbuf *s)
{
    s->s_len = 0;
    s->s_error = 0;
    s->s_flags &= ~SBUF_FINISHED;
}

int sbuf_bcat(struct sbuf *s, const void *buf, size_t len)
{
    if (s->s_error)
        return -1;
    if ((size_t)(s->s_size - s->s_len) < len + 1 && sbuf_extend(s, len) != 0) {
        s->s_error = ENOMEM;
        return -1;
    }
    memcpy(s->s_buf + s->s_len, buf, len);
    s->s_len += (ssize_t)len;
    return 0;
}

int sbuf_cat(struct sbuf *s, const char *str)
{
    return sbuf_bcat(s, str, strlen(str));
}

int sbuf_cpy(struct sbuf *s, const char *str)
{
    sbuf_clear(s);
    return sbuf_cat(s, str);
}

int sbuf_vprintf(struct sbuf *s, const char *fmt, va_list ap)
{
    if (s->s_error)
        return -1;
    va_list ap2;
    va_copy(ap2, ap);
    int n = vsnprintf(NULL, 0, fmt, ap2);
    va_end(ap2);
    if (n < 0) {
        s->s_error = EINVAL;
        return -1;
    }
    if ((size_t)(s->s_size - s->s_len) < (size_t)n + 1 && sbuf_extend(s, (size_t)n) != 0) {
        s->s_error = ENOMEM;
        return -1;
    }
    vsnprintf(s->s_buf + s->s_len, (size_t)(s->s_size - s->s_len), fmt, ap);
    s->s_len += n;
    return 0;
}

int sbuf_printf(struct sbuf *s, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = sbuf_vprintf(s, fmt, ap);
    va_end(ap);
    return r;
}

int sbuf_putc(struct sbuf *s, int c)
{
    char ch = (char)c;
    return sbuf_bcat(s, &ch, 1);
}

int sbuf_trim(struct sbuf *s)
{
    while (s->s_len > 0 && (s->s_buf[s->s_len - 1] == ' ' || s->s_buf[s->s_len - 1] == '\t' ||
                            s->s_buf[s->s_len - 1] == '\n' || s->s_buf[s->s_len - 1] == '\r'))
        s->s_len--;
    return s->s_error ? -1 : 0;
}

int sbuf_error(const struct sbuf *s)
{
    return s->s_error;
}

int sbuf_finish(struct sbuf *s)
{
    if (s->s_buf && s->s_size > s->s_len)
        s->s_buf[s->s_len] = '\0';
    s->s_flags |= SBUF_FINISHED;
    return s->s_error;
}

char *sbuf_data(struct sbuf *s)
{
    return s->s_buf;
}

ssize_t sbuf_len(struct sbuf *s)
{
    return s->s_error ? -1 : s->s_len + ((s->s_flags & SBUF_INCLUDENUL) ? 1 : 0);
}

int sbuf_done(const struct sbuf *s)
{
    return (s->s_flags & SBUF_FINISHED) != 0;
}

void sbuf_delete(struct sbuf *s)
{
    if (s == NULL)
        return;
    if (s->s_flags & SBUF_DYNAMIC)
        free(s->s_buf, M_SBUF);
    if (s->s_flags & SBUF_DYNSTRUCT)
        free(s, M_SBUF);
}

int sbuf_bcpy(struct sbuf *s, const void *buf, size_t len)
{
    sbuf_clear(s);
    return sbuf_bcat(s, buf, len);
}

int sbuf_setpos(struct sbuf *s, ssize_t pos)
{
    if (pos < 0 || pos > s->s_len)
        return -1;
    s->s_len = pos;
    return 0;
}

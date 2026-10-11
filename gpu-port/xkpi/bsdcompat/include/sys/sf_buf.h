/* FreeBSD sf_buf(9): temporary kernel mappings of a page; every page in this port already has a kernel address. */
#pragma once
struct vm_page;
struct sf_buf;
struct sf_buf *sf_buf_alloc(struct vm_page *m, int flags);
void sf_buf_free(struct sf_buf *sf);
char *sf_buf_kva(struct sf_buf *sf);
struct vm_page *sf_buf_page(struct sf_buf *sf);
#define SFB_CATCH 1
#define SFB_CPUPRIVATE 2
#define SFB_DEFAULT 0
#define SFB_NOWAIT 4

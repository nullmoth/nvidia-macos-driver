/*
 * FreeBSD pctrie(9) on xnu: a map from a 64-bit key (a field of the stored object) to the object. LinuxKPI keeps
 * its DMA mappings in one, keyed by device address. FreeBSD's is a path-compressed radix trie; this one is a
 * red-black tree with the same interface and the same rule that the caller supplies (and locks) node memory.
 */
#ifndef NMXKPI_SYS_PCTRIE_H
#define NMXKPI_SYS_PCTRIE_H
#include <sys/types.h>
#include <sys/errno.h>
#include <sys/tree.h>

struct pctrie_node {
    RB_ENTRY(pctrie_node) link;
    uint64_t key;
    void *val;
};
RB_HEAD(nmx_pctrie_tree, pctrie_node);
struct pctrie {
    struct nmx_pctrie_tree tree;
};

static inline void pctrie_init(struct pctrie *ptree) { RB_INIT(&ptree->tree); }
static inline bool pctrie_is_empty(struct pctrie *ptree) { return RB_EMPTY(&ptree->tree); }
size_t pctrie_node_size(void);
int pctrie_zone_init(void *mem, int size, int flags);

/* nmx_pctrie.c: 'node' is caller memory; insert fails with EEXIST on a duplicate key and leaves node unused */
int nmx_pctrie_insert(struct pctrie *ptree, struct pctrie_node *node, uint64_t key, void *val);
void *nmx_pctrie_lookup(struct pctrie *ptree, uint64_t key);
/* unlinks the node holding 'key' (or any node, for reclaim) and returns it for the caller to free; NULL if none */
struct pctrie_node *nmx_pctrie_remove(struct pctrie *ptree, uint64_t key);
struct pctrie_node *nmx_pctrie_remove_any(struct pctrie *ptree);

#define PCTRIE_DEFINE(name, type, field, allocfn, freefn)                                                    \
static __inline __unused int name##_PCTRIE_INSERT(struct pctrie *ptree, struct type *ptr)                 \
{                                                                                                          \
    struct pctrie_node *n = allocfn(ptree);                                                                \
    if (n == NULL)                                                                                         \
        return (ENOMEM);                                                                                   \
    if (nmx_pctrie_insert(ptree, n, (uint64_t)ptr->field, ptr) != 0) {                                     \
        freefn(ptree, n);                                                                                  \
        return (EEXIST);                                                                                   \
    }                                                                                                      \
    return (0);                                                                                            \
}                                                                                                          \
static __inline __unused struct type *name##_PCTRIE_LOOKUP(struct pctrie *ptree, uint64_t key)            \
{                                                                                                          \
    return ((struct type *)nmx_pctrie_lookup(ptree, key));                                                 \
}                                                                                                          \
static __inline __unused void name##_PCTRIE_REMOVE(struct pctrie *ptree, uint64_t key)                    \
{                                                                                                          \
    struct pctrie_node *n = nmx_pctrie_remove(ptree, key);                                                 \
    if (n != NULL)                                                                                         \
        freefn(ptree, n);                                                                                  \
}                                                                                                          \
static __inline __unused void name##_PCTRIE_RECLAIM(struct pctrie *ptree)                                 \
{                                                                                                          \
    struct pctrie_node *n;                                                                                 \
    while ((n = nmx_pctrie_remove_any(ptree)) != NULL)                                                     \
        freefn(ptree, n);                                                                                  \
}
#endif

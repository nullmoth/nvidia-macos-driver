/* pctrie(9) for the port (see sys/pctrie.h): a red-black tree keyed by the 64-bit value. Callers lock. */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/pctrie.h>

static int pctrie_cmp(struct pctrie_node *a, struct pctrie_node *b)
{
    return a->key < b->key ? -1 : a->key > b->key ? 1 : 0;
}

RB_GENERATE_STATIC(nmx_pctrie_tree, pctrie_node, link, pctrie_cmp);

size_t pctrie_node_size(void)
{
    return sizeof(struct pctrie_node);
}

int pctrie_zone_init(void *mem, int size, int flags)
{
    return 0;
}

int nmx_pctrie_insert(struct pctrie *ptree, struct pctrie_node *node, uint64_t key, void *val)
{
    node->key = key;
    node->val = val;
    return RB_INSERT(nmx_pctrie_tree, &ptree->tree, node) == NULL ? 0 : EEXIST;
}

void *nmx_pctrie_lookup(struct pctrie *ptree, uint64_t key)
{
    struct pctrie_node find = { .key = key };
    struct pctrie_node *n = RB_FIND(nmx_pctrie_tree, &ptree->tree, &find);
    return n ? n->val : NULL;
}

struct pctrie_node *nmx_pctrie_remove(struct pctrie *ptree, uint64_t key)
{
    struct pctrie_node find = { .key = key };
    struct pctrie_node *n = RB_FIND(nmx_pctrie_tree, &ptree->tree, &find);
    if (n != NULL)
        RB_REMOVE(nmx_pctrie_tree, &ptree->tree, n);
    return n;
}

struct pctrie_node *nmx_pctrie_remove_any(struct pctrie *ptree)
{
    struct pctrie_node *n = RB_ROOT(&ptree->tree);
    if (n != NULL)
        RB_REMOVE(nmx_pctrie_tree, &ptree->tree, n);
    return n;
}

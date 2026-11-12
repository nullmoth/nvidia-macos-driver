/*
 * nm_surface_table.h: who may reach which surface. Plain C with no kernel dependency, so the same code runs in the
 * kext and in the host test.
 *
 * Trust model.
 *   - A surface belongs to the process that created it (its surface client). Surface ids are scoped to the owner:
 *     two processes may use the same id and never see each other's surface through it.
 *   - Through the private client a process reaches, by id: its OWN surface, read-write; a surface whose owner has
 *     GRANTED it access, read-only; and, if the caller is privileged (administrator privilege as the kernel's
 *     user-client check reports it: the window server's case), any process's surface, read-only, provided the id
 *     names exactly one surface. An id that two other processes both use is refused rather than guessed.
 *   - Only the owner, or a privileged caller, may grant or revoke access to a surface.
 *   - Everything else is refused. There is no path by which naming an id is enough.
 * Apple's own accelerators on these releases do not restrict who may open their clients (their kexts import no
 * privilege or credential check, only the caller's process id), because every application needs them; so opening
 * is not restricted here either, and the restriction is on what an opened client may reach.
 */
#ifndef NM_SURFACE_TABLE_H
#define NM_SURFACE_TABLE_H

#include <stdint.h>

#define NM_TABLE_SLOTS 1024
#define NM_TABLE_GRANTS 4          /* the window server is the one expected grantee; four leaves room */

enum { NM_ACCESS_NONE = 0, NM_ACCESS_READ = 1, NM_ACCESS_WRITE = 2 };
enum { NM_T_OK = 0, NM_T_FULL = -1, NM_T_EXISTS = -2, NM_T_NOT_FOUND = -3, NM_T_DENIED = -4, NM_T_AMBIGUOUS = -5 };

struct nm_table_entry {
    uint8_t used;
    int32_t owner;                             /* process id of the creating process */
    uint32_t id;
    int32_t grant[NM_TABLE_GRANTS];            /* process ids, 0 = empty */
};
struct nm_surface_table { struct nm_table_entry e[NM_TABLE_SLOTS]; };

/* Adds (owner, id). Returns the slot, or NM_T_EXISTS when that owner already has that id, or NM_T_FULL. */
static inline int nm_table_add(struct nm_surface_table *t, int32_t owner, uint32_t id)
{
    int free_slot = -1;
    for (int i = 0; i < NM_TABLE_SLOTS; i++) {
        if (!t->e[i].used) { if (free_slot < 0) free_slot = i; continue; }
#ifdef NM_TABLE_OLD_LOGIC   /* the first version: ids global, first come first served. Kept only as the tests' control. */
        if (t->e[i].id == id) return NM_T_EXISTS;
#else
        if (t->e[i].id == id && t->e[i].owner == owner) return NM_T_EXISTS;
#endif
    }
    if (free_slot < 0) return NM_T_FULL;
    struct nm_table_entry *n = &t->e[free_slot];
    n->used = 1; n->owner = owner; n->id = id;
    for (int g = 0; g < NM_TABLE_GRANTS; g++) n->grant[g] = 0;
    return free_slot;
}

static inline void nm_table_remove(struct nm_surface_table *t, int slot)
{
    if (slot >= 0 && slot < NM_TABLE_SLOTS) t->e[slot].used = 0;
}

/* The owner's own slot for an id, or NM_T_NOT_FOUND. */
static inline int nm_table_own(const struct nm_surface_table *t, int32_t owner, uint32_t id)
{
    for (int i = 0; i < NM_TABLE_SLOTS; i++)
        if (t->e[i].used && t->e[i].id == id && t->e[i].owner == owner) return i;
    return NM_T_NOT_FOUND;
}

/*
 * Which surface does `caller` reach by `id`, and how? Returns the slot and sets *access, or an error.
 * Order: the caller's own surface; then a surface granted to the caller; then, for a privileged caller, the one
 * surface with that id.
 */
static inline int nm_table_lookup(const struct nm_surface_table *t, int32_t caller, int privileged, uint32_t id, int *access)
{
    *access = NM_ACCESS_NONE;
#ifdef NM_TABLE_OLD_LOGIC
    (void)caller; (void)privileged;
    for (int i = 0; i < NM_TABLE_SLOTS; i++)
        if (t->e[i].used && t->e[i].id == id) { *access = NM_ACCESS_WRITE; return i; }
    return NM_T_NOT_FOUND;
#else
    int own = nm_table_own(t, caller, id);
    if (own >= 0) { *access = NM_ACCESS_WRITE; return own; }
    int granted = -1, others = 0, only = -1;
    for (int i = 0; i < NM_TABLE_SLOTS; i++) {
        if (!t->e[i].used || t->e[i].id != id) continue;
        others++; only = i;
        for (int g = 0; g < NM_TABLE_GRANTS; g++)
            if (t->e[i].grant[g] == caller && caller != 0) { if (granted >= 0) return NM_T_AMBIGUOUS; granted = i; }
    }
    if (granted >= 0) { *access = NM_ACCESS_READ; return granted; }
    if (others == 0) return NM_T_NOT_FOUND;
    if (!privileged) return NM_T_DENIED;
    if (others > 1) return NM_T_AMBIGUOUS;
    *access = NM_ACCESS_READ;
    return only;
#endif
}

/* Grants (or revokes) `grantee`'s access to the surface (owner, id). Only the owner or a privileged caller may. */
static inline int nm_table_grant(struct nm_surface_table *t, int32_t caller, int privileged, int32_t owner, uint32_t id,
                                 int32_t grantee, int revoke)
{
    if (caller != owner && !privileged) return NM_T_DENIED;
    if (grantee <= 0) return NM_T_DENIED;
    int s = nm_table_own(t, owner, id);
    if (s < 0) return NM_T_NOT_FOUND;
    int free_g = -1;
    for (int g = 0; g < NM_TABLE_GRANTS; g++) {
        if (t->e[s].grant[g] == grantee) { if (revoke) t->e[s].grant[g] = 0; return NM_T_OK; }
        if (t->e[s].grant[g] == 0 && free_g < 0) free_g = g;
    }
    if (revoke) return NM_T_OK;
    if (free_g < 0) return NM_T_FULL;
    t->e[s].grant[free_g] = grantee;
    return NM_T_OK;
}

#endif

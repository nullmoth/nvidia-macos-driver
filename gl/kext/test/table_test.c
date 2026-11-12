/*
 * table_test.c: host test of the ownership and grant rules (nm_surface_table.h), which run unchanged in the kext.
 * Built twice: with the rules as they are (every arm must pass) and with NM_TABLE_OLD_LOGIC, the first version in
 * which ids were global and naming an id was enough (the build script requires that build to FAIL these arms: the
 * control that shows the test can tell the two apart).
 */
#include "../nm_surface_table.h"
#include <stdio.h>
#include <stdlib.h>

static int failed, arms;
static void arm(int ok, const char *name, int a, int b) { arms++; failed += !ok; printf("%s  %s  [%d, %d]\n", ok ? "PASS" : "FAIL", name, a, b); }

int main(void)
{
    struct nm_surface_table *t = calloc(1, sizeof *t);
    enum { OWNER = 100, STRANGER = 200, GRANTEE = 300, OTHER = 400, ADMIN = 1 };
    int acc = -1;
    int s1 = nm_table_add(t, OWNER, 7);
    int r = nm_table_lookup(t, OWNER, 0, 7, &acc);
    arm(s1 >= 0 && r == s1 && acc == NM_ACCESS_WRITE, "the owner reaches its own surface, read-write", r, acc);

    r = nm_table_lookup(t, STRANGER, 0, 7, &acc);
    arm(r == NM_T_DENIED && acc == NM_ACCESS_NONE, "a stranger that names the id is refused", r, acc);

    int g = nm_table_grant(t, STRANGER, 0, OWNER, 7, STRANGER, 0);
    arm(g == NM_T_DENIED && nm_table_lookup(t, STRANGER, 0, 7, &acc) == NM_T_DENIED, "a stranger cannot grant itself access", g, acc);

    g = nm_table_grant(t, OWNER, 0, OWNER, 7, GRANTEE, 0);
    r = nm_table_lookup(t, GRANTEE, 0, 7, &acc);
    arm(g == NM_T_OK && r == s1 && acc == NM_ACCESS_READ, "a process the owner granted reaches the surface, read-only", r, acc);

    g = nm_table_grant(t, OWNER, 0, OWNER, 7, GRANTEE, 1);
    r = nm_table_lookup(t, GRANTEE, 0, 7, &acc);
    arm(g == NM_T_OK && r == NM_T_DENIED && acc == NM_ACCESS_NONE, "after the grant is revoked the same process is refused", r, acc);

    int s2 = nm_table_add(t, OTHER, 7);
    int a1 = -1, a2 = -1, r1 = nm_table_lookup(t, OWNER, 0, 7, &a1), r2 = nm_table_lookup(t, OTHER, 0, 7, &a2);
    arm(s2 >= 0 && s2 != s1 && r1 == s1 && r2 == s2, "two processes using the same id get two surfaces and each reaches only its own", r1, r2);

    r = nm_table_add(t, OWNER, 7);
    arm(r == NM_T_EXISTS, "one process cannot create the same id twice", r, 0);

    r = nm_table_lookup(t, ADMIN, 1, 7, &acc);
    arm(r == NM_T_AMBIGUOUS && acc == NM_ACCESS_NONE, "a privileged caller is refused an id that two other processes use: it is not guessed", r, acc);

    nm_table_remove(t, s2);
    r = nm_table_lookup(t, ADMIN, 1, 7, &acc);
    arm(r == s1 && acc == NM_ACCESS_READ, "a privileged caller reaches the one surface with an id, read-only", r, acc);

    nm_table_remove(t, s1);
    r = nm_table_lookup(t, OWNER, 0, 7, &acc);
    arm(r == NM_T_NOT_FOUND, "after removal the id reaches nothing", r, acc);

    printf("%d arms, %d failed\n", arms, failed);
    return failed != 0;
}

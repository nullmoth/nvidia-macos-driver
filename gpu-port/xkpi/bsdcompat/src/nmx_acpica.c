/*
 * The ACPICA calls the port's users make (LinuxKPI's linux_acpi.c, the AMD driver's amdgpu_acpi.c), implemented on
 * macOS's own ACPI: AppleACPIPlatform has already loaded the tables and runs every method, reached through
 * xnu/nmx_platform.cpp. Nothing here interprets AML.
 *
 * Handles: ACPI_ROOT_OBJECT is the namespace root; every other ACPI_HANDLE is a record naming either a device node
 * (a registry entry of the ACPI plane) or an object inside one (a method or data object, which are not registry
 * entries). Records are made once per (node, name) and live until unload, as ACPICA namespace handles do.
 *
 * The acpi_* helpers at the end are FreeBSD's (sys/dev/acpica/acpi.c, BSD-2-Clause: Copyright (c) 2000 Takanori
 * Watanabe, 2000 Mitsuru IWASAKI, 2000, 2001 Michael Smith, 2000 BSDi), adapted to this file.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/kernel.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/queue.h>
#include <sys/bus.h>
#include <contrib/dev/acpica/include/acpi.h>
#include <dev/acpica/acpivar.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_ACPICA, "acpica", "ACPI objects and buffers");

ACPI_TABLE_FADT AcpiGbl_FADT;

struct nmx_acpi_handle {
    TAILQ_ENTRY(nmx_acpi_handle) link;
    void *node;                     /* retained device node */
    char name[5];                   /* "" for the device itself, else the object's 4-character name */
    device_t dev;                   /* acpi_get_device's record, made on first use */
};

static TAILQ_HEAD(, nmx_acpi_handle) handles = TAILQ_HEAD_INITIALIZER(handles);
static struct mtx handles_mtx;
MTX_SYSINIT(nmx_acpica, &handles_mtx, "acpi handles", MTX_DEF);

/* the handle for (node, name); consumes the caller's node reference */
static ACPI_HANDLE handle_for(void *node, const char *name)
{
    struct nmx_acpi_handle *h, *n = malloc(sizeof(*n), M_ACPICA, M_WAITOK | M_ZERO);
    if (n == NULL) {
        nmx_xnu_acpi_node_put(node);
        return NULL;
    }
    n->node = node;
    if (name)
        strlcpy(n->name, name, sizeof(n->name));
    mtx_lock(&handles_mtx);
    TAILQ_FOREACH(h, &handles, link)
        if (h->node == node && strcmp(h->name, n->name) == 0)
            break;
    if (h == NULL) {
        TAILQ_INSERT_TAIL(&handles, n, link);
        h = n;
        n = NULL;
    }
    mtx_unlock(&handles_mtx);
    if (n != NULL) {
        nmx_xnu_acpi_node_put(node);
        free(n, M_ACPICA);
    }
    return h;
}

static struct nmx_acpi_handle *rec(ACPI_HANDLE h)
{
    return (struct nmx_acpi_handle *)h;
}

/* the device node a handle names or lies in (a new reference), and the object name within it ("" = the device) */
static void *node_of(ACPI_HANDLE h, const char **name)
{
    if (h == NULL || h == ACPI_ROOT_OBJECT) {
        *name = "";
        return nmx_xnu_acpi_node_root();
    }
    *name = rec(h)->name;
    nmx_xnu_acpi_node_get(rec(h)->node);
    return rec(h)->node;
}

/*
 * Resolves 'path' relative to 'parent' (ACPICA rules: '\' starts at the root, each '^' goes up one scope, segments
 * are separated by '.', no upward search for single names). Returns the node (a reference) and the object name.
 */
static ACPI_STATUS resolve(ACPI_HANDLE parent, const char *path, void **node_out, char name_out[5])
{
    *node_out = NULL;
    name_out[0] = 0;
    const char *pname;
    void *node;
    if (path != NULL && path[0] == '\\') {
        node = nmx_xnu_acpi_node_root();
        path++;
    } else {
        node = node_of(parent, &pname);
        if (path == NULL || path[0] == 0) {
            *node_out = node;
            strlcpy(name_out, pname, 5);
            return node ? AE_OK : AE_NOT_FOUND;
        }
        if (pname[0] != 0) {        /* an object handle is not a scope */
            nmx_xnu_acpi_node_put(node);
            return AE_BAD_PARAMETER;
        }
        while (*path == '^' && node != NULL) {
            void *up = nmx_xnu_acpi_node_parent(node);
            nmx_xnu_acpi_node_put(node);
            node = up;
            path++;
        }
    }
    if (node == NULL)
        return AE_NOT_FOUND;
    while (*path) {
        char seg[5] = {};
        int n = 0;
        while (path[n] && path[n] != '.' && n < 4) {
            seg[n] = path[n];
            n++;
        }
        if (path[n] && path[n] != '.') {    /* a segment longer than four characters */
            nmx_xnu_acpi_node_put(node);
            return AE_BAD_PATHNAME;
        }
        path += n;
        int last = *path == 0;
        if (!last)
            path++;
        void *child = nmx_xnu_acpi_node_child(node, seg);
        if (child != NULL) {
            nmx_xnu_acpi_node_put(node);
            node = child;
            continue;
        }
        if (last && nmx_xnu_acpi_has(node, seg)) {
            *node_out = node;
            strlcpy(name_out, seg, 5);
            return AE_OK;
        }
        nmx_xnu_acpi_node_put(node);
        return AE_NOT_FOUND;
    }
    *node_out = node;
    return AE_OK;
}

ACPI_STATUS AcpiGetHandle(ACPI_HANDLE Parent, const char *Pathname, ACPI_HANDLE *RetHandle)
{
    if (RetHandle == NULL || (Parent == NULL && Pathname == NULL))
        return AE_BAD_PARAMETER;
    *RetHandle = NULL;
    void *node;
    char name[5];
    ACPI_STATUS st = resolve(Parent, Pathname, &node, name);
    if (ACPI_FAILURE(st))
        return st;
    *RetHandle = handle_for(node, name);
    return *RetHandle ? AE_OK : AE_NO_MEMORY;
}

void *AcpiOsAllocate(ACPI_SIZE Size)
{
    return malloc((size_t)Size, M_ACPICA, M_NOWAIT);
}

void AcpiOsFree(void *Memory)
{
    free(Memory, M_ACPICA);
}

/* hands 'size' bytes to the caller's ACPI_BUFFER with ACPICA's rules: allocate on request, else check the length */
static ACPI_STATUS buffer_reserve(ACPI_BUFFER *buf, ACPI_SIZE size)
{
    if (buf->Length == ACPI_ALLOCATE_BUFFER || buf->Length == ACPI_ALLOCATE_LOCAL_BUFFER) {
        buf->Pointer = AcpiOsAllocate(size);
        if (buf->Pointer == NULL)
            return AE_NO_MEMORY;
        buf->Length = size;
        bzero(buf->Pointer, (size_t)size);
        return AE_OK;
    }
    if (buf->Length < size || buf->Pointer == NULL) {
        buf->Length = size;
        return AE_BUFFER_OVERFLOW;
    }
    buf->Length = size;
    bzero(buf->Pointer, (size_t)size);
    return AE_OK;
}

/* ACPICA argument objects -> boundary objects; strings and buffers are referenced, packages copied */
static int count_args(const ACPI_OBJECT *o, int depth)
{
    if (depth > 8)
        return -1;
    if (o->Type != ACPI_TYPE_PACKAGE)
        return 1;
    int n = 1;
    for (UINT32 i = 0; i < o->Package.Count; i++) {
        int c = count_args(&o->Package.Elements[i], depth + 1);
        if (c < 0)
            return -1;
        n += c;
    }
    return n;
}

static int conv_arg(const ACPI_OBJECT *o, struct nmx_acpi_obj *dst, struct nmx_acpi_obj **pool, int depth)
{
    bzero(dst, sizeof(*dst));
    switch (o->Type) {
    case ACPI_TYPE_INTEGER:
        dst->type = NMX_ACPI_INTEGER;
        dst->integer = o->Integer.Value;
        return 0;
    case ACPI_TYPE_STRING:
        dst->type = NMX_ACPI_STRING;
        dst->ptr = o->String.Pointer;
        dst->len = o->String.Length;
        return 0;
    case ACPI_TYPE_BUFFER:
        dst->type = NMX_ACPI_BUFFER;
        dst->ptr = o->Buffer.Pointer;
        dst->len = o->Buffer.Length;
        return 0;
    case ACPI_TYPE_PACKAGE: {
        dst->type = NMX_ACPI_PACKAGE;
        dst->count = o->Package.Count;
        struct nmx_acpi_obj *el = *pool;
        *pool += o->Package.Count;
        dst->ptr = el;
        for (UINT32 i = 0; i < o->Package.Count; i++)
            if (conv_arg(&o->Package.Elements[i], &el[i], pool, depth + 1) != 0)
                return -1;
        return 0;
    }
    default:
        return -1;              /* references and other local types cannot cross to macOS's interpreter */
    }
}

/* boundary result tree -> ACPICA object tree laid out in one buffer (objects first, then data) */
static ACPI_SIZE result_size(const struct nmx_acpi_obj *o, int depth)
{
    ACPI_SIZE n = sizeof(ACPI_OBJECT);
    if (o->type == NMX_ACPI_STRING)
        n += roundup2(o->len + 1, 8);
    else if (o->type == NMX_ACPI_BUFFER)
        n += roundup2(o->len, 8);
    else if (o->type == NMX_ACPI_PACKAGE && depth <= 8)
        for (uint32_t i = 0; i < o->count; i++)
            n += result_size(&((const struct nmx_acpi_obj *)o->ptr)[i], depth + 1);
    return n;
}

static void result_fill(const struct nmx_acpi_obj *o, ACPI_OBJECT *dst, char **cursor, int depth)
{
    switch (o->type) {
    case NMX_ACPI_INTEGER:
        dst->Type = ACPI_TYPE_INTEGER;
        dst->Integer.Value = o->integer;
        break;
    case NMX_ACPI_STRING:
        dst->Type = ACPI_TYPE_STRING;
        dst->String.Length = (UINT32)o->len;
        dst->String.Pointer = *cursor;
        memcpy(*cursor, o->ptr, o->len);
        (*cursor)[o->len] = 0;
        *cursor += roundup2(o->len + 1, 8);
        break;
    case NMX_ACPI_BUFFER:
        dst->Type = ACPI_TYPE_BUFFER;
        dst->Buffer.Length = (UINT32)o->len;
        dst->Buffer.Pointer = (UINT8 *)*cursor;
        memcpy(*cursor, o->ptr, o->len);
        *cursor += roundup2(o->len, 8);
        break;
    case NMX_ACPI_PACKAGE: {
        dst->Type = ACPI_TYPE_PACKAGE;
        dst->Package.Count = depth <= 8 ? o->count : 0;
        ACPI_OBJECT *el = (ACPI_OBJECT *)*cursor;
        *cursor += sizeof(ACPI_OBJECT) * dst->Package.Count;
        dst->Package.Elements = el;
        for (UINT32 i = 0; i < dst->Package.Count; i++)
            result_fill(&((const struct nmx_acpi_obj *)o->ptr)[i], &el[i], cursor, depth + 1);
        break;
    }
    default:
        dst->Type = ACPI_TYPE_ANY;
        break;
    }
}

static ACPI_STATUS status_of(int err)
{
    switch (err) {
    case 0: return AE_OK;
    case ENOENT: return AE_NOT_FOUND;
    case EINVAL: return AE_BAD_PARAMETER;
    case ENOMEM: return AE_NO_MEMORY;
    default: return AE_ERROR;
    }
}

ACPI_STATUS AcpiEvaluateObject(ACPI_HANDLE Object, ACPI_STRING Pathname, ACPI_OBJECT_LIST *Params,
                               ACPI_BUFFER *Ret)
{
    void *node;
    char name[5];
    ACPI_STATUS st = resolve(Object, Pathname, &node, name);
    if (ACPI_FAILURE(st))
        return st;
    if (name[0] == 0) {             /* a device has no value to evaluate */
        nmx_xnu_acpi_node_put(node);
        return AE_TYPE;
    }
    struct nmx_acpi_obj *args = NULL;
    int nargs = 0, total = 0;
    if (Params != NULL && Params->Count > 0) {
        if (Params->Count > 7) {
            nmx_xnu_acpi_node_put(node);
            return AE_LIMIT;
        }
        for (UINT32 i = 0; i < Params->Count; i++) {
            int c = count_args(&Params->Pointer[i], 0);
            if (c < 0) {
                nmx_xnu_acpi_node_put(node);
                return AE_BAD_PARAMETER;
            }
            total += c;
        }
        args = malloc(sizeof(*args) * (size_t)total, M_ACPICA, M_WAITOK | M_ZERO);
        if (args == NULL) {
            nmx_xnu_acpi_node_put(node);
            return AE_NO_MEMORY;
        }
        nargs = (int)Params->Count;
        struct nmx_acpi_obj *pool = args + nargs;
        for (int i = 0; i < nargs; i++)
            if (conv_arg(&Params->Pointer[i], &args[i], &pool, 0) != 0) {
                free(args, M_ACPICA);
                nmx_xnu_acpi_node_put(node);
                return AE_BAD_PARAMETER;
            }
    }
    struct nmx_acpi_obj *res = NULL;
    size_t res_size = 0;
    int err = nmx_xnu_acpi_evaluate(node, name, args, nargs, Ret ? &res : NULL, &res_size);
    free(args, M_ACPICA);
    nmx_xnu_acpi_node_put(node);
    if (err)
        return status_of(err);
    if (Ret == NULL) {
        nmx_xnu_acpi_free(res, res_size);
        return AE_OK;
    }
    if (res == NULL) {              /* the method returned nothing */
        if (Ret->Length != ACPI_ALLOCATE_BUFFER && Ret->Length != ACPI_ALLOCATE_LOCAL_BUFFER)
            Ret->Length = 0;
        else
            Ret->Pointer = NULL, Ret->Length = 0;
        return AE_OK;
    }
    ACPI_SIZE need = result_size(res, 0);
    st = buffer_reserve(Ret, need);
    if (ACPI_SUCCESS(st)) {
        char *cursor = (char *)Ret->Pointer + sizeof(ACPI_OBJECT);
        result_fill(res, (ACPI_OBJECT *)Ret->Pointer, &cursor, 0);
    }
    nmx_xnu_acpi_free(res, res_size);
    return st;
}

ACPI_STATUS AcpiEvaluateObjectTyped(ACPI_HANDLE Object, ACPI_STRING Pathname, ACPI_OBJECT_LIST *Params,
                                    ACPI_BUFFER *Ret, ACPI_OBJECT_TYPE ReturnType)
{
    if (Ret == NULL)
        return AE_BAD_PARAMETER;
    int allocate = Ret->Length == ACPI_ALLOCATE_BUFFER || Ret->Length == ACPI_ALLOCATE_LOCAL_BUFFER;
    ACPI_STATUS st = AcpiEvaluateObject(Object, Pathname, Params, Ret);
    if (ACPI_FAILURE(st) || ReturnType == ACPI_TYPE_ANY)
        return st;
    if (Ret->Length == 0)
        return AE_NULL_OBJECT;
    if (((ACPI_OBJECT *)Ret->Pointer)->Type == ReturnType)
        return AE_OK;
    if (allocate) {
        AcpiOsFree(Ret->Pointer);
        Ret->Pointer = NULL;
    }
    Ret->Length = 0;
    return AE_TYPE;
}

ACPI_STATUS AcpiGetName(ACPI_HANDLE Object, UINT32 NameType, ACPI_BUFFER *Ret)
{
    if (Ret == NULL || NameType > ACPI_FULL_PATHNAME_NO_TRAILING)
        return AE_BAD_PARAMETER;
    const char *oname;
    void *node = node_of(Object, &oname);
    if (node == NULL)
        return AE_BAD_PARAMETER;
    char path[256];
    if (NameType == ACPI_SINGLE_NAME) {
        if (oname[0]) {
            snprintf(path, sizeof(path), "%-4.4s", oname);
            for (int i = 0; i < 4; i++)
                if (path[i] == ' ')
                    path[i] = '_';
        } else {
            nmx_xnu_acpi_node_name(node, path);
        }
        nmx_xnu_acpi_node_put(node);
    } else {
        /* the full path, built from the leaf up: "\_SB_.PC00.GFX0" */
        char segs[32][5];
        int n = 0;
        if (oname[0] && n < 32) {
            snprintf(segs[n], 5, "%-4.4s", oname);
            for (int i = 0; i < 4; i++)
                if (segs[n][i] == ' ')
                    segs[n][i] = '_';
            n++;
        }
        while (node != NULL) {
            void *up = nmx_xnu_acpi_node_parent(node);
            if (up != NULL && n < 32)       /* the root itself has no name */
                nmx_xnu_acpi_node_name(node, segs[n++]);
            nmx_xnu_acpi_node_put(node);
            node = up;
        }
        /* ACPI_FULL_PATHNAME_NO_TRAILING drops each segment's '_' padding */
        int trim = NameType == ACPI_FULL_PATHNAME_NO_TRAILING;
        size_t p = 0;
        path[p++] = '\\';
        for (int i = n - 1; i >= 0 && p + 6 < sizeof(path); i--) {
            if (i != n - 1)
                path[p++] = '.';
            int l = 4;
            while (trim && l > 1 && segs[i][l - 1] == '_')
                l--;
            memcpy(path + p, segs[i], (size_t)l);
            p += (size_t)l;
        }
        path[p] = 0;
    }
    size_t len = strlen(path) + 1;
    ACPI_STATUS st = buffer_reserve(Ret, len);
    if (ACPI_SUCCESS(st))
        memcpy(Ret->Pointer, path, len);
    return st;
}

/* an EISA id integer (as _HID/_CID return them) in its text form, e.g. 0x0A0CD041 -> "PNP0C0A" */
static void eisa_to_string(UINT64 v, char out[8])
{
    static const char hex[] = "0123456789ABCDEF";
    uint32_t id = (uint32_t)v;
    id = ((id & 0xff) << 24) | ((id & 0xff00) << 8) | ((id >> 8) & 0xff00) | (id >> 24);
    out[0] = (char)('@' + ((id >> 26) & 0x1f));
    out[1] = (char)('@' + ((id >> 21) & 0x1f));
    out[2] = (char)('@' + ((id >> 16) & 0x1f));
    out[3] = hex[(id >> 12) & 0xf];
    out[4] = hex[(id >> 8) & 0xf];
    out[5] = hex[(id >> 4) & 0xf];
    out[6] = hex[id & 0xf];
    out[7] = 0;
}

/* the text of an id object (_HID/_UID/_CID element): string as is, integer as an EISA id (or decimal for _UID) */
static int id_text(const struct nmx_acpi_obj *o, int is_uid, char *out, size_t max)
{
    if (o->type == NMX_ACPI_STRING) {
        strlcpy(out, (const char *)o->ptr, max);
        return 1;
    }
    if (o->type == NMX_ACPI_INTEGER) {
        if (is_uid)
            snprintf(out, max, "%llu", (unsigned long long)o->integer);
        else {
            char e[8];
            eisa_to_string(o->integer, e);
            strlcpy(out, e, max);
        }
        return 1;
    }
    return 0;
}

ACPI_STATUS AcpiGetObjectInfo(ACPI_HANDLE Object, ACPI_DEVICE_INFO **ReturnBuffer)
{
    if (ReturnBuffer == NULL)
        return AE_BAD_PARAMETER;
    *ReturnBuffer = NULL;
    const char *oname;
    void *node = node_of(Object, &oname);
    if (node == NULL)
        return AE_BAD_PARAMETER;
    char hid[64] = "", uid[64] = "", cids[8][64];
    int ncid = 0;
    UINT16 valid = 0;
    UINT64 adr = 0;
    char nm[5];
    if (oname[0] == 0) {
        struct nmx_acpi_obj *r;
        size_t rs;
        if (nmx_xnu_acpi_evaluate(node, "_HID", NULL, 0, &r, &rs) == 0 && r) {
            if (id_text(r, 0, hid, sizeof(hid)))
                valid |= ACPI_VALID_HID;
            nmx_xnu_acpi_free(r, rs);
        }
        if (nmx_xnu_acpi_evaluate(node, "_UID", NULL, 0, &r, &rs) == 0 && r) {
            if (id_text(r, 1, uid, sizeof(uid)))
                valid |= ACPI_VALID_UID;
            nmx_xnu_acpi_free(r, rs);
        }
        if (nmx_xnu_acpi_evaluate(node, "_CID", NULL, 0, &r, &rs) == 0 && r) {
            if (r->type == NMX_ACPI_PACKAGE) {
                const struct nmx_acpi_obj *el = r->ptr;
                for (uint32_t i = 0; i < r->count && ncid < 8; i++)
                    if (id_text(&el[i], 0, cids[ncid], sizeof(cids[0])))
                        ncid++;
            } else if (id_text(r, 0, cids[0], sizeof(cids[0]))) {
                ncid = 1;
            }
            if (ncid)
                valid |= ACPI_VALID_CID;
            nmx_xnu_acpi_free(r, rs);
        }
        if (nmx_xnu_acpi_evaluate(node, "_ADR", NULL, 0, &r, &rs) == 0 && r) {
            if (r->type == NMX_ACPI_INTEGER) {
                adr = r->integer;
                valid |= ACPI_VALID_ADR;
            }
            nmx_xnu_acpi_free(r, rs);
        }
        nmx_xnu_acpi_node_name(node, nm);
    } else {
        snprintf(nm, sizeof(nm), "%-4.4s", oname);
    }
    nmx_xnu_acpi_node_put(node);

    /* one allocation: the structure, the id array, then the strings */
    size_t size = sizeof(ACPI_DEVICE_INFO) + sizeof(ACPI_PNP_DEVICE_ID) * (size_t)ncid + strlen(hid) + 1 + strlen(uid) + 1;
    for (int i = 0; i < ncid; i++)
        size += strlen(cids[i]) + 1;
    ACPI_DEVICE_INFO *info = AcpiOsAllocate(size);
    if (info == NULL)
        return AE_NO_MEMORY;
    bzero(info, size);
    char *str = (char *)info + sizeof(ACPI_DEVICE_INFO) + sizeof(ACPI_PNP_DEVICE_ID) * (size_t)ncid;
    info->InfoSize = (UINT32)size;
    memcpy(&info->Name, nm, 4);
    info->Type = oname[0] ? ACPI_TYPE_ANY : ACPI_TYPE_DEVICE;
    info->Valid = valid;
    info->Address = adr;
    memset(info->HighestDstates, 0xff, sizeof(info->HighestDstates));
    memset(info->LowestDstates, 0xff, sizeof(info->LowestDstates));
#define PUT_ID(dst, text) do { size_t l_ = strlen(text) + 1; memcpy(str, (text), l_); (dst).String = str; \
                               (dst).Length = (UINT32)l_; str += l_; } while (0)
    PUT_ID(info->HardwareId, hid);
    PUT_ID(info->UniqueId, uid);
    info->CompatibleIdList.Count = (UINT32)ncid;
    info->CompatibleIdList.ListSize = (UINT32)(sizeof(ACPI_PNP_DEVICE_ID_LIST) + sizeof(ACPI_PNP_DEVICE_ID) * (size_t)ncid);
    for (int i = 0; i < ncid; i++)
        PUT_ID(info->CompatibleIdList.Ids[i], cids[i]);
#undef PUT_ID
    *ReturnBuffer = info;
    return AE_OK;
}

/* walks device objects only: methods and data objects are not enumerable through macOS's ACPI plane */
static ACPI_STATUS walk(void *node, UINT32 level, UINT32 max_depth, ACPI_WALK_CALLBACK down, ACPI_WALK_CALLBACK up,
                        void *ctx, void **ret)
{
    void *c = NULL;
    while ((c = nmx_xnu_acpi_node_next_child(node, c)) != NULL) {
        nmx_xnu_acpi_node_get(c);
        ACPI_HANDLE h = handle_for(c, "");      /* consumes the extra reference */
        ACPI_STATUS st = AE_OK;
        if (down != NULL)
            st = down(h, level, ctx, ret);
        if (st == AE_OK && level < max_depth)
            st = walk(c, level + 1, max_depth, down, up, ctx, ret);
        else if (st == AE_CTRL_DEPTH)
            st = AE_OK;
        if (st == AE_OK && up != NULL)
            st = up(h, level, ctx, ret);
        if (st != AE_OK) {
            nmx_xnu_acpi_node_put(c);
            return st;
        }
        nmx_xnu_acpi_node_put(c);       /* the iteration reference; next_child re-finds by pointer */
    }
    return AE_OK;
}

ACPI_STATUS AcpiWalkNamespace(ACPI_OBJECT_TYPE Type, ACPI_HANDLE StartObject, UINT32 MaxDepth,
                              ACPI_WALK_CALLBACK DescendingCallback, ACPI_WALK_CALLBACK AscendingCallback,
                              void *Context, void **ReturnValue)
{
    if (MaxDepth == 0 || (DescendingCallback == NULL && AscendingCallback == NULL))
        return AE_BAD_PARAMETER;
    if (Type != ACPI_TYPE_DEVICE && Type != ACPI_TYPE_ANY)
        return AE_OK;
    const char *oname;
    void *start = node_of(StartObject, &oname);
    if (start == NULL)
        return AE_BAD_PARAMETER;
    ACPI_STATUS st = walk(start, 1, MaxDepth, DescendingCallback, AscendingCallback, Context, ReturnValue);
    nmx_xnu_acpi_node_put(start);
    return st == AE_CTRL_TERMINATE ? AE_OK : st;
}

/*
 * Tables by signature: a copy of each is kept for the life of the kext, as ACPICA keeps its mapped tables (the AMD
 * driver reads the VFCT table for the VBIOS image that UEFI firmware leaves there).
 */
struct table_copy {
    TAILQ_ENTRY(table_copy) link;
    char sig[5];
    UINT32 instance;
    ACPI_TABLE_HEADER *table;
    size_t len;
};
static TAILQ_HEAD(, table_copy) tables = TAILQ_HEAD_INITIALIZER(tables);

ACPI_STATUS AcpiGetTable(char *Signature, UINT32 Instance, ACPI_TABLE_HEADER **OutTable)
{
    if (Signature == NULL || OutTable == NULL)
        return AE_BAD_PARAMETER;
    *OutTable = NULL;
    if (Instance == 0)
        Instance = 1;
    struct table_copy *t;
    mtx_lock(&handles_mtx);
    TAILQ_FOREACH(t, &tables, link)
        if (strncmp(t->sig, Signature, 4) == 0 && t->instance == Instance)
            break;
    mtx_unlock(&handles_mtx);
    if (t != NULL) {
        *OutTable = t->table;
        return AE_OK;
    }
    size_t len = nmx_xnu_acpi_table(Signature, (int)Instance, NULL, 0);
    if (len < sizeof(ACPI_TABLE_HEADER))
        return AE_NOT_FOUND;
    void *buf = malloc(len, M_ACPICA, M_WAITOK);
    t = malloc(sizeof(*t), M_ACPICA, M_WAITOK | M_ZERO);
    if (buf == NULL || t == NULL || nmx_xnu_acpi_table(Signature, (int)Instance, buf, len) != len) {
        free(buf, M_ACPICA);
        free(t, M_ACPICA);
        return AE_NO_MEMORY;
    }
    strlcpy(t->sig, Signature, sizeof(t->sig));
    t->instance = Instance;
    t->table = buf;
    t->len = len;
    mtx_lock(&handles_mtx);
    TAILQ_INSERT_TAIL(&tables, t, link);
    mtx_unlock(&handles_mtx);
    *OutTable = t->table;
    return AE_OK;
}

void AcpiPutTable(ACPI_TABLE_HEADER *Table)
{
}

const char *AcpiFormatException(ACPI_STATUS Status)
{
    switch (Status) {
    case AE_OK: return "AE_OK";
    case AE_ERROR: return "AE_ERROR";
    case AE_NO_ACPI_TABLES: return "AE_NO_ACPI_TABLES";
    case AE_NO_NAMESPACE: return "AE_NO_NAMESPACE";
    case AE_NO_MEMORY: return "AE_NO_MEMORY";
    case AE_NOT_FOUND: return "AE_NOT_FOUND";
    case AE_NOT_EXIST: return "AE_NOT_EXIST";
    case AE_TYPE: return "AE_TYPE";
    case AE_NULL_OBJECT: return "AE_NULL_OBJECT";
    case AE_BUFFER_OVERFLOW: return "AE_BUFFER_OVERFLOW";
    case AE_LIMIT: return "AE_LIMIT";
    case AE_TIME: return "AE_TIME";
    case AE_BAD_PARAMETER: return "AE_BAD_PARAMETER";
    case AE_BAD_PATHNAME: return "AE_BAD_PATHNAME";
    case AE_BAD_DATA: return "AE_BAD_DATA";
    case AE_SUPPORT: return "AE_SUPPORT";
    case AE_CTRL_TERMINATE: return "AE_CTRL_TERMINATE";
    case AE_CTRL_DEPTH: return "AE_CTRL_DEPTH";
    default: return "AE_UNKNOWN_STATUS";
    }
}

/* the FADT, for its flags (the AMD driver reads ACPI_FADT_LOW_POWER_S0 to tell s2idle machines) */
static void nmx_acpica_init(void *arg)
{
    size_t n = nmx_xnu_acpi_table("FACP", 1, &AcpiGbl_FADT, sizeof(AcpiGbl_FADT));
    if (n == 0) {
        bzero(&AcpiGbl_FADT, sizeof(AcpiGbl_FADT));
        printf("NullMoth xkpi: no FADT; ACPI power flags read as zero\n");
    } else if (n > sizeof(AcpiGbl_FADT)) {
        /* a newer FADT than this ACPICA knows: the known prefix was copied */
    } else if (n < sizeof(AcpiGbl_FADT)) {
        bzero((char *)&AcpiGbl_FADT + n, sizeof(AcpiGbl_FADT) - n);
    }
}
SYSINIT(nmx_acpica, SI_SUB_DRIVERS, SI_ORDER_FIRST, nmx_acpica_init, NULL);

static void nmx_acpica_uninit(void *arg)
{
    struct nmx_acpi_handle *h;
    mtx_lock(&handles_mtx);
    while ((h = TAILQ_FIRST(&handles)) != NULL) {
        TAILQ_REMOVE(&handles, h, link);
        mtx_unlock(&handles_mtx);
        if (h->dev != NULL)
            device_delete_child(root_bus, h->dev);
        nmx_xnu_acpi_node_put(h->node);
        free(h, M_ACPICA);
        mtx_lock(&handles_mtx);
    }
    struct table_copy *t;
    while ((t = TAILQ_FIRST(&tables)) != NULL) {
        TAILQ_REMOVE(&tables, t, link);
        mtx_unlock(&handles_mtx);
        free(t->table, M_ACPICA);
        free(t, M_ACPICA);
        mtx_lock(&handles_mtx);
    }
    mtx_unlock(&handles_mtx);
}
SYSUNINIT(nmx_acpica, SI_SUB_DRIVERS, SI_ORDER_FIRST, nmx_acpica_uninit, NULL);

/* ---- FreeBSD's acpivar.h helpers ------------------------------------------------------------------------------------- */

ACPI_HANDLE acpi_get_handle(device_t dev)
{
    struct nmx_acpi_handle *h;
    mtx_lock(&handles_mtx);
    TAILQ_FOREACH(h, &handles, link)
        if (h->dev == dev && dev != NULL)
            break;
    mtx_unlock(&handles_mtx);
    if (h != NULL)
        return h;
    void *pci = nmx_device_xnu(dev);
    void *node = pci ? nmx_xnu_acpi_node_for_pci(pci) : NULL;
    return node ? handle_for(node, "") : NULL;
}

device_t acpi_get_device(ACPI_HANDLE handle)
{
    if (handle == NULL || handle == ACPI_ROOT_OBJECT || rec(handle)->name[0] != 0)
        return NULL;
    struct nmx_acpi_handle *h = rec(handle);
    if (h->dev != NULL)
        return h->dev;
    device_t d = device_add_child(root_bus, "acpi", -1);
    if (d == NULL)
        return NULL;
    mtx_lock(&handles_mtx);
    if (h->dev == NULL) {
        h->dev = d;
        d = NULL;
    }
    mtx_unlock(&handles_mtx);
    if (d != NULL)
        device_delete_child(root_bus, d);
    return h->dev;
}

ACPI_STATUS acpi_GetInteger(ACPI_HANDLE handle, char *path, UINT32 *number)
{
    ACPI_STATUS status;
    ACPI_BUFFER buf;
    ACPI_OBJECT param;

    if (handle == NULL)
        handle = ACPI_ROOT_OBJECT;

    /* assume an Integer object, or a method returning one */
    buf.Pointer = &param;
    buf.Length = sizeof(param);
    status = AcpiEvaluateObject(handle, path, NULL, &buf);
    if (ACPI_SUCCESS(status)) {
        if (param.Type == ACPI_TYPE_INTEGER)
            *number = (UINT32)param.Integer.Value;
        else
            status = AE_TYPE;
    }

    /* some firmware returns a Buffer where an Integer is meant: fetch it whole and convert */
    if (status == AE_BUFFER_OVERFLOW) {
        if ((buf.Pointer = AcpiOsAllocate(buf.Length)) == NULL) {
            status = AE_NO_MEMORY;
        } else {
            status = AcpiEvaluateObject(handle, path, NULL, &buf);
            if (ACPI_SUCCESS(status))
                status = acpi_ConvertBufferToInteger(&buf, number);
            AcpiOsFree(buf.Pointer);
        }
    }
    return status;
}

ACPI_STATUS acpi_ConvertBufferToInteger(ACPI_BUFFER *bufp, UINT32 *number)
{
    ACPI_OBJECT *p = (ACPI_OBJECT *)bufp->Pointer;
    if (p->Type == ACPI_TYPE_INTEGER) {
        *number = (UINT32)p->Integer.Value;
        return AE_OK;
    }
    if (p->Type != ACPI_TYPE_BUFFER)
        return AE_TYPE;
    if (p->Buffer.Length > sizeof(int))
        return AE_BAD_DATA;
    *number = 0;
    for (UINT32 i = 0; i < p->Buffer.Length; i++)
        *number += (UINT32)p->Buffer.Pointer[i] << (i * 8);
    return AE_OK;
}

/* ACPI 6.2 section 9.1.1: function 0 of a _DSM returns the bitfield of the functions it implements */
UINT64 acpi_DSMQuery(ACPI_HANDLE h, const uint8_t *uuid, int revision)
{
    ACPI_BUFFER buf;
    UINT64 ret = 0;

    if (!ACPI_SUCCESS(acpi_EvaluateDSM(h, uuid, revision, 0, NULL, &buf))) {
        printf("NullMoth xkpi: _DSM function enumeration failed\n");
        return 0;
    }
    ACPI_OBJECT *obj = (ACPI_OBJECT *)buf.Pointer;
    if (obj == NULL)
        return 0;
    switch (obj->Type) {
    case ACPI_TYPE_BUFFER:
        for (UINT32 i = 0; i < MIN(obj->Buffer.Length, sizeof(ret)); i++)
            ret |= ((uint64_t)obj->Buffer.Pointer[i]) << (i * 8);
        break;
    case ACPI_TYPE_INTEGER:
        /* buggy firmware answers the query with an Integer; use it */
        ret = obj->Integer.Value;
        break;
    default:
        printf("NullMoth xkpi: _DSM query returned type %u\n", obj->Type);
        break;
    }
    AcpiOsFree(obj);
    return ret;
}

ACPI_STATUS acpi_EvaluateDSM(ACPI_HANDLE handle, const uint8_t *uuid, int revision, UINT64 function,
                             ACPI_OBJECT *package, ACPI_BUFFER *out_buf)
{
    return acpi_EvaluateDSMTyped(handle, uuid, revision, function, package, out_buf, ACPI_TYPE_ANY);
}

ACPI_STATUS acpi_EvaluateDSMTyped(ACPI_HANDLE handle, const uint8_t *uuid, int revision, UINT64 function,
                                  ACPI_OBJECT *package, ACPI_BUFFER *out_buf, ACPI_OBJECT_TYPE type)
{
    ACPI_OBJECT arg[4];
    ACPI_OBJECT_LIST arglist;
    ACPI_BUFFER buf;
    ACPI_STATUS status;

    if (out_buf == NULL)
        return AE_NO_MEMORY;

    arg[0].Type = ACPI_TYPE_BUFFER;
    arg[0].Buffer.Length = ACPI_UUID_LENGTH;
    arg[0].Buffer.Pointer = __DECONST(uint8_t *, uuid);
    arg[1].Type = ACPI_TYPE_INTEGER;
    arg[1].Integer.Value = (UINT64)revision;
    arg[2].Type = ACPI_TYPE_INTEGER;
    arg[2].Integer.Value = function;
    if (package) {
        arg[3] = *package;
    } else {
        arg[3].Type = ACPI_TYPE_PACKAGE;
        arg[3].Package.Count = 0;
        arg[3].Package.Elements = NULL;
    }
    arglist.Pointer = arg;
    arglist.Count = 4;
    buf.Pointer = NULL;
    buf.Length = ACPI_ALLOCATE_BUFFER;
    status = AcpiEvaluateObjectTyped(handle, "_DSM", &arglist, &buf, type);
    if (ACPI_FAILURE(status))
        return status;
    *out_buf = buf;
    return status;
}

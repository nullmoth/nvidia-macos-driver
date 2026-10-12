/*
 * Platform services for the port: GPU firmware files, ACPI tables and methods, SMBIOS (DMI) strings.
 * Compiled with Apple's headers only.
 *
 * Firmware: the Linux firmware blobs a GPU needs (redistributable files from linux-firmware) are installed under
 * /Library/NullMoth/firmware with their Linux names, and read here through the vnode KPI.
 * ACPI: tables come from the ACPI platform expert's "ACPI Tables" property; methods are evaluated on the GPU's
 * node in the ACPI plane through IOACPIPlatformDevice, so Apple's interpreter runs them.
 * DMI: the raw SMBIOS table published by AppleSMBIOS ("SMBIOS" property), parsed by structure type.
 */
#include <IOKit/IOLib.h>
#include <IOKit/IOService.h>
#include <IOKit/IORegistryEntry.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSArray.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSBoolean.h>
#include <sys/vnode.h>
#include <sys/mount.h>
#include <sys/fcntl.h>
#include <sys/uio.h>
#include <sys/errno.h>
#include <string.h>
#include "nmx_xnu.h"

/* ---- firmware ----------------------------------------------------------------------------------------------------- */

#define FIRMWARE_ROOT "/Library/NullMoth/firmware/"
#define FIRMWARE_MAX (64u << 20)    /* the largest GPU firmware file is a few MB; anything bigger is not one */

extern "C" int nmx_xnu_wait_firmware_root(int max_secs)
{
    for (int i = 0; i < max_secs * 2; i++) {
        /* no path lookup at all until a root vnode exists: before that, a lookup has nothing to start from */
        vnode_t root = vfs_rootvnode();
        if (root != NULLVP) {
            vnode_put(root);
            vfs_context_t ctx = vfs_context_create(nullptr);
            if (ctx != nullptr) {
                vnode_t vp = NULLVP;
                int err = vnode_lookup(FIRMWARE_ROOT, 0, &vp, ctx);
                if (err == 0)
                    vnode_put(vp);
                vfs_context_rele(ctx);
                if (err == 0)
                    return 0;
            }
        }
        IOSleep(500);
    }
    return ETIMEDOUT;
}

extern "C" int nmx_xnu_firmware_load(const char *name, void **data, size_t *size)
{
    *data = nullptr;
    *size = 0;
    if (name == nullptr || strnstr(name, "..", strlen(name) + 1) != nullptr || name[0] == '/')
        return EINVAL;
    char path[512];
    if (snprintf(path, sizeof(path), "%s%s", FIRMWARE_ROOT, name) >= (int)sizeof(path))
        return ENAMETOOLONG;

    vfs_context_t ctx = vfs_context_create(nullptr);
    if (ctx == nullptr)
        return ENOMEM;
    vnode_t vp = NULLVP;
    int err = vnode_open(path, FREAD, 0, 0, &vp, ctx);
    if (err) {
        vfs_context_rele(ctx);
        return err;
    }
    off_t fsize = 0;
    struct vnode_attr va;
    VATTR_INIT(&va);
    VATTR_WANTED(&va, va_data_size);
    err = vnode_getattr(vp, &va, ctx);
    if (err == 0)
        fsize = (off_t)va.va_data_size;
    if (err == 0 && (fsize <= 0 || fsize > FIRMWARE_MAX))
        err = EFBIG;
    void *buf = nullptr;
    if (err == 0) {
        buf = IOMalloc((size_t)fsize);
        if (buf == nullptr)
            err = ENOMEM;
    }
    if (err == 0) {
        int resid = 0;
        err = vn_rdwr(UIO_READ, vp, (caddr_t)buf, (int)fsize, 0, UIO_SYSSPACE, IO_NODELOCKED | IO_NOAUTH,
                      vfs_context_ucred(ctx), &resid, vfs_context_proc(ctx));
        if (err == 0 && resid != 0)
            err = EIO;
    }
    vnode_close(vp, FREAD, ctx);
    vfs_context_rele(ctx);
    if (err) {
        if (buf)
            IOFree(buf, (size_t)fsize);
        return err;
    }
    *data = buf;
    *size = (size_t)fsize;
    return 0;
}

extern "C" void nmx_xnu_firmware_free(void *data, size_t size)
{
    if (data)
        IOFree(data, size);
}

/* ---- ACPI --------------------------------------------------------------------------------------------------------- */

static OSDictionary *copy_acpi_tables(void)
{
    IORegistryEntry *pe = IORegistryEntry::fromPath("/", gIOServicePlane);
    OSDictionary *tables = nullptr;
    if (pe) {
        IORegistryIterator *it = IORegistryIterator::iterateOver(pe, gIOServicePlane, kIORegistryIterateRecursively);
        if (it) {
            while (IORegistryEntry *e = it->getNextObject()) {
                OSDictionary *t = OSDynamicCast(OSDictionary, e->getProperty("ACPI Tables"));
                if (t) {
                    t->retain();
                    tables = t;
                    break;
                }
            }
            it->release();
        }
        pe->release();
    }
    return tables;
}

extern "C" size_t nmx_xnu_acpi_table(const char *sig, int instance, void *buf, size_t max)
{
    OSDictionary *tables = copy_acpi_tables();
    if (tables == nullptr)
        return 0;
    /* the platform expert names the second and later tables of a signature "SSDT-1", "SSDT-2", ... */
    char key[16];
    if (instance <= 1)
        snprintf(key, sizeof(key), "%.4s", sig);
    else
        snprintf(key, sizeof(key), "%.4s-%d", sig, instance - 1);
    size_t n = 0;
    OSData *t = OSDynamicCast(OSData, tables->getObject(key));
    if (t) {
        n = t->getLength() < max ? t->getLength() : max;
        memcpy(buf, t->getBytesNoCopy(), n);
        if (n < t->getLength())
            n = t->getLength();     /* report the full length so the caller can retry with a larger buffer */
    }
    tables->release();
    return n;
}

/*
 * ACPI namespace nodes are the IOACPIPlatformDevice entries of the ACPI plane; its root ("acpi", the platform
 * expert) is the namespace root. Only devices are registry entries: methods and data objects are reached by name
 * through the device that contains them (validateObject / evaluateObject). Names in the plane drop ACPI's trailing
 * underscore padding ("_SB_" is "_SB"), so comparisons ignore it.
 */
static const IORegistryPlane *acpi_plane(void)
{
    static const IORegistryPlane *plane;
    if (plane == nullptr)
        plane = IORegistryEntry::getPlane("IOACPIPlane");
    return plane;
}

static bool same_seg(const char *a, const char *b)
{
    char x[5] = {}, y[5] = {};
    strlcpy(x, a, sizeof(x));
    strlcpy(y, b, sizeof(y));
    for (int i = 3; i >= 0 && (x[i] == '_' || x[i] == 0); i--)
        x[i] = 0;
    for (int i = 3; i >= 0 && (y[i] == '_' || y[i] == 0); i--)
        y[i] = 0;
    return strncmp(x, y, 4) == 0;
}

extern "C" void *nmx_xnu_acpi_node_root(void)
{
    if (acpi_plane() == nullptr)
        return nullptr;
    IORegistryEntry *root = IORegistryEntry::fromPath("/", acpi_plane());
    if (root == nullptr)
        return nullptr;
    IORegistryEntry *acpi = root->getChildEntry(acpi_plane());     /* "acpi": the namespace root */
    if (acpi)
        acpi->retain();
    root->release();
    return acpi;
}

extern "C" void *nmx_xnu_acpi_node_for_pci(void *pci)
{
    if (pci == nullptr || acpi_plane() == nullptr)
        return nullptr;
    /* a PCI device is attached in the ACPI plane to the ACPI node describing it */
    IORegistryEntry *e = ((IORegistryEntry *)pci)->getParentEntry(acpi_plane());
    IOACPIPlatformDevice *n = OSDynamicCast(IOACPIPlatformDevice, e);
    if (n)
        n->retain();
    return n;
}

extern "C" void nmx_xnu_acpi_node_get(void *node)
{
    if (node)
        ((OSObject *)node)->retain();
}

extern "C" void nmx_xnu_acpi_node_put(void *node)
{
    if (node)
        ((OSObject *)node)->release();
}

extern "C" void *nmx_xnu_acpi_node_parent(void *node)
{
    if (node == nullptr || acpi_plane() == nullptr)
        return nullptr;
    IORegistryEntry *p = ((IORegistryEntry *)node)->getParentEntry(acpi_plane());
    if (p == nullptr || OSDynamicCast(IOACPIPlatformDevice, p) == nullptr) {
        /* the parent of a top-level node is the namespace root, which is not an IOACPIPlatformDevice */
        return p && p->getParentEntry(acpi_plane()) != nullptr ? (p->retain(), (void *)p) : nullptr;
    }
    p->retain();
    return p;
}

/* the next child device of 'parent' after 'prev' (NULL = first); references are the caller's */
extern "C" void *nmx_xnu_acpi_node_next_child(void *parent, void *prev)
{
    if (parent == nullptr || acpi_plane() == nullptr)
        return nullptr;
    OSIterator *it = ((IORegistryEntry *)parent)->getChildIterator(acpi_plane());
    if (it == nullptr)
        return nullptr;
    void *found = nullptr;
    bool past = prev == nullptr;
    while (OSObject *o = it->getNextObject()) {
        IOACPIPlatformDevice *d = OSDynamicCast(IOACPIPlatformDevice, o);
        if (d == nullptr)
            continue;
        if (!past) {
            past = (void *)d == prev;
            continue;
        }
        d->retain();
        found = d;
        break;
    }
    it->release();
    return found;
}

extern "C" void *nmx_xnu_acpi_node_child(void *parent, const char *seg)
{
    void *c = nullptr;
    while ((c = nmx_xnu_acpi_node_next_child(parent, c)) != nullptr) {
        const char *name = ((IORegistryEntry *)c)->getName(acpi_plane());
        if (name && same_seg(name, seg))
            return c;
        /* keep the reference only for the match; the iterator above re-finds 'c' by pointer */
        ((OSObject *)c)->release();
    }
    return nullptr;
}

/* the node's 4-character ACPI name, padded with '_' as ACPI writes it */
extern "C" int nmx_xnu_acpi_node_name(void *node, char out[5])
{
    memset(out, '_', 4);
    out[4] = 0;
    if (node == nullptr || acpi_plane() == nullptr)
        return 0;
    const char *name = ((IORegistryEntry *)node)->getName(acpi_plane());
    if (name == nullptr)
        return 0;
    for (int i = 0; i < 4 && name[i] && name[i] != '@'; i++)
        out[i] = name[i];
    return 1;
}

extern "C" int nmx_xnu_acpi_has(void *node, const char *name)
{
    IOACPIPlatformDevice *d = OSDynamicCast(IOACPIPlatformDevice, (OSObject *)node);
    return d != nullptr && d->validateObject(name) == kIOReturnSuccess;
}

/* ---- evaluating objects: ACPI values <-> OSNumber / OSString / OSData / OSArray ---------------------------------- */

static OSObject *to_os(const nmx_acpi_obj *o, int depth)
{
    if (depth > 8)
        return nullptr;
    switch (o->type) {
    case NMX_ACPI_INTEGER:
        return OSNumber::withNumber(o->integer, 64);
    case NMX_ACPI_STRING:
        return OSString::withCString((const char *)o->ptr);
    case NMX_ACPI_BUFFER:
        return OSData::withBytes(o->ptr, (unsigned)o->len);
    case NMX_ACPI_PACKAGE: {
        OSArray *a = OSArray::withCapacity(o->count ? o->count : 1);
        if (a == nullptr)
            return nullptr;
        const nmx_acpi_obj *el = (const nmx_acpi_obj *)o->ptr;
        for (uint32_t i = 0; i < o->count; i++) {
            OSObject *c = to_os(&el[i], depth + 1);
            if (c == nullptr) {
                a->release();
                return nullptr;
            }
            a->setObject(c);
            c->release();
        }
        return a;
    }
    default:
        return nullptr;
    }
}

/* bytes the result tree needs: the nodes, then string and buffer data, each 8-byte aligned */
static size_t tree_size(OSObject *o, int depth)
{
    size_t n = sizeof(nmx_acpi_obj);
    if (depth > 8)
        return n;
    if (OSString *s = OSDynamicCast(OSString, o))
        n += (s->getLength() + 1 + 7) & ~(size_t)7;
    else if (OSData *d = OSDynamicCast(OSData, o))
        n += (d->getLength() + 7) & ~(size_t)7;
    else if (OSArray *a = OSDynamicCast(OSArray, o))
        for (unsigned i = 0; i < a->getCount(); i++)
            n += tree_size(a->getObject(i), depth + 1);
    return n;
}

/* fills 'dst' from 'o', taking extra space from *cursor */
static void tree_fill(OSObject *o, nmx_acpi_obj *dst, char **cursor, int depth)
{
    memset(dst, 0, sizeof(*dst));
    if (OSNumber *num = OSDynamicCast(OSNumber, o)) {
        dst->type = NMX_ACPI_INTEGER;
        dst->integer = num->unsigned64BitValue();
    } else if (OSBoolean *b = OSDynamicCast(OSBoolean, o)) {
        dst->type = NMX_ACPI_INTEGER;
        dst->integer = b->isTrue() ? 1 : 0;
    } else if (OSString *s = OSDynamicCast(OSString, o)) {
        dst->type = NMX_ACPI_STRING;
        dst->len = s->getLength();
        memcpy(*cursor, s->getCStringNoCopy(), dst->len + 1);
        dst->ptr = *cursor;
        *cursor += (dst->len + 1 + 7) & ~(size_t)7;
    } else if (OSData *d = OSDynamicCast(OSData, o)) {
        dst->type = NMX_ACPI_BUFFER;
        dst->len = d->getLength();
        memcpy(*cursor, d->getBytesNoCopy(), dst->len);
        dst->ptr = *cursor;
        *cursor += (dst->len + 7) & ~(size_t)7;
    } else if (OSArray *a = OSDynamicCast(OSArray, o)) {
        dst->type = NMX_ACPI_PACKAGE;
        dst->count = depth > 8 ? 0 : a->getCount();
        nmx_acpi_obj *el = (nmx_acpi_obj *)*cursor;
        *cursor += sizeof(nmx_acpi_obj) * dst->count;
        dst->ptr = el;
        for (uint32_t i = 0; i < dst->count; i++)
            tree_fill(a->getObject(i), &el[i], cursor, depth + 1);
    } else {
        dst->type = NMX_ACPI_ANY;   /* a reference or other object the caller cannot use */
    }
}

extern "C" int nmx_xnu_acpi_evaluate(void *node, const char *name, const nmx_acpi_obj *args, int nargs,
                                     nmx_acpi_obj **result, size_t *result_size)
{
    if (result)
        *result = nullptr;
    if (result_size)
        *result_size = 0;
    IOACPIPlatformDevice *d = OSDynamicCast(IOACPIPlatformDevice, (OSObject *)node);
    if (d == nullptr || name == nullptr || nargs < 0 || nargs > 7)
        return ENOENT;
    OSObject *params[7] = {};
    for (int i = 0; i < nargs; i++) {
        params[i] = to_os(&args[i], 0);
        if (params[i] == nullptr) {
            for (int j = 0; j < i; j++)
                params[j]->release();
            return EINVAL;
        }
    }
    OSObject *out = nullptr;
    IOReturn r = d->evaluateObject(name, &out, nargs ? params : nullptr, (IOItemCount)nargs);
    for (int i = 0; i < nargs; i++)
        params[i]->release();
    if (r != kIOReturnSuccess) {
        if (out)
            out->release();
        return r == kIOReturnNotFound || r == kIOReturnBadArgument ? ENOENT : EIO;
    }
    if (out == nullptr || result == nullptr) {
        if (out)
            out->release();
        return 0;
    }
    size_t size = tree_size(out, 0);
    char *mem = (char *)IOMallocZero(size);
    if (mem == nullptr) {
        out->release();
        return ENOMEM;
    }
    char *cursor = mem + sizeof(nmx_acpi_obj);
    tree_fill(out, (nmx_acpi_obj *)mem, &cursor, 0);
    out->release();
    *result = (nmx_acpi_obj *)mem;
    *result_size = size;
    return 0;
}

extern "C" void nmx_xnu_acpi_free(void *p, size_t size)
{
    if (p)
        IOFree(p, size);
}

/* ---- SMBIOS / DMI ------------------------------------------------------------------------------------------------- */

/* the n-th string of an SMBIOS structure (1-based), or NULL */
static const char *smbios_string(const uint8_t *s, const uint8_t *end, int n)
{
    if (n <= 0)
        return nullptr;
    const char *p = (const char *)s + s[1];
    while ((const uint8_t *)p < end && *p) {
        if (--n == 0)
            return p;
        p += strnlen(p, (size_t)(end - (const uint8_t *)p)) + 1;
    }
    return nullptr;
}

/* Linux DMI field -> (SMBIOS structure type, string-index byte offset) */
/* numeric: the field is a byte value (chassis type), given to LinuxKPI as its decimal text */
struct dmi_map { int field; uint8_t type; uint8_t offset; uint8_t numeric; };
static const dmi_map dmi_fields[] = {
    { 1, 0, 0x04 }, { 2, 0, 0x05 }, { 3, 0, 0x08 },                                   /* BIOS vendor, version, date */
    { 6, 1, 0x04 }, { 7, 1, 0x05 }, { 8, 1, 0x06 }, { 9, 1, 0x07 },                   /* system maker..serial */
    { 11, 1, 0x19 }, { 12, 1, 0x1a },                                                 /* SKU, family */
    { 13, 2, 0x04 }, { 14, 2, 0x05 }, { 15, 2, 0x06 }, { 16, 2, 0x07 }, { 17, 2, 0x08 },/* board */
    { 18, 3, 0x04 }, { 19, 3, 0x05, 1 }, { 20, 3, 0x06 }, { 21, 3, 0x07 }, { 22, 3, 0x08 }, /* chassis */
};

extern "C" size_t nmx_xnu_dmi_field(int field, char *buf, size_t max)
{
    if (max == 0)
        return 0;
    buf[0] = 0;
    OSDictionary *match = IOService::serviceMatching("AppleSMBIOS");
    if (match == nullptr)
        return 0;
    IOService *smb = IOService::copyMatchingService(match);
    match->release();
    if (smb == nullptr)
        return 0;
    OSData *table = OSDynamicCast(OSData, smb->getProperty("SMBIOS"));
    size_t out = 0;
    if (table) {
        const uint8_t *p = (const uint8_t *)table->getBytesNoCopy();
        const uint8_t *end = p + table->getLength();
        const dmi_map *m = nullptr;
        for (const dmi_map &d : dmi_fields)
            if (d.field == field) { m = &d; break; }
        while (m && p + 4 <= end && p[1] >= 4) {
            const uint8_t *next = p + p[1];
            while (next + 1 < end && !(next[0] == 0 && next[1] == 0))
                next++;
            next += 2;
            if (p[0] == m->type && m->offset < p[1] && m->numeric) {
                snprintf(buf, max, "%u", p[m->offset] & 0x7f);  /* bit 7 is the chassis lock flag */
                out = strlen(buf);
                break;
            }
            if (p[0] == m->type && m->offset < p[1]) {
                const char *s = smbios_string(p, end, p[m->offset]);
                if (s) {
                    strlcpy(buf, s, max);
                    out = strlen(buf);
                }
                break;
            }
            if (p[0] == 127)
                break;
            p = next;
        }
    }
    smb->release();
    return out;
}

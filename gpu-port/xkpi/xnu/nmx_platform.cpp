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
#include <sys/vnode.h>
#include <sys/fcntl.h>
#include <sys/uio.h>
#include <sys/errno.h>
#include <string.h>
#include "nmx_xnu.h"

/* ---- firmware ----------------------------------------------------------------------------------------------------- */

#define FIRMWARE_ROOT "/Library/NullMoth/firmware/"
#define FIRMWARE_MAX (64u << 20)    /* the largest GPU firmware file is a few MB; anything bigger is not one */

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

static IOACPIPlatformDevice *acpi_node_for(void *pci)
{
    if (pci == nullptr)
        return nullptr;
    IORegistryEntry *e = (IORegistryEntry *)pci;
    const IORegistryPlane *acpiPlane = IORegistryEntry::getPlane("IOACPIPlane");
    if (acpiPlane == nullptr)
        return nullptr;
    /* a PCI device is attached in the ACPI plane to the ACPI node describing it */
    return OSDynamicCast(IOACPIPlatformDevice, e->getParentEntry(acpiPlane));
}

extern "C" int nmx_xnu_acpi_eval(void *pci, const char *method, const uint64_t *int_args, int nargs, const void *buf_arg,
                                 size_t buf_len, uint64_t *int_result, void *buf_result, size_t *buf_result_len)
{
    IOACPIPlatformDevice *node = acpi_node_for(pci);
    if (node == nullptr)
        return ENOENT;
    OSObject *params[8] = {};
    int np = 0;
    for (int i = 0; i < nargs && np < 7; i++)
        params[np++] = OSNumber::withNumber(int_args[i], 64);
    if (buf_arg && np < 8)
        params[np++] = OSData::withBytes(buf_arg, (unsigned)buf_len);
    OSObject *result = nullptr;
    IOReturn r = node->evaluateObject(method, &result, params, (IOItemCount)np);
    for (int i = 0; i < np; i++)
        if (params[i])
            params[i]->release();
    if (r != kIOReturnSuccess)
        return r == kIOReturnNotFound ? ENOENT : EIO;
    if (result) {
        if (OSNumber *num = OSDynamicCast(OSNumber, result)) {
            if (int_result)
                *int_result = num->unsigned64BitValue();
        } else if (OSData *data = OSDynamicCast(OSData, result)) {
            if (buf_result && buf_result_len) {
                size_t n = data->getLength() < *buf_result_len ? data->getLength() : *buf_result_len;
                memcpy(buf_result, data->getBytesNoCopy(), n);
                *buf_result_len = data->getLength();
            }
        }
        result->release();
    }
    return 0;
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
struct dmi_map { int field; uint8_t type; uint8_t offset; };
static const dmi_map dmi_fields[] = {
    { 1, 0, 0x04 }, { 2, 0, 0x05 }, { 3, 0, 0x08 },                                   /* BIOS vendor, version, date */
    { 6, 1, 0x04 }, { 7, 1, 0x05 }, { 8, 1, 0x06 }, { 9, 1, 0x07 },                   /* system maker..serial */
    { 11, 1, 0x19 }, { 12, 1, 0x1a },                                                 /* SKU, family */
    { 13, 2, 0x04 }, { 14, 2, 0x05 }, { 15, 2, 0x06 }, { 16, 2, 0x07 }, { 17, 2, 0x08 },/* board */
    { 18, 3, 0x04 }, { 20, 3, 0x06 }, { 21, 3, 0x07 }, { 22, 3, 0x08 },               /* chassis */
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

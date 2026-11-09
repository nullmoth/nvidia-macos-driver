#!/usr/bin/env python3
"""Which census families NMKbdBacklight has a backend for, from the ACPI census (laptop-families-acpi.tsv: one row per family,
real iasl disassembly of the DSDT and every SSDT). usage: census_kbl_coverage.py <laptop-families-acpi.tsv> [--list]

A family is "covered" by a backend when the census shows the same thing that backend's probe needs:
  asus-wmi       ASUS management GUID and WMNB device id 0x00050021 (the keyboard backlight id; 0x00050011 is the LCD backlight)
  ideapad-kblc   Lenovo_VPC2004 and KBLC;  ideapad-plain  Lenovo_VPC2004 and HALS and SALS
  dell-smbios    Dell SMBIOS call GUID (the probe also needs the descriptor GUID, 8D9DDCBC..., which the census lists unlabelled)
  msi-ec         MSI platform GUID and an EC field named KBBL (the probe additionally needs the firmware string in its table:
                 the census cannot see it, so this is an upper bound)
  hid            not decided by ACPI at all; not counted here
  thinkpad       the census does not list MLCG/MLCS (names without 'kbd'/'backlight'), so it cannot count them: reported as unknown
Families with a keyboard-backlight method of another name are listed as UNHANDLED with the method; families whose census data
suggests a backend but whose probe would refuse are listed as REFUSED with the reason."""
import csv, re, sys, collections

K = 'keyboard_backlight_methods (device.method(args)->calls)'
V = 'vendor_interfaces_found_in_disassembly'
G = 'wmi_guids (guid name [source] machines with it, method wired)'
E = 'ec_brightness_or_kbd_fields (region.field@byte.bit/width)'
DELL_DESC = '8D9DDCBC-A997-11DA-B012-B622A1EF5492'

def classify(r):
    v, g, k, e = r[V], r[G], r[K], r[E]
    out = {'covered': [], 'refused': [], 'unhandled': []}
    asus_guid = 'ASUS management' in g
    ids = re.findall(r'ASUS_WMNB_devids:([0-9a-fx,]+)', v)
    idset = {x for s in ids for x in s.split(',')}
    if asus_guid or ids:
        if '0x00050021' in idset and asus_guid: out['covered'].append('asus-wmi')
        elif '0x00050021' not in idset: out['refused'].append('asus-wmi: no keyboard backlight id 0x00050021 (only %s; 0x00050011 is ASUS_WMI_DEVID_BACKLIGHT, the display, in asus-wmi.h)' % (','.join(sorted(idset)) or 'none'))
        else: out['refused'].append('asus-wmi: WMNB ids but no ASUS management GUID')
    if 'Lenovo_VPC2004' in v:
        if 'Lenovo_KBLC' in v: out['covered'].append('ideapad-kblc')
        elif 'Lenovo_HALS' in v and 'Lenovo_SALS' in v: out['covered'].append('ideapad-plain')
    if 'Dell SMBIOS call' in g:
        (out['covered'] if DELL_DESC in g.upper() else out['refused']).append('dell-smbios' if DELL_DESC in g.upper() else 'dell-smbios: descriptor GUID %s not in the family' % DELL_DESC)
    # ThinkPad: the census has no MLCG/MLCS column, but every ThinkPad row whose keyboard method is KBLS(2) on MFFD/VOL3/HIDD has
    # it: MLCG and MLCS are wrappers over that KBLS (tools/fixtures/kbl_aml/thinkpad_kbls.dsl, read from the real DSDT of each of the
    # 11 families, all with MLCG, MLCS and LEN0068). So the ThinkPad backend drives KBLS through the method it already calls.
    if r['family'].startswith('ThinkPad') and re.search(r'(MFFD|VOL3|HIDD)\.KBLS\(2\)', k):
        out['covered'].append('thinkpad (MLCG/MLCS over KBLS, AML-verified)')
    if 'MSI platform WMI' in g:
        if re.search(r'EC\.KBBL@', e): out['covered'].append('msi-ec (upper bound: firmware string unchecked)')
        else: out['refused'].append('msi-ec: no EC KBBL field in the census')
    return out

def method_names(r):
    return sorted({m.split('->')[0].split('=')[0] for m in r[K].split(';') if m})

def main(path, show):
    rows = list(csv.DictReader(open(path), delimiter='\t'))
    cover = collections.defaultdict(list); refused = collections.defaultdict(list); unhandled = collections.defaultdict(list)
    for r in rows:
        c = classify(r)
        for b in c['covered']: cover[b.split(' ')[0]].append(r['family'])
        for b in c['refused']: refused[b].append(r['family'])
        for m in method_names(r):
            name = m.split('(')[0].split('.')[-1]
            handled = (name == 'KBLC' and 'ideapad-kblc' in ' '.join(c['covered']))
            handled = handled or (name == 'SKBL' and 'ideapad-plain' in ' '.join(c['covered']))   # Yoga 14sARE: SMI helper under HALS/SALS
            handled = handled or (name == 'KBLS' and any(x.startswith('thinkpad') for x in c['covered']))
            known_other = {'EC0.KBLS(0)': 'ASUS: read-only getter of the keyboard RGB state (WMI status for id 0x0010005B), no setter',
                           '.SKBL(1)': 'HONOR: stub that answers STAT=1 and does nothing', 'VPC0.SKBL(1)': 'Lenovo Yoga: SMI helper under HALS/SALS, which the plain backend drives'}
            if not handled and m in known_other: m = '%s  [read from the real AML: %s]' % (m, known_other[m])
            if not handled: unhandled[m].append('%s / %s' % (r['vendor'], r['family']))
    print('families in census:', len(rows))
    for b, f in sorted(cover.items()): print('COVERED   %-14s %3d families' % (b, len(f)))
    for b, f in sorted(refused.items()): print('REFUSED   %3d  %s' % (len(f), b))
    for m, f in sorted(unhandled.items(), key=lambda x: -len(x[1])): print('UNHANDLED %3d  %s' % (len(f), m))
    if show:
        for b, f in sorted(cover.items()): print('\n%s:' % b, '; '.join(f))
        for b, f in sorted(refused.items()): print('\nREFUSED %s:' % b, '; '.join(f))
        for m, f in sorted(unhandled.items()): print('\nUNHANDLED %s:' % m, '; '.join(f))

if __name__ == '__main__':
    main(sys.argv[1], '--list' in sys.argv)

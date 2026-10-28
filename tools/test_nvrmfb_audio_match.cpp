#include "../kexts/NVRMFB/nvrm_audio_match.h"
#include "../kexts/NVRMFB/nvrm_eld.h"
#include <cassert>
#include <cstdio>
#include <cstring>


int main()
{
    uint8_t port = 255;
    assert(nvrm_audio_port_number(5, 1, &port) && port == 0);
    assert(nvrm_audio_port_number(4, 2, &port) && port == 0);
    assert(nvrm_audio_port_number(5, 2, &port) && port == 1);
    assert(nvrm_audio_port_number(6, 2, &port) && port == 2);
    assert(nvrm_audio_port_number(7, 2, &port) && port == 3);
    assert(!nvrm_audio_port_number(3, 2, &port));

    uint8_t field_edid[256] = {};
    field_edid[8]=0x12; field_edid[9]=0x34; field_edid[10]=0x56; field_edid[11]=0x78;
    field_edid[57]=0xfc;
    memcpy(field_edid+59,"Test Monitor\n",13);
    field_edid[126]=1;
    field_edid[128]=2; field_edid[129]=3; field_edid[130]=8;
    field_edid[132]=(1<<5)|3; field_edid[133]=9; field_edid[134]=7; field_edid[135]=7;
    uint8_t field_eld[NVRM_ELD_MAX] = {};
    nvrm_build_eld(field_edid,sizeof field_edid,0,field_eld);
    field_eld[6]=32; field_eld[8]=123; // unrelated synchronization and port-id fields

    nvrm_audio_identity monitor = {}, hardware = {};
    uint8_t built[NVRM_ELD_MAX];
    size_t size = nvrm_build_eld(field_edid, sizeof field_edid, 0, built);
    assert(nvrm_audio_identity_parse(built, size, &monitor));
    assert(nvrm_audio_identity_parse(field_eld, sizeof field_eld, &hardware));
    assert(nvrm_audio_identity_equal(&monitor, &hardware));
    // Both pin 04 and pin 05 are valid: their identity, not the NID, selects the sink.
    nvrm_audio_identity pins[2] = {{}, hardware};
    assert(nvrm_audio_unique_match(&monitor, 1, 0, pins, 2) == 1);
    pins[0] = hardware; pins[1] = {};
    assert(nvrm_audio_unique_match(&monitor, 1, 0, pins, 2) == 0);
    auto other = monitor; other.product[2] ^= 1;
    nvrm_audio_identity displays[2] = {monitor, other};
    pins[0] = other; pins[1] = hardware;
    assert(nvrm_audio_unique_match(displays, 2, 0, pins, 2) == 1);
    assert(nvrm_audio_unique_match(displays, 2, 1, pins, 2) == 0);
    auto temp = pins[0]; pins[0] = pins[1]; pins[1] = temp;
    assert(nvrm_audio_unique_match(displays, 2, 0, pins, 2) == 0);
    displays[1] = monitor;
    assert(nvrm_audio_unique_match(displays, 2, 0, pins, 2) == -1);
    pins[1] = hardware;
    assert(nvrm_audio_unique_match(&monitor, 1, 0, pins, 2) == -1);
    for (size_t length = 0; length < 20; ++length)
        assert(!nvrm_audio_identity_parse(field_eld, length, &hardware));
    uint8_t bad[256] = {};
    memcpy(bad, field_eld, sizeof field_eld); bad[2] = 255;
    assert(!nvrm_audio_identity_parse(bad, sizeof bad, &hardware));
    memcpy(bad, field_eld, sizeof field_eld); bad[5] &= 15;
    assert(!nvrm_audio_identity_parse(bad, sizeof bad, &hardware));
    uint8_t codec[4] = {0, 1, 5, 0};
    assert(nvrm_audio_codec_valid(codec, 4));
    assert(!nvrm_audio_codec_valid(codec, 3));
    codec[2] = 0; assert(!nvrm_audio_codec_valid(codec, 4));
    codec[2] = 5; codec[0] = 16; assert(!nvrm_audio_codec_valid(codec, 4));
    codec[0] = 0; codec[3] = 1; assert(!nvrm_audio_codec_valid(codec, 4));
    assert(nvrm_audio_fallback_allowed(1, false, false));
    assert(!nvrm_audio_fallback_allowed(2, false, false));
    assert(!nvrm_audio_fallback_allowed(1, true, false));
    assert(!nvrm_audio_fallback_allowed(1, false, true));
    assert(nvrm_audio_snapshot_current(1, 4, 4, 0, 0, 2));
    assert(!nvrm_audio_snapshot_current(1, 3, 4, 0, 0, 2));
    assert(!nvrm_audio_snapshot_current(1, 4, 4, 1, 0, 2));
    assert(!nvrm_audio_snapshot_current(1, 4, 4, 0, 1, 2));
    puts("PASS: synthetic ELD/EDID, pin order, dual displays, ambiguity, malformed ELD/config, MST/fallback, stale/unavailable snapshots");
}

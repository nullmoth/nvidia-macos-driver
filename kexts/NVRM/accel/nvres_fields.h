// Where the fields this driver reads sit inside the family's resource object, per release.
// Each value is read from the family's own code for that release: getResourceInfo (width, height, pitches, size, base
// level), update_iosurface_valid_bits (valid-bits pointer, surface cache), objTypeName (type), deleteBackingMemory (face
// size, backing memory), get_surface_buffer_reqbit (flags), purgeable (video memory).
//   10.14 and later : one layout, 64-bit pitches and sizes.
//   10.12, 10.13    : pitches and sizes are 32-bit, so everything after them sits 8 bytes lower.
//   10.11           : the head of the object is laid out differently as well.
// The surface cache object the resource points to is the same on 10.11 to 15: surface +0x10 (its init), control context
// +0x20 (its setControlFunc).
// Width and height on 10.11 are taken from the 10.12 layout (the fields either side of them match); 10.11 has no
// getResourceInfo to confirm them.
#pragma once
#if defined(NM_ERA) && NM_ERA < 1012
enum { kNvResFlags = 0x18, kNvResType = 0x20, kNvResValid = 0x40, kNvResVidMem = 0x98, kNvResCache = 0xe0 };
#define NV_RES_NARROW 1
#elif defined(NM_ERA) && NM_ERA < 1014
enum { kNvResFlags = 0xc, kNvResType = 0x14, kNvResValid = 0x30, kNvResVidMem = 0x88, kNvResCache = 0xd8 };
#define NV_RES_NARROW 1
#else
enum { kNvResFlags = 0xc, kNvResType = 0x14, kNvResValid = 0x30, kNvResVidMem = 0x88, kNvResCache = 0xe0 };
#define NV_RES_NARROW 0
#endif
enum { kNvResBaseLevel = 0xae, kNvResWidth = 0xb0, kNvResHeight = 0xb2 };
static inline unsigned long long nvResPitch(const volatile unsigned char *r)
{ return NV_RES_NARROW ? *(const volatile unsigned int *)(r + 0xb8) : *(const volatile unsigned long long *)(r + 0xb8); }
static inline unsigned long long nvResBackingPitch(const volatile unsigned char *r)
{ return NV_RES_NARROW ? *(const volatile unsigned int *)(r + 0xbc) : *(const volatile unsigned long long *)(r + 0xc0); }
static inline unsigned long long nvResFaceSize(const volatile unsigned char *r)
{ return NV_RES_NARROW ? *(const volatile unsigned int *)(r + 0xc8) : *(const volatile unsigned long long *)(r + 0xd0); }

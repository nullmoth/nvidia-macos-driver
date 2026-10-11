/* FreeBSD <sys/endian.h> on xnu: byte-order helpers LinuxKPI builds its cpu_to_le* family from (x86_64 is little-endian). */
#pragma once
#include <libkern/OSByteOrder.h>
#define _LITTLE_ENDIAN 1234
#define _BIG_ENDIAN 4321
#define _BYTE_ORDER _LITTLE_ENDIAN
#define bswap16(x) OSSwapInt16(x)
#define bswap32(x) OSSwapInt32(x)
#define bswap64(x) OSSwapInt64(x)
#define htobe16(x) bswap16(x)
#define htobe32(x) bswap32(x)
#define htobe64(x) bswap64(x)
#define htole16(x) ((uint16_t)(x))
#define htole32(x) ((uint32_t)(x))
#define htole64(x) ((uint64_t)(x))
#define be16toh(x) bswap16(x)
#define be32toh(x) bswap32(x)
#define be64toh(x) bswap64(x)
#define le16toh(x) ((uint16_t)(x))
#define le32toh(x) ((uint32_t)(x))
#define le64toh(x) ((uint64_t)(x))
static inline uint16_t be16dec(const void *p) { const uint8_t *b = p; return (uint16_t)((b[0] << 8) | b[1]); }
static inline uint32_t be32dec(const void *p) { const uint8_t *b = p; return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3]; }
static inline uint64_t be64dec(const void *p) { const uint8_t *b = p; return ((uint64_t)be32dec(b) << 32) | be32dec(b + 4); }
static inline uint16_t le16dec(const void *p) { const uint8_t *b = p; return (uint16_t)(b[0] | (b[1] << 8)); }
static inline uint32_t le32dec(const void *p) { const uint8_t *b = p; return b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24); }
static inline uint64_t le64dec(const void *p) { const uint8_t *b = p; return le32dec(b) | ((uint64_t)le32dec(b + 4) << 32); }
static inline void be16enc(void *p, uint16_t v) { uint8_t *b = p; b[0] = v >> 8; b[1] = v; }
static inline void be32enc(void *p, uint32_t v) { uint8_t *b = p; b[0] = v >> 24; b[1] = v >> 16; b[2] = v >> 8; b[3] = v; }
static inline void be64enc(void *p, uint64_t v) { be32enc(p, (uint32_t)(v >> 32)); be32enc((uint8_t *)p + 4, (uint32_t)v); }
static inline void le16enc(void *p, uint16_t v) { uint8_t *b = p; b[0] = v; b[1] = v >> 8; }
static inline void le32enc(void *p, uint32_t v) { uint8_t *b = p; b[0] = v; b[1] = v >> 8; b[2] = v >> 16; b[3] = v >> 24; }
static inline void le64enc(void *p, uint64_t v) { le32enc(p, (uint32_t)v); le32enc((uint8_t *)p + 4, (uint32_t)(v >> 32)); }

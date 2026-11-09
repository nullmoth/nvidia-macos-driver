/* The one pixel rule of the packed 4:2:2 formats, written in the subset shared by C and GLSL so that the host test and
   the conversion shader (tools/y422_convert.comp) run the same text. `uint` is a 32-bit unsigned in both.
   own / nb: the 32-bit pair words (bytes G0 B G1 R for GBGR422, B G0 R G1 for BGRG422, little endian) of the pixel's
   own pair and of the neighbouring pair on its side. Returns the RGBA8 word (R in the low byte, alpha 255). */
uint y422_byte(uint w, uint i) { return (w >> (8u * i)) & 255u; }
uint y422_mix(uint own, uint other) { return (3u * own + other + 2u) >> 2u; }
uint y422_pixel(uint fmt, uint own, uint nb, uint odd)
{
    uint g0 = fmt == 240u ? 0u : 1u, b = fmt == 240u ? 1u : 0u, g1 = fmt == 240u ? 2u : 3u, r = fmt == 240u ? 3u : 2u;
    uint rr = y422_mix(y422_byte(own, r), y422_byte(nb, r));
    uint bb = y422_mix(y422_byte(own, b), y422_byte(nb, b));
    uint gg = y422_byte(own, odd != 0u ? g1 : g0);
    return rr | (gg << 8u) | (bb << 16u) | (255u << 24u);
}

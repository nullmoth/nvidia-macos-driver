/* Decodes recorded 32-lane query output with ttu_abi.h and prints one line per lane: lane hit t u v primitive geometry instance.
 * usage: test_ttu_abi <results.bin> */
#include "ttu_abi.h"
#include <stdio.h>
int main(int argc, char **argv)
{
   if (argc != 2) return 2;
   FILE *f = fopen(argv[1], "rb");
   if (!f) return 3;
   uint32_t rec[32][24];
   if (fread(rec, sizeof rec, 1, f) != 1) return 4;
   fclose(f);
   for (int l = 0; l < 32; l++) {
      struct ttu_hit h = ttu_decode(&rec[l][8]);
      printf("%d %d %.9g %.9g %.9g %u %u %d %d\n", l, h.hit, h.t, h.u, h.v, h.primitive, h.geometry, h.instance, h.front_face);
   }
   struct ttu_request r;
   const float o[3] = { 0.25f, 0.25f, -1.f }, d[3] = { 0, 0, 1 };
   ttu_pack(&r, o, d, 0.f, 10.f, 0xff, 0x123456780ull, 1);
   printf("pack %08x %08x %08x %08x %08x %08x\n", r.w[2], r.w[7], r.w[11], r.w[14], r.w[16], r.w[17]);
   return 0;
}

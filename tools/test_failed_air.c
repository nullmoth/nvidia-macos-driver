/* Proves plugin/nvmtl_failed_air.h. cc -I plugin -o /tmp/tfa tools/test_failed_air.c && /tmp/tfa */
#include "nvmtl_failed_air.h"
#include <stdlib.h>
static int fails;
#define ARM(name, cond) do { int ok_ = (cond); printf("%s  %s\n", ok_ ? "ok  " : "FAIL", name); if (!ok_) fails++; } while (0)
int main(void)
{
    ARM("an empty folder admits a normal function", nvmtl_failed_air_admit(0, 0, 4000) == NVMTL_FAILED_AIR_ADMIT);
    ARM("a function above the per-file limit is not kept", nvmtl_failed_air_admit(0, 0, NVMTL_FAILED_AIR_MAX_FILE + 1) == NVMTL_FAILED_AIR_TOO_BIG);
    ARM("the file-count limit stops the folder growing", nvmtl_failed_air_admit(NVMTL_FAILED_AIR_MAX_FILES, 10, 100) == NVMTL_FAILED_AIR_FULL_FILES);
    ARM("the byte limit stops the folder growing", nvmtl_failed_air_admit(3, NVMTL_FAILED_AIR_MAX_TOTAL - 50, 100) == NVMTL_FAILED_AIR_FULL_BYTES);
    ARM("a file that exactly fills the byte limit is kept", nvmtl_failed_air_admit(3, NVMTL_FAILED_AIR_MAX_TOTAL - 100, 100) == NVMTL_FAILED_AIR_ADMIT);
    ARM("the same text digests to the same value, other text does not", nvmtl_failed_air_digest("abc", 3) == nvmtl_failed_air_digest("abc", 3) && nvmtl_failed_air_digest("abc", 3) != nvmtl_failed_air_digest("abd", 3));
    char n[160];
    ARM("a plain function name keeps its letters", nvmtl_failed_air_file_name(n, sizeof n, "TextureVertexFunc", 0xabcull) && !strcmp(n, "TextureVertexFunc-0000000000000abc.ll"));
    ARM("path characters in a name cannot leave the folder", nvmtl_failed_air_file_name(n, sizeof n, "../../etc/x y", 1) && !strchr(n, '/') && strncmp(n, "..", 2) != 0);
    ARM("an empty name still gets a file name", nvmtl_failed_air_file_name(n, sizeof n, "", 1) && n[0] == 'f');
    char longname[300]; memset(longname, 'a', sizeof longname - 1); longname[sizeof longname - 1] = 0;
    ARM("a very long name is cut to fit", nvmtl_failed_air_file_name(n, sizeof n, longname, 1) && strlen(n) < 100);
    ARM("a name buffer that is too small is refused, not cut", !nvmtl_failed_air_file_name(n, 8, "kernel", 1));
    char h[600];
    ARM("the header is one comment line holding the stage and the reason", nvmtl_failed_air_header(h, sizeof h, "kernel", "air.x expects 5\noperands") && h[0] == ';' && strchr(h, '\n') == h + strlen(h) - 1 && strstr(h, "kernel") && strstr(h, "expects 5 operands"));
    printf("%s\n", fails ? "FAILED" : "all arms pass");
    return fails != 0;
}

/* Optional mapped-chunk accounting for the command-pool cache. */
#include <CommonCrypto/CommonDigest.h>
#include <fcntl.h>
static uint32_t (*nvmtl_pool_mapping_cost)(VkCommandPool);
static pthread_once_t nvmtl_pool_cost_once = PTHREAD_ONCE_INIT;
static void nvmtl_pool_cost_load(void) {
    if (g_lib) nvmtl_pool_mapping_cost = (void *)dlsym(g_lib, "nvmtl_nvk_pool_mappings_v1");
    if (nvmtl_pool_mapping_cost) return;
    const char *path = getenv("NVMTL_POOL_COST_LIB");
    if (!path || !*path) return;
    void *lib = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!lib) return;
    const char *(*expected)(void) = (void *)dlsym(lib, "nvmtl_pool_cost_icd_sha256_v1");
    uint32_t (*query)(VkCommandPool) = (void *)dlsym(lib, "nvmtl_nvk_pool_mappings_v1");
    Dl_info info; int fd = -1, ok = 0;
    if (expected && query && dladdr((void *)pvkCreateCommandPool, &info) && info.dli_fname)
        fd = open(info.dli_fname, O_RDONLY | O_CLOEXEC);
    if (fd >= 0) {
        CC_SHA256_CTX ctx;CC_SHA256_Init(&ctx);unsigned char b[65536],digest[32];ssize_t n;
        do {n=read(fd,b,sizeof b);if(n>0)CC_SHA256_Update(&ctx,b,(CC_LONG)n);} while(n>0 || (n<0 && errno==EINTR));
        close(fd);
        if(n==0){CC_SHA256_Final(digest,&ctx);char hex[65];for(unsigned i=0;i<32;i++)snprintf(hex+2*i,3,"%02x",digest[i]);ok=!strcmp(hex,expected());}
    }
    if (ok) {nvmtl_pool_mapping_cost=query;nvlog("vk: mapped-chunk pool accounting enabled for the verified backend");}
    else {dlclose(lib);nvlog("vk: pool accounting backend mismatch; retaining legacy cache limit");}
}
static unsigned nvmtl_pool_mapping_budget(void) { return 512; }

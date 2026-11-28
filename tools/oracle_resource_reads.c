/* Invoked by test_translation_resource_reads.py with an isolated plugin and SPIR-V. */
#include <mach/mach.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#include "nvmtl_vk.h"
#define LOAD(n) __typeof__(&n) p_##n = (__typeof__(&n))dlsym(lib, #n); if(!p_##n) {puts(#n); return 2;}
#define OK(x) do {int rc=(x); if(rc) {fprintf(stderr,"%s = %d\n",#x,rc);return 3;}} while(0)
int main(int argc,char **argv) {
 if(argc!=3)return 2;
 void *lib=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL); if(!lib){puts(dlerror());return 2;}
 LOAD(nvmtl_vk_init);LOAD(nvmtl_vk_queue_create);LOAD(nvmtl_vk_buffer_create);
 LOAD(nvmtl_vk_compute_pipeline_create);LOAD(nvmtl_vk_cmd_begin);LOAD(nvmtl_vk_bind_buffer);
 LOAD(nvmtl_vk_cmd_bind_set);LOAD(nvmtl_vk_cmd_dispatch);LOAD(nvmtl_vk_submit_wait_at);
 LOAD(nvmtl_vk_pipeline_destroy);LOAD(nvmtl_vk_buffer_destroy);LOAD(nvmtl_vk_queue_destroy);
 FILE *f=fopen(argv[2],"rb");if(!f)return 2;fseek(f,0,SEEK_END);long n=ftell(f);rewind(f);void *spv=malloc(n);if(fread(spv,1,n,f)!=(size_t)n)return 2;fclose(f);
 OK(p_nvmtl_vk_init());nvk_queue q={0};nvk_buffer input={0},output={0};nvk_pipeline pipe={0};nvk_cmdbuf cmd={0};
 OK(p_nvmtl_vk_queue_create(&q));OK(p_nvmtl_vk_buffer_create(256,1,&input));OK(p_nvmtl_vk_buffer_create(256,1,&output));
 if(!input.map||!output.map)return 2;
 *(uint32_t*)input.map=0x12345678;*(uint32_t*)output.map=0xdeadbeef;
 OK(p_nvmtl_vk_compute_pipeline_create(spv,n,&pipe));OK(p_nvmtl_vk_cmd_begin(&q,&cmd));
 OK(p_nvmtl_vk_bind_buffer(&cmd,0,0,&input,0));OK(p_nvmtl_vk_bind_buffer(&cmd,0,1,&output,0));
 p_nvmtl_vk_cmd_bind_set(&cmd,&pipe);p_nvmtl_vk_cmd_dispatch(&cmd,&pipe,1,1,1);OK(p_nvmtl_vk_submit_wait_at(&cmd,"resource-read-test"));
 uint32_t got=*(volatile uint32_t*)output.map;printf("output=%08x expected=12345678\n",got);
 p_nvmtl_vk_pipeline_destroy(&pipe);p_nvmtl_vk_buffer_destroy(&input);p_nvmtl_vk_buffer_destroy(&output);p_nvmtl_vk_queue_destroy(&q);free(spv);return got==0x12345678?0:1;
}

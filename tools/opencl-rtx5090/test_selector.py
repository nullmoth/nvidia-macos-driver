#!/usr/bin/env python3
"""Compile actual production functions with bounded CPU allocation/compiler stubs."""
from pathlib import Path
import argparse,hashlib,json,os,subprocess,tempfile
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--original', type=Path, required=True, help='Mesa after NullMoth 3.0 and DLSS, before selective retry')
parser.add_argument('--candidate', type=Path, required=True, help='Mesa with the supplied selective retry patch')
args=parser.parse_args()
output=tempfile.TemporaryDirectory(prefix='nvk-selector-cpu-')
ROOT=Path(output.name)
P=args.candidate/'src/nouveau/vulkan/nvk_shader.c'
source=P.read_text()
def function(text,name):
    at=text.index('\n'+name+'('); start=text.rfind('\n',0,text.rfind('\n',0,at))+1
    brace=text.index('{',at); level=1;i=brace+1
    while level:
        level+=(text[i]=='{')-(text[i]=='}');i+=1
    return text[start:i]+'\n'
functions='\n'.join(function(source,n) for n in ['nvk_nak_spill_retry_enabled','nvk_physical_device_compiler_flags','nvk_nak_retry_is_better','nvk_compile_nir'])
preamble=r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#undef assert
#define assert(condition) do { if (!(condition)) { fprintf(stderr,"check failed: %s at %d\n",#condition,__LINE__); _Exit(1); } } while (0)
typedef int VkResult;
typedef unsigned VkShaderCreateFlagsEXT;
typedef unsigned nir_variable_mode;
#define MESA_SHADER_COMPUTE 5
#define MESA_SHADER_FRAGMENT 4
#define VK_SUCCESS 0
#define VK_ERROR_UNKNOWN -1
#define VK_ERROR_OUT_OF_HOST_MEMORY -2
#define VK_SHADER_CREATE_CAPTURE_INTERNAL_REPRESENTATIONS_BIT_MESA 1u
#define VK_SHADER_CREATE_NO_TASK_SHADER_BIT_EXT 2u
#define VK_PIPELINE_ROBUSTNESS_BUFFER_BEHAVIOR_ROBUST_BUFFER_ACCESS_2_EXT 3u
#define nir_var_mem_ubo 1u
#define nir_var_mem_ssbo 2u
#define NVK_DEBUG_NO_CBUF 1u
#define align(v,a) (((v)+(a)-1)&~((a)-1))
struct nak_compiler {int unused;};
struct nak_fs_key {int value;};
struct nak_shader_info {uint32_t num_spills_to_mem,num_fills_from_mem,num_spills_to_reg,num_fills_from_reg,max_warps_per_sm;};
struct nak_shader_bin {struct nak_shader_info info;uint32_t code_size;const void *code;const char *asm_str;int identity;};
typedef struct nir_shader {struct {int stage;} info;struct nir_shader *child;bool processed,alive;unsigned constant_data_size;char data[4];void *constant_data;int identity;} nir_shader;
struct nvk_instance {struct {struct {bool ssbo_align_4b;} misc;} drirc;};
struct nvk_physical_device {struct nak_compiler *nak;unsigned debug_flags;int info;};
struct nvk_device {int unused;};
struct vk_pipeline_robustness_state {unsigned uniform_buffers,storage_buffers;};
struct nvk_shader {struct nak_shader_bin *nak;struct nak_shader_info info;const char *asm_str;const void *code_ptr;unsigned code_size;void *data_ptr;unsigned data_size;char *nir_str;};
static struct nak_compiler compiler;
static struct nvk_physical_device physical={.nak=&compiler};
static struct nvk_instance instance;
static struct nak_fs_key fs_key={42};
static struct nak_shader_info normal_stats,retry_stats;
static bool normal_fail,retry_fail,data_fail;
static unsigned calls,clones,nir_frees[2],bin_frees[2],bin_allocs[2];
static nir_shader *nir_nodes[2];
static unsigned expected_robust;
static bool expected_dump,expected_task;
static const struct nvk_physical_device *nvk_device_physical(struct nvk_device *d){(void)d;return &physical;}
static const struct nvk_instance *nvk_physical_device_instance(const struct nvk_physical_device *p){assert(p==&physical);return &instance;}
static bool nvk_use_edb_buffer_views(const struct nvk_physical_device *p){(void)p;return true;}
static uint64_t nak_debug_flags(const struct nak_compiler *n){assert(n==&compiler);return 0x45;}
static unsigned nvk_min_cbuf_alignment(const int *i){(void)i;return 16;}
static int vk_errorf(const void *p,int e,const char *s){(void)p;(void)s;return e;}
static int vk_error(const void *p,int e){(void)p;return e;}
static nir_shader *nir_shader_clone(void *parent,const nir_shader *src){
 assert(parent==NULL||parent==src);assert(!src->processed);assert(src->alive);assert(!src->child);
 nir_shader *n=calloc(1,sizeof(*n));assert(n);*n=*src;n->identity=1;n->constant_data=n->data;nir_nodes[1]=n;if(parent)((nir_shader*)parent)->child=n;clones++;return n;
}
/* Model ralloc invalidation immediately; defer storage release to avoid old-code UB. */
static void ralloc_free(void *ptr){nir_shader *n=ptr;if(!n)return;assert(n->alive);if(n->child)ralloc_free(n->child);n->child=NULL;n->alive=false;nir_frees[n->identity]++;}
static struct nak_shader_bin *nak_compile_shader_with_load_sink_control(nir_shader *nir,bool dump,const struct nak_compiler *nak,nir_variable_mode robust,const struct nak_fs_key *key,bool task,bool skip){
 assert(nak==&compiler&&key==&fs_key&&robust==expected_robust&&dump==expected_dump&&task==expected_task);
 assert(nir->alive);assert(!nir->processed);assert(skip==(nir->identity==1));assert(calls==(unsigned)nir->identity);calls++;
 /* Simulate the supported NIR_DEBUG clone/serialize replacement of children. */
 if(nir->child){ralloc_free(nir->child);nir->child=NULL;}
 nir->processed=true;memcpy(nir->data,skip?"RETR":"NORM",4);nir->constant_data_size=4;
 if(skip?retry_fail:normal_fail)return NULL;
 struct nak_shader_bin *bin=calloc(1,sizeof(*bin));assert(bin);bin->identity=nir->identity;
 bin->info=skip?retry_stats:normal_stats;bin->code=skip?"retrycode":"normalcode";bin->asm_str=skip?"retryasm":"normalasm";bin->code_size=9;bin_allocs[bin->identity]++;return bin;
}
static struct nak_shader_bin *nak_compile_shader(nir_shader *nir,bool dump,const struct nak_compiler *nak,nir_variable_mode robust,const struct nak_fs_key *key,bool task){return nak_compile_shader_with_load_sink_control(nir,dump,nak,robust,key,task,false);}
static void nak_shader_bin_destroy(struct nak_shader_bin *bin){assert(bin);assert(++bin_frees[bin->identity]<=bin_allocs[bin->identity]);free(bin);}
static char *nir_shader_as_str(nir_shader *nir,void *ctx){assert(!ctx&&nir->alive);return strdup(nir->identity?"retry_nir":"normal_nir");}
static void *fixture_malloc(size_t bytes){return data_fail?NULL:malloc(bytes);}
#define malloc fixture_malloc
'''
main=r'''
#undef malloc
static unsigned tests;
static struct nak_shader_info info(uint32_t sm,uint32_t fm,uint32_t sr,uint32_t fr,uint32_t w){return(struct nak_shader_info){sm,fm,sr,fr,w};}
static void env(const char *name,const char *value){if(value)assert(!setenv(name,value,1));else assert(!unsetenv(name));}
static void run_case(const char *label,const char *policy,const char *nosink,int stage,struct nak_shader_info a,struct nak_shader_info b,bool fail_a,bool fail_b,bool fail_data,unsigned want_calls,unsigned want_clones,int want_identity,int want_result){
 env("NVMTL_NAK_SPILL_RETRY",policy);env("NVMTL_NAK_NO_LDSINK",nosink);
 normal_stats=a;retry_stats=b;normal_fail=fail_a;retry_fail=fail_b;data_fail=fail_data;
 calls=clones=0;memset(nir_frees,0,sizeof(nir_frees));memset(nir_nodes,0,sizeof(nir_nodes));memset(bin_frees,0,sizeof(bin_frees));memset(bin_allocs,0,sizeof(bin_allocs));
 struct nvk_device dev={0};struct nvk_shader shader={0};struct vk_pipeline_robustness_state robust={3,3};
 nir_shader *nir=calloc(1,sizeof(*nir));assert(nir);nir->alive=true;nir_nodes[0]=nir;nir->info.stage=stage;nir->constant_data=nir->data;memcpy(nir->data,"ORIG",4);
 expected_dump=true;expected_task=false;expected_robust=3;
 int result=nvk_compile_nir(&dev,nir,3,&robust,&fs_key,&shader);
 if(result!=want_result||calls!=want_calls||clones!=want_clones){fprintf(stderr,"case %s result/calls/clones: %d/%u/%u\n",label,result,calls,clones);_Exit(1);}
 if(want_identity<0){assert(!shader.nak);assert(!shader.data_ptr);assert(!shader.nir_str);}
 else {
   assert(shader.nak&&shader.nak->identity==want_identity);
   assert(shader.info.num_spills_to_mem==shader.nak->info.num_spills_to_mem);
   assert(shader.asm_str==shader.nak->asm_str&&shader.code_ptr==shader.nak->code&&shader.code_size==shader.nak->code_size);
   if(result==VK_SUCCESS){assert(shader.data_size==16);assert(!memcmp(shader.data_ptr,want_identity?"RETR":"NORM",4));for(unsigned i=4;i<16;i++)assert(((char*)shader.data_ptr)[i]==0);assert(!strcmp(shader.nir_str,want_identity?"retry_nir":"normal_nir"));}
   nak_shader_bin_destroy(shader.nak);
 }
 free(shader.data_ptr);free(shader.nir_str);
 assert(nir->alive);assert(nir_frees[0]==0&&nir_frees[1]==clones);
 ralloc_free(nir);assert(nir_frees[0]==1&&nir_frees[1]==clones);
 for(unsigned i=0;i<2;i++)free(nir_nodes[i]);
assert(bin_frees[0]==bin_allocs[0]&&bin_frees[1]==bin_allocs[1]);tests++;
}
int main(void){
 env("NVMTL_NAK_ARR_SCRATCH",NULL);env("NVMTL_NAK_NO_LDSINK",NULL);env("NVMTL_NAK_SPILL_RETRY",NULL);
 uint64_t baseline=nvk_physical_device_compiler_flags(&physical);assert(!(baseline&(UINT64_C(1)<<16)));
 env("NVMTL_NAK_SPILL_RETRY","1");assert((nvk_physical_device_compiler_flags(&physical)^baseline)==(UINT64_C(1)<<16));
 env("NVMTL_NAK_NO_LDSINK","1");assert(nvk_physical_device_compiler_flags(&physical)==baseline);
 env("NVMTL_NAK_NO_LDSINK",NULL);env("NVMTL_NAK_ARR_SCRATCH","0");assert((nvk_physical_device_compiler_flags(&physical)^baseline)==((UINT64_C(1)<<16)|(UINT64_C(1)<<15)));env("NVMTL_NAK_ARR_SCRATCH",NULL);
 struct nak_shader_info spill=info(45,45,0,0,8),clean=info(0,0,0,0,28);
 const char *off[]={NULL,"","0","true","01","1x"};for(unsigned i=0;i<6;i++)run_case("off",off[i],NULL,5,spill,clean,false,false,false,1,0,0,0);
 run_case("global-off","1","1",5,spill,clean,false,false,false,1,0,0,0);
 run_case("global-prefix","1","10",5,spill,clean,false,false,false,1,0,0,0);
 run_case("fragment","1",NULL,4,spill,clean,false,false,false,1,0,0,0);
 run_case("zero-memory-spills","1",NULL,5,info(0,0,8,8,8),clean,false,false,false,1,1,0,0);
 run_case("improvement","1",NULL,5,spill,clean,false,false,false,2,1,1,0);
 run_case("fills-only","1",NULL,5,info(0,1,0,0,8),clean,false,false,false,2,1,1,0);
 run_case("spills-only","1",NULL,5,info(1,0,0,0,8),clean,false,false,false,2,1,1,0);
 run_case("partial-improvement","1",NULL,5,spill,info(44,45,0,0,8),false,false,false,2,1,1,0);
 run_case("tie","1",NULL,5,spill,spill,false,false,false,2,1,0,0);
 run_case("more-memory-spills","1",NULL,5,spill,info(46,45,0,0,48),false,false,false,2,1,0,0);
 run_case("lower-occupancy","1",NULL,5,spill,info(0,0,0,0,7),false,false,false,2,1,0,0);
 run_case("more-register-spills","1",NULL,5,spill,info(0,0,1,0,28),false,false,false,2,1,0,0);
 run_case("more-register-fills","1",NULL,5,spill,info(0,0,0,1,28),false,false,false,2,1,0,0);
 run_case("less-register-spills","1",NULL,5,info(45,45,2,2,8),info(0,0,1,2,28),false,false,false,2,1,1,0);
 run_case("normal-null","1",NULL,5,spill,clean,true,false,false,1,1,-1,-1);
 run_case("retry-null","1",NULL,5,spill,clean,false,true,false,2,1,0,0);
 run_case("normal-data-oom","1",NULL,5,spill,spill,false,false,true,2,1,0,-2);
 run_case("retry-data-oom","1",NULL,5,spill,clean,false,false,true,2,1,1,-2);
 run_case("wide-memory-sum","1",NULL,5,info(UINT32_MAX,UINT32_MAX,0,0,8),info(UINT32_MAX,0,0,0,8),false,false,false,2,1,1,0);
 run_case("wide-register-sum","1",NULL,5,info(45,45,UINT32_MAX,UINT32_MAX,8),info(0,0,UINT32_MAX,0,8),false,false,false,2,1,1,0);
 printf("PASS actual-production-functions %u cases\n",tests);return 0;
}
'''
# Exact source guards prove the fixture's actual helper is wired to the real NIR/Rust hook.
nir=(args.candidate/'src/nouveau/compiler/nak_nir.c').read_text()
rust=(args.candidate/'src/nouveau/compiler/nak/api.rs').read_text()
original_rust=(args.original/'src/nouveau/compiler/nak/api.rs').read_text()
assert 'if (!raw_pressure && !skip_load_sink) OPT(nir, nak_cg_ldsink);' in nir
assert 'nir, nak, robust2_modes, fs_key, has_task_shader, skip_load_sink,' in rust
assert rust.count('    skip_load_sink: bool,')==2
# Existing exported C ABI must remain exactly six arguments in both languages.
marker='pub extern "C" fn nak_compile_shader('
old_sig=original_rust[original_rust.index(marker):original_rust.index('{',original_rust.index(marker))]
assert rust[rust.index(marker):rust.index('{',rust.index(marker))]==old_sig
old_header=(args.original/'src/nouveau/compiler/nak.h').read_text()
new_header=(args.candidate/'src/nouveau/compiler/nak.h').read_text()
marker='struct nak_shader_bin *\nnak_compile_shader('
old_decl=old_header[old_header.index(marker):old_header.index(';',old_header.index(marker))+1]
assert old_decl in new_header
wrapper=rust[rust.index('pub extern "C" fn nak_compile_shader('):rust.index('#[unsafe(no_mangle)]',rust.index('pub extern "C" fn nak_compile_shader('))]
assert 'nir, dump_asm, nak, robust2_modes, fs_key, has_task_shader, false,' in wrapper
assert 'skip_load_sink' not in wrapper
assert 'nak_compile_shader_with_load_sink_control(' in wrapper

start='    let sm = ShaderModelInfo::new';end='    let mut asm = String::new();'
rs=rust.index(start,rust.index('fn nak_compile_shader_internal(')); os_=original_rust.index(start,original_rust.index('fn nak_compile_shader_internal('))
assert rust[rs:rust.index(end,rs)]==original_rust[os_:original_rust.index(end,os_)]
variants={'actual':functions}
mutations={
 'old_child_ownership':('nir_shader_clone(NULL, nir)','nir_shader_clone(nir, nir)'),
 'accept_ties':('retry_mem < normal_mem','retry_mem <= normal_mem'),
 'ignore_occupancy':('retry->max_warps_per_sm >= normal->max_warps_per_sm','true'),
 'omit_selected_nir':('            nir = retry_nir;','            /* negative: omitted selected NIR */'),
 'omit_loser_destroy':('            nak_shader_bin_destroy(shader->nak);','            /* negative: leaked loser */'),
 'omit_cache_bit':('(nvk_nak_spill_retry_enabled() ? UINT64_C(1) << 16 : 0)','0'),
 'retry_nonspilling':('(shader->nak->info.num_spills_to_mem ||\n                    shader->nak->info.num_fills_from_mem)','true'),
 'ignore_global_disable':("no_sink[0] == '1'","no_sink[0] == 'x'"),
 'skip_wrong_pass':('         has_task_shader, true);','         has_task_shader, false);')}
for name,(old,new) in mutations.items():
 assert functions.count(old)==1,(name,functions.count(old));variants[name]=functions.replace(old,new)
results=[]
environment={**os.environ,'ASAN_OPTIONS':'detect_leaks=0:halt_on_error=1','UBSAN_OPTIONS':'halt_on_error=1'}
for name,body in variants.items():
 path=ROOT/(name+'.c');path.write_text(preamble+body+main);exe=ROOT/name
 build=subprocess.run(['/usr/bin/clang','-std=c11','-O1','-g','-Wall','-Wextra','-Werror','-fsanitize=address,undefined',str(path),'-o',str(exe)],text=True,capture_output=True,timeout=20)
 assert build.returncode==0,(name,build.stderr)
 run=subprocess.run([str(exe)],text=True,capture_output=True,env=environment,timeout=5)
 (ROOT/(name+'.stdout')).write_text(run.stdout);(ROOT/(name+'.stderr')).write_text(run.stderr)
 ok=(run.returncode==0) if name=='actual' else (run.returncode!=0)
 results.append({'name':name,'exit_code':run.returncode,'expected_rejected':name!='actual','passed':ok,'source_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256(exe.read_bytes()).hexdigest()})
 assert ok,(name,run.stdout,run.stderr)
result={'status':'PASS','production_source_sha256':hashlib.sha256(P.read_bytes()).hexdigest(),'actual_functions':['nvk_nak_spill_retry_enabled','nvk_physical_device_compiler_flags','nvk_nak_retry_is_better','nvk_compile_nir'],'source_checks':['unchanged Rust prepass chooser','explicit skip bool propagation','actual C load-sink guard','unchanged six-argument C/Rust exported ABI; legacy wrapper passesfalse'],'cases':26,'negative_controls':9,'sanitizers':['address','undefined'],'scope':'Actual production decision/compile-owner functions; compiler, allocator parent tree, physical device and NIR API are CPU stubs. This does not validate real NIR cloning, Rust code generation, or GPU correctness.','results':results}
(ROOT/'cpu-result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'status':'PASS','cases':26,'negative_controls_rejected':9}))

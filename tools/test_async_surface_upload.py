#!/usr/bin/env python3
"""Exercise the production async imported-surface upload path with mocked Vulkan fences.

NVMTL_SURFIN_ASYNC controls only the imported-host path. CPU fallback follows the existing
NVMTL_ASYNC_SURFACE_UPLOAD policy, so a sync A/B must set both switches to 0.
"""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
vk = (root / "plugin/nvmtl_vk.c").read_text()

ordered_start = vk.index("// NVMTL_SURFIN_ORDERED")
type_start = vk.index("typedef struct nvmtl_asyncup", ordered_start)
gate_start = vk.index("// Serialize operations that record or consume work for one image.")
gate_end = vk.index("\nstatic void nvmtl_vk_image_destroy_body", gate_start)
image_gate = vk[gate_start:gate_end]
ordered = vk[ordered_start:type_start]
tick_start = vk.index("static void nvmtl_surfin_tick_start(void)\n{", type_start)
tracking_head = vk[type_start:tick_start]
record_start = vk.index("static nvmtl_asyncup *nvmtl_surfin_record_new(void)\n{", tick_start)
writer_body_start = vk.index("static int nvmtl_vk_image_write_host_body(", record_start)
writer_start = vk.index("int nvmtl_vk_image_write_host(", writer_body_start)
tracking_tail = vk[record_start:writer_start]
writer_end = vk.index("\n// A CPU-written IOSurface", writer_start)
writer = vk[writer_body_start:writer_end]

assert "static dispatch_source_t g_surfin_timer;" in tracking_head
assert "dispatch_source_set_timer(timer, dispatch_time(DISPATCH_TIME_NOW, 500 * NSEC_PER_MSEC)" in vk
assert "dispatch_source_set_event_handler(timer, ^{ nvmtl_surfin_reap(); });" in vk

fixture = r'''
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <vulkan/vulkan_core.h>
#include "nvmtl_vk.h"
#include "nvmtl_tex_upload.h"

typedef void *dispatch_source_t;
struct nvmtl_inflight { nvk_cmdbuf *c; VkFence fence; };
static int g_state = 1;
static VkDevice g_dev = (VkDevice)(uintptr_t)1;
static size_t g_host_align;
static unsigned submits, waits, finishes, destroys, cmds, copies, barrier_count;
static VkImageMemoryBarrier barriers[32];
static VkPipelineStageFlags src_stages[32], dst_stages[32];
static VkBufferImageCopy seen_copy;
static int ready[256], finish_signals = 1;
static pthread_mutex_t poll_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t poll_cond = PTHREAD_COND_INITIALIZER;
static int block_poll, poll_entered, poll_release;
static _Atomic int store_started;
static pthread_mutex_t finish_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t finish_cond = PTHREAD_COND_INITIALIZER;
static int block_finish, finish_entered, finish_release;
static int block_submit_begin, submit_entered, submit_release;
static unsigned block_finish_id;

static void nvlog(const char *format, ...) { (void)format; }
static VkImageAspectFlags nvmtl_barrier_aspect(const nvk_image *img) { (void)img; return VK_IMAGE_ASPECT_COLOR_BIT; }
static VkImageAspectFlags nvmtl_copy_aspect(const nvk_image *img) { (void)img; return VK_IMAGE_ASPECT_COLOR_BIT; }
static nvk_buffer *nvmtl_host_pages(nvk_buffer *cache, void *base, size_t bytes, nvk_buffer *tmp) {
    if (cache && cache->buf && cache->map == base && cache->size >= bytes) return cache;
    *tmp = (nvk_buffer){.buf=(void *)(uintptr_t)0x91, .map=base, .size=bytes, .imported=1};
    return tmp;
}
static void nvmtl_host_pages_done(nvk_buffer *cache, nvk_buffer *pages) { (void)cache; if (pages && pages != cache) destroys++; }
int nvmtl_vk_cmd_begin(nvk_queue *queue, nvk_cmdbuf *cb) {
    (void)queue; cmds++; cb->cb = (void *)(uintptr_t)0x92; return 0;
}
int nvmtl_vk_submit_wait_at(nvk_cmdbuf *cb, const char *who) {
    assert(cb && cb->cb && who); waits++; return 0;
}
int nvmtl_vk_submit_begin_at(nvk_cmdbuf *cb, nvmtl_inflight *inflight, const char *who) {
    assert(cb && cb->cb && who); submits++; inflight->c = cb; inflight->fence = (VkFence)(uintptr_t)submits;
    pthread_mutex_lock(&finish_mutex);
    if (block_submit_begin) {
        submit_entered=1; pthread_cond_broadcast(&finish_cond);
        while (!submit_release) pthread_cond_wait(&finish_cond,&finish_mutex);
        block_submit_begin=0;
    }
    pthread_mutex_unlock(&finish_mutex);
    return 0;
}
int nvmtl_vk_submit_finish(nvmtl_inflight *inflight) {
    unsigned id = (unsigned)(uintptr_t)inflight->fence; assert(id && id < 256); finishes++;
    pthread_mutex_lock(&finish_mutex);
    if (block_finish && id == block_finish_id) {
        finish_entered = 1; pthread_cond_broadcast(&finish_cond);
        while (!finish_release) pthread_cond_wait(&finish_cond, &finish_mutex);
        block_finish = 0;
    }
    pthread_mutex_unlock(&finish_mutex);
    if (finish_signals) ready[id] = 1;
    if (!ready[id]) { g_state=-1; return -1; }
    return 0;
}
void nvmtl_vk_buffer_destroy(nvk_buffer *buffer) { (void)buffer; destroys++; }
static VkResult mock_wait(VkDevice device, uint32_t count, const VkFence *fences, VkBool32 all, uint64_t timeout) {
    (void)device; assert(count == 1 && all == VK_TRUE && timeout == 0);
    const unsigned id = (unsigned)(uintptr_t)fences[0]; assert(id && id < 256);
    pthread_mutex_lock(&poll_mutex);
    if (block_poll) {
        poll_entered = 1; pthread_cond_broadcast(&poll_cond);
        while (!poll_release) pthread_cond_wait(&poll_cond, &poll_mutex);
        block_poll = 0;
    }
    pthread_mutex_unlock(&poll_mutex);
    return ready[id] ? VK_SUCCESS : VK_TIMEOUT;
}
static VkResult (*pvkWaitForFences)(VkDevice, uint32_t, const VkFence *, VkBool32, uint64_t) = mock_wait;
static void pvkCmdPipelineBarrier(VkCommandBuffer cb, VkPipelineStageFlags src, VkPipelineStageFlags dst,
    VkDependencyFlags flags, uint32_t memory_count, const VkMemoryBarrier *memory,
    uint32_t buffer_count, const VkBufferMemoryBarrier *buffers,
    uint32_t image_count, const VkImageMemoryBarrier *images) {
    (void)cb; (void)flags; (void)memory_count; (void)memory; (void)buffer_count; (void)buffers;
    assert(image_count == 1 && images && barrier_count < 32);
    barriers[barrier_count] = *images; src_stages[barrier_count] = src; dst_stages[barrier_count] = dst; barrier_count++;
}
static void pvkCmdCopyBufferToImage(VkCommandBuffer cb, VkBuffer buffer, VkImage image, VkImageLayout layout,
    uint32_t count, const VkBufferImageCopy *regions) {
    (void)cb; (void)buffer; (void)image;
    assert(layout == VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL && count == 1 && regions);
    seen_copy = *regions; copies++;
}
typedef struct { unsigned refs; unsigned *page_refs; unsigned *unlocks; } Lease;
static void *keep_lease(void *ptr) { Lease *lease=ptr; lease->refs++; return lease; }
static void drop_lease(void *ptr) {
    Lease *lease=ptr; assert(lease->refs); lease->refs--;
    if (!lease->refs) { assert(*lease->page_refs); (*lease->page_refs)--; (*lease->unlocks)++; }
}
static void init_owner(Lease *lease, unsigned *pages, unsigned *unlocks) {
    *pages = 2; *unlocks = 0; lease->refs = 1; lease->page_refs = pages; lease->unlocks = unlocks;
}
static void caller_drop_and_cache_evict(Lease *lease, unsigned *pages) {
    drop_lease(lease); assert(*pages == 2); (*pages)--; // caller then drops cache/texture; the record remains the last owner
}
static nvk_image make_image(VkImageLayout layout, uint32_t mips, uint32_t layers) {
    return (nvk_image){.img=(void *)(uintptr_t)0x93, .w=16, .h=8, .layout=layout,
        .mips=mips, .layers=layers, .bpp=4, .samples=1};
}
static int upload(nvk_image *image, nvk_buffer *cache, Lease *lease) {
    return nvmtl_vk_image_write_host(NULL, image, cache->map, 64, cache->size, cache,
        lease, lease ? keep_lease : NULL, lease ? drop_lease : NULL);
}
typedef struct { nvk_image *image; nvk_buffer *cache; Lease *lease; int rc; } UploadArg;
static nvk_image *consumer_image;
static _Atomic int consumer_started, consumer_done;
static int consumer_rc;
static void *upload_thread(void *raw) {
    UploadArg *arg=raw; arg->rc=upload(arg->image,arg->cache,arg->lease); return NULL;
}
static void *consume_thread(void *unused) {
    (void)unused; atomic_store(&consumer_started,1); consumer_rc=nvmtl_vk_image_upload_barrier(consumer_image);
    atomic_store(&consumer_done,1); return NULL;
}
static void assert_ordered_barrier(VkImageLayout old_layout) {
    assert(barrier_count == 2);
    assert(barriers[0].oldLayout == old_layout);
    assert(barriers[0].newLayout == VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
    assert(src_stages[0] == VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
    assert(dst_stages[0] == VK_PIPELINE_STAGE_TRANSFER_BIT);
    assert((barriers[0].srcAccessMask & (VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT))
           == (VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT));
    assert(barriers[0].dstAccessMask & VK_ACCESS_TRANSFER_WRITE_BIT);
    assert(barriers[1].oldLayout == VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL);
    assert(barriers[1].newLayout == VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    assert(barriers[0].subresourceRange.baseMipLevel == 0 && barriers[0].subresourceRange.baseArrayLayer == 0);
}
static nvk_image *race_image;
static nvmtl_asyncup replacement_record;
static void *store_thread(void *unused) { (void)unused; atomic_store(&store_started,1); nvmtl_asyncup_store(race_image,(uint64_t)(uintptr_t)&replacement_record); return NULL; }
static void *reap_thread(void *unused) { (void)unused; nvmtl_surfin_reap(); return NULL; }
static void async_case(VkImageLayout prior_layout, int race) {
    unsigned page_refs, unlocks; Lease lease; init_owner(&lease, &page_refs, &unlocks);
    unsigned char source[4096]; nvk_buffer cache={.buf=(void *)(uintptr_t)0x94,.map=source,.size=sizeof source,.imported=1};
    nvk_image image=make_image(prior_layout,1,1);
    assert(!upload(&image,&cache,&lease));
    assert(submits==1 && waits==0 && lease.refs==2 && page_refs==2 && g_surfin_live==1);
    assert(image.layout==VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL && image.up_token);
    assert_ordered_barrier(prior_layout);
    assert(copies==1 && seen_copy.bufferRowLength==16 && seen_copy.imageExtent.width==16 && seen_copy.imageExtent.height==8);
    caller_drop_and_cache_evict(&lease,&page_refs);
    assert(lease.refs==1 && page_refs==1 && !unlocks);

    if (!race) {
        ready[1]=0; nvmtl_surfin_reap();
        assert(image.up_token && lease.refs==1 && page_refs==1 && !unlocks && finishes==0);
        ready[1]=1; nvmtl_surfin_reap();
        assert(!image.up_token && lease.refs==0 && page_refs==0 && unlocks==1 && finishes==1);
    } else {
        ready[1]=1; block_poll=1; poll_entered=0; poll_release=0; atomic_store(&store_started,0);
        pthread_t poller, writer;
        race_image=&image;
        assert(!pthread_create(&poller,NULL,reap_thread,NULL));
        pthread_mutex_lock(&poll_mutex); while(!poll_entered) pthread_cond_wait(&poll_cond,&poll_mutex); pthread_mutex_unlock(&poll_mutex);
        assert(!pthread_create(&writer,NULL,store_thread,NULL));
        while(!atomic_load(&store_started)) usleep(1000);
        usleep(10000); assert(image.up_token); // the replacement waits for the poll's token lock
        pthread_mutex_lock(&poll_mutex); poll_release=1; pthread_cond_broadcast(&poll_cond); pthread_mutex_unlock(&poll_mutex);
        assert(!pthread_join(poller,NULL) && !pthread_join(writer,NULL));
        assert(image.up_token==(uint64_t)(uintptr_t)&replacement_record && lease.refs==0 && page_refs==0 && unlocks==1 && finishes==1);
        nvmtl_asyncup_unpublish(&replacement_record, (uint64_t)(uintptr_t)&replacement_record);
    }
    assert(g_surfin_live==0 && g_surfin_npend==0);
}
static void sync_case(int owner_present, int mips, int layers, int ordered_off) {
    unsigned page_refs=owner_present?2:0, unlocks=0;
    Lease lease={.refs=owner_present?1:0,.page_refs=&page_refs,.unlocks=&unlocks};
    unsigned char source[4096]; nvk_buffer cache={.buf=(void *)(uintptr_t)0x94,.map=source,.size=sizeof source,.imported=1};
    nvk_image image=make_image(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,(uint32_t)mips,(uint32_t)layers);
    assert(!upload(&image,&cache,owner_present?&lease:NULL));
    assert(waits==1 && submits==0 && lease.refs==(owner_present?1u:0u) && g_surfin_live==0);
    assert(barrier_count==2 && image.layout==VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    assert(barriers[0].subresourceRange.levelCount==(uint32_t)mips);
    assert(barriers[0].subresourceRange.layerCount==(uint32_t)layers);
    if (ordered_off) {
        assert(barriers[0].oldLayout==VK_IMAGE_LAYOUT_UNDEFINED);
        assert(src_stages[0]==VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT && barriers[0].srcAccessMask==0);
    } else assert_ordered_barrier(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    if (owner_present) { // writer was synchronous; caller releases its lease after the waited copy
        drop_lease(&lease); assert(page_refs==1 && unlocks==1); page_refs--;
    }
}
static void bounded_case(void) {
    nvk_image images[5]; Lease leases[5]; unsigned page_refs[5], unlocks[5];
    unsigned char sources[5][4096]; nvk_buffer caches[5];
    for (unsigned i=0;i<5;i++) {
        init_owner(&leases[i],&page_refs[i],&unlocks[i]);
        caches[i]=(nvk_buffer){.buf=(void *)(uintptr_t)0x94,.map=sources[i],.size=sizeof sources[i],.imported=1};
        images[i]=make_image(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,1,1);
    }
    for(unsigned i=0;i<4;i++) { assert(!upload(&images[i],&caches[i],&leases[i])); drop_lease(&leases[i]); page_refs[i]--; }
    assert(g_surfin_live==4 && g_surfin_npend==4);
    finish_signals=1; block_finish=1; block_finish_id=1; finish_entered=finish_release=0;
    UploadArg fifth={&images[4],&caches[4],&leases[4],-99};
    consumer_image=&images[0]; atomic_store(&consumer_started,0); atomic_store(&consumer_done,0);
    pthread_t submitter, consumer;
    assert(!pthread_create(&submitter,NULL,upload_thread,&fifth));
    pthread_mutex_lock(&finish_mutex); while(!finish_entered) pthread_cond_wait(&finish_cond,&finish_mutex); pthread_mutex_unlock(&finish_mutex);
    assert(images[0].up_token);
    assert(!pthread_create(&consumer,NULL,consume_thread,NULL));
    while(!atomic_load(&consumer_started)) usleep(1000);
    usleep(10000); assert(!atomic_load(&consumer_done) && images[0].up_token);
    pthread_mutex_lock(&finish_mutex); finish_release=1; pthread_cond_broadcast(&finish_cond); pthread_mutex_unlock(&finish_mutex);
    assert(!pthread_join(consumer,NULL) && atomic_load(&consumer_done));
    assert(!pthread_join(submitter,NULL) && fifth.rc==0); drop_lease(&leases[4]); page_refs[4]--;
    assert(g_surfin_live==4 && g_surfin_npend==4 && !images[0].up_token);
    assert(!leases[0].refs && !page_refs[0] && unlocks[0]==1);
    for(unsigned i=1;i<5;i++) { ready[i+1]=1; nvmtl_vk_image_upload_barrier(&images[i]); }
    assert(g_surfin_live==0 && g_surfin_npend==0);
}
static void cross_store_case(void) {
    nvk_image image=make_image(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,1,1);
    nvmtl_asyncup *generic=calloc(1,sizeof *generic); assert(generic);
    generic->infl.fence=(VkFence)(uintptr_t)1;
    nvmtl_asyncup *surface=nvmtl_surfin_record_new(); assert(surface);
    surface->infl.fence=(VkFence)(uintptr_t)2;
    nvmtl_surfin_store(&image,surface); // later queue submit publishes first
    nvmtl_asyncup_store(&image,(uint64_t)(uintptr_t)generic); // earlier submit publishes last
    assert(image.up_token==(uint64_t)(uintptr_t)generic && g_surfin_npend==2);
    ready[1]=ready[2]=1; nvmtl_vk_image_upload_barrier(&image);
    assert(!image.up_token && !g_surfin_npend && !g_surfin_live && finishes==2);
}
static void failed_wait_case(void) {
    unsigned page_refs, unlocks; Lease lease; init_owner(&lease, &page_refs, &unlocks);
    unsigned char source[4096]; nvk_buffer cache={.buf=(void *)(uintptr_t)0x94,.map=source,.size=sizeof source,.imported=1};
    nvk_image image=make_image(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,1,1);
    const void *backing=image.img;
    assert(!upload(&image,&cache,&lease));
    assert(image.up_token && g_asyncup_pending && g_surfin_npend==1 && lease.refs==2);
    nvmtl_asyncup *quarantined=g_asyncup_pending;
    quarantined->stage=(nvk_buffer){.buf=(void *)(uintptr_t)0x95,.mem=(void *)(uintptr_t)0x96,.size=4096};
    caller_drop_and_cache_evict(&lease,&page_refs);
    assert(lease.refs==1 && page_refs==1 && !unlocks);

    finish_signals=0; ready[1]=0;
    const unsigned before_destroys=destroys;
    assert(nvmtl_vk_image_upload_barrier(&image));
    assert(g_state==-1 && !image.up_token && !g_asyncup_pending && g_surfin_npend==0);
    assert(image.img==backing && quarantined->img==NULL && quarantined->stage.buf==(void *)(uintptr_t)0x95);
    assert(quarantined->lease==&lease && lease.refs==1 && page_refs==1 && !unlocks);
    assert(destroys==before_destroys && g_surfin_live==0);

    // A later consumer sees the down backend and cannot retire the uncertain image or quarantine.
    assert(nvmtl_vk_image_upload_barrier(&image));
    assert(image.img==backing && quarantined->stage.buf==(void *)(uintptr_t)0x95);
    assert(lease.refs==1 && page_refs==1 && !unlocks && destroys==before_destroys && finishes==1);
}
static void submit_publication_gate_case(void) {
    unsigned page_refs, unlocks; Lease lease; init_owner(&lease,&page_refs,&unlocks);
    unsigned char source[4096]; nvk_buffer cache={.buf=(void *)(uintptr_t)0x94,.map=source,.size=sizeof source,.imported=1};
    nvk_image image=make_image(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,1,1);
    UploadArg writer={&image,&cache,&lease,-99};
    pthread_t producer, consumer;
    pthread_mutex_lock(&finish_mutex);
    block_submit_begin=1; submit_entered=submit_release=0;
    block_finish=1; block_finish_id=1; finish_entered=finish_release=0;
    pthread_mutex_unlock(&finish_mutex);
    consumer_image=&image; consumer_rc=-99;
    atomic_store(&consumer_started,0); atomic_store(&consumer_done,0);
    assert(!pthread_create(&producer,NULL,upload_thread,&writer));
    pthread_mutex_lock(&finish_mutex);
    while(!submit_entered) pthread_cond_wait(&finish_cond,&finish_mutex);
    pthread_mutex_unlock(&finish_mutex);
    assert(!image.up_token && !g_asyncup_pending && g_surfin_npend==0);

    assert(!pthread_create(&consumer,NULL,consume_thread,NULL));
    while(!atomic_load(&consumer_started)) usleep(1000);
    usleep(10000); assert(!atomic_load(&consumer_done));
    pthread_mutex_lock(&finish_mutex); submit_release=1; pthread_cond_broadcast(&finish_cond); pthread_mutex_unlock(&finish_mutex);
    assert(!pthread_join(producer,NULL) && writer.rc==0);

    pthread_mutex_lock(&finish_mutex);
    while(!finish_entered) pthread_cond_wait(&finish_cond,&finish_mutex);
    pthread_mutex_unlock(&finish_mutex);
    assert(image.up_token && g_asyncup_pending && g_surfin_npend==1 && !atomic_load(&consumer_done));
    caller_drop_and_cache_evict(&lease,&page_refs);
    assert(lease.refs==1 && page_refs==1 && !unlocks);
    pthread_mutex_lock(&finish_mutex); finish_release=1; pthread_cond_broadcast(&finish_cond); pthread_mutex_unlock(&finish_mutex);
    assert(!pthread_join(consumer,NULL) && atomic_load(&consumer_done) && consumer_rc==0);
    assert(!image.up_token && !g_asyncup_pending && g_surfin_npend==0);
    assert(lease.refs==0 && page_refs==0 && unlocks==1 && g_surfin_live==0);
}
int main(int argc,char **argv) {
    assert(argc==2);
    if (!strcmp(argv[1],"async")) async_case(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,0);
    else if (!strcmp(argv[1],"same")) async_case(VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,0);
    else if (!strcmp(argv[1],"race")) async_case(VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,1);
    else if (!strcmp(argv[1],"bounded")) bounded_case();
    else if (!strcmp(argv[1],"cross-store")) cross_store_case();
    else if (!strcmp(argv[1],"failed-wait")) failed_wait_case();
    else if (!strcmp(argv[1],"submit-publication-gate")) submit_publication_gate_case();
    else if (!strcmp(argv[1],"sync-owner")) sync_case(1,1,1,0);
    else if (!strcmp(argv[1],"sync-no-owner")) sync_case(0,1,1,0);
    else if (!strcmp(argv[1],"ordered-off")) sync_case(0,4,3,1);
    else return 2;
    puts("PASS: imported surface upload ordering and lease lifetime");
}
'''

def build_and_run(directory: Path, code: str, args: list[str], env: dict[str, str], expect_failure: bool = False) -> None:
    source = directory / "probe.c"
    binary = directory / "probe"
    source.write_text(code)
    includes = ["-I" + str(root / "plugin"), "-I" + os.environ.get("VULKAN_HEADERS", str(Path.home() / "Vulkan-Headers/include"))]
    subprocess.run(["xcrun", "clang", "-std=c11", "-pthread", "-fsanitize=address,undefined", "-fno-omit-frame-pointer",
                    *includes, str(source), "-o", str(binary)], check=True)
    run_env = os.environ.copy()
    for key, value in env.items():
        if value is None:
            run_env.pop(key, None)
        else:
            run_env[key] = value
    result = subprocess.run([str(binary), *args], env=run_env, text=True, capture_output=True, timeout=20)
    if expect_failure:
        if result.returncode == 0:
            raise AssertionError("negative control unexpectedly passed")
    else:
        if result.returncode:
            raise AssertionError(f"{result.args} failed ({result.returncode})\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}")
        print(result.stdout, end="")


def harness_for(source: str) -> str:
    gate_start = source.index("// Serialize operations that record or consume work for one image.")
    gate_end = source.index("\nstatic void nvmtl_vk_image_destroy_body", gate_start)
    first = source.index("// NVMTL_SURFIN_ORDERED")
    type_pos = source.index("typedef struct nvmtl_asyncup", first)
    source_ordered = source[first:type_pos]
    tick_pos = source.index("static void nvmtl_surfin_tick_start(void)\n{", type_pos)
    record_pos = source.index("static nvmtl_asyncup *nvmtl_surfin_record_new(void)\n{", tick_pos)
    writer_body_pos = source.index("static int nvmtl_vk_image_write_host_body(", record_pos)
    writer_pos = source.index("int nvmtl_vk_image_write_host(", writer_body_pos)
    writer_end = source.index("\n// A CPU-written IOSurface", writer_pos)
    return """#include <mach/mach.h>\n""" + source[gate_start:gate_end] + source_ordered + source[type_pos:tick_pos] + "\nstatic void nvmtl_surfin_tick_start(void) {}\n" + source[record_pos:writer_body_pos] + source[writer_body_pos:writer_end]


with tempfile.TemporaryDirectory() as td:
    directory = Path(td)
    base = (root / "plugin/nvmtl_vk.c").read_text()
    injected = harness_for(base)
    fixture_prefix, fixture_suffix = fixture.split("typedef struct { unsigned refs;", 1)
    fixture_suffix = "typedef struct { unsigned refs;" + fixture_suffix
    code = fixture_prefix + injected + fixture_suffix
    build_and_run(directory, code, ["async"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["same"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["race"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["bounded"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["cross-store"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["failed-wait"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["submit-publication-gate"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["sync-owner"], {"NVMTL_SURFIN_ASYNC": None, "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["sync-no-owner"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None})
    build_and_run(directory, code, ["ordered-off"], {"NVMTL_SURFIN_ASYNC": "0", "NVMTL_SURFIN_ORDERED": "0"})

    broken = base.replace("VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,\n        (VkImageLayout)img->layout, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL",
                          "0, VK_ACCESS_TRANSFER_WRITE_BIT,\n        VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL", 1)
    broken = broken.replace("pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,\n                          0, 0, NULL, 0, NULL, 1, &barrier);",
                            "pvkCmdPipelineBarrier(c->cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,\n                          0, 0, NULL, 0, NULL, 1, &barrier);", 1)
    build_and_run(directory, fixture_prefix + harness_for(broken) + fixture_suffix,
                  ["async"], {"NVMTL_SURFIN_ASYNC": "1", "NVMTL_SURFIN_ORDERED": None}, expect_failure=True)

    # Compile the actual early guards from destroy/reback/migrate with a failing barrier mock. This verifies that
    # an uncertain fence short-circuits before any backing mutation, and that migrate leaves old_out untouched.
    def guard_prefix(name: str) -> tuple[str, str]:
        start = base.index(name + "(")
        declaration = base.rfind("\n", 0, start) + 1
        brace = base.index("{", start)
        barrier = base.index("if (nvmtl_vk_image_upload_barrier(i))", brace)
        end = base.index("\n", barrier) + 1
        signature = base[declaration:brace].strip()
        return signature, base[brace + 1:end]

    destroy_sig, destroy_prefix = guard_prefix("nvmtl_vk_image_destroy_body")
    reback_sig, reback_prefix = guard_prefix("nvmtl_vk_image_reback_body")
    migrate_sig, migrate_prefix = guard_prefix("nvmtl_vk_image_migrate_body")
    guard_fixture = r'''#include <assert.h>
#include <stdint.h>
#include <mach/mach.h>
#include <vulkan/vulkan_core.h>
#include "nvmtl_vk.h"
static int g_state=1, mutations;
int nvmtl_vk_image_upload_barrier(nvk_image *image) { assert(image); return -1; }
'''
    guard_checks = r'''
int main(void) {
    nvk_image image={.img=(void *)(uintptr_t)0x10,.mem=(void *)(uintptr_t)0x20,
        .view=(void *)(uintptr_t)0x30,.alloc=4096,.sysmem=0,.layout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        .usage=VK_IMAGE_USAGE_TRANSFER_SRC_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT,.samples=1,.mips=1,.layers=1,.w=8,.h=8};
    nvk_image old_out={.img=(void *)(uintptr_t)0x40,.mem=(void *)(uintptr_t)0x50};
    nvk_image before=image, old_before=old_out;
    nvmtl_vk_image_destroy_body(&image);
    assert(!mutations && image.img==before.img && image.mem==before.mem && image.view==before.view);
    assert(nvmtl_vk_image_reback_body(&image,1,0)==-1);
    assert(!mutations && image.img==before.img && image.mem==before.mem && image.view==before.view);
    assert(nvmtl_vk_image_migrate_body(&image,1,0,&old_out)==-1);
    assert(!mutations && image.img==before.img && image.mem==before.mem && image.view==before.view);
    assert(old_out.img==old_before.img && old_out.mem==old_before.mem);
}
'''
    guarded = guard_fixture + "\n" + destroy_sig + " {\n" + destroy_prefix + "mutations++;\n}\n"
    guarded += reback_sig + " {\n" + reback_prefix + "mutations++; return 0;\n}\n"
    guarded += migrate_sig + " {\n" + migrate_prefix + "mutations++; return 0;\n}\n" + guard_checks
    guard_source = directory / "guard.c"
    guard_binary = directory / "guard"
    guard_source.write_text(guarded)
    includes = ["-I" + str(root / "plugin"), "-I" + os.environ.get("VULKAN_HEADERS", str(Path.home() / "Vulkan-Headers/include"))]
    subprocess.run(["xcrun", "clang", "-std=c11", *includes, str(guard_source), "-o", str(guard_binary)], check=True)
    subprocess.run([str(guard_binary)], check=True, timeout=10)
    print("PASS: failed upload barrier prevents destroy/reback/migrate backing changes")

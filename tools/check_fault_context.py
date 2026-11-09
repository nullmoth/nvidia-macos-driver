#!/usr/bin/env python3
"""check_fault_context.py - the fault-context rings (plugin/nvmtl_fault_context.h) are wired into the plugin.

A GPU fault names an address and a semaphore number. The rings turn that into a kernel name, the storage views it had
bound, and the resources that died just before, but only if every entry point records: each dispatch path, the storage
bind, the submit, the image death, the heap-slot death, the texture's label, and both device-loss sites dump. This
lists them with the least count each needs and fails on any short.

    python3 tools/check_fault_context.py [plugin-dir]
"""
import re, sys, os
here = os.path.dirname(os.path.abspath(__file__))
pdir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin")
def read(n):
    try: return open(os.path.join(pdir, n)).read()
    except OSError: return ""
vk = read("nvmtl_vk.c"); objc = read("NVMTLObjects.m"); hdr = read("nvmtl_fault_context.h"); vkh = read("nvmtl_vk.h"); dev = read("NVMTLDevice.m"); emb = read("NVMTLEmbedded.m")
hooks = [
    ("nvmtl_vk.c", r"nvmtl_fx_note_dispatch\(c, p\);\n\s*pvkCmdDispatch(Indirect)?\(", 4),   # threads, indirect tg, groups, indirect
    ("nvmtl_vk.c", r"nvmtl_image_grace_reap\(\);", 1),                                       # NVMTL_IMAGE_GRACE: dead images reaped as submits finish
    ("nvmtl_vk.c", r"c->fx_seq = g_cmd_submitted;", 1),
    ("nvmtl_vk.c", r"fault context: this command buffer's address page", 1),   # the binding-table pages by GPU address at device loss
    ("NVMTLObjects.m", r"nvmtl_vk_dispatch_key\(", 2),                        # both compute dispatch paths carry the kernel hash
    ("NVMTLDevice.m", r"nvlog_every\(\"pipeline key:", 1),                      # never throttled
    ("nvmtl_vk.c", r"nvmtl_heap_binding_refused\(", 4),
    ("nvmtl_vk.c", r"VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, &si, NULL, NULL \}", 1),   # a dropped slot's storage descriptor goes back to the dummy                        # definition + compute + vertex + fragment: a module indexing a heap binding the heap lacks never runs
    ("nvmtl_vk.c", r"nvmtl_fx_bind_storage\(&c->fx", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_submit\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_image_gone\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_slot_gone\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_dump\(\"", 2),                                              # queue submit failed, fence wait failed
    ("NVMTLObjects.m", r"nvmtl_vk_image_dying_label\(_label\.UTF8String\)", 1),
    ("nvmtl_vk.h", r"nvmtl_fx_cb fx;", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_note_buffer_gone\(b, ", 2),                              # every buffer death (destroy + park free) with its GPU range
    ("nvmtl_vk.c", r"nvmtl_pl_stamp\(&pg->life, c->fx_seq\)", 2),                      # every page of a submit (and its prologue) carries the sequence
    ("nvmtl_vk.c", r"nvmtl_page_cool_reap\(\);", 1),                                   # cooling pages released as submits finish
    ("nvmtl_vk.c", r"nvmtl_pl_release\(&g_pl, &page->life, finished, keep > 0\)", 1),  # the owner's release asks the page, not the witness alone
    ("nvmtl_vk.c", r"nvmtl_page_addresses_log\(\);", 1),                               # exact page ranges at device loss
    ("nvmtl_vk.c", r"nvmtl_live_add\(out, ", 3),                               # own/pooled, imported, placed buffers enter the live registry
    ("nvmtl_vk.c", r"nvmtl_live_remove\(b\);", 2),                                      # destroy + park free leave it
    ("nvmtl_vk.c", r"nvmtl_live_relabel\(&page->buffer, NVMTL_LV_PAGE\)", 1),
    ("nvmtl_vk.c", r"nvmtl_fault_file_write\(c\);", 1),                                 # the fault file at device loss
    ("nvmtl_vk.c", r"nvmtl_buffer_grace_reap\(\);", 1),                                  # freed buffers destroyed only after their submits complete
    ("nvmtl_vk.c", r"nvmtl_live_relabel\(b, NVMTL_LV_DYING\)", 1),
    ("nvmtl_vk.c", r"root dispatch %u b%u \+%u", 1),                                      # the pointers inside a root buffer, resolved
    ("nvmtl_vk.c", r"nvmtl_fx_copy_note\(&g_fx", 1),                                     # the copy ring
    ("nvmtl_vk.c", r"nvmtl_fx_writers\(&g_fx", 1),                                       # the writer census per root at loss
    ("nvmtl_vk.c", r"nvmtl_fx_barriers_after\(&g_fx", 1),                                # the barriers between the last dispatches at loss
    ("nvmtl_vk.c", r"t_fx_cb = out->cb; t_fx_ord = 0;", 1),                               # a begun cb is the thread's current one
    ("nvmtl_vk.c", r"nvmtl_vk_heap_zero\(&", 2),                                         # pool blocks (mapped under the lock, VRAM after it)
    ("NVMTLObjects.m", r"nvmtl_vk_heap_zero\(&h->_hm", 1),                               # every app heap, cleared once on creation
    ("nvmtl_vk.c", r"nvmtl_lv_remove\(&g_lv, b->reg, g_cmd_submitted\)", 1),            # provenance keeps the death sequence
    ("nvmtl_vk.c", r"nvmtl_im_died\(&g_im, ", 3),                                       # image death: destroy, free_backing, re-back
    ("nvmtl_vk.c", r"nvmtl_im_add\(&g_im, ", 2),                                        # image birth: commit, re-back
    ("nvmtl_vk.c", r"nvmtl_vw_add(_ex)?\(&g_vw, ", 6),                                        # every heap-bound view pairs with its image
    ("nvmtl_vk.c", r"nvmtl_slots_write\(slot, 2, ", 2),                                 # binding 2: put_storage + the drop's dummy
    ("nvmtl_vk.c", r"nvmtl_slots_alloc\(slot\)", 2),                                   # put + put_texel
    ("nvmtl_vk.c", r"nvmtl_slots_drop\(slot\)", 1),
    ("nvmtl_vk.c", r"nvmtl_fault_slot_report\(q\[k\], f\)", 2),                     # the slot a pointee names, from a host view and via a copy
    ("nvmtl_vk.c", r"nvmtl_sl_census\(&g_sl", 1),
    ("nvmtl_vk.c", r"NVMTL_HEAP_PREFILL", 2),
    ("nvmtl_vk.c", r"nvmtl_slot_drop_reap\(\);", 1),                                    # a slot drop waits for the submits in flight at the texture's death
    ("nvmtl_vk.c", r"holds handle 0x%016llx: bit 62", 1),                                # the full gpuResourceID the kernel masked
    ("nvmtl_vk.c", r"nvmtl_dwit_mark\(c, c->fx.total\);", 4),                           # a witness word after every compute dispatch form
    ("nvmtl_vk.c", r"nvmtl_dwit_mark\(c, 0x8000u", 1),                                   # and at every render pass end
    ("nvmtl_vk.c", r"out->begin_id = \(uint32_t\)g_cmd_begun;", 1),
    ("nvmtl_vk.c", r"nvmtl_dwit_last\(c\)", 1),                                        # the faulting dispatch singled out at loss
    ("nvmtl_vk.c", r"nvmtl_sl_write\(&g_sl, k, 2, nvmtl_heap_storage_dummy\(\), 0\)", 1),   # the prefill is recorded (the heap's storage dummy)
    ("NVMTLObjects.m", r"nvmtl_vk_image_parked\(img\)", 2),                            # both parks
    ("NVMTLObjects.m", r"nvmtl_vk_image_recycled\(out\)", 2),                          # both unparks
    ("NVMTLEmbedded.m", r"VIEW NVMTLTexture %p", 1),
    ("nvmtl_vk.c", r"nvmtl_loss_mutex_lock\(&g_", 14),                                   # every lock in the loss path is bounded
    ("nvmtl_vk.c", r"nvmtl_loss_read\(", 5),                                             # every host read in the loss path goes through the kernel
    ("nvmtl_vk.c", r"nvmtl_loss_section\(f, ", 11),                                       # flushed, announced sections
    ("nvmtl_vk.c", r"a second device-loss dump on another thread - skipped", 1),
    ("nvmtl_vk.c", r"#define pvkAllocateMemory\(d, i, a, o\) nvmtl_mem_alloc_rec", 1),   # every memory allocation recorded with its base address
    ("nvmtl_vk.c", r"#define pvkFreeMemory\(d, m, a\) nvmtl_mem_free_rec", 1),
    ("nvmtl_vk.c", r"#define pvkBindImageMemory\(d, img, m, o\) nvmtl_mem_bind_image_rec", 1),
    ("nvmtl_vk.c", r"nvmtl_mb_find\(&g_mb, image->img, &bmem, &boff\)", 1),            # image records carry their real bind
    ("nvmtl_vk.c", r"nvmtl_mm_dump\(&g_mm, f\)", 1),
    ("nvmtl_vk.c", r"nvmtl_loss_section\(f, \"memories\"\)", 1),
    ("nvmtl_vk.c", r"nvmtl_heaps_dump\(f, nvmtl_fx_emit_both\)", 1),                   # every live app heap at loss
    ("NVMTLObjects.m", r"nvmtl_heap_list_for_loss\(h\)", 1),
    ("nvmtl_vk.c", r"image->placed \? image->hz_off", 1),                               # placed images record their bound range
    ("nvmtl_vk.c", r"nvmtl_im_set_params\(&g_im, id, ", 1),                            # image requirement vs the memory its bind gave it
    ("nvmtl_vk.c", r"nvmtl_vw_add_ex\(&g_vw, ", 5),                                     # every view records what it covers
    ("nvmtl_vk.c", r"nvmtl_fault_tic_scan\(v2, ", 1),                                   # the storage descriptor's words and address
    ("nvmtl_vk.c", r"nvmtl_nvk_layout_ok\(\)", 1),                                    # the texture-header reader runs only on the measured ICD build
    ("nvmtl_vk.c", r"nvmtl_fault_nvk_slot\(slot, 2, v2, ", 1),
    ("nvmtl_vk.c", r"STALE INDEX - the set names another header", 1),
    ("nvmtl_vk.c", r"nvmtl_vw_tic_note\(", 6),                                          # every recorded view carries its indices and header addresses (5 creations + the definition)
    ("nvmtl_vk.c", r"nvmtl_vw_note_died\(", 4),                                         # the grace's lone view, an image's own view, free_backing (+ definition)
    ("nvmtl_vk.c", r"nvmtl_vw_index_history\(&g_vw, &g_im, ti, v2", 1),                # the stale-header history at loss
    ("nvmtl_vk.c", r"nvmtl_loss_section\(f, \"views\"\)", 1),
    ("nvmtl_vk.c", r"nvmtl_tabshadow_tick\(\);", 1),                                   # the GPU-side table shadow runs from submit_finish
    ("nvmtl_vk.c", r"nvmtl_tabshadow_census\(f\);", 1),
    ("nvmtl_vk.c", r"GPU-side shadow of table\[%u\]", 1),
    ("nvmtl_vk.c", r"NVK image table %s: alloc %u \(was %u\)", 1),                       # every table growth logged with the bind path named
    ("nvmtl_vk.c", r"nvmtl_setshadow_element\(binding, slot, &ge\)", 1),              # the GPU's copy of the set element beside the host's
    ("nvmtl_vk.c", r"heap set binding %u census", 1),
    ("nvmtl_vk.c", r"nvmtl_heap_write_fence\(\);", 4),                                  # every heap descriptor write site (knob)
    ("nvmtl_vk.c", r"was encoded against heap set %p", 1),
    ("nvmtl_vk.c", r"BINDLESS heap layout binding %u: type", 1),
    ("nvmtl_vk.c", r"queue\x27s programmed header pool: %u entries", 1),              # the pool limit the queue programmed vs the index used
    ("nvmtl_vk.c", r"heap set bounds: GPU 0x%llx, %u bytes", 1),
    ("nvmtl_vk.c", r"TARGET SUMMARY: slot %u -> binding 2 index %u", 1),
    ("nvmtl_vk.c", r"per 4096-slot band \(differing/non-zero\)", 1),
    ("nvmtl_vk.c", r"nvmtl_loss_section\(f, \"images\"\)", 1),                        # images early, census last
]
src = {"nvmtl_vk.c": vk, "NVMTLObjects.m": objc, "nvmtl_vk.h": vkh, "NVMTLDevice.m": dev, "NVMTLEmbedded.m": emb}
short = []; total = 0
for f, pat, least in hooks:
    n = len(re.findall(pat, src[f])); total += n
    if n < least: short.append(f"{f}: {pat} found {n}, needs {least}")
if not hdr: short.append("plugin/nvmtl_fault_context.h is missing")
print(f"fault context: {len(hooks)} entry points, {total} sites, {len(short)} short")
for s in short: print("SHORT", s)
sys.exit(1 if short else 0)

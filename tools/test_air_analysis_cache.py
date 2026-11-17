#!/usr/bin/env python3
"""Check repeated AIR analysis and conservative fallbacks using production code."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'plugin/NVMTLLibraryLoad.m').read_text()
objects = (ROOT / 'plugin/NVMTLObjects.m').read_text()


def section(start, end):
    first = source.index(start)
    return source[first:source.index(end, first)]


fixture = r'''
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <assert.h>
#include <stdatomic.h>
#include <CommonCrypto/CommonDigest.h>
@interface NVMTLFunction : NSObject { @public NSString *_air, *_fname; } @end
@implementation NVMTLFunction @end
static _Atomic unsigned queries, scans;
static BOOL ready = YES;
static int query_mode;
static BOOL nvmtl_xlate_ready(void) { return ready; }
static void xlate_free(void *bytes, size_t length) { (void)length; free(bytes); }
static int query(const char *air, uint8_t **out, size_t *length, char *err, size_t capacity) {
    (void)err; (void)capacity;
    atomic_fetch_add(&queries, 1);
    if (query_mode == 1) return -1;
    const char *json = query_mode == 2 ? "{}" : strstr(air, ".MTL_VISIBLE_FN_REF") ? "[\"helper\"]" : "[]";
    *length = strlen(json); *out = (uint8_t *)strdup(json); return 0;
}
static int (*xlate_vrefs)(const char *, uint8_t **, size_t *, char *, size_t) = query;
@@MARKERS@@
#define nvmtl_air_grid_only_scan instrumented_grid_scan
@@GRID@@
#undef nvmtl_air_grid_only_scan
static BOOL nvmtl_air_grid_only_scan(NSString *air, NSString *name) {
    atomic_fetch_add(&scans, 1); return instrumented_grid_scan(air, name);
}
@@GRID_WRAPPER@@
@@LINK@@
static _Atomic unsigned hashes;
static unsigned char *counted_hash(const void *data, CC_LONG len, unsigned char *md) {
    atomic_fetch_add(&hashes,1); return CC_SHA256(data,len,md);
}
#define CC_SHA256 counted_hash
@@AIR_KEY@@
#undef CC_SHA256
@@CONSTANTS@@
static NSString *air_text(NSString *prefix, BOOL mark) {
    NSString *padding = [@"; padding\n" stringByPaddingToLength:8192 withString:@"; padding\n" startingAtIndex:0];
    NSString *text = [prefix stringByAppendingString:padding];
    return mark ? nvmtl_air_text_made(text) : text;
}
int main(void) { @autoreleasepool {
    NSString *metadata = @"!0 = !{ptr @grid, !1, !2}\n!1 = !{}\n!2 = !{!3}\n"
                          "!3 = !{i32 0, !\"air.thread_position_in_grid\"}\n"
                          "!4 = !{ptr @grouped, !1, !5}\n!5 = !{!6}\n"
                          "!6 = !{i32 0, !\"air.thread_position_in_threadgroup\"}\n";
    NSString *air = air_text(metadata, YES);
    unsigned before = scans;
    for (unsigned i = 0; i < 1000; i++) {
        NVMTLFunction *fn = [NVMTLFunction new]; fn->_air = air; fn->_fname = @"grid";
        assert(nvmtl_air_grid_only(fn));
        fn->_fname = @"grouped"; assert(!nvmtl_air_grid_only(fn));
    }
    assert(scans == before + 2); // new functions share analyses, with separate entries
    for (unsigned i = 0; i < 1000; i++) assert(!nvmtl_needs_visible_link(air, @"visible function reference"));
    assert(queries == 1); // a successful negative query overrides the fallback text

    NSString *table = air_text(@"declare void @air.get_function_pointer_visible_function_table()\n", YES);
    NSString *visible = air_text(@"declare void @helper.MTL_VISIBLE_FN_REF()\n", YES);
    for (unsigned i = 0; i < 1000; i++) {
        assert(nvmtl_needs_visible_link(table, nil));
        assert(nvmtl_needs_visible_link(visible, nil));
    }
    assert(queries == 3);
    for (NSString *form in @[@"@air.get_function_pointer_visible_function_table",
             @"@air.get_size_visible_function_table", @"@air.is_null_visible_function_table",
             @"@air.get_null_intersection_function_table", @"@air.set_buffer_intersection_function_table",
             @"@air.set_intersection_function_table", @"!\"air.visible_function_table\"",
             @"!\"air.intersection_function_table\""]) assert(nvmtl_air_reads_function_table(form));
    assert(!nvmtl_air_reads_function_table(@"function_table"));
    assert(!nvmtl_air_reads_function_table(@"@air.get_size_visible_function_tablé"));
    assert(!nvmtl_air_reads_function_table(nil));
    NSString *failed = air_text(@"; query can recover\n", YES);
    query_mode = 1;
    assert(nvmtl_needs_visible_link(failed, @"no linked function supplies it"));
    assert(!nvmtl_needs_visible_link(failed, nil));
    query_mode = 2;
    assert(nvmtl_needs_visible_link(failed, @"Metal visible function table"));
    query_mode = 0;
    assert(!nvmtl_needs_visible_link(failed, nil));
    assert(queries == 7);
    ready = NO;
    assert(nvmtl_needs_visible_link(air, @".MTL_UNRESOLVED_VISIBLE_FN"));
    assert(!nvmtl_needs_visible_link(air, nil));
    ready = YES;
    xlate_vrefs = NULL;
    assert(nvmtl_needs_visible_link(air, @"visible function reference"));
    xlate_vrefs = query;

    // External/mutable text is unmarked: edits must change the next answer.
    NSMutableString *mutable = [air_text(metadata, NO) mutableCopy];
    NVMTLFunction *fn = [NVMTLFunction new]; fn->_air = mutable; fn->_fname = @"grid";
    assert(nvmtl_air_grid_only(fn)); assert(!nvmtl_needs_visible_link(mutable, nil));
    [mutable appendString:@"\n; addrspace(3)\ndeclare void @helper.MTL_VISIBLE_FN_REF()\n"];
    assert(!nvmtl_air_grid_only(fn)); assert(nvmtl_needs_visible_link(mutable, nil));
    assert(queries == 9);
    assert(!nvmtl_air_grid_only(nil));
    fn->_air = nil; assert(!nvmtl_air_grid_only(fn));
    assert(!nvmtl_needs_visible_link(nil, nil));

    fn->_air = air;
    for (unsigned i = 0; i < 40; i++) {
        fn->_fname = [NSString stringWithFormat:@"unknown%u", i];
        assert(!nvmtl_air_grid_only(fn));
    }
    NSDictionary *entries = objc_getAssociatedObject(air, NVMTL_AIRGRID_KEY);
    assert(entries.count == 32);
    fn->_fname = @"grid"; before = scans; assert(nvmtl_air_grid_only(fn)); assert(scans == before);
    fn->_fname = @"unknown39"; assert(!nvmtl_air_grid_only(fn)); assert(scans == before + 1);

    // Independent immutable objects cannot share an answer. Parallel readers
    // see complete snapshots of the per-entry map while it is populated.
    NSString *other = air_text([metadata stringByAppendingString:@"; addrspace(3)\n"], YES);
    dispatch_apply(64, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^(size_t i) {
        @autoreleasepool {
            NVMTLFunction *f = [NVMTLFunction new];
            f->_air = i % 2 ? air : other; f->_fname = @"grid";
            assert(nvmtl_air_grid_only(f) == (i % 2 != 0));
            assert(!nvmtl_needs_visible_link(other, nil));
        }
    });
    // A diagnostic key hashes a shared module once across new functions.
    NSString *constants=air_text(@"!1 = !{ptr @f.MTL_FC_INIT_a, !\"float\", !\"alpha\", i32 7, i1 true}\n"
                                 "!2 = !{ptr @f.MTL_FC_INIT_unrecognized}\n",YES);
    uint64_t key=0; NSArray *constantRows=nil;
    for(unsigned i=0;i<1000;i++) {
        NVMTLFunction *f=[NVMTLFunction new];f->_air=constants;
        uint64_t next=nvmtl_function_air_key(f); assert(next && (!key || key==next)); key=next;
        NSUInteger count=0;NSArray *rows=nvmtl_air_function_constants(constants,&count);
        assert(count==2 && rows.count==1 && [rows[0][@"name"] isEqualToString:@"alpha"] && [rows[0][@"required"] boolValue]);
        assert(!constantRows || rows==constantRows);constantRows=rows;
    }
    assert(hashes==1);
    NSUInteger n=99;assert(!nvmtl_air_function_constants(nil,&n).count && !n);
    NSArray *emptyRows=nvmtl_air_function_constants(air,&n);
    assert(!emptyRows.count && !n && emptyRows==nvmtl_air_function_constants(air,NULL));
    NSMutableString *editable=[constants mutableCopy];NVMTLFunction *edit=[NVMTLFunction new];edit->_air=editable;
    uint64_t oldKey=nvmtl_function_air_key(edit);
    assert(nvmtl_air_function_constants(editable,&n).count==1 && n==2);
    [editable appendString:@"\n!3 = !{ptr @f.MTL_FC_INIT_b, !\"int\", !\"beta\", i32 8, i1 false}\n"];
    assert(nvmtl_air_function_constants(editable,&n).count==2 && n==3 && nvmtl_function_air_key(edit)!=oldKey);
    dispatch_apply(64,dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^(size_t i) { @autoreleasepool {
        (void)i;
        NVMTLFunction *f=[NVMTLFunction new];f->_air=constants;
        NSUInteger rows=0;assert(nvmtl_function_air_key(f)==key);
        assert(nvmtl_air_function_constants(constants,&rows)==constantRows && rows==2);
    }});
    assert(!nvmtl_function_air_key(nil));
    // Linker/lowerer output must retain the same immutable-text contract as
    // decoded AIR. Repeated dispatches used to rehash all these bytes each time.
    NSMutableData *compiled = [[constants dataUsingEncoding:NSUTF8StringEncoding] mutableCopy];
    NSString *linked = nvmtl_air_text_from_bytes(compiled.bytes, compiled.length);
    assert([linked isEqualToString:constants]);
    memset(compiled.mutableBytes, 'x', compiled.length); // the text owns a copy
    NVMTLFunction *linkedFn = [NVMTLFunction new]; linkedFn->_air = linked;
    before = hashes;
    for (unsigned i = 0; i < 1000; i++) {
        assert(nvmtl_function_air_key(linkedFn) == key);
        NVMTLFunction *fresh = [NVMTLFunction new]; fresh->_air = linked;
        assert(nvmtl_function_air_key(fresh) == key);
    }
    assert(hashes == before + 1);
    NSString *different = [constants stringByAppendingString:@"; different link\n"];
    NSData *bytes = [different dataUsingEncoding:NSUTF8StringEncoding];
    NVMTLFunction *otherLink = [NVMTLFunction new];
    otherLink->_air = nvmtl_air_text_from_bytes(bytes.bytes, bytes.length);
    assert(nvmtl_function_air_key(otherLink) != key);
    // A mutable copy of trusted text is external again; edits stay visible.
    editable = [linked mutableCopy]; edit = [NVMTLFunction new]; edit->_air = editable;
    oldKey = nvmtl_function_air_key(edit);
    [editable appendString:@"; mutable edit\n"];
    assert(nvmtl_function_air_key(edit) != oldKey);
    assert(!nvmtl_air_text_from_bytes(NULL, 0));
    const unsigned char invalid[] = {0xff};
    assert(!nvmtl_air_text_from_bytes(invalid, sizeof invalid));
    puts("PASS: decoded/linked immutable AIR, literal table searches, digest/constant reuse, row accounting, mutable inputs, failures/recovery, bounded storage and concurrent readers");
} }
'''
markers = section('static const void *NVMTL_AIRTEXT_KEY', '// newFunctionWithName scanned')
grid = section('static NSArray<NSString *> *nvmtl_air_tuple_tokens(', 'BOOL nvmtl_air_grid_only(')
wrapper = section('BOOL nvmtl_air_grid_only(', '// One `air.struct_type_info`')
link = section('BOOL nvmtl_air_reads_function_table(', '// A link that failed')
air_key=objects[objects.index('uint64_t nvmtl_function_air_key('):objects.index('static void nvmtl_heap_register(')]
start=objects.index('static NSArray<NSDictionary *> *nvmtl_air_function_constants(')
constants=objects[start:objects.index('- (NSDictionary<NSString *, MTLFunctionConstant *> *)functionConstantsDictionary',start)]
for key, value in [('MARKERS', markers), ('GRID', grid), ('GRID_WRAPPER', wrapper), ('LINK', link),
                   ('AIR_KEY',air_key),('CONSTANTS',constants)]:
    fixture = fixture.replace('@@' + key + '@@', value)
with tempfile.TemporaryDirectory(prefix='nvmtl-air-analysis-') as directory:
    p = Path(directory)
    (p / 'test.m').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O2', '-Wall', '-Wextra', '-Werror', '-Wno-deprecated-declarations',
                    '-fsanitize=address,undefined', '-fno-sanitize-recover=all', str(p / 'test.m'),
                    '-framework', 'Foundation', '-o', str(p / 'test')], check=True)
    subprocess.run([str(p / 'test')], check=True, timeout=30)

/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// nvmtl_canon.h - link a kernel with the visible functions it calls, inside LLVM, and reduce the result to what the
// translator needs: one kernel, its function constants folded, every helper inlined, every aggregate scalarised.
// This is what Apple's stitching pipeline does before code generation, and it replaces the textual linker for the
// direct-reference case (a kernel's `@<name>.MTL_VISIBLE_FN_REF` declarations). Visible-function tables keep their
// own path.
//
// Steps, in order:
//   1. parse the kernel and each module that defines a referenced function, and link the modules into the kernel;
//   2. bind each `.MTL_VISIBLE_FN_REF` declaration to its definition and make the definition internal. A stitched
//      function can take fewer parameters than the kernel's declaration passes: a stitching graph cannot name an
//      input it never uses, so the stitcher drops trailing inputs that no node reads (MPS's
//      reductionStitched::write_fn is declared with three, stitched with two). The calls are rebuilt with the leading
//      arguments the definition takes, which is all it can read; any other difference is left unbound;
//   3. fold each known function constant into its `air.fc_initializer` global;
//   4. mark every defined function always-inline and run the cleanup pipeline below, with the target triple
//      cleared for the run (the pass builder refuses the air64 architecture) and restored after;
//   5. verify the module and print it.
// Measured on 42 MPS kernel variants: the matrix-vector kernels go from 13-19 MB of linked text to about 165 KB,
// each run takes at most 1.4 s, and the generic address space no longer reaches the translator.
//
// Plain C over the LLVM C API, loaded from a libLLVM handle, so the same code runs in the plugin and in an offline
// replay tool.
#pragma once
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define NVMTL_CANON_PIPELINE \
    "globalopt,ipsccp,globaldce,function(simplifycfg),always-inline,function(sroa,early-cse<memssa>,simplifycfg,sroa),globaldce"

typedef void *nvmtl_canon_ref;
typedef struct {
    nvmtl_canon_ref (*ContextCreate)(void);
    void (*ContextDispose)(nvmtl_canon_ref);
    void (*ContextSetDiagnosticHandler)(nvmtl_canon_ref, void (*)(nvmtl_canon_ref, void *), void *);
    char *(*GetDiagInfoDescription)(nvmtl_canon_ref);
    nvmtl_canon_ref (*CreateMemoryBufferWithMemoryRangeCopy)(const char *, size_t, const char *);
    int (*ParseIRInContext)(nvmtl_canon_ref, nvmtl_canon_ref, nvmtl_canon_ref *, char **);
    int (*LinkModules2)(nvmtl_canon_ref, nvmtl_canon_ref);
    void (*DisposeModule)(nvmtl_canon_ref);
    void (*DisposeMessage)(char *);
    nvmtl_canon_ref (*GetFirstFunction)(nvmtl_canon_ref); nvmtl_canon_ref (*GetNextFunction)(nvmtl_canon_ref);
    nvmtl_canon_ref (*GetFirstGlobal)(nvmtl_canon_ref); nvmtl_canon_ref (*GetNextGlobal)(nvmtl_canon_ref);
    const char *(*GetValueName2)(nvmtl_canon_ref, size_t *);
    int (*IsDeclaration)(nvmtl_canon_ref);
    const char *(*GetSection)(nvmtl_canon_ref); void (*SetSection)(nvmtl_canon_ref, const char *);
    nvmtl_canon_ref (*GetNamedFunction)(nvmtl_canon_ref, const char *);
    void (*ReplaceAllUsesWith)(nvmtl_canon_ref, nvmtl_canon_ref);
    void (*DeleteFunction)(nvmtl_canon_ref);
    nvmtl_canon_ref (*GlobalGetValueType)(nvmtl_canon_ref);
    int (*GetTypeKind)(nvmtl_canon_ref); unsigned (*GetIntTypeWidth)(nvmtl_canon_ref);
    nvmtl_canon_ref (*ConstInt)(nvmtl_canon_ref, unsigned long long, int); nvmtl_canon_ref (*ConstReal)(nvmtl_canon_ref, double);
    void (*SetInitializer)(nvmtl_canon_ref, nvmtl_canon_ref); void (*SetExternallyInitialized)(nvmtl_canon_ref, int);
    void (*SetGlobalConstant)(nvmtl_canon_ref, int);
    unsigned (*GetEnumAttributeKindForName)(const char *, size_t);
    nvmtl_canon_ref (*CreateEnumAttribute)(nvmtl_canon_ref, unsigned, uint64_t);
    void (*AddAttributeAtIndex)(nvmtl_canon_ref, unsigned, nvmtl_canon_ref);
    nvmtl_canon_ref (*GetEnumAttributeAtIndex)(nvmtl_canon_ref, unsigned, unsigned);
    void (*RemoveEnumAttributeAtIndex)(nvmtl_canon_ref, unsigned, unsigned);
    const char *(*GetTarget)(nvmtl_canon_ref); void (*SetTarget)(nvmtl_canon_ref, const char *);
    nvmtl_canon_ref (*CreatePassBuilderOptions)(void); void (*DisposePassBuilderOptions)(nvmtl_canon_ref);
    nvmtl_canon_ref (*RunPasses)(nvmtl_canon_ref, const char *, nvmtl_canon_ref, nvmtl_canon_ref);
    char *(*GetErrorMessage)(nvmtl_canon_ref); void (*DisposeErrorMessage)(char *);
    int (*VerifyModule)(nvmtl_canon_ref, int, char **);
    char *(*PrintModuleToString)(nvmtl_canon_ref);
    void (*SetLinkage)(nvmtl_canon_ref, int);
    unsigned (*CountParamTypes)(nvmtl_canon_ref); void (*GetParamTypes)(nvmtl_canon_ref, nvmtl_canon_ref *);
    nvmtl_canon_ref (*GetReturnType)(nvmtl_canon_ref);
    nvmtl_canon_ref (*GetFirstUse)(nvmtl_canon_ref); nvmtl_canon_ref (*GetNextUse)(nvmtl_canon_ref);
    nvmtl_canon_ref (*GetUser)(nvmtl_canon_ref);
    nvmtl_canon_ref (*IsACallInst)(nvmtl_canon_ref); nvmtl_canon_ref (*GetCalledValue)(nvmtl_canon_ref);
    nvmtl_canon_ref (*GetOperand)(nvmtl_canon_ref, unsigned); nvmtl_canon_ref (*TypeOf)(nvmtl_canon_ref);
    nvmtl_canon_ref (*CreateBuilderInContext)(nvmtl_canon_ref); void (*DisposeBuilder)(nvmtl_canon_ref);
    void (*PositionBuilderBefore)(nvmtl_canon_ref, nvmtl_canon_ref);
    nvmtl_canon_ref (*BuildCall2)(nvmtl_canon_ref, nvmtl_canon_ref, nvmtl_canon_ref, nvmtl_canon_ref *, unsigned, const char *);
    void (*InstructionEraseFromParent)(nvmtl_canon_ref);
} nvmtl_canon_api;

// Fills `a` from an open libLLVM. 0 on success; otherwise the name of the first missing symbol.
static const char *nvmtl_canon_api_load(nvmtl_canon_api *a, void *lib)
{
    static const char *names[] = {
        "LLVMContextCreate", "LLVMContextDispose", "LLVMContextSetDiagnosticHandler", "LLVMGetDiagInfoDescription",
        "LLVMCreateMemoryBufferWithMemoryRangeCopy", "LLVMParseIRInContext", "LLVMLinkModules2", "LLVMDisposeModule",
        "LLVMDisposeMessage", "LLVMGetFirstFunction", "LLVMGetNextFunction", "LLVMGetFirstGlobal", "LLVMGetNextGlobal",
        "LLVMGetValueName2", "LLVMIsDeclaration", "LLVMGetSection", "LLVMSetSection", "LLVMGetNamedFunction",
        "LLVMReplaceAllUsesWith", "LLVMDeleteFunction", "LLVMGlobalGetValueType", "LLVMGetTypeKind", "LLVMGetIntTypeWidth",
        "LLVMConstInt", "LLVMConstReal", "LLVMSetInitializer", "LLVMSetExternallyInitialized", "LLVMSetGlobalConstant",
        "LLVMGetEnumAttributeKindForName", "LLVMCreateEnumAttribute", "LLVMAddAttributeAtIndex",
        "LLVMGetEnumAttributeAtIndex", "LLVMRemoveEnumAttributeAtIndex", "LLVMGetTarget", "LLVMSetTarget",
        "LLVMCreatePassBuilderOptions", "LLVMDisposePassBuilderOptions", "LLVMRunPasses", "LLVMGetErrorMessage",
        "LLVMDisposeErrorMessage", "LLVMVerifyModule", "LLVMPrintModuleToString", "LLVMSetLinkage", "LLVMCountParamTypes",
        "LLVMGetParamTypes", "LLVMGetReturnType", "LLVMGetFirstUse", "LLVMGetNextUse", "LLVMGetUser", "LLVMIsACallInst",
        "LLVMGetCalledValue", "LLVMGetOperand", "LLVMTypeOf", "LLVMCreateBuilderInContext", "LLVMDisposeBuilder",
        "LLVMPositionBuilderBefore", "LLVMBuildCall2", "LLVMInstructionEraseFromParent",
    };
    _Static_assert(sizeof names / sizeof *names == sizeof(nvmtl_canon_api) / sizeof(void *), "one name per entry point");
    void **slot = (void **)a;
    for (size_t k = 0; k < sizeof names / sizeof *names; k++)
        if (!(slot[k] = dlsym(lib, names[k]))) return names[k];
    return NULL;
}

typedef struct { uint32_t index, size; const uint8_t *bytes; } nvmtl_canon_fc;
typedef struct {
    unsigned linked, bound, adapted, unbound, folded, unfolded, inlined;
    double ms;
} nvmtl_canon_stats;

typedef struct { char *buf; size_t n; const nvmtl_canon_api *a; } nvmtl_canon_diag;
static void nvmtl_canon_on_diag(nvmtl_canon_ref di, void *ctx);

static void nvmtl_canon_why(char *why, size_t whyn, const char *fmt, const char *a, const char *b)
{
    if (why && whyn) snprintf(why, whyn, fmt, a ? a : "", b ? b : "");
}

// The leading parameter types of `def` equal the first ones `decl` passes, and both return the same type.
static int nvmtl_canon_prefix_compatible(const nvmtl_canon_api *a, nvmtl_canon_ref declTy, nvmtl_canon_ref defTy)
{
    const unsigned want = a->CountParamTypes(declTy), have = a->CountParamTypes(defTy);
    if (have > want || have > 64 || a->GetReturnType(declTy) != a->GetReturnType(defTy)) return 0;
    nvmtl_canon_ref dp[64], fp[64];
    if (want > 64) return 0;
    a->GetParamTypes(declTy, dp); a->GetParamTypes(defTy, fp);
    for (unsigned i = 0; i < have; i++) if (dp[i] != fp[i]) return 0;
    return 1;
}

// Rebuilds every call of `decl` as a call of `def` with the leading arguments `def` takes. 0 when every use was a
// direct call it could rebuild.
static int nvmtl_canon_adapt_calls(const nvmtl_canon_api *a, nvmtl_canon_ref ctx, nvmtl_canon_ref decl, nvmtl_canon_ref def)
{
    nvmtl_canon_ref defTy = a->GlobalGetValueType(def);
    const unsigned have = a->CountParamTypes(defTy);
    size_t cap = 16, n = 0; nvmtl_canon_ref *calls = (nvmtl_canon_ref *)malloc(cap * sizeof *calls);
    if (!calls) return -1;
    for (nvmtl_canon_ref u = a->GetFirstUse(decl); u; u = a->GetNextUse(u)) {
        nvmtl_canon_ref user = a->GetUser(u);
        if (!a->IsACallInst(user) || a->GetCalledValue(user) != decl) continue;
        if (n == cap) { nvmtl_canon_ref *g = (nvmtl_canon_ref *)realloc(calls, (cap *= 2) * sizeof *g); if (!g) { free(calls); return -1; } calls = g; }
        calls[n++] = user;
    }
    nvmtl_canon_ref b = a->CreateBuilderInContext(ctx);
    for (size_t i = 0; i < n; i++) {
        nvmtl_canon_ref args[64];
        for (unsigned k = 0; k < have; k++) args[k] = a->GetOperand(calls[i], k);
        a->PositionBuilderBefore(b, calls[i]);
        nvmtl_canon_ref nc = a->BuildCall2(b, defTy, def, args, have, "");
        a->ReplaceAllUsesWith(calls[i], nc);
        a->InstructionEraseFromParent(calls[i]);
    }
    a->DisposeBuilder(b);
    free(calls);
    return 0;
}

static nvmtl_canon_ref nvmtl_canon_parse(const nvmtl_canon_api *a, nvmtl_canon_ref ctx, const char *text, size_t len,
                                         const char *what, char *why, size_t whyn)
{
    nvmtl_canon_ref buf = a->CreateMemoryBufferWithMemoryRangeCopy(text, len, what);   // the parser takes it over
    nvmtl_canon_ref m = NULL; char *e = NULL;
    if (!buf || a->ParseIRInContext(ctx, buf, &m, &e) || !m) {
        nvmtl_canon_why(why, whyn, "parse %s: %.300s", what, e ? e : "failed");
        if (e) a->DisposeMessage(e);
        return NULL;
    }
    return m;
}

// Returns the canonical module as text (free it with a->DisposeMessage), or NULL with the reason in `why`.
// `mods` holds one module text per distinct defining module; `fc` the function constant values the pipeline sets.
static char *nvmtl_canon_run(const nvmtl_canon_api *a, const char *kernel, size_t klen,
                             const char *const *mods, const size_t *mlen, size_t nmods,
                             const nvmtl_canon_fc *fc, size_t nfc, nvmtl_canon_stats *st, char *why, size_t whyn)
{
    struct timespec t0, t1; clock_gettime(CLOCK_MONOTONIC, &t0);
    nvmtl_canon_stats s; memset(&s, 0, sizeof s);
    if (why && whyn) why[0] = 0;
    char diagbuf[512] = { 0 }; nvmtl_canon_diag diag = { diagbuf, sizeof diagbuf, a };
    nvmtl_canon_ref ctx = a->ContextCreate();
    if (!ctx) { nvmtl_canon_why(why, whyn, "no LLVM context%s%s", NULL, NULL); return NULL; }
    a->ContextSetDiagnosticHandler(ctx, nvmtl_canon_on_diag, &diag);
    char *out = NULL;
    nvmtl_canon_ref k = nvmtl_canon_parse(a, ctx, kernel, klen, "kernel", why, whyn);
    if (!k) goto done;

    // 1. link
    for (size_t i = 0; i < nmods; i++) {
        char what[32]; snprintf(what, sizeof what, "module %zu", i);
        nvmtl_canon_ref m = nvmtl_canon_parse(a, ctx, mods[i], mlen[i], what, why, whyn);
        if (!m) goto done;
        diagbuf[0] = 0;
        if (a->LinkModules2(k, m)) {   // the source module is destroyed either way
            nvmtl_canon_why(why, whyn, "link %s: %.300s", what, diagbuf[0] ? diagbuf : "failed");
            goto done;
        }
        s.linked++;
    }

    // 2. bind each `@X.MTL_VISIBLE_FN_REF` declaration to `@X`
    for (nvmtl_canon_ref fn = a->GetFirstFunction(k), next; fn; fn = next) {
        next = a->GetNextFunction(fn);
        size_t n = 0; const char *nm = a->GetValueName2(fn, &n);
        const char *tag = nm ? strstr(nm, ".MTL_VISIBLE_FN_REF") : NULL;
        if (!tag || !a->IsDeclaration(fn)) continue;
        char base[1024]; snprintf(base, sizeof base, "%.*s", (int)(tag - nm), nm);
        nvmtl_canon_ref def = a->GetNamedFunction(k, base);
        if (!def || a->IsDeclaration(def)) { s.unbound++; continue; }
        nvmtl_canon_ref declTy = a->GlobalGetValueType(fn), defTy = a->GlobalGetValueType(def);
        const unsigned want = a->CountParamTypes(declTy), have = a->CountParamTypes(defTy);
        if (want != have) {
            if (!nvmtl_canon_prefix_compatible(a, declTy, defTy) || nvmtl_canon_adapt_calls(a, ctx, fn, def)) { s.unbound++; continue; }
            s.adapted++;
        }
        a->ReplaceAllUsesWith(fn, def); a->DeleteFunction(fn);
        a->SetLinkage(def, 8);   // internal: once inlined, the standalone body is dead
        s.bound++;
    }

    // 3. fold function constants into their air.fc_initializer globals
    for (nvmtl_canon_ref g = a->GetFirstGlobal(k); g; g = a->GetNextGlobal(g)) {
        const char *sec = a->GetSection(g); size_t n = 0; const char *nm = a->GetValueName2(g, &n);
        if (!sec || strcmp(sec, "air.fc_initializer") || !nm) continue;
        const char *p = strstr(nm, "MTL_FC_INIT_"); if (!p) continue;
        const uint32_t want = (uint32_t)strtoul(p + 12, NULL, 10); size_t j;
        for (j = 0; j < nfc && fc[j].index != want; j++);
        if (j == nfc) { s.unfolded++; continue; }
        nvmtl_canon_ref ty = a->GlobalGetValueType(g), c = NULL; const int kind = a->GetTypeKind(ty);
        uint64_t raw = 0; for (uint32_t b = 0; b < fc[j].size && b < 8; b++) raw |= (uint64_t)fc[j].bytes[b] << (8 * b);
        if (kind == 8) {   // integer
            const unsigned w = a->GetIntTypeWidth(ty); if (w < 64) raw &= (1ull << w) - 1;
            c = a->ConstInt(ty, raw, 0);
        } else if (kind == 2) {   // float
            float v; uint32_t r = (uint32_t)raw; memcpy(&v, &r, 4); c = a->ConstReal(ty, v);
        }
        if (!c) { s.unfolded++; continue; }   // other types keep the translator's constant path
        a->SetInitializer(g, c); a->SetExternallyInitialized(g, 0); a->SetGlobalConstant(g, 1); a->SetSection(g, "");
        s.folded++;
    }

    // 4. inline the whole tree (stitched functions are always-inline in Apple's pipeline), then clean up
    {
        const unsigned ai = a->GetEnumAttributeKindForName("alwaysinline", 12), ni = a->GetEnumAttributeKindForName("noinline", 8);
        for (nvmtl_canon_ref fn = a->GetFirstFunction(k); fn; fn = a->GetNextFunction(fn)) {
            if (a->IsDeclaration(fn)) continue;
            const char *sec = a->GetSection(fn); if (sec && !strcmp(sec, "air.static_init")) continue;
            if (a->GetEnumAttributeAtIndex(fn, ~0u, ni)) a->RemoveEnumAttributeAtIndex(fn, ~0u, ni);
            a->AddAttributeAtIndex(fn, ~0u, a->CreateEnumAttribute(ctx, ai, 0)); s.inlined++;
        }
        char triple[256]; snprintf(triple, sizeof triple, "%s", a->GetTarget(k) ? a->GetTarget(k) : "");
        a->SetTarget(k, "");
        nvmtl_canon_ref opts = a->CreatePassBuilderOptions();
        nvmtl_canon_ref err = a->RunPasses(k, NVMTL_CANON_PIPELINE, NULL, opts);
        a->DisposePassBuilderOptions(opts);
        a->SetTarget(k, triple);
        if (err) {
            char *m = a->GetErrorMessage(err);
            nvmtl_canon_why(why, whyn, "passes: %.300s%s", m ? m : "failed", NULL);
            if (m) a->DisposeErrorMessage(m);
            goto done;
        }
    }

    // 5. verify and print
    {
        char *verr = NULL;
        if (a->VerifyModule(k, 2 /* return status */, &verr)) {
            nvmtl_canon_why(why, whyn, "verify: %.400s%s", verr ? verr : "failed", NULL);
            if (verr) a->DisposeMessage(verr);
            goto done;
        }
        if (verr) a->DisposeMessage(verr);
    }
    out = a->PrintModuleToString(k);
    if (!out) nvmtl_canon_why(why, whyn, "print failed%s%s", NULL, NULL);
done:
    if (k) a->DisposeModule(k);
    a->ContextDispose(ctx);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    s.ms = (double)(t1.tv_sec - t0.tv_sec) * 1e3 + (double)(t1.tv_nsec - t0.tv_nsec) / 1e6;
    if (st) *st = s;
    return out;
}

// The context's diagnostics: keep the first one's text for the reason (the linker reports conflicts this way).
static void nvmtl_canon_on_diag(nvmtl_canon_ref di, void *ctx)
{
    nvmtl_canon_diag *d = (nvmtl_canon_diag *)ctx;
    if (!d || !d->buf || d->buf[0] || !d->a) return;
    char *m = d->a->GetDiagInfoDescription(di);
    if (m) { snprintf(d->buf, d->n, "%s", m); d->a->DisposeMessage(m); }
}

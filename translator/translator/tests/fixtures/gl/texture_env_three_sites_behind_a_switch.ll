; ANGLE's texture environment for a WebGL2 fragment shader that samples `u[1]` at three sites of an if / else-if chain
; (xterm.js's glyph shader has sixteen), as Metal links it. The user main is a helper that is not inlined, and the entry
; keeps the sixteen element addresses of the texture and sampler arrays in one local object. With one or two sites Metal
; inlines the helper; with three the chain becomes a switch and the helper stays a call. The helper reads each address
; back out of the object it was handed and then the texture and sampler out of that element, a load through a load.
source_filename = "main0"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "spirv-unknown-vulkan1.2"

%struct.ANGLE_FragmentOut = type { <4 x float>, i32, [12 x i8] }
%struct.ANGLE_FragmentIn = type { i32 }
%struct.ANGLE_TextureEnvs = type { %"struct.metal::array" }
%"struct.metal::array" = type { [16 x %struct.ANGLE_TextureEnv] }
%struct.ANGLE_TextureEnv = type { ptr, ptr }
%struct.ANGLEUniformBlock = type { <2 x float>, i32, i32, i32, i32, <2 x i32>, <4 x i32>, i32, i32, <2 x i32> }
%"struct.metal::sampler" = type { ptr addrspace(2) }
%"struct.metal::texture2d" = type { ptr addrspace(1) }

@__metal_implicit_fc_pred_0 = internal addrspace(2) global i8 0, align 1
@_ZL26ANGLEWriteHelperSampleMask = internal unnamed_addr addrspace(2) global i8 undef, align 1
@_Z26ANGLEWriteHelperSampleMask.MTL_FC_INIT_5_b = internal unnamed_addr addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@_ZL27ANGLEEmulateAlphaToCoverage = internal unnamed_addr addrspace(2) global i8 undef, align 1
@_Z27ANGLEEmulateAlphaToCoverage.MTL_FC_INIT_4_b = internal unnamed_addr addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@_ZL26ANGLEMultisampledRendering = internal unnamed_addr addrspace(2) global i8 undef, align 1
@_Z26ANGLEMultisampledRendering.MTL_FC_INIT_2_b = internal unnamed_addr addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1

; Function Attrs: mustprogress nofree nosync nounwind willreturn
define internal void @_GLOBAL__sub_I_a3.metal() #0 section "air.static_init" {
  %1 = load i8, ptr addrspace(2) @_Z26ANGLEMultisampledRendering.MTL_FC_INIT_2_b, align 1, !tbaa !30, !range !34
  store i8 %1, ptr addrspace(2) @_ZL26ANGLEMultisampledRendering, align 1, !tbaa !30
  %2 = load i8, ptr addrspace(2) @_Z27ANGLEEmulateAlphaToCoverage.MTL_FC_INIT_4_b, align 1, !tbaa !30, !range !34
  store i8 %2, ptr addrspace(2) @_ZL27ANGLEEmulateAlphaToCoverage, align 1, !tbaa !30
  %3 = load i8, ptr addrspace(2) @_Z26ANGLEWriteHelperSampleMask.MTL_FC_INIT_5_b, align 1, !tbaa !30, !range !34
  store i8 %3, ptr addrspace(2) @_ZL26ANGLEWriteHelperSampleMask, align 1, !tbaa !30
  %4 = icmp ne i8 %1, 0
  %5 = icmp ne i8 %3, 0
  %6 = select i1 %4, i1 true, i1 %5
  %7 = zext i1 %6 to i8
  %8 = tail call i8 @air.normalize_function_constant_predicate.i8(i8 %7) #5
  store i8 %8, ptr addrspace(2) @__metal_implicit_fc_pred_0, align 1
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare i8 @air.normalize_function_constant_predicate.i8(i8) local_unnamed_addr #1

; Function Attrs: convergent mustprogress nofree nounwind willreturn
define <{ <4 x float>, i32 }> @main0(i32 %0, ptr addrspace(2) noundef readonly align 16 captures(none) dereferenceable(64) "air-buffer-no-alias" %1, ptr byval([16 x ptr addrspace(1)]) %2, ptr byval([16 x ptr addrspace(2)]) %3, <4 x float> noundef %4) local_unnamed_addr #2 {
  %6 = alloca %struct.ANGLE_FragmentOut, align 16
  %7 = alloca %struct.ANGLE_FragmentIn, align 4
  %8 = alloca %struct.ANGLE_TextureEnvs, align 8
  %9 = getelementptr inbounds %struct.ANGLE_FragmentIn, ptr %7, i64 0, i32 0
  store i32 %0, ptr %9, align 4, !tbaa !35
  %10 = bitcast ptr %8 to ptr
  call void @llvm.lifetime.start.p0(ptr %8)
  %11 = bitcast ptr %8 to ptr
  store ptr %2, ptr %11, align 8, !tbaa !38
  %12 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 0, i32 1
  %13 = bitcast ptr %12 to ptr
  store ptr %3, ptr %13, align 8, !tbaa !41
  %14 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 1
  %15 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 1, i32 0
  %16 = bitcast ptr %15 to ptr
  store ptr %14, ptr %16, align 8, !tbaa !38
  %17 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 1
  %18 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 1, i32 1
  %19 = bitcast ptr %18 to ptr
  store ptr %17, ptr %19, align 8, !tbaa !41
  %20 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 2
  %21 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 2, i32 0
  %22 = bitcast ptr %21 to ptr
  store ptr %20, ptr %22, align 8, !tbaa !38
  %23 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 2
  %24 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 2, i32 1
  %25 = bitcast ptr %24 to ptr
  store ptr %23, ptr %25, align 8, !tbaa !41
  %26 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 3
  %27 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 3, i32 0
  %28 = bitcast ptr %27 to ptr
  store ptr %26, ptr %28, align 8, !tbaa !38
  %29 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 3
  %30 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 3, i32 1
  %31 = bitcast ptr %30 to ptr
  store ptr %29, ptr %31, align 8, !tbaa !41
  %32 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 4
  %33 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 4, i32 0
  %34 = bitcast ptr %33 to ptr
  store ptr %32, ptr %34, align 8, !tbaa !38
  %35 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 4
  %36 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 4, i32 1
  %37 = bitcast ptr %36 to ptr
  store ptr %35, ptr %37, align 8, !tbaa !41
  %38 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 5
  %39 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 5, i32 0
  %40 = bitcast ptr %39 to ptr
  store ptr %38, ptr %40, align 8, !tbaa !38
  %41 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 5
  %42 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 5, i32 1
  %43 = bitcast ptr %42 to ptr
  store ptr %41, ptr %43, align 8, !tbaa !41
  %44 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 6
  %45 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 6, i32 0
  %46 = bitcast ptr %45 to ptr
  store ptr %44, ptr %46, align 8, !tbaa !38
  %47 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 6
  %48 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 6, i32 1
  %49 = bitcast ptr %48 to ptr
  store ptr %47, ptr %49, align 8, !tbaa !41
  %50 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 7
  %51 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 7, i32 0
  %52 = bitcast ptr %51 to ptr
  store ptr %50, ptr %52, align 8, !tbaa !38
  %53 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 7
  %54 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 7, i32 1
  %55 = bitcast ptr %54 to ptr
  store ptr %53, ptr %55, align 8, !tbaa !41
  %56 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 8
  %57 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 8, i32 0
  %58 = bitcast ptr %57 to ptr
  store ptr %56, ptr %58, align 8, !tbaa !38
  %59 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 8
  %60 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 8, i32 1
  %61 = bitcast ptr %60 to ptr
  store ptr %59, ptr %61, align 8, !tbaa !41
  %62 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 9
  %63 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 9, i32 0
  %64 = bitcast ptr %63 to ptr
  store ptr %62, ptr %64, align 8, !tbaa !38
  %65 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 9
  %66 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 9, i32 1
  %67 = bitcast ptr %66 to ptr
  store ptr %65, ptr %67, align 8, !tbaa !41
  %68 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 10
  %69 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 10, i32 0
  %70 = bitcast ptr %69 to ptr
  store ptr %68, ptr %70, align 8, !tbaa !38
  %71 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 10
  %72 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 10, i32 1
  %73 = bitcast ptr %72 to ptr
  store ptr %71, ptr %73, align 8, !tbaa !41
  %74 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 11
  %75 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 11, i32 0
  %76 = bitcast ptr %75 to ptr
  store ptr %74, ptr %76, align 8, !tbaa !38
  %77 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 11
  %78 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 11, i32 1
  %79 = bitcast ptr %78 to ptr
  store ptr %77, ptr %79, align 8, !tbaa !41
  %80 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 12
  %81 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 12, i32 0
  %82 = bitcast ptr %81 to ptr
  store ptr %80, ptr %82, align 8, !tbaa !38
  %83 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 12
  %84 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 12, i32 1
  %85 = bitcast ptr %84 to ptr
  store ptr %83, ptr %85, align 8, !tbaa !41
  %86 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 13
  %87 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 13, i32 0
  %88 = bitcast ptr %87 to ptr
  store ptr %86, ptr %88, align 8, !tbaa !38
  %89 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 13
  %90 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 13, i32 1
  %91 = bitcast ptr %90 to ptr
  store ptr %89, ptr %91, align 8, !tbaa !41
  %92 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 14
  %93 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 14, i32 0
  %94 = bitcast ptr %93 to ptr
  store ptr %92, ptr %94, align 8, !tbaa !38
  %95 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 14
  %96 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 14, i32 1
  %97 = bitcast ptr %96 to ptr
  store ptr %95, ptr %97, align 8, !tbaa !41
  %98 = getelementptr inbounds [16 x ptr addrspace(1)], ptr %2, i64 0, i64 15
  %99 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 15, i32 0
  %100 = bitcast ptr %99 to ptr
  store ptr %98, ptr %100, align 8, !tbaa !38
  %101 = getelementptr inbounds [16 x ptr addrspace(2)], ptr %3, i64 0, i64 15
  %102 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %8, i64 0, i32 0, i32 0, i64 15, i32 1
  %103 = bitcast ptr %102 to ptr
  store ptr %101, ptr %103, align 8, !tbaa !41
  %104 = load i8, ptr addrspace(2) @_ZL26ANGLEWriteHelperSampleMask, align 1, !tbaa !30, !range !34
  %105 = icmp eq i8 %104, 0
  br i1 %105, label %108, label %106

106:                                              ; preds = %5
  %107 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %6, i64 0, i32 1
  store i32 -1, ptr %107, align 16, !tbaa !42
  br label %108

108:                                              ; preds = %106, %5
  call fastcc void @_Z13ANGLE__0_mainR17ANGLE_FragmentOutR16ANGLE_FragmentInR17ANGLE_TextureEnvs(ptr noundef nonnull align 16 dereferenceable(32) %6, ptr noundef nonnull align 4 dereferenceable(4) %7, ptr noundef nonnull align 8 dereferenceable(256) %8) #6
  %109 = load i8, ptr addrspace(2) @_ZL26ANGLEMultisampledRendering, align 1, !tbaa !30, !range !34
  %110 = icmp eq i8 %109, 0
  br i1 %110, label %111, label %114

111:                                              ; preds = %108
  %112 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %6, i64 0, i32 1
  %113 = load i32, ptr %112, align 16, !tbaa !42
  br label %167

114:                                              ; preds = %108
  %115 = getelementptr inbounds %struct.ANGLEUniformBlock, ptr addrspace(2) %1, i64 0, i32 8
  %116 = load i32, ptr addrspace(2) %115, align 4, !tbaa !44, !alias.scope !46, !noalias !49
  %117 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %6, i64 0, i32 1
  %118 = load i8, ptr addrspace(2) @_ZL27ANGLEEmulateAlphaToCoverage, align 1, !tbaa !30, !range !34
  %119 = icmp eq i8 %118, 0
  br i1 %119, label %167, label %120

120:                                              ; preds = %114
  %121 = getelementptr inbounds %struct.ANGLEUniformBlock, ptr addrspace(2) %1, i64 0, i32 3
  %122 = load i32, ptr addrspace(2) %121, align 16, !tbaa !52, !alias.scope !46, !noalias !49
  %123 = and i32 %122, 2097152
  %124 = icmp eq i32 %123, 0
  br i1 %124, label %167, label %125

125:                                              ; preds = %120
  %126 = extractelement <4 x float> %4, i64 0
  %127 = call i32 @air.convert.s.i32.f.f32(float %126) #5
  %128 = extractelement <4 x float> %4, i64 1
  %129 = call i32 @air.convert.s.i32.f.f32(float %128) #5
  %130 = shl i32 %127, 1
  %131 = and i32 %129, 1
  %132 = sub i32 %130, %131
  %133 = and i32 %132, 3
  %134 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %6, i64 0, i32 0
  %135 = load <4 x float>, ptr %134, align 16
  %136 = extractelement <4 x float> %135, i64 3
  %137 = call fast float @air.fast_saturate.f32(float %136) #5
  %138 = fmul fast float %137, 5.100000e+02
  %139 = call fast float @air.fast_clamp.f32(float %138, float 0xC1E0000000000000, float 0x41DFFFFFE0000000) #5
  %140 = call i32 @air.convert.s.i32.f.f32(float %139) #5
  %141 = call i32 @air.get_num_samples.i32(i32 0) #5
  switch i32 %141, label %164 [
    i32 2, label %142
    i32 4, label %149
    i32 8, label %156
  ]

142:                                              ; preds = %125
  %143 = mul nsw i32 %133, -64
  %144 = add i32 %140, 225
  %145 = add i32 %144, %143
  %146 = ashr i32 %145, 8
  %147 = mul nsw i32 %146, 3
  %148 = ashr i32 %147, 1
  br label %164

149:                                              ; preds = %125
  %150 = mul nuw nsw i32 %133, 992
  %151 = add i32 %140, 113
  %152 = add i32 %151, %150
  %153 = lshr i32 %152, 5
  %154 = and i32 %153, 28
  %155 = lshr i32 1030416, %154
  br label %164

156:                                              ; preds = %125
  %157 = mul nuw nsw i32 %133, 2032
  %158 = add i32 %140, 57
  %159 = add i32 %158, %157
  %160 = lshr i32 %159, 6
  %161 = and i32 %160, 31
  %162 = shl nsw i32 -1, %161
  %163 = xor i32 %162, -1
  br label %164

164:                                              ; preds = %156, %149, %142, %125
  %165 = phi i32 [ %140, %125 ], [ %163, %156 ], [ %155, %149 ], [ %148, %142 ]
  %166 = and i32 %165, %116
  store i32 %166, ptr %117, align 16, !tbaa !42
  br label %167

167:                                              ; preds = %164, %120, %114, %111
  %168 = phi i32 [ %113, %111 ], [ %166, %164 ], [ %116, %120 ], [ %116, %114 ]
  call void @llvm.lifetime.end.p0(ptr %8)
  %169 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %6, i64 0, i32 0
  %170 = load <4 x float>, ptr %169, align 16, !tbaa !53
  %171 = insertvalue <{ <4 x float>, i32 }> undef, <4 x float> %170, 0
  %172 = insertvalue <{ <4 x float>, i32 }> %171, i32 %168, 1
  ret <{ <4 x float>, i32 }> %172
}

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare i32 @air.get_num_samples.i32(i32) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare i32 @air.convert.s.i32.f.f32(float) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare float @air.fast_clamp.f32(float, float, float) local_unnamed_addr #1

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare float @air.fast_saturate.f32(float) local_unnamed_addr #1

; Function Attrs: convergent mustprogress nofree nounwind willreturn
define internal fastcc void @_Z13ANGLE__0_mainR17ANGLE_FragmentOutR16ANGLE_FragmentInR17ANGLE_TextureEnvs(ptr noundef nonnull writeonly align 16 captures(none) dereferenceable(32) %0, ptr noundef nonnull readonly align 4 captures(none) dereferenceable(4) %1, ptr noundef nonnull readonly align 8 captures(none) dereferenceable(256) %2) unnamed_addr #2 {
  %4 = getelementptr inbounds %struct.ANGLE_FragmentOut, ptr %0, i64 0, i32 0
  store <4 x float> zeroinitializer, ptr %4, align 16, !tbaa !53
  %5 = getelementptr inbounds %struct.ANGLE_FragmentIn, ptr %1, i64 0, i32 0
  %6 = load i32, ptr %5, align 4, !tbaa !35
  switch i32 %6, label %40 [
    i32 1, label %7
    i32 2, label %18
    i32 3, label %29
  ]

7:                                                ; preds = %3
  %8 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 0
  %9 = load ptr, ptr %8, align 8, !tbaa !38
  %10 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 1
  %11 = load ptr, ptr %10, align 8, !tbaa !41
  %12 = getelementptr inbounds %"struct.metal::sampler", ptr %11, i64 0, i32 0
  %13 = load ptr addrspace(2), ptr %12, align 8, !tbaa.struct !54
  %14 = getelementptr inbounds %"struct.metal::texture2d", ptr %9, i64 0, i32 0
  %15 = load ptr addrspace(1), ptr %14, align 8, !tbaa !57
  %16 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly captures(none) %15, ptr addrspace(2) readonly captures(none) %13, <2 x float> splat (float 5.000000e-01), i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #7
  %17 = extractvalue { <4 x float>, i8 } %16, 0
  br label %40

18:                                               ; preds = %3
  %19 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 0
  %20 = load ptr, ptr %19, align 8, !tbaa !38
  %21 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 1
  %22 = load ptr, ptr %21, align 8, !tbaa !41
  %23 = getelementptr inbounds %"struct.metal::sampler", ptr %22, i64 0, i32 0
  %24 = load ptr addrspace(2), ptr %23, align 8, !tbaa.struct !54
  %25 = getelementptr inbounds %"struct.metal::texture2d", ptr %20, i64 0, i32 0
  %26 = load ptr addrspace(1), ptr %25, align 8, !tbaa !57
  %27 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly captures(none) %26, ptr addrspace(2) readonly captures(none) %24, <2 x float> splat (float 5.000000e-01), i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #7
  %28 = extractvalue { <4 x float>, i8 } %27, 0
  br label %40

29:                                               ; preds = %3
  %30 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 0
  %31 = load ptr, ptr %30, align 8, !tbaa !38
  %32 = getelementptr inbounds %struct.ANGLE_TextureEnvs, ptr %2, i64 0, i32 0, i32 0, i64 1, i32 1
  %33 = load ptr, ptr %32, align 8, !tbaa !41
  %34 = getelementptr inbounds %"struct.metal::sampler", ptr %33, i64 0, i32 0
  %35 = load ptr addrspace(2), ptr %34, align 8, !tbaa.struct !54
  %36 = getelementptr inbounds %"struct.metal::texture2d", ptr %31, i64 0, i32 0
  %37 = load ptr addrspace(1), ptr %36, align 8, !tbaa !57
  %38 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly captures(none) %37, ptr addrspace(2) readonly captures(none) %35, <2 x float> splat (float 5.000000e-01), i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0) #7
  %39 = extractvalue { <4 x float>, i8 } %38, 0
  br label %40

40:                                               ; preds = %29, %18, %7, %3
  %41 = phi <4 x float> [ %17, %7 ], [ %28, %18 ], [ %39, %29 ], [ <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00, float 1.000000e+00>, %3 ]
  store <4 x float> %41, ptr %4, align 16, !tbaa !53
  ret void
}

; Function Attrs: convergent mustprogress nofree nounwind willreturn memory(argmem: read)
declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly captures(none), ptr addrspace(2) readonly captures(none), <2 x float>, i1, <2 x i32>, i1, float, float, i32) local_unnamed_addr #3

declare void @llvm.lifetime.end.i64(i64)

declare void @llvm.lifetime.start.i64(i64)

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.start.p0(ptr captures(none)) #4

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.end.p0(ptr captures(none)) #4

attributes #0 = { mustprogress nofree nosync nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind willreturn memory(none) }
attributes #2 = { convergent mustprogress nofree nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #3 = { convergent mustprogress nofree nounwind willreturn memory(argmem: read) }
attributes #4 = { nocallback nofree nosync nounwind willreturn memory(argmem: readwrite) }
attributes #5 = { nounwind willreturn memory(none) }
attributes #6 = { convergent nobuiltin "no-builtins" }
attributes #7 = { convergent nounwind willreturn memory(argmem: read) }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!llvm.ident = !{!9}
!air.version = !{!10}
!air.language_version = !{!11}
!air.compile_options = !{!12, !13, !14}
!air.fragment = !{!15}
!air.function_constants = !{!27, !28, !29}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!10 = !{i32 2, i32 7, i32 0}
!11 = !{!"Metal", i32 3, i32 2, i32 0}
!12 = !{!"air.compile.denorms_disable"}
!13 = !{!"air.compile.fast_math_enable"}
!14 = !{!"air.compile.framebuffer_fetch_enable"}
!15 = !{ptr @main0, !16, !20}
!16 = !{!17, !18}
!17 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"_uo"}
!18 = !{!"air.function_constant", !19, !"air.sample_mask", !"air.arg_type_name", !"uint", !"air.arg_name", !"ANGLE_metal_SampleMask"}
!19 = !{ptr addrspace(2) @__metal_implicit_fc_pred_0, !"bool", !"ANGLESampleMaskWriteEnabled"}
!20 = !{!21, !22, !24, !25, !26}
!21 = !{i32 0, !"air.fragment_input", !"generated(4_uvpi)", !"air.flat", !"air.arg_type_name", !"int", !"air.arg_name", !"_uvp"}
!22 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 64, !"air.location_index", i32 17, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !23, !"air.arg_type_size", i32 64, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"ANGLEUniformBlock", !"air.arg_name", !"ANGLE_angleUniforms"}
!23 = !{i32 0, i32 8, i32 0, !"float2", !"ANGLE_depthRange", i32 8, i32 4, i32 0, !"uint", !"ANGLE_renderArea", i32 12, i32 4, i32 0, !"uint", !"ANGLE_flipXY", i32 16, i32 4, i32 0, !"uint", !"ANGLE_misc", i32 20, i32 4, i32 0, !"int", !"ANGLE_baseInstance", i32 24, i32 8, i32 0, !"uint2", !"ANGLE_acbBufferOffsets", i32 32, i32 16, i32 0, !"int4", !"ANGLE_xfbBufferOffsets", i32 48, i32 4, i32 0, !"int", !"ANGLE_xfbVerticesPerInstance", i32 52, i32 4, i32 0, !"uint", !"ANGLE_coverageMask", i32 56, i32 8, i32 0, !"uint2", !"ANGLE_unused"}
!24 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 16, !"air.sample", !"air.arg_type_name", !"array<texture2d<float, sample>, 16>", !"air.arg_name", !"ANGLE__1_u_texture"}
!25 = !{i32 3, !"air.sampler", !"air.location_index", i32 0, i32 16, !"air.arg_type_name", !"array<sampler, 16>", !"air.arg_name", !"ANGLE__2_u_sampler"}
!26 = !{i32 4, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"gl_FragCoord"}
!27 = !{ptr addrspace(2) @_Z26ANGLEMultisampledRendering.MTL_FC_INIT_2_b, !"bool", !"ANGLEMultisampledRendering", i32 2, i1 true}
!28 = !{ptr addrspace(2) @_Z27ANGLEEmulateAlphaToCoverage.MTL_FC_INIT_4_b, !"bool", !"ANGLEEmulateAlphaToCoverage", i32 4, i1 true}
!29 = !{ptr addrspace(2) @_Z26ANGLEWriteHelperSampleMask.MTL_FC_INIT_5_b, !"bool", !"ANGLEWriteHelperSampleMask", i32 5, i1 true}
!30 = !{!31, !31, i64 0}
!31 = !{!"bool", !32, i64 0}
!32 = !{!"omnipotent char", !33, i64 0}
!33 = !{!"Simple C++ TBAA"}
!34 = !{i8 0, i8 2}
!35 = !{!36, !37, i64 0}
!36 = !{!"_ZTS16ANGLE_FragmentIn", !37, i64 0}
!37 = !{!"int", !32, i64 0}
!38 = !{!39, !40, i64 0}
!39 = !{!"_ZTS16ANGLE_TextureEnvIN5metal9texture2dIfLNS0_6accessE0ELNS0_16memory_coherenceE0EvEEE", !40, i64 0, !40, i64 8}
!40 = !{!"any pointer", !32, i64 0}
!41 = !{!39, !40, i64 8}
!42 = !{!43, !37, i64 16}
!43 = !{!"_ZTS17ANGLE_FragmentOut", !32, i64 0, !37, i64 16}
!44 = !{!45, !37, i64 52}
!45 = !{!"_ZTS17ANGLEUniformBlock", !32, i64 0, !37, i64 8, !37, i64 12, !37, i64 16, !37, i64 20, !32, i64 24, !32, i64 32, !37, i64 48, !37, i64 52, !32, i64 56}
!46 = !{!47}
!47 = distinct !{!47, !48, !"air-alias-scope-arg(1)"}
!48 = distinct !{!48, !"air-alias-scopes(main0)"}
!49 = !{!50, !51}
!50 = distinct !{!50, !48, !"air-alias-scope-textures"}
!51 = distinct !{!51, !48, !"air-alias-scope-samplers"}
!52 = !{!45, !37, i64 16}
!53 = !{!32, !32, i64 0}
!54 = !{i64 0, i64 8, !55}
!55 = !{!56, !56, i64 0}
!56 = !{!"__metal_sampler_t", !32, i64 0}
!57 = !{!58, !59, i64 0}
!58 = !{!"_ZTSN5metal9texture2dIfLNS_6accessE0ELNS_16memory_coherenceE0EvEE", !59, i64 0}
!59 = !{!"__metal_texture_2d_t", !32, i64 0}

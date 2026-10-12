; ModuleID = 'nvmtl-air'
source_filename = "dawn_entry_point"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64-apple-macosx15.8.0"

%struct.tint_struct_1 = type { <4 x float>, <4 x float> }
%struct.tint_array.0 = type { [1 x i32] }
%struct.tint_struct_5 = type { %struct.tint_struct_1, i32, %struct.tint_array.1 }
%struct.tint_array.1 = type { [3 x i32] }
%struct.tint_struct_3 = type { %struct.tint_array }
%struct.tint_array = type { [1 x %struct.tint_struct_4] }
%struct.tint_struct_4 = type { <4 x float> }

; Function Attrs: mustprogress nofree nosync nounwind willreturn
define <{ <4 x float>, <4 x float> }> @dawn_entry_point(i32 noundef %v_65, i32 noundef %v_66, ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %v_67, ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %v_68, ptr addrspace(2) noundef readonly captures(none) "air-buffer-no-alias" %v_69) local_unnamed_addr #0 {
entry:
  %retval.i = alloca %struct.tint_struct_1, align 16
  %v_70 = alloca i32, align 4
  %0 = bitcast ptr %v_70 to ptr
  call void @llvm.lifetime.start.p0(i64 4, ptr nonnull %0) #5
  store i32 0, ptr %v_70, align 4, !tbaa !31
  %1 = bitcast ptr %retval.i to ptr
  call void @llvm.lifetime.start.p0(i64 32, ptr nonnull %1)
  %mul.i = mul i32 %v_66, 15
  %conv.i = zext i32 %mul.i to i64
  %arrayidx.i.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv.i
  %2 = bitcast ptr addrspace(1) %arrayidx.i.i to ptr addrspace(1)
  %3 = load float, ptr addrspace(1) %2, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit.i = insertelement <4 x float> undef, float %3, i64 0
  %add.i = add i32 %mul.i, 1
  %conv2.i = zext i32 %add.i to i64
  %arrayidx.i121.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv2.i
  %4 = bitcast ptr addrspace(1) %arrayidx.i121.i to ptr addrspace(1)
  %5 = load float, ptr addrspace(1) %4, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit5.i = insertelement <4 x float> %vecinit.i, float %5, i64 1
  %add7.i = add i32 %mul.i, 2
  %conv8.i = zext i32 %add7.i to i64
  %arrayidx.i122.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv8.i
  %6 = bitcast ptr addrspace(1) %arrayidx.i122.i to ptr addrspace(1)
  %7 = load float, ptr addrspace(1) %6, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit11.i = insertelement <4 x float> %vecinit5.i, float %7, i64 2
  %add13.i = add i32 %mul.i, 3
  %conv14.i = zext i32 %add13.i to i64
  %arrayidx.i123.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv14.i
  %8 = bitcast ptr addrspace(1) %arrayidx.i123.i to ptr addrspace(1)
  %9 = load float, ptr addrspace(1) %8, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit17.i = insertelement <4 x float> %vecinit11.i, float %9, i64 3
  %add19.i = add i32 %mul.i, 4
  %conv20.i = zext i32 %add19.i to i64
  %arrayidx.i124.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv20.i
  %10 = bitcast ptr addrspace(1) %arrayidx.i124.i to ptr addrspace(1)
  %11 = load float, ptr addrspace(1) %10, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %add24.i = add i32 %mul.i, 5
  %conv25.i = zext i32 %add24.i to i64
  %arrayidx.i125.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv25.i
  %12 = load i32, ptr addrspace(1) %arrayidx.i125.i, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %add28.i = add i32 %mul.i, 6
  %conv29.i = zext i32 %add28.i to i64
  %arrayidx.i126.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv29.i
  %13 = bitcast ptr addrspace(1) %arrayidx.i126.i to ptr addrspace(1)
  %14 = load float, ptr addrspace(1) %13, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit32.i = insertelement <3 x float> undef, float %14, i64 0
  %add34.i = add i32 %mul.i, 7
  %conv35.i = zext i32 %add34.i to i64
  %arrayidx.i127.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv35.i
  %15 = bitcast ptr addrspace(1) %arrayidx.i127.i to ptr addrspace(1)
  %16 = load float, ptr addrspace(1) %15, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit38.i = insertelement <3 x float> %vecinit32.i, float %16, i64 1
  %add40.i = add i32 %mul.i, 8
  %conv41.i = zext i32 %add40.i to i64
  %arrayidx.i128.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv41.i
  %17 = bitcast ptr addrspace(1) %arrayidx.i128.i to ptr addrspace(1)
  %18 = load float, ptr addrspace(1) %17, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit44.i = insertelement <3 x float> %vecinit38.i, float %18, i64 2
  %add46.i = add i32 %mul.i, 9
  %conv47.i = zext i32 %add46.i to i64
  %arrayidx.i129.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv47.i
  %19 = bitcast ptr addrspace(1) %arrayidx.i129.i to ptr addrspace(1)
  %20 = load float, ptr addrspace(1) %19, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit50.i = insertelement <3 x float> undef, float %20, i64 0
  %add52.i = add i32 %mul.i, 10
  %conv53.i = zext i32 %add52.i to i64
  %arrayidx.i130.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv53.i
  %21 = bitcast ptr addrspace(1) %arrayidx.i130.i to ptr addrspace(1)
  %22 = load float, ptr addrspace(1) %21, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit56.i = insertelement <3 x float> %vecinit50.i, float %22, i64 1
  %add58.i = add i32 %mul.i, 11
  %conv59.i = zext i32 %add58.i to i64
  %arrayidx.i131.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv59.i
  %23 = bitcast ptr addrspace(1) %arrayidx.i131.i to ptr addrspace(1)
  %24 = load float, ptr addrspace(1) %23, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit62.i = insertelement <3 x float> %vecinit56.i, float %24, i64 2
  call void @llvm.memset.p0.i64(ptr noundef nonnull align 16 dereferenceable(32) %1, i8 0, i64 32, i1 false) #5
  %add64.i = add i32 %mul.i, 12
  %conv65.i = zext i32 %add64.i to i64
  %arrayidx.i132.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv65.i
  %25 = bitcast ptr addrspace(1) %arrayidx.i132.i to ptr addrspace(1)
  %26 = load float, ptr addrspace(1) %25, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit68.i = insertelement <3 x float> undef, float %26, i64 0
  %add70.i = add i32 %mul.i, 13
  %conv71.i = zext i32 %add70.i to i64
  %arrayidx.i133.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv71.i
  %27 = bitcast ptr addrspace(1) %arrayidx.i133.i to ptr addrspace(1)
  %28 = load float, ptr addrspace(1) %27, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit74.i = insertelement <3 x float> %vecinit68.i, float %28, i64 1
  %add76.i = add i32 %mul.i, 14
  %conv77.i = zext i32 %add76.i to i64
  %arrayidx.i134.i = getelementptr inbounds %struct.tint_array.0, ptr addrspace(1) %v_68, i64 0, i32 0, i64 %conv77.i
  %29 = bitcast ptr addrspace(1) %arrayidx.i134.i to ptr addrspace(1)
  %30 = load float, ptr addrspace(1) %29, align 4, !tbaa !31, !alias.scope !35, !noalias !38
  %vecinit80.i = insertelement <3 x float> %vecinit74.i, float %30, i64 2
  call fastcc void @_Z4v_4111tint_structP13tint_struct_113tint_struct_2(i32 %v_65, <4 x float> %vecinit17.i, float %11, i32 %12, <3 x float> %vecinit44.i, <3 x float> %vecinit62.i, <3 x float> %vecinit80.i, ptr noundef nonnull %retval.i, ptr addrspace(1) %v_67, ptr nonnull %v_70, ptr addrspace(1) undef, ptr addrspace(2) %v_69) #6
  %.fca.0.gep.i = getelementptr inbounds %struct.tint_struct_1, ptr %retval.i, i64 0, i32 0
  %.fca.0.load.i = load <4 x float>, ptr %.fca.0.gep.i, align 16
  %.fca.1.gep.i = getelementptr inbounds %struct.tint_struct_1, ptr %retval.i, i64 0, i32 1
  %.fca.1.load.i = load <4 x float>, ptr %.fca.1.gep.i, align 16
  call void @llvm.lifetime.end.p0(i64 32, ptr nonnull %1)
  call void @llvm.lifetime.end.p0(i64 4, ptr nonnull %0) #5
  %31 = insertvalue <{ <4 x float>, <4 x float> }> undef, <4 x float> %.fca.0.load.i, 0
  %32 = insertvalue <{ <4 x float>, <4 x float> }> %31, <4 x float> %.fca.1.load.i, 1
  ret <{ <4 x float>, <4 x float> }> %32
}

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(argmem: readwrite)
define internal fastcc void @_Z4v_4111tint_structP13tint_struct_113tint_struct_2(i32 %v_42.coerce0, <4 x float> %v_42.coerce1, float %v_42.coerce2, i32 %v_42.coerce3, <3 x float> %v_42.coerce4, <3 x float> %v_42.coerce5, <3 x float> %v_42.coerce6, ptr noundef writeonly captures(none) %v_43, ptr addrspace(1) readonly captures(none) %v_44.coerce0, ptr captures(none) %v_44.coerce1, ptr addrspace(1) readnone captures(none) %v_44.coerce2, ptr addrspace(2) readonly captures(none) %v_44.coerce3) unnamed_addr #1 {
entry:
  store i32 %v_42.coerce3, ptr %v_44.coerce1, align 4, !tbaa !31
  %div.i = sdiv i32 %v_42.coerce0, 2
  %0 = tail call reassoc nsz arcp contract afn float @air.convert.f.f32.s.i32(i32 %div.i) #7
  %vecinit = insertelement <2 x float> undef, float %0, i64 0
  %1 = srem i32 %v_42.coerce0, 2
  %2 = tail call reassoc nsz arcp contract afn float @air.convert.f.f32.s.i32(i32 %1) #7
  %vecinit3 = insertelement <2 x float> %vecinit, float %2, i64 1
  %3 = shufflevector <4 x float> %v_42.coerce1, <4 x float> poison, <2 x i32> <i32 0, i32 1>
  %4 = shufflevector <4 x float> %v_42.coerce1, <4 x float> poison, <2 x i32> <i32 2, i32 3>
  %cmp.i54 = fcmp reassoc nsz arcp contract afn ole <2 x float> %3, %4
  %5 = tail call i1 @air.all.v2i1(<2 x i1> %cmp.i54) #7
  br i1 %5, label %if.then.i, label %if.else.i

if.then.i:                                        ; preds = %entry
  %6 = tail call fast <2 x float> @air.mix.v2f32(<2 x float> %3, <2 x float> %4, <2 x float> %vecinit3) #7
  %splat.splat.i.i = shufflevector <2 x float> %6, <2 x float> undef, <3 x i32> zeroinitializer
  %mul.i.i = fmul fast <3 x float> %splat.splat.i.i, %v_42.coerce4
  %splat.splat.i73.i = shufflevector <2 x float> %6, <2 x float> undef, <3 x i32> <i32 1, i32 1, i32 1>
  %mul.i74.i = fmul fast <3 x float> %splat.splat.i73.i, %v_42.coerce5
  %add.i75.i = fadd fast <3 x float> %mul.i74.i, %v_42.coerce6
  %add.i79.i = fadd fast <3 x float> %add.i75.i, %mul.i.i
  %7 = shufflevector <3 x float> %add.i79.i, <3 x float> poison, <4 x i32> <i32 poison, i32 poison, i32 2, i32 poison>
  %vecinit5.i = shufflevector <3 x float> %add.i79.i, <3 x float> poison, <4 x i32> <i32 0, i32 1, i32 poison, i32 poison>
  %8 = extractelement <3 x float> %add.i79.i, i64 2
  %mul.i = fmul reassoc nsz arcp contract afn float %8, %v_42.coerce2
  %vecinit6.i = insertelement <4 x float> %vecinit5.i, float %mul.i, i64 2
  %vecinit7.i = shufflevector <4 x float> %vecinit6.i, <4 x float> %7, <4 x i32> <i32 0, i32 1, i32 2, i32 6>
  br label %_Z4v_17Dv2_fDv4_ffN5metal6matrixIfLi3ELi3EvEEPS_.exit

if.else.i:                                        ; preds = %entry
  %9 = tail call fast <2 x float> @air.mix.v2f32(<2 x float> %4, <2 x float> %3, <2 x float> %vecinit3) #7
  %10 = extractelement <3 x float> %v_42.coerce4, i64 0
  %11 = extractelement <3 x float> %v_42.coerce4, i64 1
  %12 = extractelement <3 x float> %v_42.coerce4, i64 2
  %13 = extractelement <3 x float> %v_42.coerce5, i64 0
  %14 = extractelement <3 x float> %v_42.coerce5, i64 1
  %15 = extractelement <3 x float> %v_42.coerce5, i64 2
  %16 = extractelement <3 x float> %v_42.coerce6, i64 0
  %17 = extractelement <3 x float> %v_42.coerce6, i64 1
  %18 = extractelement <3 x float> %v_42.coerce6, i64 2
  %mul.i83.i = fmul reassoc nsz arcp contract afn float %18, %14
  %mul9.i.i = fmul reassoc nsz arcp contract afn float %17, %15
  %sub.i.i = fsub reassoc nsz arcp contract afn float %mul.i83.i, %mul9.i.i
  %mul11.i.i = fmul reassoc nsz arcp contract afn float %16, %15
  %19 = fmul reassoc nsz arcp contract afn float %18, %13
  %add.i84.i = fsub reassoc nsz arcp contract afn float %mul11.i.i, %19
  %mul12.i.i = fmul reassoc nsz arcp contract afn float %17, %13
  %mul13.i.i = fmul reassoc nsz arcp contract afn float %16, %14
  %sub14.i.i = fsub reassoc nsz arcp contract afn float %mul12.i.i, %mul13.i.i
  %mul15.i.i = fmul reassoc nsz arcp contract afn float %sub.i.i, %10
  %mul16.i.i = fmul reassoc nsz arcp contract afn float %add.i84.i, %11
  %add17.i.i = fadd reassoc nsz arcp contract afn float %mul15.i.i, %mul16.i.i
  %mul18.i.i = fmul reassoc nsz arcp contract afn float %sub14.i.i, %12
  %add19.i.i = fadd reassoc nsz arcp contract afn float %add17.i.i, %mul18.i.i
  %mul25.i.i = fmul reassoc nsz arcp contract afn float %15, %11
  %mul26.i.i = fmul reassoc nsz arcp contract afn float %14, %12
  %sub27.i.i = fsub reassoc nsz arcp contract afn float %mul25.i.i, %mul26.i.i
  %mul36.i.i = fmul reassoc nsz arcp contract afn float %13, %12
  %20 = fmul reassoc nsz arcp contract afn float %15, %10
  %add37.i.i = fsub reassoc nsz arcp contract afn float %mul36.i.i, %20
  %mul45.i.i = fmul reassoc nsz arcp contract afn float %14, %10
  %21 = fmul reassoc nsz arcp contract afn float %13, %11
  %sub47.i.i = fsub reassoc nsz arcp contract afn float %mul45.i.i, %21
  %22 = extractelement <2 x float> %9, i64 1
  %23 = fmul fast float %22, %add37.i.i
  %24 = fadd fast float %sub47.i.i, %23
  %25 = extractelement <2 x float> %9, i64 0
  %26 = fmul fast float %25, %sub27.i.i
  %27 = fadd fast float %24, %26
  %div.i55 = fdiv reassoc nsz arcp contract afn float %add19.i.i, %27
  %splat.splatinsert.i = insertelement <2 x float> poison, float %div.i55, i64 0
  %splat.splat.i = shufflevector <2 x float> %splat.splatinsert.i, <2 x float> poison, <2 x i32> zeroinitializer
  %mul17.i = fmul reassoc nsz arcp contract afn <2 x float> %splat.splat.i, %9
  %vecinit19.i = shufflevector <2 x float> %mul17.i, <2 x float> poison, <4 x i32> <i32 0, i32 1, i32 poison, i32 poison>
  %mul20.i = fmul reassoc nsz arcp contract afn float %div.i55, %v_42.coerce2
  %vecinit21.i = insertelement <4 x float> %vecinit19.i, float %mul20.i, i64 2
  %vecinit22.i = insertelement <4 x float> %vecinit21.i, float %div.i55, i64 3
  br label %_Z4v_17Dv2_fDv4_ffN5metal6matrixIfLi3ELi3EvEEPS_.exit

_Z4v_17Dv2_fDv4_ffN5metal6matrixIfLi3ELi3EvEEPS_.exit: ; preds = %if.else.i, %if.then.i
  %retval.0.i = phi <4 x float> [ %vecinit7.i, %if.then.i ], [ %vecinit22.i, %if.else.i ]
  %tint_member_16 = getelementptr inbounds %struct.tint_struct_5, ptr addrspace(2) %v_44.coerce3, i64 0, i32 0, i32 0
  %28 = load <4 x float>, ptr addrspace(2) %tint_member_16, align 16
  %29 = shufflevector <4 x float> %28, <4 x float> poison, <2 x i32> <i32 2, i32 3>
  %30 = shufflevector <4 x float> %retval.0.i, <4 x float> poison, <2 x i32> <i32 0, i32 1>
  %mul = fmul reassoc nsz arcp contract afn <2 x float> %29, %30
  %31 = tail call fast <2 x float> @air.sign.v2f32(<2 x float> %29) #7
  %32 = shufflevector <4 x float> %retval.0.i, <4 x float> poison, <2 x i32> <i32 3, i32 3>
  %mul24 = fmul reassoc nsz arcp contract afn <2 x float> %31, %32
  %sub = fsub reassoc nsz arcp contract afn <2 x float> %mul, %mul24
  %33 = shufflevector <4 x float> %retval.0.i, <4 x float> poison, <2 x i32> <i32 2, i32 3>
  %vecinit27 = shufflevector <2 x float> %sub, <2 x float> %33, <4 x i32> <i32 0, i32 1, i32 2, i32 3>
  %tint_member_7 = getelementptr inbounds %struct.tint_struct_1, ptr %v_43, i64 0, i32 0
  store <4 x float> %vecinit27, ptr %tint_member_7, align 16, !tbaa !41
  %34 = load i32, ptr %v_44.coerce1, align 4, !tbaa !31
  %conv = zext i32 %34 to i64
  %tint_member_11 = getelementptr inbounds %struct.tint_struct_3, ptr addrspace(1) %v_44.coerce0, i64 0, i32 0, i32 0, i64 %conv, i32 0
  %35 = load <4 x float>, ptr addrspace(1) %tint_member_11, align 16, !tbaa !41
  %tint_member_8 = getelementptr inbounds %struct.tint_struct_1, ptr %v_43, i64 0, i32 1
  store <4 x float> %35, ptr %tint_member_8, align 16, !tbaa !41
  ret void
}

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare <2 x float> @air.sign.v2f32(<2 x float>) local_unnamed_addr #2

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare <2 x float> @air.mix.v2f32(<2 x float>, <2 x float>, <2 x float>) local_unnamed_addr #2

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare i1 @air.all.v2i1(<2 x i1>) local_unnamed_addr #2

; Function Attrs: mustprogress nofree nosync nounwind willreturn memory(none)
declare float @air.convert.f.f32.s.i32(i32) local_unnamed_addr #2

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.end.p0(i64 immarg, ptr captures(none)) #3

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr writeonly captures(none), i8, i64, i1 immarg) #4

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.start.p0(i64 immarg, ptr captures(none)) #3

attributes #0 = { mustprogress nofree nosync nounwind willreturn "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #1 = { mustprogress nofree nosync nounwind willreturn memory(argmem: readwrite) "approx-func-fp-math"="true" "frame-pointer"="all" "min-legal-vector-width"="128" "no-builtins" "no-infs-fp-math"="false" "no-nans-fp-math"="false" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }
attributes #2 = { mustprogress nofree nosync nounwind willreturn memory(none) }
attributes #3 = { nocallback nofree nosync nounwind willreturn memory(argmem: readwrite) }
attributes #4 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #5 = { nounwind }
attributes #6 = { nobuiltin nounwind "no-builtins" }
attributes #7 = { nounwind willreturn memory(none) }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7}
!llvm.ident = !{!8}
!air.version = !{!9}
!air.language_version = !{!10}
!air.compile_options = !{!11, !12, !13}
!air.vertex = !{!14}

!0 = !{i32 1, !"wchar_size", i32 4}
!1 = !{i32 7, !"frame-pointer", i32 2}
!2 = !{i32 7, !"air.max_device_buffers", i32 31}
!3 = !{i32 7, !"air.max_constant_buffers", i32 31}
!4 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!5 = !{i32 7, !"air.max_textures", i32 128}
!6 = !{i32 7, !"air.max_read_write_textures", i32 8}
!7 = !{i32 7, !"air.max_samplers", i32 16}
!8 = !{!"Apple metal version 32023.622 (metalfe-32023.622)"}
!9 = !{i32 2, i32 7, i32 0}
!10 = !{!"Metal", i32 3, i32 2, i32 0}
!11 = !{!"air.compile.denorms_disable"}
!12 = !{!"air.compile.fast_math_enable"}
!13 = !{!"air.compile.framebuffer_fetch_enable"}
!14 = !{ptr @dawn_entry_point, !15, !18}
!15 = !{!16, !17}
!16 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"tint_member_20"}
!17 = !{!"air.vertex_output", !"user(locn0)", !"air.arg_type_name", !"float4", !"air.arg_name", !"tint_member_21"}
!18 = !{!19, !20, !21, !25, !27}
!19 = !{i32 0, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"v_65"}
!20 = !{i32 1, !"air.instance_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"v_66"}
!21 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !22, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"tint_struct_3", !"air.arg_name", !"v_67"}
!22 = !{!"air.struct_type_info", !23, i32 0, i32 16, i32 0, !"tint_array", !"tint_member_10"}
!23 = !{!"air.struct_type_info", !24, i32 0, i32 16, i32 1, !"tint_struct_4", !"elements"}
!24 = !{i32 0, i32 16, i32 0, !"float4", !"tint_member_11"}
!25 = !{i32 3, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !26, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"tint_array", !"air.arg_name", !"v_68"}
!26 = !{i32 0, i32 4, i32 1, !"uint", !"elements"}
!27 = !{i32 4, !"air.buffer", !"air.location_index", i32 30, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !28, !"air.arg_type_size", i32 48, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"tint_struct_5", !"air.arg_name", !"v_69"}
!28 = !{!"air.struct_type_info", !29, i32 0, i32 32, i32 0, !"tint_struct_6", !"tint_member_15", i32 32, i32 4, i32 0, !"uint", !"tint_member_18", !"air.struct_type_info", !30, i32 36, i32 12, i32 0, !"tint_array", !"tint_member_19"}
!29 = !{i32 0, i32 16, i32 0, !"float4", !"tint_member_16", i32 16, i32 16, i32 0, !"float4", !"tint_member_17"}
!30 = !{i32 0, i32 4, i32 3, !"uint", !"elements"}
!31 = !{!32, !32, i64 0}
!32 = !{!"int", !33, i64 0}
!33 = !{!"omnipotent char", !34, i64 0}
!34 = !{!"Simple C++ TBAA"}
!35 = !{!36}
!36 = distinct !{!36, !37, !"air-alias-scope-arg(3)"}
!37 = distinct !{!37, !"air-alias-scopes(dawn_entry_point)"}
!38 = !{!39, !40}
!39 = distinct !{!39, !37, !"air-alias-scope-arg(2)"}
!40 = distinct !{!40, !37, !"air-alias-scope-arg(4)"}
!41 = !{!33, !33, i64 0}

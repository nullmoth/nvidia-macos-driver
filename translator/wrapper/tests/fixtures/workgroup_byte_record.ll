target triple = "air64-apple-macosx15.0.0"
@shared = internal addrspace(3) global [4 x { <4 x float>, i32 }] undef
define void @test(ptr addrspace(1) %out, i32 %tid) {
entry:
  %offset = mul i32 %tid, 32
  %base = getelementptr i8, ptr addrspace(3) @shared, i32 %offset
  %pad = getelementptr i8, ptr addrspace(3) %base, i32 20
  store <3 x i32> <i32 77, i32 88, i32 99>, ptr addrspace(3) %pad, align 4
  %f = uitofp i32 %tid to float
  %v0 = fadd float %f, 10.0
  %v1 = fadd float %f, 20.0
  %v2 = fadd float %f, 30.0
  %v3 = fadd float %f, 40.0
  %a = insertelement <4 x float> undef, float %v0, i32 0
  %b = insertelement <4 x float> %a, float %v1, i32 1
  %c = insertelement <4 x float> %b, float %v2, i32 2
  %v = insertelement <4 x float> %c, float %v3, i32 3
  %i = add i32 %tid, 100
  %s0 = insertvalue { <4 x float>, i32 } undef, <4 x float> %v, 0
  %s = insertvalue { <4 x float>, i32 } %s0, i32 %i, 1
  store { <4 x float>, i32 } %s, ptr addrspace(3) %base, align 16
  call void @air.wg.barrier(i32 2, i32 1)
  %next = add i32 %tid, 1
  %source = and i32 %next, 3
  %roff = mul i32 %source, 32
  %read = getelementptr i8, ptr addrspace(3) @shared, i32 %roff
  %all = load <8 x i32>, ptr addrspace(3) %read, align 16
  %dst = getelementptr <8 x i32>, ptr addrspace(1) %out, i32 %tid
  store <8 x i32> %all, ptr addrspace(1) %dst, align 16
  ret void
}
declare void @air.wg.barrier(i32, i32)
!air.kernel = !{!0}
!0 = !{ptr @test, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}

target triple = "spirv-unknown-vulkan1.2"
%U = type { i32, i32, i32, i32 }
define i32 @read_view(ptr %p, i64 %idx) {
entry:
 %q = getelementptr inbounds [16 x i8], ptr %p, i64 0, i64 %idx
 store i8 73, ptr %q, align 1
 %b = load i8, ptr %q, align 1
 %v = zext i8 %b to i32
 ret i32 %v
}

define void @k(ptr addrspace(1) %out, i32 %tid) {
entry:
 %u = alloca %U, align 16
 %small = and i32 %tid, 15
 %idx = zext i32 %small to i64
 %v = call i32 @read_view(ptr %u, i64 %idx)
 store i32 %v, ptr addrspace(1) %out, align 4
 ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}

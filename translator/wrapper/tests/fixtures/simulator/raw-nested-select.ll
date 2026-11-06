target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %tid) {
entry:
 %cond = icmp eq i32 %tid, 0
 %unused = sub i32 %tid, 1
 %idx = zext i32 %unused to i64
 %bp = getelementptr i32, ptr addrspace(1) %b, i64 %idx
 %inner = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %bp
 %choice = select i1 %cond, ptr addrspace(1) %inner, ptr addrspace(1) %b
 %v = load i32, ptr addrspace(1) %choice, align 4
 store i32 %v, ptr addrspace(1) %out, align 4
 ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}

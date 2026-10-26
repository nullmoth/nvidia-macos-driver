target triple = "air64_v28-apple-macosx26.5.0"
%Record = type { i8, i8, [4 x i8] }
%Config = type { [2 x %Record], i8 }
@fallback = internal addrspace(2) constant %Config zeroinitializer

define void @k(ptr addrspace(2) %config, ptr addrspace(1) %out, i32 %gid) {
entry:
  %choose_fallback = icmp eq i32 %gid, 0
  %selected = select i1 %choose_fallback, ptr addrspace(2) @fallback, ptr addrspace(2) %config
  %first = getelementptr inbounds %Config, ptr addrspace(2) %selected, i64 0, i32 0, i64 0, i32 0
  %value = load i8, ptr addrspace(2) %first, align 1
  %wide = zext i8 %value to i32
  %outp = getelementptr inbounds i32, ptr addrspace(1) %out, i32 %gid
  store i32 %wide, ptr addrspace(1) %outp, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 13, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"struct Config", !"air.arg_name", !"config"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}

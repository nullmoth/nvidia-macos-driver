target triple = "air64-apple-macosx15.8.0"
%struct.P = type { i32, i32, float, float, i32, i32 }

declare i32 @air.get_global_id.i32(i32)
declare float @air.cos.f32(float)
declare float @air.convert.f.f32.s.i32(i32)
declare i32 @air.convert.s.i32.f.f32(float)

define void @__entry_nested(ptr addrspace(1) %image, ptr addrspace(1) %pts, ptr addrspace(1) %out, ptr addrspace(1) %bits, ptr addrspace(2) %stride.p) {
entry:
  %stride = load i32, ptr addrspace(2) %stride.p, align 4
  tail call fastcc void @nested(ptr addrspace(1) %image, ptr addrspace(1) %pts, ptr addrspace(1) %out, ptr addrspace(1) %bits, i32 %stride)
  ret void
}

define internal fastcc void @nested(ptr addrspace(1) %image, ptr addrspace(1) %pts, ptr addrspace(1) %out, ptr addrspace(1) %bits, i32 %stride) {
entry:
  %g = call i32 @air.get_global_id.i32(i32 0)
  %i = sext i32 %g to i64
  %x.p = getelementptr inbounds %struct.P, ptr addrspace(1) %pts, i64 %i, i32 0
  %x = load i32, ptr addrspace(1) %x.p, align 4
  %a.p = getelementptr inbounds %struct.P, ptr addrspace(1) %pts, i64 %i, i32 3
  %a = load float, ptr addrspace(1) %a.p, align 4
  %tail.p = getelementptr inbounds %struct.P, ptr addrspace(1) %pts, i64 %i, i32 4
  %tail = load i64, ptr addrspace(1) %tail.p, align 4
  %cur = getelementptr inbounds i8, ptr addrspace(1) %image, i64 %i
  %r = tail call fastcc i32 @compare(i32 %stride, ptr addrspace(1) %cur, float %a)
  %b.p = getelementptr inbounds i32, ptr addrspace(1) %bits, i64 %i
  store i32 %r, ptr addrspace(1) %b.p, align 4
  %ox.p = getelementptr inbounds %struct.P, ptr addrspace(1) %out, i64 %i, i32 0
  store i32 %x, ptr addrspace(1) %ox.p, align 4
  %ot.p = getelementptr inbounds %struct.P, ptr addrspace(1) %out, i64 %i, i32 4
  store i64 %tail, ptr addrspace(1) %ot.p, align 4
  ret void
}

define internal fastcc i32 @compare(i32 %step, ptr addrspace(1) %cur, float %angle) {
entry:
  %c = tail call fastcc float @wcos(float %angle)
  %o = call i32 @air.convert.s.i32.f.f32(float %c)
  %off = mul nsw i32 %o, %step
  %idx = sext i32 %off to i64
  %p0 = getelementptr inbounds i8, ptr addrspace(1) %cur, i64 %idx
  %v0 = load i8, ptr addrspace(1) %p0, align 1
  %p1 = getelementptr inbounds i8, ptr addrspace(1) %cur, i64 1
  %v1 = load i8, ptr addrspace(1) %p1, align 1
  %lt = icmp ult i8 %v0, %v1
  %r = zext i1 %lt to i32
  ret i32 %r
}

define internal fastcc float @wcos(float %x) {
  %r = tail call float @air.cos.f32(float %x)
  ret float %r
}

!air.kernel = !{!0}
!0 = !{ptr @__entry_nested, !1, !2}
!1 = !{}
!2 = !{!3, !4, !6, !7, !8}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"image"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !5, !"air.arg_type_size", i32 24, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"P", !"air.arg_name", !"pts"}
!5 = !{i32 0, i32 4, i32 0, !"int", !"x", i32 4, i32 4, i32 0, !"int", !"y", i32 8, i32 4, i32 0, !"float", !"s", i32 12, i32 4, i32 0, !"float", !"a", i32 16, i32 4, i32 0, !"int", !"rx", i32 20, i32 4, i32 0, !"int", !"ry"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !5, !"air.arg_type_size", i32 24, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"P", !"air.arg_name", !"out"}
!7 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"bits"}
!8 = !{i32 4, !"air.constant", !"air.location_index", i32 4, i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"stride"}

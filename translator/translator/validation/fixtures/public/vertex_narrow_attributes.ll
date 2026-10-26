target datalayout = "e-p:64:64:64"
target triple = "air64-apple-macosx14.0.0"

define void @vertex_narrow_attributes(i8 %byte, <2 x i16> %words, ptr addrspace(1) %output, i32 %vertex_id) {
  %word0 = extractelement <2 x i16> %words, i64 0
  %word1 = extractelement <2 x i16> %words, i64 1
  %byte32 = zext i8 %byte to i32
  %word032 = zext i16 %word0 to i32
  %word132 = zext i16 %word1 to i32
  %sum0 = add i32 %byte32, %word032
  %sum1 = add i32 %sum0, %word132
  %index = zext i32 %vertex_id to i64
  %address = getelementptr i32, ptr addrspace(1) %output, i64 %index
  store i32 %sum1, ptr addrspace(1) %address, align 4
  ret void
}

!air.vertex = !{!0}
!0 = !{ptr @vertex_narrow_attributes, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"uchar"}
!4 = !{i32 1, !"air.vertex_input", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"ushort2"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*"}
!6 = !{i32 3, !"air.vertex_id", !"air.arg_type_name", !"uint"}

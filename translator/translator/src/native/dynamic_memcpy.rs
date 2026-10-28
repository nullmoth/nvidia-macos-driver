use super::air_text::{call_arguments, last_token, split_args, LineBuffer};
use crate::fxhash::HashMap;
use std::borrow::Cow;

struct DynamicMemcpy {
    dst_ty: String,
    src_ty: String,
    len_ty: String,
    dst: String,
    src: String,
    len: String,
}

pub(crate) fn lower_dynamic_length_memcpy(san_ll: &str) -> Cow<'_, str> {
    let splits = plan_splits(san_ll);
    if splits.is_empty() {
        return Cow::Borrowed(san_ll);
    }
    let mut out = LineBuffer::with_capacity(san_ll.len().saturating_add(san_ll.len() / 8));
    let mut label: Option<String> = None;
    let mut next = 0usize;
    for line in san_ll.lines() {
        if let Some(block) = block_label(line) {
            label = Some(block.to_string());
            out.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        let Some(call) = parse_dynamic_memcpy(trimmed) else {
            out.push(&rewrite_phi_predecessors(line, &splits));
            continue;
        };
        let Some(predecessor) = label.clone() else {
            out.push(line);
            continue;
        };
        let indent = &line[..line.len() - trimmed.len()];
        emit_copy_loop(&mut out, indent, &call, &predecessor, next);
        label = Some(format!("__dmc_done{next}"));
        next += 1;
    }
    if san_ll.ends_with('\n') {
        out.text.push('\n');
    }
    Cow::Owned(out.text)
}

pub(crate) fn lower_dynamic_length_memcpy_owned(san_ll: String) -> String {
    match lower_dynamic_length_memcpy(&san_ll) {
        Cow::Borrowed(_) => san_ll,
        Cow::Owned(lowered) => lowered,
    }
}

fn plan_splits(san_ll: &str) -> HashMap<String, String> {
    let mut splits = HashMap::default();
    let mut label: Option<&str> = None;
    let mut original: Option<String> = None;
    let mut next = 0usize;
    for line in san_ll.lines() {
        if let Some(block) = block_label(line) {
            label = Some(block);
            original = None;
            continue;
        }
        if parse_dynamic_memcpy(line.trim_start()).is_none() {
            continue;
        }
        let Some(block) = label else { continue };
        let start = original.get_or_insert_with(|| block.to_string()).clone();
        splits.insert(start, format!("__dmc_done{next}"));
        next += 1;
    }
    splits
}

fn block_label(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let name = line.split(':').next()?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
        && line[name.len()..].starts_with(':'))
    .then_some(name)
}

fn rewrite_phi_predecessors(line: &str, splits: &HashMap<String, String>) -> String {
    if !line.contains(" phi ") {
        return line.to_string();
    }
    let mut rewritten = line.to_string();
    for (from, to) in splits {
        rewritten = rewritten.replace(&format!(", %{from} ]"), &format!(", %{to} ]"));
    }
    rewritten
}

fn emit_copy_loop(
    out: &mut LineBuffer,
    indent: &str,
    call: &DynamicMemcpy,
    predecessor: &str,
    n: usize,
) {
    let (dst_ty, src_ty, len_ty) = (&call.dst_ty, &call.src_ty, &call.len_ty);
    let (dst, src, len) = (&call.dst, &call.src, &call.len);
    out.push_fmt(format_args!("{indent}br label %__dmc_head{n}"));
    out.push_fmt(format_args!("__dmc_head{n}:"));
    out.push_fmt(format_args!(
        "{indent}%__dmc_i{n} = phi {len_ty} [ 0, %{predecessor} ], [ %__dmc_next{n}, %__dmc_body{n} ]"
    ));
    out.push_fmt(format_args!(
        "{indent}%__dmc_more{n} = icmp ult {len_ty} %__dmc_i{n}, {len}"
    ));
    out.push_fmt(format_args!(
        "{indent}br i1 %__dmc_more{n}, label %__dmc_body{n}, label %__dmc_done{n}"
    ));
    out.push_fmt(format_args!("__dmc_body{n}:"));
    out.push_fmt(format_args!(
        "{indent}%__dmc_src{n} = getelementptr i8, {src_ty} {src}, {len_ty} %__dmc_i{n}"
    ));
    out.push_fmt(format_args!(
        "{indent}%__dmc_byte{n} = load i8, {src_ty} %__dmc_src{n}, align 1"
    ));
    out.push_fmt(format_args!(
        "{indent}%__dmc_dst{n} = getelementptr i8, {dst_ty} {dst}, {len_ty} %__dmc_i{n}"
    ));
    out.push_fmt(format_args!(
        "{indent}store i8 %__dmc_byte{n}, {dst_ty} %__dmc_dst{n}, align 1"
    ));
    out.push_fmt(format_args!(
        "{indent}%__dmc_next{n} = add {len_ty} %__dmc_i{n}, 1"
    ));
    out.push_fmt(format_args!("{indent}br label %__dmc_head{n}"));
    out.push_fmt(format_args!("__dmc_done{n}:"));
}

fn parse_dynamic_memcpy(line: &str) -> Option<DynamicMemcpy> {
    if !line.contains("call") || line.starts_with("declare") {
        return None;
    }
    let suffix = line.split("@llvm.memcpy.").nth(1)?;
    let mut spaces = suffix.split('(').next()?.split('.');
    let dst_ty = pointer_type(spaces.next()?)?;
    let src_ty = pointer_type(spaces.next()?)?;
    let len_ty = spaces.next()?.to_string();
    if !len_ty.starts_with('i') || len_ty[1..].parse::<u32>().is_err() {
        return None;
    }
    let args = split_args(call_arguments(line)?);
    if args.len() != 4 || last_token(&args[3]) != "false" {
        return None;
    }
    let len = last_token(&args[2]).to_string();
    if !len.starts_with('%') {
        return None;
    }
    Some(DynamicMemcpy {
        dst_ty,
        src_ty,
        len_ty,
        dst: last_token(&args[0]).to_string(),
        src: last_token(&args[1]).to_string(),
        len,
    })
}

fn pointer_type(space: &str) -> Option<String> {
    match space.strip_prefix('p')?.parse::<u32>().ok()? {
        0 => Some("ptr".to_string()),
        n => Some(format!("ptr addrspace({n})")),
    }
}

#[cfg(test)]
mod tests {
    use super::lower_dynamic_length_memcpy;
    use std::borrow::Cow;

    fn module(body: &str) -> String {
        format!("define void @k() {{\nentry:\n  br label %19\n19:\n{body}  br label %24\n24:\n  ret void\n}}\n")
    }

    const DYNAMIC: &str = "  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) align 1 %21, \
                           ptr addrspace(1) align 1 %22, i64 %23, i1 false) #2, !alias.scope !30\n";

    #[test]
    fn a_constant_length_copy_is_left_for_the_emitter_to_unroll() {
        let ll = module(
            "  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) %a, ptr addrspace(1) %b, \
             i64 48, i1 false)\n",
        );
        assert!(matches!(
            lower_dynamic_length_memcpy(&ll),
            Cow::Borrowed(value) if std::ptr::eq(value, ll.as_str())
        ));
    }

    #[test]
    fn a_volatile_copy_is_not_rewritten_into_a_plain_loop() {
        let ll = module(&DYNAMIC.replace("i1 false", "i1 true"));
        assert!(matches!(
            lower_dynamic_length_memcpy(&ll),
            Cow::Borrowed(value) if std::ptr::eq(value, ll.as_str())
        ));
    }

    #[test]
    fn a_lone_declaration_is_not_a_call() {
        let ll =
            "declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)\n";
        assert!(matches!(
            lower_dynamic_length_memcpy(ll),
            Cow::Borrowed(value) if std::ptr::eq(value, ll)
        ));
    }

    #[test]
    fn a_dynamic_length_copy_becomes_a_byte_loop_in_the_calling_block() {
        let ll = module(DYNAMIC);
        let lowered = lower_dynamic_length_memcpy(&ll);
        assert!(!lowered.contains("@llvm.memcpy.p1.p1.i64(ptr"), "{lowered}");
        assert!(!lowered.contains("call void @__"), "{lowered}");
        assert!(
            lowered.contains("%__dmc_i0 = phi i64 [ 0, %19 ], [ %__dmc_next0, %__dmc_body0 ]"),
            "{lowered}"
        );
        assert!(lowered.contains("icmp ult i64 %__dmc_i0, %23"), "{lowered}");
        assert!(
            lowered.contains("%__dmc_src0 = getelementptr i8, ptr addrspace(1) %22, i64 %__dmc_i0"),
            "{lowered}"
        );
        assert!(
            lowered.contains("store i8 %__dmc_byte0, ptr addrspace(1) %__dmc_dst0, align 1"),
            "{lowered}"
        );
        assert!(lowered.contains("__dmc_done0:"), "{lowered}");
    }

    #[test]
    fn a_successor_phi_follows_the_split_block_to_its_loop_exit() {
        let ll = format!(
            "define void @k() {{\nentry:\n  br label %19\n19:\n{DYNAMIC}  br label %24\n24:\n  \
             %26 = phi i32 [ 0, %entry ], [ 1, %19 ]\n  ret void\n}}\n"
        );
        let lowered = lower_dynamic_length_memcpy(&ll);
        assert!(
            lowered.contains("%26 = phi i32 [ 0, %entry ], [ 1, %__dmc_done0 ]"),
            "{lowered}"
        );
        assert!(lowered.contains("  br label %19\n"), "{lowered}");
    }

    #[test]
    fn an_implicit_entry_block_is_left_alone() {
        let ll = format!("define void @k() {{\n{DYNAMIC}  ret void\n}}\n");
        assert!(matches!(
            lower_dynamic_length_memcpy(&ll),
            Cow::Borrowed(value) if std::ptr::eq(value, ll.as_str())
        ));
    }
}

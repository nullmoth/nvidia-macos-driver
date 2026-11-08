use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

pub fn specialize_air_function_constants<'a>(
    air_ll: &'a str,
    values: &[(u32, Vec<u8>)],
) -> Result<Cow<'a, str>, String> {
    let specialized = specialize_air_function_constants_unfolded(air_ll, values)?;
    // Fields a static initializer copies from the constants just supplied are folded too, so the branches they decide
    // are gone before translation instead of after it (crate::meta::fold_static_init_struct_fields).
    Ok(match crate::meta::fold_static_init_struct_fields(&specialized) {
        Some(folded) => Cow::Owned(folded),
        None => specialized,
    })
}

pub(crate) fn specialize_air_function_constants_unfolded<'a>(
    air_ll: &'a str,
    values: &[(u32, Vec<u8>)],
) -> Result<Cow<'a, str>, String> {
    if values.is_empty() {
        return Ok(Cow::Borrowed(air_ll));
    }

    let mut requested = BTreeMap::<u32, &[u8]>::new();
    for (index, bytes) in values {
        if requested.insert(*index, bytes).is_some() {
            return Err(format!(
                "AIR function-constant specialization contains duplicate index {index}"
            ));
        }
    }

    let constants = crate::meta::parse_function_constants(air_ll)
        .into_iter()
        .map(|constant| (constant.index, constant))
        .collect::<BTreeMap<_, _>>();
    let ignored = requested
        .keys()
        .filter(|index| !constants.contains_key(index))
        .copied()
        .collect::<Vec<_>>();
    for index in &ignored {
        requested.remove(index);
    }
    if requested.is_empty() {
        return Ok(Cow::Borrowed(air_ll));
    }

    let literals = requested
        .iter()
        .map(|(index, bytes)| {
            let constant = &constants[index];
            llvm_typed_literal(&constant.type_name, &constant.abi_type_encoding, bytes)
                .map(|literal| (*index, (constant.type_name.as_str(), literal)))
                .map_err(|error| format!("function constant {index}: {error}"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut replaced = BTreeSet::new();
    let mut output = String::with_capacity(air_ll.len());
    for line in air_ll.split_inclusive('\n') {
        if line.trim_start().starts_with("declare ") {
            output.push_str(line);
            continue;
        }
        if line.contains("@air.is_function_constant_defined(") {
            let Some(index) = crate::fc_specialize::fc_init_index(line) else {
                return Err(
                    "air.is_function_constant_defined has no MTL_FC_INIT operand".to_string(),
                );
            };
            if requested.contains_key(&index) {
                let (result, _) = line.split_once(" = ").ok_or_else(|| {
                    format!(
                        "function constant {index}: malformed air.is_function_constant_defined call"
                    )
                })?;
                output.push_str(result);
                output.push_str(" = icmp eq i32 0, 0");
                if line.ends_with('\n') {
                    output.push('\n');
                }
                continue;
            }
        }
        let declaration = line.trim_start();
        if !declaration.starts_with('@') || !declaration.contains(" = ") {
            output.push_str(line);
            continue;
        }
        let Some(index) = crate::fc_specialize::fc_init_index(line) else {
            output.push_str(line);
            continue;
        };
        let Some((type_name, literal)) = literals.get(&index) else {
            output.push_str(line);
            continue;
        };
        let in_section = line.contains("air.fc_initializer");
        let constant_needle = format!(" constant {type_name} undef");
        let global_needle = format!(" global {type_name} undef");
        let replacement = if line.contains(&constant_needle) {
            Some((constant_needle, format!(" constant {literal}")))
        } else if line.contains(&global_needle) {
            Some((global_needle, format!(" global {literal}")))
        } else {
            None
        };
        let Some((needle, replacement)) = replacement else {
            if !in_section {
                output.push_str(line);
                continue;
            }
            return Err(format!(
                "function constant {index}: air.fc_initializer is not an undef {type_name} global"
            ));
        };
        output.push_str(&line.replacen(&needle, &replacement, 1));
        replaced.insert(index);
    }

    let missing = requested
        .keys()
        .filter(|index| !replaced.contains(index))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "AIR function-constant specialization did not replace index(es) {missing:?}"
        ));
    }
    Ok(Cow::Owned(
        crate::meta::specialize_function_constant_metadata(&output),
    ))
}

fn llvm_typed_literal(
    type_name: &str,
    abi_type_encoding: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let bool_abi = abi_type_encoding == "b"
        || abi_type_encoding
            .rsplit_once('_')
            .is_some_and(|(_, scalar)| scalar == "b");
    if let Some(inner) = type_name
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    {
        let (lanes, scalar) = inner
            .split_once(" x ")
            .ok_or_else(|| format!("unsupported vector type `{type_name}`"))?;
        let lanes = lanes
            .parse::<usize>()
            .map_err(|error| format!("invalid vector lane count in `{type_name}`: {error}"))?;
        if !(1..=4).contains(&lanes) {
            return Err(format!("unsupported vector lane count {lanes}"));
        }
        let scalar_size = scalar_byte_size(scalar)?;
        let required = scalar_size
            .checked_mul(lanes)
            .ok_or_else(|| "function-constant payload size overflow".to_string())?;
        if bytes.len() != required {
            return Err(format!(
                "payload has {} bytes, {type_name} requires {required}",
                bytes.len()
            ));
        }
        let elements = bytes
            .chunks_exact(scalar_size)
            .map(|lane| {
                scalar_literal(scalar, lane, bool_abi).map(|literal| format!("{scalar} {literal}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(format!("{type_name} <{}>", elements.join(", ")));
    }

    let required = scalar_byte_size(type_name)?;
    if bytes.len() != required {
        return Err(format!(
            "payload has {} bytes, {type_name} requires {required}",
            bytes.len()
        ));
    }
    scalar_literal(type_name, bytes, bool_abi).map(|literal| format!("{type_name} {literal}"))
}

fn scalar_byte_size(type_name: &str) -> Result<usize, String> {
    match type_name {
        "i1" | "i8" => Ok(1),
        "i16" | "half" | "bfloat" => Ok(2),
        "i32" | "float" => Ok(4),
        "i64" | "double" => Ok(8),
        _ => Err(format!("unsupported function-constant type `{type_name}`")),
    }
}

fn scalar_literal(type_name: &str, bytes: &[u8], bool_abi: bool) -> Result<String, String> {
    let unsigned = || {
        let mut word = [0u8; 8];
        word[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(word)
    };
    match type_name {
        "i1" => match bytes[0] {
            0 => Ok("0".into()),
            1 => Ok("1".into()),
            value => Err(format!("boolean payload must be 0 or 1, got {value}")),
        },
        "i8" | "i16" | "i32" | "i64" if bool_abi => match unsigned() {
            0 => Ok("0".into()),
            1 => Ok("1".into()),
            value => Err(format!("boolean payload must be 0 or 1, got {value}")),
        },
        "i8" | "i16" | "i32" | "i64" => Ok(unsigned().to_string()),
        "half" => Ok(format!("0xH{:04X}", unsigned())),
        "bfloat" => Ok(format!("0xR{:04X}", unsigned())),
        "float" => {
            let bits = u32::from_le_bytes(bytes.try_into().expect("four-byte payload checked"));
            let widened = f64::from(f32::from_bits(bits));
            let roundtrip = (widened as f32).to_bits();
            if roundtrip != bits {
                return Err(format!(
                    "float payload 0x{bits:08x} cannot be represented exactly as an LLVM hexadecimal literal"
                ));
            }
            Ok(format!("0x{:016X}", widened.to_bits()))
        }
        "double" => Ok(format!("0x{:016X}", unsigned())),
        _ => Err(format!("unsupported function-constant type `{type_name}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specializes_scalar_and_vector_initializer_globals() {
        let ll = r#"@enabled.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@lanes.MTL_FC_INIT_3_Dv2_j = internal addrspace(2) externally_initialized constant <2 x i32> undef, section "air.fc_initializer", align 8
define void @init() {
  %defined = call i1 @air.is_function_constant_defined(ptr addrspace(2) @enabled.MTL_FC_INIT_0_b)
  ret void
}
"#;
        let specialized = specialize_air_function_constants(
            ll,
            &[
                (0, vec![1]),
                (3, [7u32.to_le_bytes(), 11u32.to_le_bytes()].concat()),
            ],
        )
        .expect("specialize");
        assert!(specialized.contains("constant i8 1, section"));
        assert!(specialized.contains("constant <2 x i32> <i32 7, i32 11>, section"));
        assert!(specialized.contains("%defined = icmp eq i32 0, 0"));

        let supplied_zero = specialize_air_function_constants(ll, &[(0, vec![0])])
            .expect("specialize a defined false value");
        assert!(supplied_zero.contains("constant i8 0, section"));
        assert!(supplied_zero.contains("%defined = icmp eq i32 0, 0"));
    }

    #[test]
    fn specializes_sectionless_initializer_globals_as_no_mans_sky_declares_them() {
        let ll = r#"@_Z20use_discard_fragment.MTL_FC_INIT_1_b = internal unnamed_addr addrspace(2) externally_initialized constant i8 undef, align 1
define void @init() {
  %v = load i8, i8 addrspace(2)* @_Z20use_discard_fragment.MTL_FC_INIT_1_b, align 1
  ret void
}
"#;
        let specialized = specialize_air_function_constants(ll, &[(1, vec![1])])
            .expect("a sectionless initializer global must specialize by its shape");
        assert!(specialized.contains("externally_initialized constant i8 1, align 1"));
        let copy = "@work.MTL_FC_INIT_1_b = internal addrspace(2) global i8 0, align 1\n";
        let ll2 = format!("{copy}{ll}");
        let specialized2 = specialize_air_function_constants(&ll2, &[(1, vec![1])])
            .expect("a working copy beside the initializer must not refuse the specialization");
        assert!(specialized2
            .contains("@work.MTL_FC_INIT_1_b = internal addrspace(2) global i8 0, align 1"));
        assert!(specialized2.contains("externally_initialized constant i8 1, align 1"));
    }

    #[test]
    fn specialized_predicates_select_metadata_roles_in_both_directions() {
        let ll = r#"@state.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@enabled = internal addrspace(2) global i8 0, align 1
@disabled = internal addrspace(2) global i8 0, align 1
define internal void @_GLOBAL__sub_I_metadata() section "air.static_init" {
  %state = load i8, ptr addrspace(2) @state.MTL_FC_INIT_0_b
  %inverse = xor i8 %state, 1
  store i8 %state, ptr addrspace(2) @enabled
  store i8 %inverse, ptr addrspace(2) @disabled
  ret void
}
!0 = !{i32 0, !"air.function_constant", !2, !"air.buffer", !"air.location_index", i32 3}
!1 = !{i32 1, !"air.function_constant", !3, !"air.texture", !"air.location_index", i32 4}
!2 = !{ptr addrspace(2) @enabled, !"bool", !"enabled"}
!3 = !{ptr addrspace(2) @disabled, !"bool", !"disabled"}
"#;

        let specialized = specialize_air_function_constants(ll, &[(0, vec![1])])
            .expect("specialize metadata predicates");
        assert!(
            specialized.contains("!0 = !{i32 0, !\"air.buffer\", !\"air.location_index\", i32 3}")
        );
        assert!(specialized
            .contains("!1 = !{i32 1, !\"air.function_constant_disabled\", !3, !\"air.texture\""));
        assert!(!specialized.contains("!0 = !{i32 0, !\"air.function_constant\""));
        let vertex = format!(
            "{specialized}!air.vertex = !{{!4}}\n!4 = !{{ptr @main, !5, !6}}\n!5 = !{{!1}}\n!6 = !{{}}\n"
        );
        assert_eq!(
            crate::meta::parse_air_vertex_meta(&vertex)
                .and_then(|meta| meta.output_role_of(0).cloned()),
            Some(crate::meta::VertOutRole::FunctionConstantDisabled)
        );
    }

    // Core Animation's shape, shrunk: the static initializer copies function constants into an internal config struct
    // (one field behind a branch, one inside a packed nested struct), and the shader reads every field through
    // `select (has_config == 0), @config, %buffer`.
    const CONFIG_STRUCT: &str = r#"%T = type { [2 x %I], i8, i8 }
%I = type <{ i8, <2 x i8>, i8 }>
@fc_generic.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@fc_mode.MTL_FC_INIT_1_h = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@fc_pair.MTL_FC_INIT_2_Dv2_h = internal addrspace(2) externally_initialized constant <2 x i8> undef, section "air.fc_initializer", align 2
@has_config = internal unnamed_addr addrspace(2) global i8 0, align 1
@config = internal unnamed_addr addrspace(2) global %T zeroinitializer, align 4
define internal void @_GLOBAL__sub_I_t() section "air.static_init" {
  %g = load i8, ptr addrspace(2) @fc_generic.MTL_FC_INIT_0_b, align 1
  store i8 %g, ptr addrspace(2) @has_config, align 1
  %m = load i8, ptr addrspace(2) @fc_mode.MTL_FC_INIT_1_h, align 1
  %p = load <2 x i8>, ptr addrspace(2) @fc_pair.MTL_FC_INIT_2_Dv2_h, align 2
  store i8 %m, ptr addrspace(2) @config, align 4
  store <2 x i8> %p, ptr addrspace(2) getelementptr inbounds (%T, ptr addrspace(2) @config, i64 0, i32 0, i64 1, i32 1), align 1
  %big = icmp ugt i8 %m, 2
  br i1 %big, label %hi, label %lo
hi:
  store i8 70, ptr addrspace(2) getelementptr inbounds (%T, ptr addrspace(2) @config, i64 0, i32 1), align 1
  br label %done
lo:
  store i8 50, ptr addrspace(2) getelementptr inbounds (%T, ptr addrspace(2) @config, i64 0, i32 1), align 1
  br label %done
done:
  ret void
}
define void @main(ptr addrspace(2) %buf) {
  %h = load i8, ptr addrspace(2) @has_config, align 1
  %use_fc = icmp eq i8 %h, 0
  %cfg = select i1 %use_fc, ptr addrspace(2) @config, ptr addrspace(2) %buf
  %f0 = load i8, ptr addrspace(2) %cfg, align 1
  %a = getelementptr inbounds %T, ptr addrspace(2) %cfg, i64 0, i32 0, i64 1, i32 1
  %pair = load <2 x i8>, ptr addrspace(2) %a, align 1
  %b = getelementptr inbounds %T, ptr addrspace(2) %cfg, i64 0, i32 1
  %level = load i8, ptr addrspace(2) %b, align 1
  %c = getelementptr inbounds %T, ptr addrspace(2) %cfg, i64 0, i32 2
  %never = load i8, ptr addrspace(2) %c, align 1
  %sum = add i8 %f0, %level
  %lane = extractelement <2 x i8> %pair, i32 1
  %all = add i8 %sum, %never
  ret void
}
"#;

    #[test]
    fn config_struct_fields_fold_into_the_shader() {
        let out = specialize_air_function_constants(CONFIG_STRUCT, &[(0, vec![0]), (1, vec![3]), (2, vec![7, 5])])
            .expect("specialize");
        // the initializer took its `hi` arm (mode 3 > 2); a field it never wrote reads the zero initializer
        assert!(out.contains("%sum = add i8 3, 70"), "{out}");
        assert!(out.contains("%lane = extractelement <2 x i8> <i8 7, i8 5>, i32 1"), "{out}");
        assert!(out.contains("%all = add i8 %sum, 0"), "{out}");
        assert!(!out.contains("= load i8, ptr addrspace(2) %b"), "{out}");
        // numbered values keep their definitions, so the module still parses in order
        assert!(out.contains("%level = or i8 0, 70"), "{out}");
    }

    #[test]
    fn config_struct_fold_follows_the_initializers_branches() {
        let out = specialize_air_function_constants(CONFIG_STRUCT, &[(0, vec![0]), (1, vec![1]), (2, vec![0, 0])])
            .expect("specialize");
        assert!(out.contains("%sum = add i8 1, 50"), "the `lo` arm's store, not the last one in the text: {out}");
    }

    #[test]
    fn config_struct_is_not_folded_when_the_select_picks_the_buffer() {
        let out = specialize_air_function_constants(CONFIG_STRUCT, &[(0, vec![1]), (1, vec![3]), (2, vec![7, 5])])
            .expect("specialize");
        assert!(out.contains("%f0 = load i8, ptr addrspace(2) %cfg"), "{out}");
        assert!(out.contains("%sum = add i8 %f0, %level"), "{out}");
    }

    #[test]
    fn config_struct_written_outside_the_initializer_is_not_folded() {
        let ll = CONFIG_STRUCT.replace(
            "  %sum = add i8 %f0, %level\n",
            "  store i8 9, ptr addrspace(2) %b, align 1\n  %sum = add i8 %f0, %level\n",
        );
        let out = specialize_air_function_constants(&ll, &[(0, vec![0]), (1, vec![3]), (2, vec![7, 5])])
            .expect("specialize");
        assert!(out.contains("%sum = add i8 %f0, %level"), "{out}");
        assert!(out.contains("%level = load i8"), "{out}");
    }

    #[test]
    fn config_struct_partial_overwrite_is_not_folded() {
        // a wider store over the pair's bytes after it was written: the pair's value is no longer one store's
        let ll = CONFIG_STRUCT.replace(
            "  %big = icmp ugt i8 %m, 2\n",
            "  store i16 513, ptr addrspace(2) getelementptr inbounds (%T, ptr addrspace(2) @config, i64 0, i32 0, i64 1, i32 0), align 1\n  %big = icmp ugt i8 %m, 2\n",
        );
        let out = specialize_air_function_constants(&ll, &[(0, vec![0]), (1, vec![3]), (2, vec![7, 5])])
            .expect("specialize");
        assert!(out.contains("%pair = load <2 x i8>"), "{out}");
        assert!(out.contains("%sum = add i8 3, 70"), "the untouched fields still fold: {out}");
    }

    #[test]
    fn rejects_unknown_and_malformed_values() {
        let ll = r#"@enabled.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
"#;
        assert!(specialize_air_function_constants(ll, &[(9, vec![1])]).is_ok());
        assert!(specialize_air_function_constants(ll, &[(0, vec![2])]).is_err());
        assert!(specialize_air_function_constants(ll, &[(0, vec![1, 0])]).is_err());
    }
}

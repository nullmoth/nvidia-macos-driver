use crate::fxhash::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AirType {
    Scalar(AirScalar),
    Vec { scalar: AirScalar, lanes: u32 },
    PackedVec { scalar: AirScalar, lanes: u32 },
    Array { elem: Box<AirType>, len: u32 },
    Matrix {
        scalar: AirScalar,
        cols: u32,
        rows: u32,
    },
    Struct(Vec<AirMember>),
    Opaque { size: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AirMember {
    pub offset: u32,
    pub ty: AirType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AirScalar {
    Float,
    Half,
    UInt,
    SInt,
    ULong,
    SLong,
    UShort,
    SShort,
    UChar,
    Bool,
}

pub fn primitive_air_type_from_name(name: &str) -> Option<AirType> {
    let raw = name.trim();
    let (n, packed) = match raw.strip_prefix("packed_") {
        Some(n) => (n, true),
        None => (raw, false),
    };
    for (base, scalar) in [
        ("ulong", AirScalar::ULong),
        ("long", AirScalar::SLong),
        ("ushort", AirScalar::UShort),
        ("short", AirScalar::SShort),
        ("uchar", AirScalar::UChar),
        ("char", AirScalar::UChar),
        ("float", AirScalar::Float),
        ("half", AirScalar::Half),
        ("uint", AirScalar::UInt),
        ("int", AirScalar::SInt),
        ("bool", AirScalar::Bool),
    ] {
        if let Some(rest) = n.strip_prefix(base) {
            return parse_dims(rest, scalar, packed);
        }
    }
    None
}

fn member_air_type(name: &str, size: u32) -> AirType {
    primitive_air_type_from_name(name).unwrap_or(AirType::Opaque { size })
}

fn parse_dims(rest: &str, scalar: AirScalar, packed: bool) -> Option<AirType> {
    if rest.is_empty() {
        return Some(AirType::Scalar(scalar));
    }
    if let Some((c, r)) = rest.split_once('x') {
        let cols = c.trim().parse().ok()?;
        let rows = r.trim().parse().ok()?;
        return Some(AirType::Matrix { scalar, cols, rows });
    }
    match rest.trim().parse::<u32>() {
        Ok(n) if n >= 1 && packed => Some(AirType::PackedVec { scalar, lanes: n }),
        Ok(n) if n >= 2 => Some(AirType::Vec { scalar, lanes: n }),
        _ => None,
    }
}

pub(super) enum Tok {
    Int(u32),
    Str(String),
    Ref(u32),
}

pub(super) fn tokenize(body: &str) -> Vec<Tok> {
    let mut out = vec![];
    for field in body.split(',') {
        let f = field.trim();
        if let Some(rest) = f.strip_prefix("i32 ") {
            if let Ok(v) = rest.trim().parse::<u32>() {
                out.push(Tok::Int(v));
            }
        } else if let Some(rest) = f.strip_prefix("!\"") {
            out.push(Tok::Str(rest.strip_suffix('"').unwrap_or(rest).to_string()));
        } else if let Some(rest) = f.strip_prefix('!') {
            if let Ok(v) = rest.trim().parse::<u32>() {
                out.push(Tok::Ref(v));
            }
        }
    }
    out
}

pub(super) struct MemberTuple {
    pub(super) offset: u32,
    pub(super) size: u32,
    pub(super) array_len: u32,
    pub(super) tyname: String,
    pub(super) nested: Option<u32>,
    pub(super) argument_node: Option<u32>,
    pub(super) argument_wrapper_id: Option<u32>,
}

impl MemberTuple {
    pub(super) fn declared_extent(&self) -> u64 {
        u64::from(self.size) * u64::from(self.array_len.max(1))
    }
}

pub(super) fn member_tuples(toks: &[Tok]) -> Vec<MemberTuple> {
    let mut out = vec![];
    let mut i = 0;
    while i < toks.len() {
        let mut nested = None;
        if let (Some(Tok::Str(s)), Some(Tok::Ref(x))) = (toks.get(i), toks.get(i + 1)) {
            if s == "air.struct_type_info" {
                nested = Some(*x);
                i += 2;
            }
        }
        let (offset, size, array_len, tyname) = match (
            toks.get(i),
            toks.get(i + 1),
            toks.get(i + 2),
            toks.get(i + 3),
        ) {
            (
                Some(Tok::Int(offset)),
                Some(Tok::Int(size)),
                Some(Tok::Int(array_len)),
                Some(Tok::Str(t)),
            ) => (*offset, *size, *array_len, t.clone()),
            _ => break,
        };
        i += 5;
        let mut argument_node = None;
        let mut argument_wrapper_id = None;
        while i < toks.len() && !struct_member_starts_at(toks, i) {
            match (toks.get(i), toks.get(i + 1)) {
                (Some(Tok::Str(s)), Some(Tok::Ref(x))) if s == "air.indirect_argument" => {
                    argument_node = Some(*x);
                }
                (Some(Tok::Str(s)), Some(Tok::Int(id))) if s == "air.indirect_argument" => {
                    argument_wrapper_id = Some(*id);
                }
                _ => {}
            }
            i += 1;
        }
        out.push(MemberTuple {
            offset,
            size,
            array_len,
            tyname,
            nested,
            argument_node,
            argument_wrapper_id,
        });
    }
    out
}

pub(super) fn parse_struct_info(
    nodes: &HashMap<u32, String>,
    id: u32,
    depth: u32,
) -> Option<AirType> {
    if depth > 16 {
        return None;
    }
    let body = nodes.get(&id)?;
    let tuples = member_tuples(&tokenize(body));
    if tuples.is_empty() || !members_are_disjoint(&tuples) {
        return None;
    }
    let members = tuples
        .into_iter()
        .map(|tuple| {
            let mut ty = match tuple.nested {
                Some(x) => parse_struct_info(nodes, x, depth + 1)
                    .unwrap_or_else(|| storage_air_type_for_size(tuple.size)),
                None if member_holds_resource_handle(nodes, tuple.argument_node) => {
                    storage_air_type_for_size(tuple.size)
                }
                None => member_air_type(&tuple.tyname, tuple.size),
            };
            if tuple.array_len > 0 {
                ty = AirType::Array {
                    elem: Box::new(ty),
                    len: tuple.array_len,
                };
            }
            AirMember {
                offset: tuple.offset,
                ty,
            }
        })
        .collect();
    Some(AirType::Struct(members))
}

fn member_holds_resource_handle(nodes: &HashMap<u32, String>, argument_node: Option<u32>) -> bool {
    let Some(body) = argument_node.and_then(|node| nodes.get(&node)) else {
        return false;
    };
    super::primary_role(&super::role_strings(body)).is_some_and(|role| role != "indirect_constant")
}

pub(super) fn struct_member_starts_at(toks: &[Tok], mut i: usize) -> bool {
    if let (Some(Tok::Str(s)), Some(Tok::Ref(_))) = (toks.get(i), toks.get(i + 1)) {
        if s == "air.struct_type_info" {
            i += 2;
        }
    }
    matches!(
        (
            toks.get(i),
            toks.get(i + 1),
            toks.get(i + 2),
            toks.get(i + 3),
            toks.get(i + 4),
        ),
        (
            Some(Tok::Int(_)),
            Some(Tok::Int(_)),
            Some(Tok::Int(_)),
            Some(Tok::Str(_)),
            Some(Tok::Str(_)),
        )
    )
}

fn members_are_disjoint(tuples: &[MemberTuple]) -> bool {
    tuples.windows(2).all(|pair| {
        u64::from(pair[0].offset) + pair[0].declared_extent() <= u64::from(pair[1].offset)
    })
}

pub(crate) fn storage_air_type_for_size(size: u32) -> AirType {
    match size {
        0 | 4 => AirType::Scalar(AirScalar::UInt),
        1 => AirType::Scalar(AirScalar::UChar),
        2 => AirType::Scalar(AirScalar::UShort),
        8 => AirType::Scalar(AirScalar::ULong),
        n if n % 4 == 0 => AirType::Array {
            elem: Box::new(AirType::Scalar(AirScalar::UInt)),
            len: n / 4,
        },
        n => AirType::Array {
            elem: Box::new(AirType::Scalar(AirScalar::UChar)),
            len: n,
        },
    }
}

pub(super) fn struct_info_ref(body: &str) -> Option<u32> {
    let p = body.find("air.struct_type_info")?;
    let after = &body[p..];
    let bang = after.find(", !")? + 3;
    let digits: String = after[bang..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

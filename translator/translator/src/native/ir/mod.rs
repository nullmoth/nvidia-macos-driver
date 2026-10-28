use super::parse::{
    is_ignored_intrinsic, parse_declaration, parse_function_header, parse_global, parse_type,
    strip_comment,
};
use crate::fxhash::{HashMap, HashSet};
use crate::meta::{self, AirScalar, AirType, KernRole};

mod alloca;
mod metadata_pointees;
mod ordinary_inline;
mod parse;
mod pointer_pointees;
mod raw_buffer;
mod static_init;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LlType {
    Void,
    Bool,
    Float,
    Half,
    BFloat,
    Int(u32),
    Ptr(u32),
    Vector(Box<LlType>, u32),
    Array(Box<LlType>, u32),
    Struct(Vec<LlType>),
    Named(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum LlTypeCapability {
    Float16,
    Int8,
    Int16,
    Int64,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TypedValue {
    pub(super) ty: LlType,
    pub(super) value: LlValue,
}

#[derive(Clone, Debug)]
pub(super) enum LlValue {
    Local(String),
    Global(String),
    Bool(bool),
    Int(u64),
    SignedInt(i64),
    Hex(u64),
    Float(f64),
    Float32Bits(u32),
    HalfBits(u16),
    BFloatBits(u16),
    Vector(Vec<TypedValue>),
    Array(Vec<TypedValue>),
    Struct(Vec<TypedValue>),
    Splat(Box<TypedValue>),
    Gep(Box<LlGep>),
    IntToPtr {
        source: Box<TypedValue>,
        destination: LlType,
    },
    Zero,
    Undef,
}

impl PartialEq for LlValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(a), Self::Local(b)) | (Self::Global(a), Self::Global(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) | (Self::Hex(a), Self::Hex(b)) => a == b,
            (Self::SignedInt(a), Self::SignedInt(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::HalfBits(a), Self::HalfBits(b)) | (Self::BFloatBits(a), Self::BFloatBits(b)) => {
                a == b
            }
            (Self::Vector(a), Self::Vector(b))
            | (Self::Array(a), Self::Array(b))
            | (Self::Struct(a), Self::Struct(b)) => a == b,
            (Self::Splat(a), Self::Splat(b)) => a == b,
            (Self::Gep(a), Self::Gep(b)) => a == b,
            (
                Self::IntToPtr {
                    source: a_source,
                    destination: a_destination,
                },
                Self::IntToPtr {
                    source: b_source,
                    destination: b_destination,
                },
            ) => a_source == b_source && a_destination == b_destination,
            (Self::Zero, Self::Zero) | (Self::Undef, Self::Undef) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct LlGep {
    pub(super) inbounds: bool,
    pub(super) source_ty: LlType,
    pub(super) base: TypedValue,
    pub(super) indices: Vec<TypedValue>,
}

#[derive(Clone, Debug)]
pub(super) struct LlFunction {
    pub(super) name: String,
    pub(super) ret: LlType,
    pub(super) params: Vec<(String, LlType)>,
    pub(super) byval_param_pointees: Vec<Option<LlType>>,
    pub(super) is_static_initializer: bool,
    pub(super) blocks: Vec<crate::native::cfg::BodyBlock>,
    pub(super) loop_controls: HashMap<String, (spirv::LoopControl, Option<u32>)>,
}

impl LlFunction {
    pub(in crate::native) fn carrier_insts(
        &self,
    ) -> impl Iterator<Item = &crate::native::tir::TirInst> {
        self.blocks
            .iter()
            .filter_map(|b| b.typed.as_ref())
            .flat_map(|t| t.insts.iter())
    }
}

pub(super) const IMAGEBLOCK_WIDTH_INTRINSIC: &str = "air.get_imageblock_width";

#[derive(Clone, Debug)]
pub(super) struct LlDeclaration {
    pub(super) name: String,
    pub(super) ret: LlType,
    pub(super) params: Vec<LlType>,
}

#[derive(Clone, Debug)]
pub(super) struct LlGlobal {
    pub(super) name: String,
    pub(super) addrspace: u32,
    pub(super) ty: LlType,
    pub(super) initializer: Option<TypedValue>,
}

#[derive(Clone, Debug)]
pub(super) struct LlModule {
    pub(super) air_data_layout: Option<crate::layout::AirDataLayout>,
    pub(super) types: HashMap<String, LlType>,
    pub(super) functions: Vec<LlFunction>,
    pub(super) declarations: Vec<LlDeclaration>,
    pub(super) globals: Vec<LlGlobal>,
    static_init_globals: HashMap<String, meta::StaticIntValue>,
    pub(super) entry_name: Option<String>,
    pub(super) preinlined_static_initializers: HashSet<String>,
    pub(super) preinlined_helper_pointer_loads: HashSet<String>,
    pub(super) preinlined_helper_type_capabilities: HashSet<LlTypeCapability>,
    pub(super) entry_functions: HashSet<String>,
    pub(super) ptr_pointees: HashMap<(String, String), LlType>,
    pub(super) local_alloca_pointees: HashMap<(String, String), LlType>,
    pub(super) imageblock_data_pointee: Option<LlType>,
    pub(super) imageblock_dimensions: Option<[u32; 2]>,
    pub(super) imageblock_shared_cells: bool,
    pub(super) imageblock_threads_per_threadgroup_param: Option<String>,
    pub(super) imageblock_cell_scale: Option<u32>,
    /// The tile's extent per axis in threadgroups ([x, y]), when every imageblock coordinate's lanes are bounded:
    /// what a whole-imageblock slice write (size left undef) copies.
    pub(super) imageblock_cell_scale_xy: Option<[u32; 2]>,
    pub(super) aliased_imageblock_planes: Vec<meta::AliasedImageblockPlane>,
    metadata_pointee_params: HashSet<(String, String)>,
    metadata_pointee_sizes: HashMap<(String, String), u64>,
    metadata_byte_buffer_params: HashSet<(String, String)>,
    pub(super) metadata_data_buffer_params: HashSet<(String, String)>,
    /// The entry carries kernel metadata, so which parameters are buffers is known (metadata_data_buffer_params).
    pub(super) has_kernel_metadata: bool,
    pub(super) metadata_primitive_buffer_pointees: HashMap<(String, String), LlType>,
    pub(super) metadata_fc_buffer_locations: HashMap<(String, String), u32>,
    pub(super) raw_buffer_params: HashSet<(String, String)>,
    pub(super) call_connected_raw_params: HashSet<(String, String)>,
    pub(super) param_connected_raw_params: HashSet<(String, String)>,
}

fn infer_metadata_fc_buffer_locations(
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    functions: &[LlFunction],
) -> HashMap<(String, String), u32> {
    let Some(kern) = kern else {
        return HashMap::default();
    };
    let Some(entry_name) = entry_name else {
        return HashMap::default();
    };
    let Some(entry) = functions
        .iter()
        .find(|function| function.name == entry_name)
    else {
        return HashMap::default();
    };
    kern.function_constant_buffer_locations
        .iter()
        .filter_map(|(index, location)| {
            let (name, ty) = entry.params.get(*index as usize)?;
            matches!(ty, LlType::Ptr(1 | 2))
                .then(|| ((entry.name.clone(), name.clone()), *location))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::native) struct ParamCallEdge {
    caller_func: String,
    caller_param: String,
    callee_func: String,
    callee_param: String,
}

fn pointer_param_alias_roots(f: &LlFunction) -> HashMap<String, String> {
    let mut roots: HashMap<String, String> = f
        .params
        .iter()
        .filter(|&(_name, ty)| matches!(ty, LlType::Ptr(_)))
        .map(|(name, _ty)| (name.clone(), name.clone()))
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for inst in f.carrier_insts() {
            if let Some((res, base)) = inst.identity_ptr_bitcast() {
                if let Some(root) = roots.get(base).cloned() {
                    if roots.insert(res.to_string(), root).is_none() {
                        changed = true;
                    }
                }
                continue;
            }

            let Some(res) = &inst.result else {
                continue;
            };
            if let Some(gep) = &inst.gep() {
                let LlValue::Local(base) = &gep.base.value else {
                    continue;
                };
                if let Some(root) = roots.get(base).cloned() {
                    if roots.insert(res.clone(), root).is_none() {
                        changed = true;
                    }
                }
                continue;
            }

            let common_root = |values: Vec<&LlValue>| -> Option<String> {
                let mut values = values.into_iter();
                let LlValue::Local(first) = values.next()? else {
                    return None;
                };
                let root = roots.get(first)?.clone();
                for value in values {
                    let LlValue::Local(name) = value else {
                        return None;
                    };
                    if roots.get(name) != Some(&root) {
                        return None;
                    }
                }
                Some(root)
            };
            if let Some(incoming) = inst.phi_values() {
                if let Some(root) = common_root(incoming.collect()) {
                    if roots.insert(res.clone(), root).is_none() {
                        changed = true;
                    }
                }
                continue;
            }
            if let Some((true_value, false_value)) = inst.select_arms().as_deref() {
                if !matches!(true_value.ty, LlType::Ptr(_))
                    || !matches!(false_value.ty, LlType::Ptr(_))
                {
                    continue;
                }
                if let Some(root) = common_root(vec![&true_value.value, &false_value.value]) {
                    if roots.insert(res.clone(), root).is_none() {
                        changed = true;
                    }
                }
            }
        }
    }
    roots
}

fn cross_buffer_pointer_phi_gep_sources(f: &LlFunction) -> HashMap<String, HashSet<LlType>> {
    let mut roots: HashMap<String, HashSet<String>> = f
        .params
        .iter()
        .filter(|&(_name, ty)| matches!(ty, LlType::Ptr(_)))
        .map(|(name, _ty)| (name.clone(), crate::fxhash::set_from([name.clone()])))
        .collect();
    let mut contains_phi: HashMap<String, bool> =
        roots.keys().cloned().map(|name| (name, false)).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for inst in f.carrier_insts() {
            if let Some((res, base)) = inst.identity_ptr_bitcast() {
                let Some(base_roots) = roots.get(base).cloned() else {
                    continue;
                };
                let base_has_phi = contains_phi.get(base).copied().unwrap_or(false);
                let result_roots = roots.entry(res.to_string()).or_default();
                let old_len = result_roots.len();
                result_roots.extend(base_roots);
                changed |= result_roots.len() != old_len;
                if base_has_phi && !contains_phi.get(res).copied().unwrap_or(false) {
                    contains_phi.insert(res.to_string(), true);
                    changed = true;
                }
                continue;
            }

            let Some(res) = &inst.result else {
                continue;
            };
            if let Some(gep) = &inst.gep() {
                let LlValue::Local(base) = &gep.base.value else {
                    continue;
                };
                let Some(base_roots) = roots.get(base).cloned() else {
                    continue;
                };
                let base_has_phi = contains_phi.get(base).copied().unwrap_or(false);
                let result = res.clone();
                let result_roots = roots.entry(result.clone()).or_default();
                let old_len = result_roots.len();
                result_roots.extend(base_roots);
                changed |= result_roots.len() != old_len;
                if base_has_phi && !contains_phi.get(&result).copied().unwrap_or(false) {
                    contains_phi.insert(result, true);
                    changed = true;
                }
                continue;
            }

            let (merge_roots, result_has_phi) = if let Some(incoming) = inst.phi_values() {
                let mut merged = HashSet::default();
                let mut complete = true;
                for value in incoming {
                    let LlValue::Local(name) = value else {
                        complete = false;
                        break;
                    };
                    let Some(value_roots) = roots.get(name) else {
                        complete = false;
                        break;
                    };
                    merged.extend(value_roots.iter().cloned());
                }
                (complete.then_some(merged), true)
            } else if let Some((true_value, false_value)) = inst.select_arms().as_deref() {
                if !matches!(true_value.ty, LlType::Ptr(_))
                    || !matches!(false_value.ty, LlType::Ptr(_))
                {
                    continue;
                }
                let (LlValue::Local(true_name), LlValue::Local(false_name)) =
                    (&true_value.value, &false_value.value)
                else {
                    continue;
                };
                let (Some(true_roots), Some(false_roots)) =
                    (roots.get(true_name), roots.get(false_name))
                else {
                    continue;
                };
                let mut merged = true_roots.clone();
                merged.extend(false_roots.iter().cloned());
                (
                    Some(merged),
                    contains_phi.get(true_name).copied().unwrap_or(false)
                        || contains_phi.get(false_name).copied().unwrap_or(false),
                )
            } else {
                continue;
            };

            let Some(merge_roots) = merge_roots else {
                continue;
            };
            let result = res.clone();
            let result_roots = roots.entry(result.clone()).or_default();
            let old_len = result_roots.len();
            result_roots.extend(merge_roots);
            changed |= result_roots.len() != old_len;
            if result_has_phi && !contains_phi.get(&result).copied().unwrap_or(false) {
                contains_phi.insert(result, true);
                changed = true;
            }
        }
    }

    let mut sources: HashMap<String, HashSet<LlType>> = HashMap::default();
    for inst in f.carrier_insts() {
        let Some(gep) = &inst.gep() else {
            continue;
        };
        let LlValue::Local(base) = &gep.base.value else {
            continue;
        };
        if !contains_phi.get(base).copied().unwrap_or(false) {
            continue;
        }
        let Some(base_roots) = roots.get(base) else {
            continue;
        };
        if base_roots.len() < 2 {
            continue;
        }
        for root in base_roots {
            sources
                .entry(root.clone())
                .or_default()
                .insert(gep.source_ty.clone());
        }
    }
    sources
}

fn infer_entry_functions(ll: &str) -> HashSet<String> {
    ["kernel", "vertex", "fragment"]
        .into_iter()
        .filter_map(|stage| meta::entry_name(ll, stage))
        .collect()
}

fn infer_metadata_byte_buffer_params(
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    functions: &[LlFunction],
) -> HashSet<(String, String)> {
    let mut out = HashSet::default();
    let Some(kern) = kern else {
        return out;
    };
    let Some(entry_name) = entry_name else {
        return out;
    };
    let Some(entry) = functions.iter().find(|f| f.name == entry_name) else {
        return out;
    };
    for (idx, (name, ty)) in entry.params.iter().enumerate() {
        let Some(arg_type) = kern.buffer_type_name(idx as u32) else {
            continue;
        };
        if arg_type != "char" && arg_type != "void" {
            continue;
        }
        if matches!(ty, LlType::Ptr(1 | 2)) {
            out.insert((entry.name.clone(), name.clone()));
        }
    }
    out
}

fn infer_metadata_data_buffer_params(
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    functions: &[LlFunction],
) -> HashSet<(String, String)> {
    let mut out = HashSet::default();
    let Some(kern) = kern else {
        return out;
    };
    let Some(entry_name) = entry_name else {
        return out;
    };
    let Some(entry) = functions.iter().find(|f| f.name == entry_name) else {
        return out;
    };
    for (idx, (name, ty)) in entry.params.iter().enumerate() {
        if !matches!(
            kern.role_of(idx as u32),
            Some(
                KernRole::Buffer(_)
                    | KernRole::AccelerationStructureShadow(_)
                    | KernRole::PrimitiveAccelerationStructureShadow(_)
            )
        ) {
            continue;
        }
        let device_buffer_array = matches!(ty, LlType::Ptr(0))
            && kern
                .buffer_type_name(idx as u32)
                .is_some_and(meta::is_device_buffer_array_type_name);
        if matches!(ty, LlType::Ptr(1 | 2)) || device_buffer_array {
            out.insert((entry.name.clone(), name.clone()));
        }
    }
    out
}

fn infer_metadata_primitive_buffer_pointees(
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    functions: &[LlFunction],
) -> HashMap<(String, String), LlType> {
    let mut out = HashMap::default();
    let (Some(kern), Some(entry_name)) = (kern, entry_name) else {
        return out;
    };
    let Some(entry) = functions
        .iter()
        .find(|function| function.name == entry_name)
    else {
        return out;
    };
    for (index, (name, ty)) in entry.params.iter().enumerate() {
        if !matches!(ty, LlType::Ptr(1..=3))
            || !matches!(kern.role_of(index as u32), Some(KernRole::Buffer(_)))
        {
            continue;
        }
        let Some(layout) = kern
            .buffer_type_name(index as u32)
            .and_then(meta::primitive_air_type_from_name)
        else {
            continue;
        };
        out.insert(
            (entry.name.clone(), name.clone()),
            ll_type_from_air_type(&layout),
        );
    }
    out
}

fn calls_imageblock_slice_write(
    functions: &[LlFunction],
    entry_functions: &HashSet<String>,
) -> bool {
    functions
        .iter()
        .filter(|function| entry_functions.contains(&function.name))
        .flat_map(|function| function.carrier_insts())
        .filter_map(|inst| inst.alias_call())
        .any(|call| {
            call.callee
                .starts_with("air.write_imageblock_slice_to_texture")
        })
}

fn infer_imageblock_cell_scale(
    functions: &[LlFunction],
    entry_functions: &HashSet<String>,
    statics: &HashMap<String, meta::StaticIntValue>,
    threads_per_threadgroup: Option<&str>,
) -> Option<u32> {
    let mut scale = 0;
    for function in functions
        .iter()
        .filter(|function| entry_functions.contains(&function.name))
    {
        let insts = function.carrier_insts().collect::<Vec<_>>();
        let defs = insts
            .iter()
            .filter_map(|inst| inst.result.as_deref().map(|result| (result, *inst)))
            .collect::<HashMap<_, _>>();
        let tile = TileCoordinates { defs: &defs, insts: &insts, statics, threads_per_threadgroup };
        for coordinate in function
            .carrier_insts()
            .filter_map(|inst| inst.alias_call())
            .filter(|call| call.callee == "air.imageblock_data")
            .filter_map(|call| call.args.first().map(|arg| arg.value.clone()))
        {
            let coordinate_scale = match coordinate {
                LlValue::Local(name) => tile.scale(&name, 16)?,
                _ => 0,
            };
            scale = scale.max(coordinate_scale);
        }
    }
    Some(scale.max(1))
}

/// Per axis, the most threadgroups an imageblock coordinate spans (see TileCoordinates::lane_scale); None when a
/// coordinate's lanes cannot be bounded.
fn infer_imageblock_lane_scales(
    functions: &[LlFunction],
    entry_functions: &HashSet<String>,
    statics: &HashMap<String, meta::StaticIntValue>,
    threads_per_threadgroup: Option<&str>,
) -> Option<[u32; 2]> {
    let mut scales = [0u32; 2];
    for function in functions.iter().filter(|function| entry_functions.contains(&function.name)) {
        let insts = function.carrier_insts().collect::<Vec<_>>();
        let defs = insts
            .iter()
            .filter_map(|inst| inst.result.as_deref().map(|result| (result, *inst)))
            .collect::<HashMap<_, _>>();
        let tile = TileCoordinates { defs: &defs, insts: &insts, statics, threads_per_threadgroup };
        for coordinate in function
            .carrier_insts()
            .filter_map(|inst| inst.alias_call())
            .filter(|call| call.callee == "air.imageblock_data")
            .filter_map(|call| call.args.first().map(|arg| arg.value.clone()))
        {
            for (lane, scale) in scales.iter_mut().enumerate() {
                let lane_scale = match &coordinate {
                    LlValue::Local(name) => tile.lane_scale(name, lane as u64, 16)?,
                    _ => 0,
                };
                *scale = (*scale).max(lane_scale);
            }
        }
    }
    Some([scales[0].max(1), scales[1].max(1)])
}

/// What the imageblock coordinate analysis reads: the entry's definitions, and for the strided form
/// `k * threads_per_threadgroup + thread_position_in_threadgroup` the loop bound of k. MPS's Lanczos kernels write
/// unroll x unroll cells per thread that way, with unroll a function constant its static initializer copies into a
/// global; with the constant supplied, k < unroll, so the cells stay inside an unroll-scaled tile (#405, Infuse).
struct TileCoordinates<'a, 'b> {
    defs: &'b HashMap<&'a str, &'a crate::native::tir::TirInst>,
    insts: &'b [&'a crate::native::tir::TirInst],
    statics: &'b HashMap<String, meta::StaticIntValue>,
    threads_per_threadgroup: Option<&'b str>,
}

impl TileCoordinates<'_, '_> {
    fn scale(&self, name: &str, depth: u32) -> Option<u32> {
        if depth == 0 {
            return None;
        }
        let Some(inst) = self.defs.get(name) else {
            return Some(1);
        };
        if !reaches_thread_position(self.defs, name, &mut HashSet::default()) {
            return Some(0);
        }
        use crate::native::tir::TirOpcode;
        let operand_scale = |operand: &crate::native::tir::TirOperand| match operand {
            crate::native::tir::TirOperand::Value { name, .. } => self.scale(name, depth - 1),
            crate::native::tir::TirOperand::Const { .. } => Some(0),
            crate::native::tir::TirOperand::Unresolved => None,
        };
        match inst.opcode {
            TirOpcode::Shl | TirOpcode::Mul => {
                let [lhs, rhs] = inst.operands.as_slice() else {
                    return None;
                };
                let (value, factor) = match (constant_uniform_int(lhs), constant_uniform_int(rhs)) {
                    (None, Some(factor)) => (lhs, factor),
                    (Some(factor), None) if inst.opcode == TirOpcode::Mul => (rhs, factor),
                    _ if inst.opcode == TirOpcode::Mul => return self.strided_term(lhs, rhs, depth),
                    _ => return None,
                };
                let factor = if inst.opcode == TirOpcode::Shl {
                    1u32.checked_shl(u32::try_from(factor).ok()?)?
                } else {
                    u32::try_from(factor).ok()?
                };
                operand_scale(value)?.checked_mul(factor)
            }
            TirOpcode::Add | TirOpcode::Or | TirOpcode::Sub => {
                let [lhs, rhs] = inst.operands.as_slice() else {
                    return None;
                };
                operand_scale(lhs)?.checked_add(operand_scale(rhs)?)
            }
            _ => {
                let mut scale = Some(0);
                inst.visit_uses(|use_name| {
                    scale = match (scale, self.scale(use_name, depth - 1)) {
                        (Some(0), Some(1)) | (Some(1), Some(1)) => Some(1),
                        (Some(0), Some(0)) => Some(0),
                        _ => None,
                    };
                });
                scale
            }
        }
    }

    /// `k * threads_per_threadgroup`, with k bounded by a constant: the term spans k_max whole threadgroups, so it
    /// adds k_max to the scale (with the thread position's 1, a coordinate stays below (k_max + 1) threadgroups).
    fn strided_term(
        &self,
        lhs: &crate::native::tir::TirOperand,
        rhs: &crate::native::tir::TirOperand,
        depth: u32,
    ) -> Option<u32> {
        let tptg = self.threads_per_threadgroup?;
        fn name_of(operand: &crate::native::tir::TirOperand) -> Option<&str> {
            match operand {
                crate::native::tir::TirOperand::Value { name, .. } => Some(name.as_str()),
                _ => None,
            }
        }
        let counter = match (name_of(lhs), name_of(rhs)) {
            (Some(a), Some(b)) if a == tptg => b,
            (Some(a), Some(b)) if b == tptg => a,
            _ => return None,
        };
        let max = self.max_value(counter, depth)?;
        u32::try_from(max).ok()
    }

    /// The largest value `name` takes: a constant, a folded static global, an insertelement of bounded lanes, or a
    /// loop counter that starts at 0, steps by 1 and leaves the loop once it reaches a constant bound.
    fn max_value(&self, name: &str, depth: u32) -> Option<u64> {
        if depth == 0 {
            return None;
        }
        use crate::native::tir::{TirOpcode, TirOperand};
        let inst = self.defs.get(name)?;
        let operand_max = |operand: &TirOperand| match operand {
            TirOperand::Const { value, .. } => const_lanes_max(value),
            TirOperand::Value { name, .. } => self.max_value(name, depth - 1),
            TirOperand::Unresolved => None,
        };
        match inst.opcode {
            TirOpcode::InsertElement => {
                let [vector, element, ..] = inst.operands.as_slice() else {
                    return None;
                };
                Some(operand_max(vector)?.max(operand_max(element)?))
            }
            TirOpcode::ZExt | TirOpcode::Trunc => operand_max(inst.operands.first()?),
            TirOpcode::Load => self.static_value(name),
            TirOpcode::Phi => self.loop_counter_max(name, inst),
            _ => None,
        }
    }

    /// scale() for one lane of a vector coordinate: an insertelement into that lane, a component-wise product with
    /// threads_per_threadgroup and a sum of lanes are followed lane by lane.
    fn lane_scale(&self, name: &str, lane: u64, depth: u32) -> Option<u32> {
        use crate::native::tir::{TirOpcode, TirOperand};
        if depth == 0 {
            return None;
        }
        let Some(inst) = self.defs.get(name) else {
            return Some(1);
        };
        if !reaches_thread_position(self.defs, name, &mut HashSet::default()) {
            return Some(0);
        }
        let operand_lane = |operand: &TirOperand| match operand {
            TirOperand::Value { name, .. } => self.lane_scale(name, lane, depth - 1),
            TirOperand::Const { .. } => Some(0),
            TirOperand::Unresolved => None,
        };
        match inst.opcode {
            TirOpcode::Mul => {
                let [lhs, rhs] = inst.operands.as_slice() else {
                    return None;
                };
                match (constant_uniform_int(lhs), constant_uniform_int(rhs)) {
                    (None, Some(factor)) => operand_lane(lhs)?.checked_mul(u32::try_from(factor).ok()?),
                    (Some(factor), None) => operand_lane(rhs)?.checked_mul(u32::try_from(factor).ok()?),
                    _ => {
                        let tptg = self.threads_per_threadgroup?;
                        let counter = match (lhs, rhs) {
                            (TirOperand::Value { name: a, .. }, TirOperand::Value { name: b, .. }) if a == tptg => b,
                            (TirOperand::Value { name: a, .. }, TirOperand::Value { name: b, .. }) if b == tptg => a,
                            _ => return None,
                        };
                        u32::try_from(self.lane_max_value(counter, lane, depth - 1)?).ok()
                    }
                }
            }
            TirOpcode::Add | TirOpcode::Or => {
                let [lhs, rhs] = inst.operands.as_slice() else {
                    return None;
                };
                operand_lane(lhs)?.checked_add(operand_lane(rhs)?)
            }
            TirOpcode::InsertElement => {
                let [vector, element, index] = inst.operands.as_slice() else {
                    return None;
                };
                if constant_uniform_int(index)? == lane {
                    match element {
                        TirOperand::Value { name, .. } => self.scale(name, depth - 1),
                        TirOperand::Const { .. } => Some(0),
                        TirOperand::Unresolved => None,
                    }
                } else {
                    operand_lane(vector)
                }
            }
            _ => self.scale(name, depth),
        }
    }

    fn lane_max_value(&self, name: &str, lane: u64, depth: u32) -> Option<u64> {
        use crate::native::tir::{TirOpcode, TirOperand};
        if depth == 0 {
            return None;
        }
        let inst = self.defs.get(name)?;
        match inst.opcode {
            TirOpcode::InsertElement => {
                let [vector, element, index] = inst.operands.as_slice() else {
                    return None;
                };
                let chosen = if constant_uniform_int(index)? == lane { element } else { vector };
                match chosen {
                    TirOperand::Const { value, .. } => const_lanes_max(value),
                    TirOperand::Value { name, .. } if std::ptr::eq(chosen, element) => self.max_value(name, depth - 1),
                    TirOperand::Value { name, .. } => self.lane_max_value(name, lane, depth - 1),
                    TirOperand::Unresolved => None,
                }
            }
            _ => self.max_value(name, depth),
        }
    }

    fn static_value(&self, name: &str) -> Option<u64> {
        let inst = self.defs.get(name)?;
        let load = inst.load().as_deref()?;
        let LlValue::Global(global) = &load.ptr.value else {
            return None;
        };
        let key = if global.starts_with('@') { global.clone() } else { format!("@{global}") };
        match self.statics.get(&key)? {
            meta::StaticIntValue::Scalar(value) => Some(u64::from(*value)),
            meta::StaticIntValue::Vector(lanes) => lanes.iter().copied().max().map(u64::from),
        }
    }

    fn bound_value(&self, operand: &crate::native::tir::TirOperand, depth: u32) -> Option<u64> {
        use crate::native::tir::{TirOpcode, TirOperand};
        match operand {
            TirOperand::Const { .. } => constant_uniform_int(operand),
            TirOperand::Value { name, .. } => {
                let inst = self.defs.get(name.as_str())?;
                match inst.opcode {
                    TirOpcode::Load => self.static_value(name),
                    TirOpcode::ZExt | TirOpcode::Trunc if depth > 0 => {
                        self.bound_value(inst.operands.first()?, depth - 1)
                    }
                    _ => None,
                }
            }
            TirOperand::Unresolved => None,
        }
    }

    fn loop_counter_max(&self, phi: &str, inst: &crate::native::tir::TirInst) -> Option<u64> {
        use crate::native::tir::{TirOpcode, TirOperand};
        let (_, incoming) = inst.phi_incoming().as_ref()?;
        let mut start = None;
        let mut step = None;
        for (value, _) in incoming {
            match value {
                LlValue::Int(0) | LlValue::Zero => start = Some(()),
                LlValue::Local(next) => step = Some(next.as_str()),
                _ => return None,
            }
        }
        start?;
        let step = step?;
        let step_inst = self.defs.get(step)?;
        let steps_by_one = step_inst.opcode == TirOpcode::Add
            && match step_inst.operands.as_slice() {
                [TirOperand::Value { name, .. }, one] | [one, TirOperand::Value { name, .. }] => {
                    name == phi && constant_uniform_int(one) == Some(1)
                }
                _ => false,
            };
        if !steps_by_one {
            return None;
        }
        // the incremented counter, or a zero-extension of it, compared with a constant bound
        let forms = std::iter::once(step.to_string())
            .chain(self.insts.iter().filter_map(|other| {
                (other.opcode == TirOpcode::ZExt
                    && matches!(other.operands.first(), Some(TirOperand::Value { name, .. }) if name == step))
                    .then(|| other.result.clone())
                    .flatten()
            }))
            .collect::<Vec<_>>();
        let names = |operand: &TirOperand| match operand {
            TirOperand::Value { name, .. } => forms.contains(name),
            _ => false,
        };
        let mut bound = None;
        for cmp in self.insts.iter().filter(|other| other.opcode == TirOpcode::ICmp) {
            let [a, b] = cmp.operands.as_slice() else {
                continue;
            };
            let predicate = cmp.cmp_predicate().as_deref().unwrap_or("");
            // leaves when counter >= bound: "bound ugt counter", "counter ult bound", "counter eq bound"
            let candidate = match predicate {
                "ugt" | "sgt" if names(b) => self.bound_value(a, 4),
                "ult" | "slt" | "eq" | "ne" if names(a) => self.bound_value(b, 4),
                "eq" | "ne" if names(b) => self.bound_value(a, 4),
                _ => None,
            };
            if let Some(candidate) = candidate {
                bound = Some(bound.map_or(candidate, |b: u64| b.max(candidate)));
            }
        }
        bound.filter(|bound| *bound >= 1).map(|bound| bound - 1)
    }
}

fn const_lanes_max(value: &LlValue) -> Option<u64> {
    match value {
        LlValue::Int(value) | LlValue::Hex(value) => Some(*value),
        LlValue::SignedInt(value) => u64::try_from(*value).ok(),
        LlValue::Zero | LlValue::Undef => Some(0),
        LlValue::Splat(element) => const_lanes_max(&element.value),
        LlValue::Vector(elements) => elements.iter().map(|e| const_lanes_max(&e.value)).try_fold(0, |m, v| Some(m.max(v?))),
        _ => None,
    }
}

fn reaches_thread_position<'a>(
    defs: &HashMap<&'a str, &'a crate::native::tir::TirInst>,
    name: &'a str,
    visited: &mut HashSet<&'a str>,
) -> bool {
    if !visited.insert(name) {
        return false;
    }
    let Some(inst) = defs.get(name) else {
        return true;
    };
    let mut reaches = false;
    inst.visit_uses(|use_name| {
        if let Some((interned, _)) = defs.get_key_value(use_name) {
            reaches |= reaches_thread_position(defs, interned, visited);
        } else {
            reaches = true;
        }
    });
    reaches
}

fn constant_uniform_int(operand: &crate::native::tir::TirOperand) -> Option<u64> {
    let crate::native::tir::TirOperand::Const { value, .. } = operand else {
        return None;
    };
    fn scalar(value: &LlValue) -> Option<u64> {
        match value {
            LlValue::Int(value) | LlValue::Hex(value) => Some(*value),
            LlValue::SignedInt(value) => u64::try_from(*value).ok(),
            LlValue::Zero => Some(0),
            LlValue::Splat(element) => scalar(&element.value),
            LlValue::Vector(elements) => {
                let mut lanes = elements.iter().map(|element| scalar(&element.value));
                let first = lanes.next()??;
                lanes.all(|lane| lane == Some(first)).then_some(first)
            }
            _ => None,
        }
    }
    scalar(value)
}

fn infer_cross_coordinate_imageblock(
    functions: &[LlFunction],
    entry_functions: &HashSet<String>,
) -> bool {
    for function in functions
        .iter()
        .filter(|function| entry_functions.contains(&function.name))
    {
        let mut coordinates = HashSet::default();
        for inst in function.carrier_insts() {
            let Some(call) = inst.alias_call() else {
                continue;
            };
            if call.callee != "air.imageblock_data" {
                continue;
            }
            let Some(coordinate) = call.args.first() else {
                continue;
            };
            let key = match &coordinate.value {
                LlValue::Local(name) => format!("local:{name}"),
                LlValue::Zero => "zero".to_string(),
                LlValue::Undef => "undef".to_string(),
                other => format!("{other:?}"),
            };
            coordinates.insert(key);
            if coordinates.len() > 1 {
                return true;
            }
        }
    }
    false
}

fn infer_imageblock_nonzero_byte_field(
    functions: &[LlFunction],
    entry_functions: &HashSet<String>,
) -> bool {
    let function_by_name = functions
        .iter()
        .map(|function| (function.name.as_str(), function))
        .collect::<HashMap<_, _>>();
    let mut reachable = entry_functions.clone();
    let mut pending = entry_functions.iter().cloned().collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        let Some(function) = function_by_name.get(name.as_str()) else {
            continue;
        };
        for call in function
            .carrier_insts()
            .filter_map(|inst| inst.call().as_deref())
        {
            if function_by_name.contains_key(call.callee.as_str())
                && reachable.insert(call.callee.clone())
            {
                pending.push(call.callee.clone());
            }
        }
    }
    for function in functions
        .iter()
        .filter(|function| reachable.contains(&function.name))
    {
        let mut roots = HashSet::default();
        let mut changed = true;
        while changed {
            changed = false;
            for inst in function.carrier_insts() {
                let Some(result) = &inst.result else {
                    continue;
                };
                if !result.starts_with('%') {
                    continue;
                }

                if inst
                    .alias_call()
                    .is_some_and(|call| call.callee == "air.imageblock_data")
                {
                    changed |= roots.insert(result.clone());
                    continue;
                }

                if let Some((alias, base)) = inst.identity_ptr_bitcast() {
                    if roots.contains(base) {
                        changed |= roots.insert(alias.to_string());
                    }
                    continue;
                }

                let Some(gep) = &inst.gep() else {
                    continue;
                };
                let LlValue::Local(base) = &gep.base.value else {
                    continue;
                };
                if !roots.contains(base) {
                    continue;
                }
                changed |= roots.insert(result.clone());
                if gep.source_ty == LlType::Int(8)
                    && gep
                        .indices
                        .iter()
                        .filter_map(typed_value_u64)
                        .any(|offset| offset != 0)
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod imageblock_reachability_tests {
    use super::*;

    fn module(entry_calls_helper: bool) -> LlModule {
        let call = if entry_calls_helper {
            "  call void @helper()\n"
        } else {
            ""
        };
        LlModule::parse(&format!(
            r#"define void @entry() {{
entry:
{call}  ret void
}}
define internal void @helper() {{
entry:
  %data = call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  %field = getelementptr i8, ptr addrspace(4) %data, i64 16
  ret void
}}
declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
"#
        ))
        .expect("module parses")
    }

    #[test]
    fn reachable_helper_nonzero_byte_field_requires_complete_imageblock_cell() {
        let reachable = module(true);
        assert!(infer_imageblock_nonzero_byte_field(
            &reachable.functions,
            &crate::fxhash::set_from(["entry".to_string()])
        ));

        let unreachable = module(false);
        assert!(!infer_imageblock_nonzero_byte_field(
            &unreachable.functions,
            &crate::fxhash::set_from(["entry".to_string()])
        ));
    }
}

fn infer_apv_imageblock_dimensions(ll: &str) -> Option<[u32; 2]> {
    let root = ll.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("!apv.imageblock_dimensions = !{!")?;
        rest.strip_suffix('}')?.parse::<u32>().ok()
    })?;
    let prefix = format!("!{root} = !{{");
    let body = ll.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix('}'))
    })?;
    let mut fields = body.split(',').map(str::trim);
    let width = fields.next()?.strip_prefix("i32 ")?.parse().ok()?;
    let height = fields.next()?.strip_prefix("i32 ")?.parse().ok()?;
    (fields.next().is_none() && width != 0 && height != 0).then_some([width, height])
}

fn infer_imageblock_data_pointee(
    kern: Option<&meta::KernMeta>,
    complete_cell_layout: bool,
) -> Option<LlType> {
    let kern = kern?;
    let mut pointee = None;
    for layout in kern.imageblock_layouts.values() {
        let candidate = imageblock_data_pointee_from_air_type(layout, complete_cell_layout)?;
        match &pointee {
            Some(existing) if existing != &candidate => return None,
            Some(_) => {}
            None => pointee = Some(candidate),
        }
    }
    pointee
}

fn imageblock_data_pointee_from_air_type(
    ty: &AirType,
    complete_cell_layout: bool,
) -> Option<LlType> {
    match ty {
        AirType::Struct(members) if complete_cell_layout && members.len() != 1 => {
            Some(ll_type_from_air_type(ty))
        }
        AirType::Struct(members) => members
            .first()
            .map(|member| ll_type_from_air_type(&member.ty)),
        _ => Some(ll_type_from_air_type(ty)),
    }
}

pub(crate) fn ll_type_from_air_type(ty: &AirType) -> LlType {
    match ty {
        AirType::Scalar(scalar) => ll_type_from_air_scalar(*scalar),
        AirType::Vec { scalar, lanes } => {
            LlType::Vector(Box::new(ll_type_from_air_scalar(*scalar)), *lanes)
        }
        AirType::PackedVec { scalar, lanes } => {
            LlType::Array(Box::new(ll_type_from_air_scalar(*scalar)), *lanes)
        }
        AirType::Array { elem, len } => LlType::Array(Box::new(ll_type_from_air_type(elem)), *len),
        AirType::Matrix { scalar, cols, rows } => LlType::Struct(vec![LlType::Array(
            Box::new(LlType::Vector(
                Box::new(ll_type_from_air_scalar(*scalar)),
                *rows,
            )),
            *cols,
        )]),
        AirType::Struct(members) => LlType::Struct(
            members
                .iter()
                .map(|member| ll_type_from_air_type(&member.ty))
                .collect(),
        ),
        AirType::Opaque { size } => ll_type_from_air_type(&meta::storage_air_type_for_size(*size)),
    }
}

fn ll_type_from_air_scalar(scalar: AirScalar) -> LlType {
    match scalar {
        AirScalar::Float => LlType::Float,
        AirScalar::Half => LlType::Half,
        AirScalar::UInt | AirScalar::SInt => LlType::Int(32),
        AirScalar::ULong | AirScalar::SLong => LlType::Int(64),
        AirScalar::UShort | AirScalar::SShort => LlType::Int(16),
        AirScalar::UChar | AirScalar::Bool => LlType::Int(8),
    }
}

fn is_ignored_global(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("@llvm.global_ctors")
        || t.starts_with("@llvm.global_dtors")
        || t.starts_with("@llvm.used")
        || t.starts_with("@llvm.compiler.used")
}

fn typed_value_u64(value: &TypedValue) -> Option<u64> {
    match value.value {
        LlValue::Int(value) | LlValue::Hex(value) => Some(value),
        LlValue::SignedInt(value) if value >= 0 => Some(value as u64),
        _ => None,
    }
}

pub(crate) fn round_up_u64(value: u64, align: u64) -> u64 {
    if align <= 1 {
        value
    } else {
        value.div_ceil(align) * align
    }
}

#[cfg(test)]
mod layout_abi_tests {
    use super::*;
    use crate::meta::{AirMember, AirScalar, AirType};

    fn module() -> LlModule {
        LlModule::parse("define void @k() {\nentry:\n  ret void\n}\n")
            .expect("minimal module parses")
    }

    fn vec(elem: LlType, lanes: u32) -> LlType {
        LlType::Vector(Box::new(elem), lanes)
    }

    #[test]
    fn scalar_sizes_match_metal_abi() {
        let m = module();
        for (ty, sz) in [
            (LlType::Bool, 1),
            (LlType::Int(8), 1),
            (LlType::Int(16), 2),
            (LlType::Half, 2),
            (LlType::BFloat, 2),
            (LlType::Int(32), 4),
            (LlType::Float, 4),
            (LlType::Int(64), 8),
            (LlType::Ptr(1), 8),
        ] {
            assert_eq!(
                m.scalar_storage_size(&ty),
                Some(sz),
                "scalar size of {ty:?}"
            );
        }
        assert_eq!(m.scalar_storage_size(&vec(LlType::Float, 4)), None);
    }

    #[test]
    fn native_rule_is_packed_style() {
        let m = module();
        assert_eq!(m.type_storage_size_align(&LlType::Float), Some((4, 4)));
        assert_eq!(m.type_storage_size_align(&LlType::Half), Some((2, 2)));
        assert_eq!(
            m.type_storage_size_align(&vec(LlType::Float, 2)),
            Some((8, 4))
        );
        assert_eq!(
            m.type_storage_size_align(&vec(LlType::Float, 3)),
            Some((12, 4))
        );
        assert_eq!(
            m.type_storage_size_align(&vec(LlType::Float, 4)),
            Some((16, 4))
        );
        assert_eq!(m.type_storage_size_align(&LlType::Int(24)), Some((3, 4)));
        assert_eq!(m.type_storage_size_align(&LlType::Int(1)), Some((1, 1)));
        assert_eq!(
            m.type_storage_size_align(&LlType::Array(Box::new(LlType::Float), 3)),
            Some((12, 4))
        );
        assert_eq!(
            m.type_storage_size_align(&LlType::Struct(vec![LlType::Int(8), LlType::Float])),
            Some((8, 4))
        );
    }

    #[test]
    fn memcpy_rule_pads_vec3_to_four_lanes() {
        let m = module();
        assert_eq!(
            m.native_memcpy_type_size_align(&LlType::Float),
            Some((4, 4))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&vec(LlType::Float, 2)),
            Some((8, 8))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&vec(LlType::Float, 3)),
            Some((16, 16))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&vec(LlType::Float, 4)),
            Some((16, 16))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&LlType::Array(Box::new(vec(LlType::Float, 3)), 2)),
            Some((32, 16))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&LlType::Array(Box::new(LlType::Float), 3)),
            Some((12, 4))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&LlType::Array(Box::new(LlType::Half), 3)),
            Some((6, 2))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&LlType::Struct(vec![
                LlType::Int(8),
                LlType::Array(Box::new(LlType::Int(8)), 3),
                LlType::Int(32),
            ])),
            Some((8, 4))
        );
    }

    #[test]
    fn memcpy_rule_uses_parsed_source_vector_alignment() {
        let m = LlModule::parse(concat!(
            "target datalayout = \"e-v24:64:64\"\n",
            "define void @k() {\nentry:\n  ret void\n}\n",
        ))
        .expect("module with custom datalayout parses");

        assert_eq!(
            m.native_memcpy_type_size_align(&vec(LlType::Int(8), 3)),
            Some((8, 8))
        );
    }

    #[test]
    fn air_metadata_rule_distinguishes_packed_from_unpacked() {
        let m = module();
        assert_eq!(
            m.air_metadata_type_size_align(&AirType::Scalar(AirScalar::Float)),
            Some((4, 4))
        );
        assert_eq!(
            m.air_metadata_type_size_align(&AirType::Vec {
                scalar: AirScalar::Float,
                lanes: 3
            }),
            Some((16, 16))
        );
        assert_eq!(
            m.air_metadata_type_size_align(&AirType::PackedVec {
                scalar: AirScalar::Float,
                lanes: 3
            }),
            Some((12, 4))
        );
        assert_eq!(
            m.air_metadata_type_size_align(&AirType::Struct(vec![
                AirMember {
                    offset: 0,
                    ty: AirType::Scalar(AirScalar::UChar)
                },
                AirMember {
                    offset: 4,
                    ty: AirType::Scalar(AirScalar::Float)
                },
            ])),
            Some((8, 4))
        );
    }

    #[test]
    fn air_metadata_incompatible_with_vulkan_block_layout_requires_a_byte_view() {
        let m = module();
        let overlapping = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Array {
                    elem: Box::new(AirType::Scalar(AirScalar::UInt)),
                    len: 8,
                },
            },
            AirMember {
                offset: 16,
                ty: AirType::Array {
                    elem: Box::new(AirType::Scalar(AirScalar::UInt)),
                    len: 8,
                },
            },
        ]);
        assert!(m.air_metadata_requires_byte_view(&overlapping));

        let inner = || {
            AirType::Struct(vec![
                AirMember {
                    offset: 0,
                    ty: AirType::Scalar(AirScalar::UShort),
                },
                AirMember {
                    offset: 12,
                    ty: AirType::Scalar(AirScalar::UChar),
                },
            ])
        };
        let stride_adjacent = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Array {
                    elem: Box::new(inner()),
                    len: 2,
                },
            },
            AirMember {
                offset: 28,
                ty: AirType::Scalar(AirScalar::UInt),
            },
        ]);
        assert!(!m.air_metadata_requires_byte_view(&stride_adjacent));

        let stride_overlap = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Array {
                    elem: Box::new(inner()),
                    len: 2,
                },
            },
            AirMember {
                offset: 20,
                ty: AirType::Scalar(AirScalar::UInt),
            },
        ]);
        assert!(m.air_metadata_requires_byte_view(&stride_overlap));

        let adjacent = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Scalar(AirScalar::UInt),
            },
            AirMember {
                offset: 4,
                ty: AirType::Scalar(AirScalar::UInt),
            },
        ]);
        assert!(!m.air_metadata_requires_byte_view(&adjacent));
    }

    #[test]
    fn differential_calculators_agree_on_scalars_and_scalar_aggregates() {
        let m = module();
        let agree = [
            LlType::Bool,
            LlType::Int(8),
            LlType::Int(16),
            LlType::Int(32),
            LlType::Int(64),
            LlType::Half,
            LlType::Float,
            LlType::Ptr(1),
            LlType::Array(Box::new(LlType::Float), 3),
            LlType::Array(Box::new(LlType::Half), 3),
            LlType::Struct(vec![LlType::Int(8), LlType::Float]),
            LlType::Struct(vec![LlType::Float, LlType::Float, LlType::Int(32)]),
        ];
        for ty in agree {
            assert_eq!(
                m.type_storage_size_align(&ty),
                m.native_memcpy_type_size_align(&ty),
                "Native and Memcpy rules must agree on {ty:?}"
            );
        }
    }

    #[test]
    fn differential_calculators_diverge_on_vectors_by_design() {
        let m = module();
        for lanes in [2u32, 3, 4] {
            let ty = vec(LlType::Float, lanes);
            let native = m.type_storage_size_align(&ty).unwrap();
            let memcpy = m.native_memcpy_type_size_align(&ty).unwrap();
            assert_ne!(
                native, memcpy,
                "Native vs Memcpy are expected to differ for float{lanes} (packed vs padded)"
            );
        }
        assert_eq!(
            m.type_storage_size_align(&vec(LlType::Float, 3)),
            Some((12, 4))
        );
        assert_eq!(
            m.native_memcpy_type_size_align(&vec(LlType::Float, 3)),
            Some((16, 16))
        );
    }
}

#[cfg(test)]
mod resolve_known_type_tests {
    use super::*;

    fn module_with_types(air: &str) -> LlModule {
        LlModule::parse(air).expect("fixture parses")
    }

    #[test]
    fn named_struct_alias_resolves_to_structural_definition() {
        let m = module_with_types(concat!(
            "%struct._half8 = type { [8 x half] }\n",
            "define void @k() {\nentry:\n  ret void\n}\n",
        ));
        let named = LlType::Named("%struct._half8".to_string());
        let structural = LlType::Struct(vec![LlType::Array(Box::new(LlType::Half), 8)]);
        assert_eq!(m.resolve_known_type(&named), structural);
        assert_eq!(m.resolve_known_type(&structural), structural);
    }

    #[test]
    fn i1_and_single_lane_vector_canonicalize() {
        let m = module_with_types("define void @k() {\nentry:\n  ret void\n}\n");
        assert_eq!(m.resolve_known_type(&LlType::Int(1)), LlType::Bool);
        assert_eq!(
            m.resolve_known_type(&LlType::Vector(Box::new(LlType::Float), 1)),
            LlType::Float
        );
    }

    #[test]
    fn unknown_named_type_is_left_as_is() {
        let m = module_with_types("define void @k() {\nentry:\n  ret void\n}\n");
        let named = LlType::Named("%struct.absent".to_string());
        assert_eq!(m.resolve_known_type(&named), named);
    }
}

#[cfg(test)]
mod raw_buffer_inference_tests {
    use super::*;

    fn params(air: &str) -> HashSet<(String, String)> {
        LlModule::parse(air)
            .expect("fixture parses")
            .raw_buffer_params
    }

    #[test]
    fn byte_gep_then_wide_load_marks_param_raw() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %p = getelementptr i8, ptr addrspace(1) %buf, i64 0\n",
            "  %v = load i32, ptr addrspace(1) %p\n",
            "  ret void\n",
            "}\n",
        );
        assert!(params(air).contains(&("k".to_string(), "%buf".to_string())));
    }

    #[test]
    fn two_distinct_typed_loads_mark_param_raw() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %a = load float, ptr addrspace(1) %buf\n",
            "  %b = load i32, ptr addrspace(1) %buf\n",
            "  ret void\n",
            "}\n",
        );
        assert!(params(air).contains(&("k".to_string(), "%buf".to_string())));
    }

    #[test]
    fn pointer_value_store_marks_destination_param_raw() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %out, ptr addrspace(1) %source) {\n",
            "entry:\n",
            "  store ptr addrspace(1) %source, ptr addrspace(1) %out, align 8\n",
            "  ret void\n",
            "}\n",
        );
        assert!(params(air).contains(&("k".to_string(), "%out".to_string())));
    }

    #[test]
    fn nested_select_gep_infers_every_pointer_param_pointee() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, i1 %x, i1 %y) {\n",
            "entry:\n",
            "  %inner = select i1 %x, ptr addrspace(1) %a, ptr addrspace(1) %b\n",
            "  %outer = select i1 %y, ptr addrspace(1) %inner, ptr addrspace(1) %c\n",
            "  %element = getelementptr half, ptr addrspace(1) %outer, i64 0\n",
            "  %value = load half, ptr addrspace(1) %element\n",
            "  ret void\n",
            "}\n",
        );
        let module = LlModule::parse(air).expect("fixture parses");
        for param in ["%a", "%b", "%c"] {
            assert_eq!(
                module
                    .ptr_pointees
                    .get(&("k".to_string(), param.to_string())),
                Some(&LlType::Half)
            );
        }
    }

    #[test]
    fn single_typed_load_leaves_param_typed() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %v = load float, ptr addrspace(1) %buf\n",
            "  ret void\n",
            "}\n",
        );
        assert!(!params(air).contains(&("k".to_string(), "%buf".to_string())));
    }

    #[test]
    fn opaque_memcpy_source_into_local_aggregate_is_raw() {
        let air = concat!(
            "%S = type { <4 x float> }\n",
            "define void @k(ptr addrspace(2) %src) {\n",
            "entry:\n",
            "  %dst = alloca %S\n",
            "  %field = getelementptr %S, ptr %dst, i64 0, i32 0\n",
            "  %source = bitcast ptr addrspace(2) %src to ptr addrspace(2)\n",
            "  call void @llvm.memcpy.p0.p2.i64(ptr %field, ptr addrspace(2) %source, i64 16, i1 false)\n",
            "  ret void\n",
            "}\n",
            "declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)\n",
        );
        assert!(params(air).contains(&("k".to_string(), "%src".to_string())));
    }

    #[test]
    fn typed_memcpy_source_into_local_aggregate_stays_typed() {
        let air = concat!(
            "%S = type { <4 x float> }\n",
            "define void @k(ptr addrspace(2) %src) {\n",
            "entry:\n",
            "  %dst = alloca %S\n",
            "  %field = getelementptr %S, ptr %dst, i64 0, i32 0\n",
            "  %source = getelementptr <4 x float>, ptr addrspace(2) %src, i64 0\n",
            "  call void @llvm.memcpy.p0.p2.i64(ptr %field, ptr addrspace(2) %source, i64 16, i1 false)\n",
            "  ret void\n",
            "}\n",
            "declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)\n",
        );
        assert!(!params(air).contains(&("k".to_string(), "%src".to_string())));
    }

    #[test]
    fn opaque_memcpy_destination_does_not_imply_local_aggregate_storage() {
        let air = concat!(
            "define void @k(ptr addrspace(2) %src) {\n",
            "entry:\n",
            "  %dst = call ptr @destination()\n",
            "  call void @llvm.memcpy.p0.p2.i64(ptr %dst, ptr addrspace(2) %src, i64 16, i1 false)\n",
            "  ret void\n",
            "}\n",
            "declare ptr @destination()\n",
            "declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)\n",
        );
        assert!(!params(air).contains(&("k".to_string(), "%src".to_string())));
    }

    #[test]
    fn non_pointer_param_is_never_raw() {
        let air = concat!(
            "define void @k(i32 %n) {\n",
            "entry:\n",
            "  ret void\n",
            "}\n",
        );
        assert!(params(air).is_empty());
    }

    #[test]
    fn raw_buffer_mark_reaches_helper_through_byte_gep_alias() {
        let air = concat!(
            "define void @entry(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %byte = getelementptr i8, ptr addrspace(1) %buf, i64 4\n",
            "  %word = load i32, ptr addrspace(1) %byte\n",
            "  %alias = bitcast ptr addrspace(1) %byte to ptr addrspace(1)\n",
            "  call void @helper(ptr addrspace(1) %alias)\n",
            "  ret void\n",
            "}\n",
            "define void @helper(ptr addrspace(1) %p) {\n",
            "entry:\n",
            "  %f = getelementptr float, ptr addrspace(1) %p, i64 0\n",
            "  %v = load float, ptr addrspace(1) %f\n",
            "  ret void\n",
            "}\n",
        );
        let module = LlModule::parse(air).expect("fixture parses");
        let entry = ("entry".to_string(), "%buf".to_string());
        let helper = ("helper".to_string(), "%p".to_string());
        assert!(module.raw_buffer_params.contains(&entry));
        assert!(module.raw_buffer_params.contains(&helper));
        assert!(module.call_connected_raw_params.contains(&entry));
        assert!(module.call_connected_raw_params.contains(&helper));
    }

    #[test]
    fn incompatible_call_connected_aggregate_views_select_raw_buffers() {
        let air = concat!(
            "define void @entry(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %alias = getelementptr [3 x i8], ptr addrspace(1) %buf, i64 0, i64 0\n",
            "  call void @helper(ptr addrspace(1) %alias)\n",
            "  ret void\n",
            "}\n",
            "define void @helper(ptr addrspace(1) %p) {\n",
            "entry:\n",
            "  %field = getelementptr [3 x float], ptr addrspace(1) %p, i64 0, i64 1\n",
            "  %value = load float, ptr addrspace(1) %field\n",
            "  ret void\n",
            "}\n",
        );
        let raw = params(air);
        assert!(raw.contains(&("entry".to_string(), "%buf".to_string())));
        assert!(raw.contains(&("helper".to_string(), "%p".to_string())));
    }

    #[test]
    fn call_connected_workgroup_reinterpretation_selects_raw_words() {
        let air = concat!(
            "define void @entry(ptr addrspace(3) %scratch) {\n",
            "entry:\n",
            "  %local = getelementptr float, ptr addrspace(3) %scratch, i64 0\n",
            "  store float 0.000000e+00, ptr addrspace(3) %local\n",
            "  call void @write_uint(ptr addrspace(3) %scratch)\n",
            "  call void @write_float(ptr addrspace(3) %scratch)\n",
            "  ret void\n",
            "}\n",
            "define void @write_uint(ptr addrspace(3) %p) {\n",
            "entry:\n",
            "  %slot = getelementptr i32, ptr addrspace(3) %p, i64 0\n",
            "  store i32 1, ptr addrspace(3) %slot\n",
            "  ret void\n",
            "}\n",
            "define void @write_float(ptr addrspace(3) %p) {\n",
            "entry:\n",
            "  %slot = getelementptr float, ptr addrspace(3) %p, i64 0\n",
            "  store float 1.000000e+00, ptr addrspace(3) %slot\n",
            "  ret void\n",
            "}\n",
            "define void @unrelated(ptr addrspace(3) %typed) {\n",
            "entry:\n",
            "  store float 2.000000e+00, ptr addrspace(3) %typed\n",
            "  ret void\n",
            "}\n",
        );
        let module = LlModule::parse(air).expect("fixture parses");
        for key in [
            ("entry".to_string(), "%scratch".to_string()),
            ("write_uint".to_string(), "%p".to_string()),
            ("write_float".to_string(), "%p".to_string()),
        ] {
            assert!(module.raw_buffer_params.contains(&key));
            assert!(module.call_connected_raw_params.contains(&key));
        }
        assert!(!module
            .raw_buffer_params
            .contains(&("unrelated".to_string(), "%typed".to_string())));
    }

    #[test]
    fn helper_raw_mark_reaches_entry_through_byte_gep_alias() {
        let air = concat!(
            "define void @entry(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %alias = getelementptr i8, ptr addrspace(1) %buf, i64 4\n",
            "  call void @helper(ptr addrspace(1) %alias)\n",
            "  ret void\n",
            "}\n",
            "define void @helper(ptr addrspace(1) %p) {\n",
            "entry:\n",
            "  %byte = getelementptr i8, ptr addrspace(1) %p, i64 0\n",
            "  %word = load i32, ptr addrspace(1) %byte\n",
            "  ret void\n",
            "}\n",
        );
        let raw = params(air);
        assert!(raw.contains(&("entry".to_string(), "%buf".to_string())));
        assert!(raw.contains(&("helper".to_string(), "%p".to_string())));
    }

    #[test]
    fn raw_buffer_mark_does_not_cross_select_between_parameters() {
        let air = concat!(
            "define void @entry(ptr addrspace(1) %a, ptr addrspace(1) %b, i1 %cond) {\n",
            "entry:\n",
            "  %byte = getelementptr i8, ptr addrspace(1) %a, i64 4\n",
            "  %word = load i32, ptr addrspace(1) %byte\n",
            "  %merged = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b\n",
            "  call void @helper(ptr addrspace(1) %merged)\n",
            "  ret void\n",
            "}\n",
            "define void @helper(ptr addrspace(1) %p) {\n",
            "entry:\n",
            "  %f = getelementptr float, ptr addrspace(1) %p, i64 0\n",
            "  %v = load float, ptr addrspace(1) %f\n",
            "  ret void\n",
            "}\n",
        );
        let raw = params(air);
        assert!(raw.contains(&("entry".to_string(), "%a".to_string())));
        assert!(!raw.contains(&("entry".to_string(), "%b".to_string())));
        assert!(!raw.contains(&("helper".to_string(), "%p".to_string())));
    }
}

#[cfg(test)]
mod pointee_inference_tests {
    use super::*;

    fn parsed(air: &str) -> LlModule {
        LlModule::parse(air).expect("fixture parses")
    }

    #[test]
    fn param_gep_source_becomes_pointer_pointee() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %p = getelementptr float, ptr addrspace(1) %buf, i64 0\n",
            "  ret void\n",
            "}\n",
        );
        assert_eq!(
            parsed(air)
                .ptr_pointees
                .get(&("k".to_string(), "%buf".to_string())),
            Some(&LlType::Float)
        );
    }

    #[test]
    fn first_gep_source_wins_for_pointer_pointee() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  %a = getelementptr float, ptr addrspace(1) %buf, i64 0\n",
            "  %b = getelementptr i32, ptr addrspace(1) %buf, i64 4\n",
            "  ret void\n",
            "}\n",
        );
        assert_eq!(
            parsed(air)
                .ptr_pointees
                .get(&("k".to_string(), "%buf".to_string())),
            Some(&LlType::Float)
        );
    }

    #[test]
    fn unindexed_param_has_no_pointee() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %buf) {\n",
            "entry:\n",
            "  ret void\n",
            "}\n",
        );
        assert!(parsed(air).ptr_pointees.is_empty());
    }

    #[test]
    fn direct_store_value_becomes_pointer_param_pointee() {
        let air = concat!(
            "define void @k(ptr addrspace(1) %out, float %value) {\n",
            "entry:\n",
            "  store float %value, ptr addrspace(1) %out, align 4\n",
            "  ret void\n",
            "}\n",
        );
        assert_eq!(
            parsed(air)
                .ptr_pointees
                .get(&("k".to_string(), "%out".to_string())),
            Some(&LlType::Float)
        );
    }

    #[test]
    fn primitive_metadata_seeds_cross_buffer_phi_roots_without_direct_geps() {
        let air = r#"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %select_a, ptr addrspace(1) %select_b, i1 %cond) {
entry:
  %select = select i1 %cond, ptr addrspace(1) %select_a, ptr addrspace(1) %select_b
  br i1 %cond, label %left, label %right
left:
  br label %join
right:
  br label %join
join:
  %merged = phi ptr addrspace(1) [ %a, %left ], [ %b, %right ]
  %p = getelementptr float, ptr addrspace(1) %merged, i64 0
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"select_a"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"select_b"}
"#;
        let unseeded = parsed(air);
        assert!(!unseeded
            .ptr_pointees
            .contains_key(&("k".to_string(), "%a".to_string())));
        assert!(!unseeded
            .ptr_pointees
            .contains_key(&("k".to_string(), "%b".to_string())));
        let module = LlModule::parse_with_primitive_phi_metadata(air).expect("fixture parses");
        assert_eq!(
            module
                .ptr_pointees
                .get(&("k".to_string(), "%a".to_string())),
            Some(&LlType::Float)
        );
        assert_eq!(
            module
                .ptr_pointees
                .get(&("k".to_string(), "%b".to_string())),
            Some(&LlType::Float)
        );
        assert!(!module
            .ptr_pointees
            .contains_key(&("k".to_string(), "%select_a".to_string())));
        assert!(!module
            .ptr_pointees
            .contains_key(&("k".to_string(), "%select_b".to_string())));
    }

    #[test]
    fn same_size_gep_reinterpret_records_alloca_pointee() {
        let air = concat!(
            "define void @k() {\n",
            "entry:\n",
            "  %a = alloca i32\n",
            "  %p = getelementptr float, ptr %a, i64 0\n",
            "  ret void\n",
            "}\n",
        );
        assert_eq!(
            parsed(air)
                .local_alloca_pointees
                .get(&("k".to_string(), "%a".to_string())),
            Some(&LlType::Float)
        );
    }

    #[test]
    fn same_type_gep_leaves_alloca_unreinterpreted() {
        let air = concat!(
            "define void @k() {\n",
            "entry:\n",
            "  %a = alloca i32\n",
            "  %p = getelementptr i32, ptr %a, i64 0\n",
            "  ret void\n",
            "}\n",
        );
        assert!(parsed(air).local_alloca_pointees.is_empty());
    }

    #[test]
    fn scalar_alloca_with_byte_view_uses_bounded_byte_storage() {
        let air = concat!(
            "define void @k() {\n",
            "entry:\n",
            "  %slot = alloca float, align 4\n",
            "  %alias = bitcast ptr %slot to ptr\n",
            "  %high = getelementptr i8, ptr %alias, i64 2\n",
            "  store half 0xH3C00, ptr %high, align 2\n",
            "  ret void\n",
            "}\n",
        );
        assert_eq!(
            parsed(air)
                .local_alloca_pointees
                .get(&("k".to_string(), "%slot".to_string())),
            Some(&LlType::Array(Box::new(LlType::Int(8)), 4))
        );
    }
}

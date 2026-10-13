use crate::fxhash::{HashMap, HashSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkedFunctionLinkage {
    pub visible_references: Vec<LinkedFunctionReference>,
    pub visible_tables: Vec<LinkedFunctionTable>,
    pub intersection_tables: Vec<IntersectionFunctionTable>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedFunctionReference {
    pub symbol: String,
    pub module_ll: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedFunctionTable {
    pub parameter_index: u32,
    pub size: u32,
    pub entries: Vec<LinkedFunction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedFunction {
    pub index: u32,
    pub symbol: String,
    pub module_ll: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntersectionFunctionTable {
    pub source: IntersectionFunctionTableSource,
    pub size: u32,
    pub entries: Vec<IntersectionFunctionEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntersectionFunctionTableSource {
    Parameter {
        parameter_index: u32,
    },
    ArgumentBuffer {
        buffer_parameter_index: u32,
        field_ordinal: u32,
        field_offset: u32,
    },
}

impl IntersectionFunctionTableSource {
    fn parameter_index(self) -> u32 {
        match self {
            Self::Parameter { parameter_index } => parameter_index,
            Self::ArgumentBuffer {
                buffer_parameter_index,
                ..
            } => buffer_parameter_index,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntersectionFunctionEntry {
    Linked(LinkedFunction),
    OpaqueTriangle {
        index: u32,
        signature: Vec<IntersectionFunctionSignature>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum IntersectionFunctionSignature {
    Instancing,
    TriangleData,
    WorldSpaceData,
    InstanceMotion,
    PrimitiveMotion,
    ExtendedLimits,
    MaxLevels,
    IntersectionFunctionBuffer,
    UserData,
}

#[derive(Clone)]
struct PointerTrace<'a> {
    table: &'a LinkedFunctionTable,
    index: String,
}

#[derive(Default)]
struct LinkedFlow<'a> {
    table_parameters: HashMap<(String, usize), &'a LinkedFunctionTable>,
    pointer_parameters: HashMap<(String, usize), &'a LinkedFunctionTable>,
}

#[derive(Clone, Debug)]
struct FunctionSignature {
    parameters: Vec<String>,
}

impl LinkedFunctionLinkage {
    pub fn is_empty(&self) -> bool {
        self.visible_references.is_empty()
            && self.visible_tables.is_empty()
            && self.intersection_tables.is_empty()
    }
}

pub fn specialize_opaque_triangle_intersection_tables(
    entry_ll: &str,
    entry_name: &str,
    linkage: &LinkedFunctionLinkage,
) -> Result<String, String> {
    if linkage.intersection_tables.is_empty() {
        return Ok(entry_ll.to_string());
    }
    let signatures = function_signatures(entry_ll)?;
    let entry_global = llvm_global(entry_name)?;
    let entry_signature = signatures.get(&entry_global).ok_or_else(|| {
        format!("intersection-table entry function {entry_name:?} is not defined")
    })?;
    let mut flow = HashMap::<(String, usize), &IntersectionFunctionTable>::default();
    let mut embedded_roots = HashMap::<String, Vec<&IntersectionFunctionTable>>::default();
    for table in &linkage.intersection_tables {
        let parameter_index = table.source.parameter_index();
        if table.size == 0 {
            return Err(format!(
                "intersection function-table parameter {} has zero size",
                parameter_index
            ));
        }
        entry_signature
            .parameters
            .get(parameter_index as usize)
            .ok_or_else(|| {
                format!(
                    "intersection function-table parameter {} exceeds entry {:?} arity {}",
                    parameter_index,
                    entry_name,
                    entry_signature.parameters.len()
                )
            })?;
        match table.source {
            IntersectionFunctionTableSource::Parameter { parameter_index } => {
                if flow
                    .insert((entry_global.clone(), parameter_index as usize), table)
                    .is_some()
                {
                    return Err(format!(
                        "duplicate intersection function-table parameter {parameter_index}"
                    ));
                }
            }
            IntersectionFunctionTableSource::ArgumentBuffer { .. } => {
                embedded_roots
                    .entry(entry_signature.parameters[parameter_index as usize].clone())
                    .or_default()
                    .push(table);
            }
        }
    }
    propagate_intersection_table_flow(
        entry_ll,
        &entry_global,
        &signatures,
        &embedded_roots,
        &mut flow,
    )?;

    let mut output = String::with_capacity(entry_ll.len());
    let mut current = None::<String>;
    let mut tables = HashMap::<String, &IntersectionFunctionTable>::default();
    let mut embedded_pointers = HashMap::<String, &IntersectionFunctionTable>::default();
    for line in entry_ll.lines() {
        let trimmed = line.trim_start();
        if let Some(global) = definition_global(trimmed) {
            current = Some(global.clone());
            tables.clear();
            embedded_pointers.clear();
            let signature = signatures
                .get(&global)
                .ok_or_else(|| format!("missing parsed signature for {global}"))?;
            for (ordinal, parameter) in signature.parameters.iter().enumerate() {
                if let Some(table) = flow.get(&(global.clone(), ordinal)) {
                    tables.insert(parameter.clone(), *table);
                }
            }
            output.push_str(line);
            output.push('\n');
            continue;
        }
        if trimmed == "}" {
            current = None;
            tables.clear();
            embedded_pointers.clear();
            output.push_str(line);
            output.push('\n');
            continue;
        }
        if current.is_some() {
            if current.as_deref() == Some(entry_global.as_str()) {
                trace_embedded_intersection_table(
                    trimmed,
                    &embedded_roots,
                    &mut embedded_pointers,
                    &mut tables,
                );
            }
            if let Some((result, instruction)) = trimmed.split_once(" = ") {
                if instruction.starts_with("bitcast ") || instruction.starts_with("addrspacecast ")
                {
                    if let Some(source) = cast_source_value(instruction) {
                        if let Some(table) = tables.get(source) {
                            tables.insert(result.trim().to_string(), *table);
                        }
                    }
                }
            }
            if let Some((callee, open, close, arguments)) = named_call(trimmed) {
                let symbol = callee.trim_start_matches('@').trim_matches('"');
                if symbol.starts_with("air.set_buffer_intersection_function_table.") {
                    if arguments.len() != 3 {
                        return Err(format!(
                            "AIR intersection function-table setter {symbol} has {} operands, expected 3",
                            arguments.len()
                        ));
                    }
                    if tables.contains_key(value_operand(arguments[0])) {
                        continue;
                    }
                }
                if let Some(family) = crate::meta::AirIntersectionFamily::parse(symbol)? {
                    if family.intersection_function_buffer {
                        let table_ordinal = family.intersection_table_argument_index();
                        if let Some(table) = arguments
                            .get(table_ordinal)
                            .and_then(|argument| tables.get(value_operand(argument)))
                        {
                            if opaque_table_matches_family(table, &family) {
                                let removed_arguments = family
                                    .opaque_triangle_removed_argument_indices()
                                    .expect("callback family has callback operands");
                                let expected_arguments = family.argument_count();
                                if arguments.len() != expected_arguments {
                                    return Err(format!(
                                        "AIR intersection call {symbol} has {} operands, expected {expected_arguments}",
                                        arguments.len(),
                                    ));
                                }
                                let kept = arguments
                                    .iter()
                                    .enumerate()
                                    .filter(|(ordinal, _)| !removed_arguments.contains(ordinal))
                                    .map(|(_, argument)| *argument)
                                    .collect::<Vec<_>>();
                                let callback_free = symbol
                                    .split('.')
                                    .filter(|token| {
                                        !matches!(
                                            *token,
                                            "intersection_function_buffer" | "user_data"
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join(".");
                                let leading = line.len() - trimmed.len();
                                output.push_str(&line[..leading]);
                                output.push_str(&trimmed[..open - callee.len()]);
                                output.push('@');
                                output.push_str(&callback_free);
                                output.push('(');
                                output.push_str(&kept.join(", "));
                                output.push_str(&trimmed[close..]);
                                output.push('\n');
                                continue;
                            }
                        }
                    }
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    Ok(output)
}

fn opaque_table_matches_family(
    table: &IntersectionFunctionTable,
    family: &crate::meta::AirIntersectionFamily,
) -> bool {
    if table.entries.len() != table.size as usize {
        return false;
    }
    let expected = opaque_triangle_signature(family);
    let Some(expected) = expected else {
        return false;
    };
    table.entries.iter().enumerate().all(|(index, entry)| {
        let IntersectionFunctionEntry::OpaqueTriangle {
            index: entry_index,
            signature,
        } = entry
        else {
            return false;
        };
        let mut signature = signature.clone();
        signature.sort_unstable();
        *entry_index == index as u32 && signature == expected
    })
}

pub fn opaque_triangle_signature(
    family: &crate::meta::AirIntersectionFamily,
) -> Option<Vec<IntersectionFunctionSignature>> {
    if !family.intersection_function_buffer {
        return None;
    }
    let mut expected = Vec::new();
    use crate::meta::AirIntersectionInstancing;
    if family.instancing != AirIntersectionInstancing::None {
        expected.push(IntersectionFunctionSignature::Instancing);
    }
    if family.triangle_data {
        expected.push(IntersectionFunctionSignature::TriangleData);
    }
    if family.world_space_data {
        expected.push(IntersectionFunctionSignature::WorldSpaceData);
    }
    if family.instance_motion {
        expected.push(IntersectionFunctionSignature::InstanceMotion);
    }
    if family.primitive_motion {
        expected.push(IntersectionFunctionSignature::PrimitiveMotion);
    }
    if family.extended_limits {
        expected.push(IntersectionFunctionSignature::ExtendedLimits);
    }
    if family.instancing == AirIntersectionInstancing::MultiLevel {
        expected.push(IntersectionFunctionSignature::MaxLevels);
    }
    expected.push(IntersectionFunctionSignature::IntersectionFunctionBuffer);
    if family.user_data {
        expected.push(IntersectionFunctionSignature::UserData);
    }
    expected.sort_unstable();
    Some(expected)
}

fn trace_embedded_intersection_table<'a>(
    line: &str,
    roots: &HashMap<String, Vec<&'a IntersectionFunctionTable>>,
    pointers: &mut HashMap<String, &'a IntersectionFunctionTable>,
    tables: &mut HashMap<String, &'a IntersectionFunctionTable>,
) {
    let Some((result, instruction)) = line.split_once(" = ") else {
        return;
    };
    let result = result.trim();
    if instruction.starts_with("getelementptr ")
        || instruction.starts_with("getelementptr inbounds ")
    {
        let operands = split_top_level(instruction, ',');
        let Some(base) = operands.get(1).map(|operand| value_operand(operand)) else {
            return;
        };
        let Some(field_ordinal) = operands.last().and_then(|operand| integer_operand(operand))
        else {
            return;
        };
        if let Some(table) = roots.get(base).and_then(|tables| {
            tables.iter().copied().find(|table| {
                matches!(
                    table.source,
                    IntersectionFunctionTableSource::ArgumentBuffer {
                        field_ordinal: authored,
                        ..
                    } if authored == field_ordinal
                )
            })
        }) {
            pointers.insert(result.to_string(), table);
        }
        return;
    }
    if instruction.starts_with("load ") {
        let operands = split_top_level(instruction, ',');
        if let Some(pointer) = operands.get(1).map(|operand| value_operand(operand)) {
            if let Some(table) = pointers.get(pointer) {
                tables.insert(result.to_string(), *table);
            }
        }
    }
}

fn propagate_intersection_table_flow<'a>(
    ll: &str,
    entry_global: &str,
    signatures: &HashMap<String, FunctionSignature>,
    embedded_roots: &HashMap<String, Vec<&'a IntersectionFunctionTable>>,
    flow: &mut HashMap<(String, usize), &'a IntersectionFunctionTable>,
) -> Result<(), String> {
    loop {
        let mut changed = false;
        let mut current = None::<String>;
        let mut tables = HashMap::<String, &'a IntersectionFunctionTable>::default();
        let mut embedded_pointers = HashMap::<String, &'a IntersectionFunctionTable>::default();
        for line in ll.lines() {
            let trimmed = line.trim_start();
            if let Some(global) = definition_global(trimmed) {
                current = Some(global.clone());
                tables.clear();
                embedded_pointers.clear();
                let signature = signatures
                    .get(&global)
                    .ok_or_else(|| format!("missing parsed signature for {global}"))?;
                for (ordinal, parameter) in signature.parameters.iter().enumerate() {
                    if let Some(table) = flow.get(&(global.clone(), ordinal)) {
                        tables.insert(parameter.clone(), *table);
                    }
                }
                continue;
            }
            if trimmed == "}" {
                current = None;
                continue;
            }
            if current.is_none() {
                continue;
            }
            if current.as_deref() == Some(entry_global) {
                trace_embedded_intersection_table(
                    trimmed,
                    embedded_roots,
                    &mut embedded_pointers,
                    &mut tables,
                );
            }
            if let Some((result, instruction)) = trimmed.split_once(" = ") {
                if instruction.starts_with("bitcast ") || instruction.starts_with("addrspacecast ")
                {
                    if let Some(source) = cast_source_value(instruction) {
                        if let Some(table) = tables.get(source) {
                            tables.insert(result.trim().to_string(), *table);
                        }
                    }
                }
            }
            let Some((callee, _, _, arguments)) = named_call(trimmed) else {
                continue;
            };
            let Some(callee_signature) = signatures.get(callee) else {
                continue;
            };
            for (ordinal, argument) in arguments
                .iter()
                .take(callee_signature.parameters.len())
                .enumerate()
            {
                if let Some(table) = tables.get(value_operand(argument)) {
                    let key = (callee.to_string(), ordinal);
                    match flow.get(&key) {
                        Some(previous) if !std::ptr::eq(*previous, *table) => {
                            return Err(format!(
                                "function parameter {ordinal} of {callee} receives multiple intersection tables"
                            ));
                        }
                        Some(_) => {}
                        None => {
                            flow.insert(key, *table);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

pub fn specialize_visible_function_references(
    entry_ll: &str,
    linkage: &LinkedFunctionLinkage,
) -> Result<String, String> {
    validate_linkage(linkage)?;
    let authored = linkage
        .visible_references
        .iter()
        .map(|reference| (reference.symbol.as_str(), reference))
        .collect::<HashMap<_, _>>();
    let mut used = HashSet::<&str>::default();
    let mut appended_modules = HashSet::<&str>::default();
    let mut pending = Vec::<&LinkedFunctionReference>::new();
    let mut output = rewrite_visible_reference_stubs(entry_ll, &authored, &mut used, &mut pending)?;

    while let Some(reference) = pending.pop() {
        if !appended_modules.insert(reference.module_ll.as_str()) {
            continue;
        }
        let rewritten = rewrite_visible_reference_stubs(
            &reference.module_ll,
            &authored,
            &mut used,
            &mut pending,
        )?;
        append_dependency_module(&mut output, &rewritten);
    }
    let direct_functions = used
        .into_iter()
        .map(llvm_global)
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(crate::native::inline_direct_function_pointer_consumers(
        &output,
        &direct_functions,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AirVisibleFunctionReference {
    stub_global: String,
    symbol: String,
}

pub fn visible_function_reference_symbols(module: &str) -> Result<Vec<String>, String> {
    air_visible_function_references(module).map(|references| {
        references
            .into_iter()
            .map(|reference| reference.symbol)
            .collect()
    })
}

fn rewrite_visible_reference_stubs<'a>(
    module: &str,
    authored: &HashMap<&str, &'a LinkedFunctionReference>,
    used: &mut HashSet<&'a str>,
    pending: &mut Vec<&'a LinkedFunctionReference>,
) -> Result<String, String> {
    let references = air_visible_function_references(module)?;
    let mut replacements = Vec::new();
    for reference in references {
        let Some(dependency) = authored.get(reference.symbol.as_str()) else {
            replacements.push((
                reference.stub_global,
                llvm_global(&format!("{}{UNRESOLVED_VISIBLE_SUFFIX}", reference.symbol))?,
            ));
            continue;
        };
        used.insert(dependency.symbol.as_str());
        pending.push(dependency);
        replacements.push((reference.stub_global, llvm_global(&reference.symbol)?));
    }
    let mut output = String::with_capacity(module.len());
    for line in module.lines() {
        if line
            .trim_start()
            .starts_with("!air.visible_function_references =")
        {
            continue;
        }
        let mut line = line.to_string();
        for (stub, target) in &replacements {
            line = line.replace(stub, target);
        }
        output.push_str(&line);
        output.push('\n');
    }
    Ok(output)
}

fn air_visible_function_references(
    module: &str,
) -> Result<Vec<AirVisibleFunctionReference>, String> {
    const MARKER: &str = "!\"air.visible_function_reference\", ptr ";
    let mut references = Vec::new();
    let mut stubs = HashMap::<String, String>::default();
    for line in module.lines() {
        let Some(marker) = line.find(MARKER) else {
            continue;
        };
        let body = &line[marker + MARKER.len()..];
        let at = body
            .find('@')
            .ok_or_else(|| format!("AIR visible-function reference has no stub global: {line}"))?;
        let stub_end = llvm_metadata_global_end(body, at).ok_or_else(|| {
            format!("AIR visible-function reference has malformed stub global: {line}")
        })?;
        let stub_global = body[at..stub_end].to_string();
        let rest = &body[stub_end..];
        let name_start = rest.find(", !\"").ok_or_else(|| {
            format!("AIR visible-function reference has no logical symbol: {line}")
        })? + 4;
        let (encoded, _) = llvm_quoted_string(&rest[name_start..]).ok_or_else(|| {
            format!("AIR visible-function reference has malformed logical symbol: {line}")
        })?;
        let symbol = decode_llvm_string(encoded)?;
        if let Some(previous) = stubs.insert(stub_global.clone(), symbol.clone()) {
            if previous != symbol {
                return Err(format!(
                    "AIR visible-function stub {stub_global} maps to both {previous:?} and {symbol:?}"
                ));
            }
            continue;
        }
        references.push(AirVisibleFunctionReference {
            stub_global,
            symbol,
        });
    }
    Ok(references)
}

fn llvm_metadata_global_end(text: &str, at: usize) -> Option<usize> {
    if text.as_bytes().get(at) != Some(&b'@') {
        return None;
    }
    if text.as_bytes().get(at + 1) != Some(&b'\"') {
        return text[at..]
            .find(|character: char| character == ',' || character.is_ascii_whitespace())
            .map(|relative| at + relative);
    }
    let (_, consumed) = llvm_quoted_string(&text[at + 2..])?;
    Some(at + 2 + consumed)
}

fn llvm_quoted_string(text: &str) -> Option<(&str, usize)> {
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\"' && !escaped {
            return Some((&text[..index], index + 1));
        }
        escaped = byte == b'\\' && !escaped;
        if byte != b'\\' {
            escaped = false;
        }
    }
    None
}

fn decode_llvm_string(encoded: &str) -> Result<String, String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let pair = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| format!("unterminated LLVM string escape in {encoded:?}"))?;
            let hex = std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| format!("invalid LLVM string escape in {encoded:?}"))?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| format!("non-UTF-8 LLVM symbol in {encoded:?}"))
}

pub fn trace_visible_function_table_parameters(
    entry_ll: &str,
    entry_name: &str,
    parameter_indices: &[u32],
) -> Result<HashMap<String, HashSet<String>>, String> {
    let tables = parameter_indices
        .iter()
        .map(|parameter_index| LinkedFunctionTable {
            parameter_index: *parameter_index,
            size: 1,
            entries: Vec::new(),
        })
        .collect::<Vec<_>>();
    let (signatures, flow) = visible_table_flow(entry_ll, entry_name, &tables)?;
    let mut values = HashMap::<String, HashSet<String>>::default();
    for ((function, ordinal), _) in flow.table_parameters {
        let parameter = signatures
            .get(&function)
            .and_then(|signature| signature.parameters.get(ordinal))
            .ok_or_else(|| format!("missing parameter {ordinal} of {function}"))?;
        values
            .entry(function)
            .or_default()
            .insert(parameter.clone());
    }
    Ok(values)
}

fn visible_table_flow<'a>(
    entry_ll: &str,
    entry_name: &str,
    tables: &'a [LinkedFunctionTable],
) -> Result<(HashMap<String, FunctionSignature>, LinkedFlow<'a>), String> {
    let signatures = function_signatures(entry_ll)?;
    let entry_global = llvm_global(entry_name)?;
    let entry_signature = signatures.get(&entry_global).ok_or_else(|| {
        format!("linked function-table entry function {entry_name:?} is not defined")
    })?;
    let mut flow = LinkedFlow::default();
    for table in tables {
        let parameter = entry_signature
            .parameters
            .get(table.parameter_index as usize)
            .ok_or_else(|| {
                format!(
                    "linked function table parameter {} exceeds entry {:?} arity {}",
                    table.parameter_index,
                    entry_name,
                    entry_signature.parameters.len()
                )
            })?
            .clone();
        if flow
            .table_parameters
            .insert(
                (entry_global.clone(), table.parameter_index as usize),
                table,
            )
            .is_some()
        {
            return Err(format!(
                "multiple linked function tables target entry parameter {} ({parameter})",
                table.parameter_index
            ));
        }
    }
    propagate_linked_flow(entry_ll, &signatures, &mut flow)?;
    Ok((signatures, flow))
}

pub fn specialize_visible_function_tables(
    entry_ll: &str,
    entry_name: &str,
    linkage: &LinkedFunctionLinkage,
) -> Result<String, String> {
    if linkage.visible_tables.is_empty() {
        return Ok(entry_ll.to_string());
    }
    validate_linkage(linkage)?;
    let (signatures, flow) = visible_table_flow(entry_ll, entry_name, &linkage.visible_tables)?;

    struct Dispatcher<'a> {
        name: String,
        entries: Vec<&'a LinkedFunction>,
        return_type: String,
        argument_types: Vec<String>,
    }

    let mut tables_by_value = HashMap::<String, &LinkedFunctionTable>::default();
    let mut pointers = HashMap::<String, PointerTrace<'_>>::default();
    let mut sentinel_integers = HashMap::<String, PointerTrace<'_>>::default();
    let mut output = String::with_capacity(entry_ll.len());
    let mut linked_modules = Vec::<&str>::new();
    let mut linked_module_set = HashSet::<&str>::default();
    let mut dispatchers = Vec::<Dispatcher<'_>>::new();
    let mut dispatcher_names = HashMap::<String, String>::default();
    let mut presence_counter = 0usize;
    let mut current_function = None::<String>;
    for line in entry_ll.lines() {
        let mut line = line.to_string();
        let mut trimmed = line.trim_start();
        if let Some(global) = definition_global(trimmed) {
            let closes_inline = trimmed.ends_with('}');
            current_function = Some(global.clone());
            tables_by_value.clear();
            pointers.clear();
            sentinel_integers.clear();
            seed_function_values(
                &global,
                &signatures,
                &flow,
                &mut tables_by_value,
                &mut pointers,
            )?;
            line = append_pointer_slot_parameters(&line, &global, &signatures, &flow)?;
            output.push_str(&line);
            output.push('\n');
            if closes_inline {
                current_function = None;
            }
            continue;
        }
        if trimmed == "}" {
            output.push_str(&line);
            output.push('\n');
            current_function = None;
            tables_by_value.clear();
            pointers.clear();
            sentinel_integers.clear();
            continue;
        }
        let Some(current_global) = current_function.as_deref() else {
            output.push_str(&line);
            output.push('\n');
            continue;
        };
        if let Some(rewritten) = append_pointer_slot_call_arguments(
            &line,
            current_global,
            &signatures,
            &flow,
            &pointers,
        )? {
            output.push_str(&rewritten);
            output.push('\n');
            continue;
        }
        trimmed = line.trim_start();
        let mut table_query_replacement = None;
        if let Some((result, instruction)) = trimmed.split_once(" = ") {
            let result = result.trim();
            if instruction.contains("@air.get_function_pointer_visible_function_table(") {
                let arguments = call_arguments(instruction)?;
                if arguments.len() < 2 {
                    return Err(format!(
                        "visible function-table lookup has {} arguments: {trimmed}",
                        arguments.len()
                    ));
                }
                let table_value = value_operand(arguments[0]);
                if let Some(table) = tables_by_value.get(table_value) {
                    pointers.insert(
                        result.to_string(),
                        PointerTrace {
                            table,
                            index: arguments[1].to_string(),
                        },
                    );
                }
            } else if instruction.contains("@air.get_size_visible_function_table(") {
                let arguments = call_arguments(instruction)?;
                if let Some(table) = arguments
                    .first()
                    .and_then(|argument| tables_by_value.get(value_operand(argument)))
                {
                    let size = table.size;
                    table_query_replacement = Some(format!("{result} = add i32 0, {size}"));
                }
            } else if instruction.contains("@air.is_null_visible_function_table(") {
                let arguments = call_arguments(instruction)?;
                if arguments
                    .first()
                    .is_some_and(|argument| tables_by_value.contains_key(value_operand(argument)))
                {
                    table_query_replacement = Some(format!("{result} = or i1 false, false"));
                }
            } else if let Some((pointer_value, equal_to_null)) =
                null_pointer_comparison(instruction)
            {
                if let Some(pointer) = pointers.get(pointer_value) {
                    table_query_replacement = Some(authored_null_comparison(
                        result,
                        pointer.table,
                        &pointer.index,
                        equal_to_null,
                        &mut presence_counter,
                        entry_ll,
                    )?);
                }
            } else if instruction.starts_with("ptrtoint ") {
                if let Some(source) = cast_source_value(instruction) {
                    if let Some(pointer) = pointers.get(source).cloned() {
                        sentinel_integers.insert(result.to_string(), pointer);
                    }
                }
            } else if instruction.starts_with("trunc ") {
                if let Some(source) = cast_source_value(instruction) {
                    if let Some(pointer) = sentinel_integers.get(source).cloned() {
                        sentinel_integers.insert(result.to_string(), pointer);
                    }
                }
            } else if let Some((integer, equal_to_sentinel)) =
                opaque_sentinel_comparison(instruction)
            {
                if sentinel_integers.contains_key(integer) {
                    table_query_replacement =
                        Some(format!("{result} = or i1 false, {}", !equal_to_sentinel));
                }
            } else if instruction.starts_with("bitcast ")
                || instruction.starts_with("addrspacecast ")
            {
                if let Some(source) = cast_source_value(instruction) {
                    if let Some(pointer) = pointers.get(source).cloned() {
                        pointers.insert(result.to_string(), pointer);
                    }
                }
            }
        }
        if let Some(replacement) = table_query_replacement {
            let leading = line.len() - trimmed.len();
            output.push_str(&line[..leading]);
            output.push_str(&replacement);
            output.push('\n');
            continue;
        }

        let Some((callee_start, callee_end, callee)) = indirect_call_callee(trimmed) else {
            output.push_str(&line);
            output.push('\n');
            continue;
        };
        let Some(pointer) = pointers.get(callee).cloned() else {
            output.push_str(&line);
            output.push('\n');
            continue;
        };
        let call = indirect_call_shape(trimmed, callee_start, callee_end)?;
        let (global, prepend_index) = if let Some(index) = integer_operand(&pointer.index) {
            let function = pointer
                .table
                .entries
                .iter()
                .find(|entry| entry.index == index)
                .ok_or_else(|| {
                    format!(
                        "linked visible function table parameter {} has no entry at slot {index}",
                        pointer.table.parameter_index
                    )
                })?;
            if !linked_function_matches_call(function, &call)? {
                return Err(format!(
                    "linked visible function {:?} at table parameter {} slot {index} does not match indirect call type",
                    function.symbol, pointer.table.parameter_index
                ));
            }
            if linked_module_set.insert(function.module_ll.as_str()) {
                linked_modules.push(&function.module_ll);
            }
            (llvm_global(&function.symbol)?, false)
        } else {
            if !pointer.index.trim_start().starts_with("i32 ") {
                return Err(format!(
                    "linked visible function table parameter {} has a non-i32 slot operand {:?}",
                    pointer.table.parameter_index, pointer.index
                ));
            }
            let key = format!(
                "{}|{}|{}",
                pointer.table.parameter_index,
                call.return_type,
                call.argument_types.join(",")
            );
            let name = if let Some(name) = dispatcher_names.get(&key) {
                name.clone()
            } else {
                let name = format!(
                    "metal2vulkan.linked.table.p{}.dispatch.{}",
                    pointer.table.parameter_index,
                    dispatchers.len()
                );
                dispatcher_names.insert(key, name.clone());
                let mut entries = Vec::new();
                for function in &pointer.table.entries {
                    if linked_function_matches_call(function, &call)? {
                        entries.push(function);
                    }
                }
                if entries.is_empty() {
                    return Err(format!(
                        "linked visible function table parameter {} has no function matching indirect call type",
                        pointer.table.parameter_index
                    ));
                }
                dispatchers.push(Dispatcher {
                    name: name.clone(),
                    entries: entries.clone(),
                    return_type: call.return_type,
                    argument_types: call.argument_types,
                });
                for function in entries {
                    if linked_module_set.insert(function.module_ll.as_str()) {
                        linked_modules.push(&function.module_ll);
                    }
                }
                name
            };
            (llvm_global(&name)?, true)
        };
        let leading = line.len() - trimmed.len();
        output.push_str(&line[..leading + callee_start]);
        output.push_str(&global);
        output.push_str(&trimmed[callee_end..=callee_end]);
        if prepend_index {
            output.push_str(&pointer.index);
            if trimmed.as_bytes().get(callee_end + 1) != Some(&b')') {
                output.push_str(", ");
            }
        }
        output.push_str(&trimmed[callee_end + 1..]);
        output.push('\n');
    }
    for dispatcher in dispatchers {
        output.push('\n');
        output.push_str(&dispatcher_definition(
            &dispatcher.name,
            &dispatcher.entries,
            &dispatcher.return_type,
            &dispatcher.argument_types,
        )?);
    }
    for module in linked_modules {
        output.push('\n');
        let resolved = specialize_visible_function_references(module, linkage)?;
        append_dependency_module(&mut output, &resolved);
    }
    Ok(output)
}

fn null_pointer_comparison(instruction: &str) -> Option<(&str, bool)> {
    let (equal_to_null, operands) = if let Some(operands) = instruction.strip_prefix("icmp eq ") {
        (true, operands)
    } else {
        (false, instruction.strip_prefix("icmp ne ")?)
    };
    let operands = split_top_level(operands, ',');
    if operands.len() != 2 {
        return None;
    }
    let left = value_operand(operands[0]);
    let right = value_operand(operands[1]);
    match (left, right) {
        (pointer, "null") if pointer.starts_with('%') => Some((pointer, equal_to_null)),
        ("null", pointer) if pointer.starts_with('%') => Some((pointer, equal_to_null)),
        _ => None,
    }
}

fn opaque_sentinel_comparison(instruction: &str) -> Option<(&str, bool)> {
    let (equal_to_sentinel, operands) = if let Some(operands) = instruction.strip_prefix("icmp eq ")
    {
        (true, operands)
    } else {
        (false, instruction.strip_prefix("icmp ne ")?)
    };
    let operands = split_top_level(operands, ',');
    if operands.len() != 2 {
        return None;
    }
    let left = value_operand(operands[0]);
    let right = value_operand(operands[1]);
    match (left, right) {
        (integer, "1") if integer.starts_with('%') => Some((integer, equal_to_sentinel)),
        ("1", integer) if integer.starts_with('%') => Some((integer, equal_to_sentinel)),
        _ => None,
    }
}

fn authored_null_comparison(
    result: &str,
    table: &LinkedFunctionTable,
    index: &str,
    equal_to_null: bool,
    counter: &mut usize,
    module: &str,
) -> Result<String, String> {
    if let Some(index) = integer_operand(index) {
        let populated = table.entries.iter().any(|entry| entry.index == index);
        return Ok(format!(
            "{result} = or i1 false, {}",
            populated != equal_to_null
        ));
    }
    if !index.trim_start().starts_with("i32 ") {
        return Err(format!(
            "linked visible function table parameter {} has a non-i32 slot operand {index:?}",
            table.parameter_index
        ));
    }
    let slot = value_operand(index);
    let mut lines = Vec::new();
    let mut present = None::<String>;
    for entry in &table.entries {
        let comparison = fresh_presence_value(module, counter);
        lines.push(format!(
            "{comparison} = icmp eq i32 {slot}, {}",
            entry.index
        ));
        present = Some(if let Some(previous) = present {
            let combined = fresh_presence_value(module, counter);
            lines.push(format!("{combined} = or i1 {previous}, {comparison}"));
            combined
        } else {
            comparison
        });
    }
    let present = present.unwrap_or_else(|| "false".into());
    if equal_to_null {
        lines.push(format!("{result} = xor i1 {present}, true"));
    } else {
        lines.push(format!("{result} = or i1 {present}, false"));
    }
    Ok(lines.join("\n"))
}

fn fresh_presence_value(module: &str, counter: &mut usize) -> String {
    loop {
        let value = format!("%metal2vulkan.table.present.{}", *counter);
        *counter += 1;
        if !contains_llvm_value(module, &value) {
            return value;
        }
    }
}

fn contains_llvm_value(text: &str, value: &str) -> bool {
    text.match_indices(value).any(|(start, _)| {
        let end = start + value.len();
        let is_boundary = |byte: Option<&u8>| {
            byte.is_none_or(|byte| {
                !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'.' | b'$' | b'-')
            })
        };
        is_boundary(
            start
                .checked_sub(1)
                .and_then(|index| text.as_bytes().get(index)),
        ) && is_boundary(text.as_bytes().get(end))
    })
}

thread_local! {
    static DEDUPE_DEPENDENCIES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
pub(crate) struct DedupeDependencies;
impl DedupeDependencies {
    pub(crate) fn on() -> Self {
        DEDUPE_DEPENDENCIES.with(|d| d.set(true));
        DedupeDependencies
    }
}
impl Drop for DedupeDependencies {
    fn drop(&mut self) {
        DEDUPE_DEPENDENCIES.with(|d| d.set(false));
    }
}

fn append_dependency_module(output: &mut String, module: &str) {
    if DEDUPE_DEPENDENCIES.with(|d| d.get()) {
        return append_dependency_module_dedupe(output, module);
    }
    for line in module.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("; ModuleID =")
            || trimmed.starts_with("source_filename =")
            || trimmed.starts_with("target datalayout =")
            || trimmed.starts_with("target triple =")
            || trimmed.starts_with("attributes #")
            || trimmed.starts_with('!')
        {
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
}

fn append_dependency_module_dedupe(output: &mut String, module: &str) {
    let already: HashSet<String> = output
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if t.starts_with("define ") || t.starts_with("declare ") {
                t.find('@')
                    .map(|at| t[at..].split('(').next().unwrap_or("").to_string())
            } else if t.starts_with('@') {
                t.split(" = ").next().map(|g| g.trim().to_string())
            } else {
                None
            }
        })
        .collect();
    let mut skipping = false;
    for line in module.lines() {
        let trimmed = line.trim_start();
        if skipping {
            if trimmed == "}" {
                skipping = false;
            }
            continue;
        }
        if trimmed.starts_with("define ") || trimmed.starts_with("declare ") {
            let global = trimmed
                .find('@')
                .map(|at| trimmed[at..].split('(').next().unwrap_or("").to_string())
                .unwrap_or_default();
            if already.contains(&global) {
                skipping = trimmed.starts_with("define ") && !trimmed.ends_with('}');
                continue;
            }
        } else if trimmed.starts_with('@') {
            if let Some(g) = trimmed.split(" = ").next() {
                if already.contains(g.trim()) {
                    continue;
                }
            }
        }
        if trimmed.starts_with("; ModuleID =")
            || trimmed.starts_with("source_filename =")
            || trimmed.starts_with("target datalayout =")
            || trimmed.starts_with("target triple =")
            || trimmed.starts_with("attributes #")
            || trimmed.starts_with('!')
        {
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
}

pub fn link_extern_definitions(entry_ll: &str, modules: &[String]) -> Result<String, String> {
    fn sym(line: &str) -> Option<String> {
        let t = line.trim_start();
        let at = t.find('@')?;
        let rest = &t[at + 1..];
        let end = rest.find('(')?;
        Some(rest[..end].trim_matches('"').to_string())
    }
    fn user(s: &str) -> bool {
        !s.starts_with("air.") && !s.starts_with("llvm.")
    }
    fn defined(ll: &str) -> HashSet<String> {
        ll.lines()
            .filter(|l| l.trim_start().starts_with("define "))
            .filter_map(sym)
            .collect()
    }
    fn local_defined(ll: &str) -> HashSet<String> {
        ll.lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("define ") && {
                    let head = &t[..t.find('@').unwrap_or(0)];
                    head.contains(" internal ") || head.contains(" private ")
                }
            })
            .filter_map(sym)
            .collect()
    }
    fn declared(ll: &str) -> HashSet<String> {
        ll.lines()
            .filter(|l| l.trim_start().starts_with("declare "))
            .filter_map(sym)
            .filter(|s| user(s))
            .collect()
    }
    let mut out = strip_dyld_tables(entry_ll)?;
    let mut appended = HashSet::<usize>::default();
    loop {
        let have = defined(&out);
        let mut pending: Vec<String> = declared(&out)
            .into_iter()
            .filter(|s| !have.contains(s))
            .collect();
        pending.sort();
        if pending.is_empty() {
            return Ok(out);
        }
        let mut progressed = false;
        for s in &pending {
            let Some(k) = modules.iter().position(|m| defined(m).contains(s)) else {
                continue;
            };
            if !appended.insert(k) {
                continue;
            }
            let module = &modules[k];
            let clash: Vec<String> = local_defined(module)
                .intersection(&defined(&out))
                .cloned()
                .collect();
            if !clash.is_empty() {
                return Err(format!(
                    "dynamic library module {k} and the caller both define local {}",
                    clash.join(", ")
                ));
            }
            let mdef = defined(module);
            out = out
                .lines()
                .filter(|l| {
                    !(l.trim_start().starts_with("declare ")
                        && sym(l).is_some_and(|x| mdef.contains(&x)))
                })
                .map(|l| format!("{l}\n"))
                .collect();
            append_dependency_module_dedupe(&mut out, module);
            progressed = true;
        }
        if !progressed {
            return Err(format!("no dynamic library defines {}", pending.join(", ")));
        }
    }
}

pub fn strip_dyld_tables(ll: &str) -> Result<String, String> {
    fn gname(t: &str) -> Option<&str> {
        let r = t.strip_prefix('@')?;
        Some(&r[..r.find([' ', '=']).unwrap_or(r.len())])
    }
    let dropped: Vec<String> = ll
        .lines()
        .filter_map(|l| gname(l.trim_start()))
        .filter(|n| n.starts_with("air.dyld_"))
        .map(|n| format!("@{n}"))
        .collect();
    if dropped.is_empty() {
        return Ok(ll.to_string());
    }
    let names = |e: &str| {
        dropped.iter().any(|d| {
            e.contains(d.as_str())
                && !e[e.find(d.as_str()).unwrap() + d.len()..]
                    .starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.')
        })
    };
    let mut out = String::with_capacity(ll.len());
    for line in ll.lines() {
        let t = line.trim_start();
        if gname(t).is_some_and(|n| n.starts_with("air.dyld_")) {
            continue;
        }
        if gname(t).is_some_and(|n| n == "llvm.used" || n == "llvm.compiler.used") {
            let open = t
                .find("] [")
                .ok_or_else(|| format!("@llvm.used has an unexpected shape: {t}"))?
                + 2;
            let mut depth = 0i32;
            let mut close = None;
            for (i, c) in t[open..].char_indices() {
                match c {
                    '[' | '(' => depth += 1,
                    ']' | ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(open + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let close =
                close.ok_or_else(|| format!("@llvm.used initializer is not closed: {t}"))?;
            let (mut parts, mut cur, mut d) = (Vec::new(), String::new(), 0i32);
            for c in t[open + 1..close].chars() {
                match c {
                    '[' | '(' => d += 1,
                    ']' | ')' => d -= 1,
                    _ => {}
                }
                if c == ',' && d == 0 {
                    parts.push(cur.trim().to_string());
                    cur.clear();
                } else {
                    cur.push(c);
                }
            }
            if !cur.trim().is_empty() {
                parts.push(cur.trim().to_string());
            }
            let keep: Vec<String> = parts.into_iter().filter(|e| !names(e)).collect();
            if keep.is_empty() {
                continue;
            }
            let head = &t[..t.find(" [").unwrap()];
            let elem = t[head.len() + 2..]
                .split(" x ")
                .nth(1)
                .and_then(|r| r.split(']').next())
                .unwrap_or("ptr");
            out.push_str(&format!(
                "{head} [{} x {elem}] [{}]{}\n",
                keep.len(),
                keep.join(", "),
                &t[close + 1..]
            ));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if let Some(l) = out.lines().find(|l| names(l)) {
        return Err(format!(
            "a dropped dynamic-loader table is still referenced: {l}"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod extern_link_tests {
    use super::link_extern_definitions;
    const K: &str = "define void @k(float addrspace(1)* %0) #0 {\n  %2 = call float @_Z1ff(float 1.0)\n  ret void\n}\ndeclare float @_Z1ff(float) #1\ndeclare float @air.convert.f.f32.u.i32(i32) #2\n";
    const F: &str =
        "define float @_Z1ff(float %0) #0 {\n  %2 = fmul fast float %0, 3.0\n  ret float %2\n}\n";
    const G: &str = "define float @_Z1ff(float %0) #0 {\n  %2 = call float @_Z1gf(float %0)\n  ret float %2\n}\ndeclare float @_Z1gf(float)\n";
    const H: &str = "define float @_Z1gf(float %0) #0 {\n  ret float %0\n}\n";

    #[test]
    fn a_declared_extern_becomes_the_dylibs_body_and_its_declare_is_gone() {
        let out = link_extern_definitions(K, &[F.to_string()]).expect("links");
        assert!(out.contains("define float @_Z1ff(float %0)"), "{out}");
        assert!(!out.contains("declare float @_Z1ff"), "{out}");
        assert!(
            out.contains("declare float @air.convert.f.f32.u.i32"),
            "an intrinsic is not an extern: {out}"
        );
    }

    #[test]
    fn an_extern_no_dylib_defines_is_refused_by_name() {
        let e = link_extern_definitions(K, &[H.to_string()]).expect_err("must refuse");
        assert!(e.contains("_Z1ff"), "{e}");
    }

    #[test]
    fn a_dylib_that_calls_another_dylib_links_both() {
        let out = link_extern_definitions(K, &[G.to_string(), H.to_string()]).expect("links");
        assert!(
            out.contains("define float @_Z1ff") && out.contains("define float @_Z1gf"),
            "{out}"
        );
        assert!(!out.contains("declare float @_Z1gf"), "{out}");
    }

    #[test]
    fn the_loaders_import_tables_are_gone_after_the_link_and_other_used_globals_stay() {
        let k = format!("@air.dyld_lib_table = internal constant [1 x ptr] [ptr @air.dyld_str_table], section \"air.dyld\"\n@air.dyld_str_table = internal constant [8 x i8] c\"dl.mlib\\00\"\n@air.dyld_flat_table = internal constant [1 x ptr] [ptr @_Z1ff]\n@keep = global i32 0\n@llvm.used = appending global [4 x ptr] [ptr @air.dyld_lib_table, ptr @air.dyld_str_table, ptr @air.dyld_flat_table, ptr @keep], section \"llvm.metadata\"\n{K}");
        let out = link_extern_definitions(&k, &[F.to_string()]).expect("links");
        assert!(!out.contains("air.dyld_"), "{out}");
        assert!(
            out.contains(
                "@llvm.used = appending global [1 x ptr] [ptr @keep], section \"llvm.metadata\""
            ),
            "{out}"
        );
        let only = k
            .replace(", ptr @keep]", "]")
            .replace("[4 x ptr]", "[3 x ptr]");
        let out = link_extern_definitions(&only, &[F.to_string()]).expect("links");
        assert!(
            !out.contains("@llvm.used"),
            "an emptied @llvm.used is dropped: {out}"
        );
    }

    #[test]
    fn a_reader_of_a_dropped_table_is_refused() {
        let k = format!("@air.dyld_flat_table = internal constant [1 x ptr] [ptr @_Z1ff]\n@x = global ptr @air.dyld_flat_table\n{K}");
        let e = link_extern_definitions(&k, &[F.to_string()]).expect_err("must refuse");
        assert!(e.contains("still referenced"), "{e}");
    }

    #[test]
    fn two_different_local_helpers_with_one_name_are_refused() {
        let k = format!("{K}define internal float @_ZL1hf(float %0) #0 {{\n  ret float 1.0\n}}\n");
        let f = "define float @_Z1ff(float %0) #0 {\n  %2 = call float @_ZL1hf(float %0)\n  ret float %2\n}\ndefine internal float @_ZL1hf(float %0) #0 {\n  ret float 2.0\n}\n";
        let e = link_extern_definitions(&k, &[f.to_string()]).expect_err("must refuse");
        assert!(e.contains("_ZL1hf"), "{e}");
    }
}

fn validate_linkage(linkage: &LinkedFunctionLinkage) -> Result<(), String> {
    let mut parameters = HashSet::default();
    let mut symbols = HashMap::<&str, &str>::default();
    for reference in &linkage.visible_references {
        if !module_defines(&reference.module_ll, &reference.symbol) {
            return Err(format!(
                "linked module does not define authored visible function reference {:?}",
                reference.symbol
            ));
        }
        if symbols
            .insert(&reference.symbol, &reference.module_ll)
            .is_some()
        {
            return Err(format!(
                "duplicate authored visible function reference {:?}",
                reference.symbol
            ));
        }
    }
    for table in &linkage.visible_tables {
        if !parameters.insert(table.parameter_index) {
            return Err(format!(
                "duplicate linked visible function-table parameter {}",
                table.parameter_index
            ));
        }
        if table.size == 0 {
            return Err(format!(
                "linked visible function-table parameter {} has zero size",
                table.parameter_index
            ));
        }
        let mut previous = None;
        for entry in &table.entries {
            if entry.index >= table.size {
                return Err(format!(
                    "linked visible function-table parameter {} entry {} exceeds size {}",
                    table.parameter_index, entry.index, table.size
                ));
            }
            if previous.is_some_and(|index| index >= entry.index) {
                return Err(format!(
                    "linked visible function-table parameter {} entries must be sorted and unique",
                    table.parameter_index
                ));
            }
            previous = Some(entry.index);
            if !module_defines(&entry.module_ll, &entry.symbol) {
                return Err(format!(
                    "linked module does not define authored function {:?}",
                    entry.symbol
                ));
            }
            if let Some(previous_module) = symbols.insert(&entry.symbol, &entry.module_ll) {
                if previous_module != entry.module_ll {
                    return Err(format!(
                        "linked function symbol {:?} is defined by multiple modules",
                        entry.symbol
                    ));
                }
            }
        }
    }
    Ok(())
}

fn function_signatures(ll: &str) -> Result<HashMap<String, FunctionSignature>, String> {
    let mut functions = HashMap::default();
    let mut signature = String::new();
    let mut collecting = false;
    for line in ll.lines() {
        let trimmed = line.trim_start();
        if !collecting && trimmed.starts_with("define ") {
            collecting = true;
            signature.clear();
        }
        if collecting {
            signature.push_str(trimmed);
            signature.push(' ');
            if trimmed.contains('{') {
                let global = definition_global(&signature).ok_or_else(|| {
                    format!("linked function definition has no global: {signature}")
                })?;
                let open = signature
                    .find(&format!("{global}("))
                    .map(|index| index + global.len())
                    .ok_or_else(|| format!("linked function {global} has no parameter list"))?;
                let close = matching_paren(&signature, open).ok_or_else(|| {
                    format!("linked function {global} has an unterminated parameter list")
                })?;
                let parameters = parameter_values(&signature[open + 1..close], &global)?;
                if functions
                    .insert(global.clone(), FunctionSignature { parameters })
                    .is_some()
                {
                    return Err("linked module contains duplicate function definitions".into());
                }
                collecting = false;
            }
        }
    }
    if collecting {
        return Err("linked module has an unterminated function definition header".into());
    }
    Ok(functions)
}

fn parameter_values(body: &str, global: &str) -> Result<Vec<String>, String> {
    let body = body.trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    split_top_level(body, ',')
        .into_iter()
        .map(|parameter| {
            parameter
                .split_whitespace()
                .last()
                .filter(|value| value.starts_with('%'))
                .map(str::to_string)
                .ok_or_else(|| format!("function {global} parameter has no SSA name: {parameter}"))
        })
        .collect()
}

fn definition_global(line: &str) -> Option<String> {
    let line = line.trim_start();
    line.starts_with("define ")
        .then(|| line.find('@'))
        .flatten()
        .and_then(|at| global_token(line, at).map(|(global, _)| global.to_string()))
}

fn global_token(text: &str, at: usize) -> Option<(&str, usize)> {
    if text.as_bytes().get(at) != Some(&b'@') {
        return None;
    }
    if text.as_bytes().get(at + 1) == Some(&b'"') {
        let mut escaped = false;
        for (relative, byte) in text.as_bytes()[at + 2..].iter().enumerate() {
            if *byte == b'"' && !escaped {
                let end = at + 2 + relative + 1;
                let open = text[end..].find('(')? + end;
                return Some((&text[at..end], open));
            }
            escaped = *byte == b'\\' && !escaped;
            if *byte != b'\\' {
                escaped = false;
            }
        }
        None
    } else {
        let open = text[at..].find('(')? + at;
        Some((text[at..open].trim_end(), open))
    }
}

fn named_call(line: &str) -> Option<(&str, usize, usize, Vec<&str>)> {
    let call = line.find("call ")?;
    let at = line[call + 5..].find('@')? + call + 5;
    let (global, open) = global_token(line, at)?;
    let close = matching_paren(line, open)?;
    let arguments = split_top_level(&line[open + 1..close], ',');
    Some((global, open, close, arguments))
}

fn seed_function_values<'a>(
    global: &str,
    signatures: &HashMap<String, FunctionSignature>,
    flow: &LinkedFlow<'a>,
    tables: &mut HashMap<String, &'a LinkedFunctionTable>,
    pointers: &mut HashMap<String, PointerTrace<'a>>,
) -> Result<(), String> {
    let signature = signatures
        .get(global)
        .ok_or_else(|| format!("missing parsed signature for {global}"))?;
    for (ordinal, parameter) in signature.parameters.iter().enumerate() {
        if let Some(table) = flow.table_parameters.get(&(global.to_string(), ordinal)) {
            tables.insert(parameter.clone(), *table);
        }
        if let Some(table) = flow.pointer_parameters.get(&(global.to_string(), ordinal)) {
            pointers.insert(
                parameter.clone(),
                PointerTrace {
                    table,
                    index: format!("i32 {}", pointer_slot_parameter(global, ordinal, signature)),
                },
            );
        }
    }
    Ok(())
}

fn propagate_linked_flow<'a>(
    ll: &str,
    signatures: &HashMap<String, FunctionSignature>,
    flow: &mut LinkedFlow<'a>,
) -> Result<(), String> {
    loop {
        let mut changed = false;
        let mut current = None::<String>;
        let mut tables = HashMap::<String, &'a LinkedFunctionTable>::default();
        let mut pointers = HashMap::<String, &'a LinkedFunctionTable>::default();
        for line in ll.lines() {
            let trimmed = line.trim_start();
            if let Some(global) = definition_global(trimmed) {
                current = Some(global.clone());
                tables.clear();
                pointers.clear();
                let signature = signatures
                    .get(&global)
                    .ok_or_else(|| format!("missing parsed signature for {global}"))?;
                for (ordinal, parameter) in signature.parameters.iter().enumerate() {
                    if let Some(table) = flow.table_parameters.get(&(global.clone(), ordinal)) {
                        tables.insert(parameter.clone(), *table);
                    }
                    if let Some(table) = flow.pointer_parameters.get(&(global.clone(), ordinal)) {
                        pointers.insert(parameter.clone(), *table);
                    }
                }
                continue;
            }
            if trimmed == "}" {
                current = None;
                continue;
            }
            let Some(current_global) = current.as_deref() else {
                continue;
            };
            if let Some((result, instruction)) = trimmed.split_once(" = ") {
                let result = result.trim();
                if instruction.contains("@air.get_function_pointer_visible_function_table(") {
                    let arguments = call_arguments(instruction)?;
                    if let Some(table) = arguments
                        .first()
                        .and_then(|argument| tables.get(value_operand(argument)))
                    {
                        pointers.insert(result.to_string(), *table);
                    }
                } else if instruction.starts_with("bitcast ")
                    || instruction.starts_with("addrspacecast ")
                {
                    if let Some(table) = audit_cast_pointer(instruction, &pointers) {
                        pointers.insert(result.to_string(), table);
                    }
                } else if instruction.starts_with("phi ") || instruction.starts_with("select ") {
                    let used = pointers
                        .iter()
                        .filter(|(value, _)| contains_llvm_value(instruction, value))
                        .map(|(_, table)| *table)
                        .collect::<Vec<_>>();
                    if let Some(table) = same_table(&used) {
                        pointers.insert(result.to_string(), table);
                    }
                }
            }
            let Some((callee, _, _, arguments)) = named_call(trimmed) else {
                continue;
            };
            let Some(callee_signature) = signatures.get(callee) else {
                continue;
            };
            for (ordinal, argument) in arguments
                .iter()
                .take(callee_signature.parameters.len())
                .enumerate()
            {
                let value = value_operand(argument);
                if let Some(table) = tables.get(value) {
                    changed |= insert_flow_parameter(
                        &mut flow.table_parameters,
                        (callee.to_string(), ordinal),
                        table,
                        "function table",
                    )?;
                }
                if let Some(table) = pointers.get(value) {
                    changed |= insert_flow_parameter(
                        &mut flow.pointer_parameters,
                        (callee.to_string(), ordinal),
                        table,
                        "function pointer",
                    )?;
                }
            }
            let _ = current_global;
        }
        if !changed {
            return Ok(());
        }
    }
}

fn audit_cast_pointer<'a>(
    instruction: &str,
    pointers: &HashMap<String, &'a LinkedFunctionTable>,
) -> Option<&'a LinkedFunctionTable> {
    cast_source_value(instruction).and_then(|source| pointers.get(source).copied())
}

fn same_table<'a>(tables: &[&'a LinkedFunctionTable]) -> Option<&'a LinkedFunctionTable> {
    let first = *tables.first()?;
    tables
        .iter()
        .all(|table| std::ptr::eq(*table, first))
        .then_some(first)
}

fn insert_flow_parameter<'a>(
    parameters: &mut HashMap<(String, usize), &'a LinkedFunctionTable>,
    key: (String, usize),
    table: &'a LinkedFunctionTable,
    kind: &str,
) -> Result<bool, String> {
    if let Some(previous) = parameters.get(&key) {
        if !std::ptr::eq(*previous, table) {
            return Err(format!(
                "linked {kind} parameter {} of {} receives multiple authored tables",
                key.1, key.0
            ));
        }
        Ok(false)
    } else {
        parameters.insert(key, table);
        Ok(true)
    }
}

fn pointer_slot_parameter(_global: &str, ordinal: usize, signature: &FunctionSignature) -> String {
    let base = format!("%metal2vulkan.table.param{ordinal}.slot");
    if !signature.parameters.contains(&base) {
        return base;
    }
    for suffix in 1usize.. {
        let candidate = format!("{base}.{suffix}");
        if !signature.parameters.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn pointer_parameter_ordinals(flow: &LinkedFlow<'_>, global: &str) -> Vec<usize> {
    let mut ordinals = flow
        .pointer_parameters
        .keys()
        .filter_map(|(function, ordinal)| (function == global).then_some(*ordinal))
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    ordinals
}

fn append_pointer_slot_parameters(
    line: &str,
    global: &str,
    signatures: &HashMap<String, FunctionSignature>,
    flow: &LinkedFlow<'_>,
) -> Result<String, String> {
    let ordinals = pointer_parameter_ordinals(flow, global);
    if ordinals.is_empty() {
        return Ok(line.to_string());
    }
    let signature = signatures
        .get(global)
        .ok_or_else(|| format!("missing parsed signature for {global}"))?;
    let at = line
        .find(global)
        .ok_or_else(|| format!("definition line does not contain {global}"))?;
    let open = line[at + global.len()..]
        .find('(')
        .map(|offset| offset + at + global.len())
        .ok_or_else(|| format!("definition {global} has no parameter list"))?;
    let close = matching_paren(line, open)
        .ok_or_else(|| format!("multiline definition parameters for {global} are unsupported"))?;
    let additions = ordinals
        .into_iter()
        .map(|ordinal| format!("i32 {}", pointer_slot_parameter(global, ordinal, signature)))
        .collect::<Vec<_>>()
        .join(", ");
    let separator = if line[open + 1..close].trim().is_empty() {
        ""
    } else {
        ", "
    };
    Ok(format!(
        "{}{}{}{}",
        &line[..close],
        separator,
        additions,
        &line[close..]
    ))
}

fn append_pointer_slot_call_arguments(
    line: &str,
    caller: &str,
    signatures: &HashMap<String, FunctionSignature>,
    flow: &LinkedFlow<'_>,
    pointers: &HashMap<String, PointerTrace<'_>>,
) -> Result<Option<String>, String> {
    let Some((callee, open, close, arguments)) = named_call(line) else {
        return Ok(None);
    };
    let ordinals = pointer_parameter_ordinals(flow, callee);
    if ordinals.is_empty() || !signatures.contains_key(callee) {
        return Ok(None);
    }
    let mut additions = Vec::new();
    for ordinal in ordinals {
        let argument = arguments.get(ordinal).ok_or_else(|| {
            format!("call from {caller} to {callee} omits linked pointer parameter {ordinal}")
        })?;
        let value = value_operand(argument);
        let pointer = pointers.get(value).ok_or_else(|| {
            format!(
                "call from {caller} to {callee} passes untraced linked pointer parameter {ordinal}: {value}"
            )
        })?;
        additions.push(pointer.index.clone());
    }
    let separator = if line[open + 1..close].trim().is_empty() {
        ""
    } else {
        ", "
    };
    Ok(Some(format!(
        "{}{}{}{}",
        &line[..close],
        separator,
        additions.join(", "),
        &line[close..]
    )))
}

fn call_arguments(instruction: &str) -> Result<Vec<&str>, String> {
    let open = instruction
        .find('(')
        .ok_or_else(|| format!("call has no argument list: {instruction}"))?;
    let close = matching_paren(instruction, open)
        .ok_or_else(|| format!("call has an unterminated argument list: {instruction}"))?;
    Ok(split_top_level(&instruction[open + 1..close], ','))
}

fn value_operand(argument: &str) -> &str {
    argument.split_whitespace().last().unwrap_or_default()
}

fn integer_operand(argument: &str) -> Option<u32> {
    value_operand(argument).parse().ok()
}

fn cast_source_value(instruction: &str) -> Option<&str> {
    let (_, source_and_destination) = instruction.split_once(' ')?;
    let (source, _) = source_and_destination.rsplit_once(" to ")?;
    source
        .split_whitespace()
        .last()
        .filter(|value| value.starts_with('%'))
}

fn indirect_call_callee(line: &str) -> Option<(usize, usize, &str)> {
    let call = line.find("call ")?;
    let after_call = &line[call + 5..];
    let open = after_call.find('(')? + call + 5;
    let head = &line[..open];
    let end = head.len();
    let start = head.rfind(char::is_whitespace).map_or(0, |index| index + 1);
    let callee = &line[start..end];
    callee.starts_with('%').then_some((start, end, callee))
}

struct IndirectCallShape {
    return_type: String,
    argument_types: Vec<String>,
}

fn indirect_call_shape(
    line: &str,
    callee_start: usize,
    callee_end: usize,
) -> Result<IndirectCallShape, String> {
    let call = line[..callee_start]
        .rfind("call ")
        .ok_or_else(|| format!("indirect call has no call opcode: {line}"))?;
    let mut return_type = line[call + 5..callee_start].trim();
    while let Some((first, rest)) = return_type.split_once(' ') {
        if matches!(
            first,
            "fast" | "nnan" | "ninf" | "nsz" | "arcp" | "contract" | "afn" | "reassoc"
        ) {
            return_type = rest.trim_start();
        } else {
            break;
        }
    }
    if return_type.is_empty() {
        return Err(format!("indirect call has no return type: {line}"));
    }
    let close = matching_paren(line, callee_end)
        .ok_or_else(|| format!("indirect call has an unterminated argument list: {line}"))?;
    let arguments = &line[callee_end + 1..close];
    let argument_types = if arguments.trim().is_empty() {
        Vec::new()
    } else {
        split_top_level(arguments, ',')
            .into_iter()
            .map(argument_type)
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(IndirectCallShape {
        return_type: return_type.to_string(),
        argument_types,
    })
}

fn argument_type(argument: &str) -> Result<String, String> {
    let value = value_operand(argument);
    let type_end = argument
        .rfind(value)
        .filter(|index| *index > 0)
        .ok_or_else(|| format!("linked indirect-call argument has no typed value: {argument}"))?;
    let ty = argument[..type_end].trim_end();
    if ty.is_empty() {
        return Err(format!(
            "linked indirect-call argument has no type: {argument}"
        ));
    }
    Ok(ty.to_string())
}

fn linked_function_matches_call(
    function: &LinkedFunction,
    call: &IndirectCallShape,
) -> Result<bool, String> {
    let (return_type, argument_types) = linked_function_type(function)?;
    let call_return = crate::native::parse_llvm_type_prefix(&call.return_type)?;
    let call_arguments = call
        .argument_types
        .iter()
        .map(|argument| crate::native::parse_llvm_type_prefix(argument))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(return_type == call_return && argument_types == call_arguments)
}

fn linked_function_type(
    function: &LinkedFunction,
) -> Result<(crate::native::ir::LlType, Vec<crate::native::ir::LlType>), String> {
    let global = llvm_global(&function.symbol)?;
    let mut signature = String::new();
    let mut collecting = false;
    for line in function.module_ll.lines() {
        let trimmed = line.trim_start();
        if !collecting && trimmed.starts_with("define ") && trimmed.contains(&format!("{global}("))
        {
            collecting = true;
        }
        if collecting {
            signature.push_str(trimmed);
            signature.push(' ');
            if trimmed.contains('{') {
                break;
            }
        }
    }
    if signature.is_empty() {
        return Err(format!(
            "linked module does not define authored function {:?}",
            function.symbol
        ));
    }
    let at = signature
        .find(&global)
        .ok_or_else(|| format!("linked function {:?} has no global", function.symbol))?;
    let open = signature[at + global.len()..]
        .find('(')
        .map(|offset| offset + at + global.len())
        .ok_or_else(|| format!("linked function {:?} has no parameters", function.symbol))?;
    let close = matching_paren(&signature, open).ok_or_else(|| {
        format!(
            "linked function {:?} has unterminated parameters",
            function.symbol
        )
    })?;
    let return_type = crate::native::parse_llvm_return_type(&signature[..at])?;
    let argument_types = if signature[open + 1..close].trim().is_empty() {
        Vec::new()
    } else {
        split_top_level(&signature[open + 1..close], ',')
            .into_iter()
            .map(crate::native::parse_llvm_type_prefix)
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok((return_type, argument_types))
}

fn dispatcher_definition(
    name: &str,
    entries: &[&LinkedFunction],
    return_type: &str,
    argument_types: &[String],
) -> Result<String, String> {
    let global = llvm_global(name)?;
    let parameters = argument_types
        .iter()
        .enumerate()
        .map(|(index, ty)| format!("{ty} %arg{index}"))
        .collect::<Vec<_>>();
    let forwarded = argument_types
        .iter()
        .enumerate()
        .map(|(index, ty)| format!("{ty} %arg{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let parameter_separator = if parameters.is_empty() { "" } else { ", " };
    let mut output = format!(
        "define internal {return_type} {global}(i32 %metal2vulkan_slot{}{}) {{\nentry:\n  switch i32 %metal2vulkan_slot, label %invalid [",
        parameter_separator,
        parameters.join(", ")
    );
    for entry in entries {
        output.push_str(&format!(
            " i32 {}, label %case_{}",
            entry.index, entry.index
        ));
    }
    output.push_str(" ]\n\n");
    let is_void = return_type == "void";
    for entry in entries {
        output.push_str(&format!("case_{}:\n", entry.index));
        let target = llvm_global(&entry.symbol)?;
        if is_void {
            output.push_str(&format!(
                "  call void {target}({forwarded})\n  ret void\n\n"
            ));
        } else {
            output.push_str(&format!(
                "  %result_{} = call {return_type} {target}({forwarded})\n  br label %exit\n\n",
                entry.index
            ));
        }
    }
    output.push_str("invalid:\n  unreachable\n");
    if !is_void {
        output.push_str("\nexit:\n  %result = phi ");
        output.push_str(return_type);
        output.push(' ');
        for (ordinal, entry) in entries.iter().enumerate() {
            if ordinal != 0 {
                output.push_str(", ");
            }
            output.push_str(&format!(
                "[ %result_{}, %case_{} ]",
                entry.index, entry.index
            ));
        }
        output.push_str(&format!("\n  ret {return_type} %result\n"));
    }
    output.push_str("}\n");
    Ok(output)
}

pub(crate) const UNRESOLVED_VISIBLE_SUFFIX: &str = ".MTL_UNRESOLVED_VISIBLE_FN";

fn llvm_global(symbol: &str) -> Result<String, String> {
    if symbol.is_empty() || symbol.contains(['\n', '\r', '\0']) {
        return Err(format!("invalid linked LLVM function symbol {symbol:?}"));
    }
    if symbol
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'$' | b'-'))
    {
        Ok(format!("@{symbol}"))
    } else {
        Ok(format!(
            "@\"{}\"",
            symbol.replace('\\', "\\5C").replace('"', "\\22")
        ))
    }
}

fn module_defines(module: &str, symbol: &str) -> bool {
    llvm_global(symbol).is_ok_and(|global| {
        module.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("define ") && line.contains(&format!("{global}("))
        })
    })
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (relative, byte) in text.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + relative);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str, delimiter: char) -> Vec<&str> {
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut fields = Vec::new();
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' if depth > 0 => depth -= 1,
            _ if ch == delimiter && depth == 0 => {
                fields.push(text[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    fields.push(text[start..].trim());
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_visible_references_close_the_authored_dependency_graph() {
        let entry = r#"define void @main() {
entry:
  %value = call i32 @leaf.MTL_VISIBLE_FN_REF(i32 7)
  ret void
}
declare i32 @leaf.MTL_VISIBLE_FN_REF(i32) section "air.externally_defined"
!air.visible_function_references = !{!0}
!0 = !{!"air.visible_function_reference", ptr @leaf.MTL_VISIBLE_FN_REF, !"leaf"}
"#;
        let leaf = r#"define i32 @leaf(i32 %value) {
entry:
  %result = call i32 @base.MTL_VISIBLE_FN_REF(i32 %value)
  ret i32 %result
}
declare i32 @base.MTL_VISIBLE_FN_REF(i32) section "air.externally_defined"
!air.visible_function_references = !{!7}
!7 = !{!"air.visible_function_reference", ptr @base.MTL_VISIBLE_FN_REF, !"base"}
"#;
        let base = "define i32 @base(i32 %value) {\nentry:\n  ret i32 %value\n}\n";
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![
                LinkedFunctionReference {
                    symbol: "base".into(),
                    module_ll: base.into(),
                },
                LinkedFunctionReference {
                    symbol: "leaf".into(),
                    module_ll: leaf.into(),
                },
            ],
            visible_tables: vec![],
            intersection_tables: vec![],
        };

        let specialized = specialize_visible_function_references(entry, &linkage).unwrap();
        assert!(specialized.contains("call i32 @leaf(i32 7)"));
        assert!(specialized.contains("call i32 @base(i32 %value)"));
        assert!(specialized.contains("define i32 @leaf("));
        assert!(specialized.contains("define i32 @base("));
        assert!(!specialized.contains("MTL_VISIBLE_FN_REF"));
        assert!(!specialized.contains("!air.visible_function_references"));
    }

    #[test]
    fn direct_visible_reference_specializes_internal_function_pointer_consumer() {
        let entry = r#"define i32 @main(i32 %value) {
entry:
  %result = call i32 @apply(ptr @linked.MTL_VISIBLE_FN_REF, i32 %value)
  ret i32 %result
}
define internal i32 @apply(ptr %function, i32 %value) {
entry:
  %result = call i32 %function(i32 %value)
  ret i32 %result
}
declare i32 @linked.MTL_VISIBLE_FN_REF(i32) section "air.externally_defined"
!air.visible_function_references = !{!0}
!0 = !{!"air.visible_function_reference", ptr @linked.MTL_VISIBLE_FN_REF, !"linked"}
"#;
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![LinkedFunctionReference {
                symbol: "linked".into(),
                module_ll: "define i32 @linked(i32 %value) { ret i32 %value }\n".into(),
            }],
            visible_tables: vec![],
            intersection_tables: vec![],
        };

        let specialized = specialize_visible_function_references(entry, &linkage).unwrap();
        assert!(specialized.contains("call i32 @linked(i32 %value)"));
        assert!(!specialized.contains("call i32 %function"));
        assert!(!specialized.contains("call i32 @apply(ptr @linked"));
    }

    #[test]
    fn an_unsupplied_visible_reference_is_left_unresolved_never_linked() {
        let entry = r#"define void @main() {
  call void @missing.MTL_VISIBLE_FN_REF()
  ret void
}
declare void @missing.MTL_VISIBLE_FN_REF() section "air.externally_defined"
!air.visible_function_references = !{!0}
!0 = !{!"air.visible_function_reference", ptr @missing.MTL_VISIBLE_FN_REF, !"missing"}
"#;
        let specialized = specialize_visible_function_references(
            entry,
            &LinkedFunctionLinkage {
                visible_references: vec![],
                visible_tables: vec![],
                intersection_tables: vec![],
            },
        )
        .unwrap();
        assert!(
            !specialized.contains(".MTL_VISIBLE_FN_REF"),
            "{specialized}"
        );
        assert!(
            specialized.contains("call void @missing.MTL_UNRESOLVED_VISIBLE_FN()"),
            "{specialized}"
        );
    }

    const ENTRY: &str = r#"
define void @main(ptr addrspace(1) %output, ptr addrspace(1) %table) {
entry:
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %table, i32 0)
  %cast = bitcast ptr %fp to ptr
  %value = call i32 %cast(i32 41)
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}
"#;

    #[test]
    fn linkage_retains_intersection_tables_even_without_visible_tables() {
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![],
            intersection_tables: vec![IntersectionFunctionTable {
                source: IntersectionFunctionTableSource::Parameter { parameter_index: 2 },
                size: 1,
                entries: vec![IntersectionFunctionEntry::Linked(LinkedFunction {
                    index: 0,
                    symbol: "intersection".into(),
                    module_ll: "define i1 @intersection() { ret i1 true }".into(),
                })],
            }],
        };
        assert!(!linkage.is_empty());
        assert_eq!(
            linkage.intersection_tables[0].source,
            IntersectionFunctionTableSource::Parameter { parameter_index: 2 }
        );
        assert_eq!(
            specialize_visible_function_tables(ENTRY, "main", &linkage).unwrap(),
            ENTRY
        );
    }

    #[test]
    fn authored_intersection_table_setter_is_consumed_only_for_traced_destination() {
        let entry = r#"
define void @main(ptr addrspace(1) %destination, ptr addrspace(1) %source, i32 %index) {
entry:
  call void @air.set_buffer_intersection_function_table.p1i8(ptr addrspace(1) %destination, ptr addrspace(1) %source, i32 %index)
  ret void
}
declare void @air.set_buffer_intersection_function_table.p1i8(ptr addrspace(1), ptr addrspace(1), i32)
"#;
        let table = |parameter_index| IntersectionFunctionTable {
            source: IntersectionFunctionTableSource::Parameter { parameter_index },
            size: 1,
            entries: vec![],
        };
        let linkage = |parameter_index| LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![],
            intersection_tables: vec![table(parameter_index)],
        };

        let specialized =
            specialize_opaque_triangle_intersection_tables(entry, "main", &linkage(0)).unwrap();
        assert_eq!(
            specialized
                .lines()
                .filter(|line| line.contains("air.set_buffer_intersection_function_table"))
                .count(),
            1,
            "only the declaration remains after authored destination specialization"
        );

        let untraced =
            specialize_opaque_triangle_intersection_tables(entry, "main", &linkage(1)).unwrap();
        assert!(untraced.lines().any(|line| {
            line.contains("call void @air.set_buffer_intersection_function_table")
        }));
    }

    #[test]
    fn fully_opaque_triangle_table_specializes_callback_query() {
        let entry = r#"
define void @main(ptr addrspace(1) %output, ptr addrspace(1) %table, ptr addrspace(1) %as) {
entry:
  %hit = call { i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 } @air.intersect.intersection_function_buffer.triangle_data(<3 x float> zeroinitializer, <3 x float> zeroinitializer, float 0.0, float 1.0, ptr addrspace(1) %as, ptr addrspace(1) %table, i64 0, i64 1, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  ret void
}
"#;
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![],
            intersection_tables: vec![IntersectionFunctionTable {
                source: IntersectionFunctionTableSource::Parameter { parameter_index: 1 },
                size: 1,
                entries: vec![IntersectionFunctionEntry::OpaqueTriangle {
                    index: 0,
                    signature: vec![
                        IntersectionFunctionSignature::TriangleData,
                        IntersectionFunctionSignature::IntersectionFunctionBuffer,
                    ],
                }],
            }],
        };
        let specialized =
            specialize_opaque_triangle_intersection_tables(entry, "main", &linkage).unwrap();
        assert!(specialized.contains("@air.intersect.triangle_data("));
        assert!(!specialized.contains("@air.intersect.intersection_function_buffer"));
        let call = specialized
            .lines()
            .find(|line| line.contains("%hit = call"))
            .unwrap();
        assert_eq!(named_call(call).unwrap().3.len(), 18);
    }

    #[test]
    fn null_or_signature_mismatched_slots_do_not_erase_callbacks() {
        let entry = r#"
define void @main(ptr addrspace(1) %table, ptr addrspace(1) %as) {
entry:
  %hit = call { i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 } @air.intersect.intersection_function_buffer.triangle_data(<3 x float> zeroinitializer, <3 x float> zeroinitializer, float 0.0, float 1.0, ptr addrspace(1) %as, ptr addrspace(1) %table, i64 0, i64 1, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  ret void
}
"#;
        for entries in [
            vec![],
            vec![IntersectionFunctionEntry::OpaqueTriangle {
                index: 0,
                signature: vec![IntersectionFunctionSignature::IntersectionFunctionBuffer],
            }],
        ] {
            let linkage = LinkedFunctionLinkage {
                visible_references: vec![],
                visible_tables: vec![],
                intersection_tables: vec![IntersectionFunctionTable {
                    source: IntersectionFunctionTableSource::Parameter { parameter_index: 0 },
                    size: 1,
                    entries,
                }],
            };
            assert_eq!(
                specialize_opaque_triangle_intersection_tables(entry, "main", &linkage).unwrap(),
                entry
            );
        }
    }

    #[test]
    fn opaque_triangle_user_data_family_removes_the_fifth_callback_operand() {
        let entry = r#"
define void @main(ptr addrspace(1) %table, ptr addrspace(1) %as) {
entry:
  %hit = call { i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 } @air.intersect.intersection_function_buffer.triangle_data.user_data(<3 x float> zeroinitializer, <3 x float> zeroinitializer, float 0.0, float 1.0, ptr addrspace(1) %as, ptr addrspace(1) %table, i64 1, i64 8, ptr addrspace(1) null, ptr null, i64 0, i32 10, i32 11, i32 12, i32 13, i32 14, i32 15, i32 16, i32 17, i32 18, i1 false, i32 91, i32 92)
  ret void
}
"#;
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![],
            intersection_tables: vec![IntersectionFunctionTable {
                source: IntersectionFunctionTableSource::Parameter { parameter_index: 0 },
                size: 1,
                entries: vec![IntersectionFunctionEntry::OpaqueTriangle {
                    index: 0,
                    signature: vec![
                        IntersectionFunctionSignature::TriangleData,
                        IntersectionFunctionSignature::IntersectionFunctionBuffer,
                        IntersectionFunctionSignature::UserData,
                    ],
                }],
            }],
        };
        let specialized =
            specialize_opaque_triangle_intersection_tables(entry, "main", &linkage).unwrap();
        let call = specialized
            .lines()
            .find(|line| line.contains("%hit = call"))
            .unwrap();
        assert!(call.contains("@air.intersect.triangle_data("));
        assert_eq!(named_call(call).unwrap().3.len(), 18);
        assert!(call.contains("ptr null, i64 0, i32 10"), "{call}");
        assert!(!call.contains("i32 91"), "{call}");
        assert!(!call.contains("i32 92"), "{call}");
    }

    #[test]
    fn opaque_table_loaded_from_authored_argument_buffer_field_is_specialized() {
        let entry = r#"
%struct.Args = type { ptr addrspace(1), i64 }
define void @main(ptr addrspace(1) %args, ptr addrspace(1) %as) {
entry:
  %slot = getelementptr inbounds %struct.Args, ptr addrspace(1) %args, i64 0, i32 0
  %table = load ptr addrspace(1), ptr addrspace(1) %slot, align 8
  %hit = call { i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 } @air.intersect.intersection_function_buffer.triangle_data(<3 x float> zeroinitializer, <3 x float> zeroinitializer, float 0.0, float 1.0, ptr addrspace(1) %as, ptr addrspace(1) %table, i64 1, i64 8, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  ret void
}
"#;
        let linkage = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![],
            intersection_tables: vec![IntersectionFunctionTable {
                source: IntersectionFunctionTableSource::ArgumentBuffer {
                    buffer_parameter_index: 0,
                    field_ordinal: 0,
                    field_offset: 0,
                },
                size: 1,
                entries: vec![IntersectionFunctionEntry::OpaqueTriangle {
                    index: 0,
                    signature: vec![
                        IntersectionFunctionSignature::TriangleData,
                        IntersectionFunctionSignature::IntersectionFunctionBuffer,
                    ],
                }],
            }],
        };
        let specialized =
            specialize_opaque_triangle_intersection_tables(entry, "main", &linkage).unwrap();
        assert!(specialized.contains("@air.intersect.triangle_data("));
        assert!(!specialized.contains("@air.intersect.intersection_function_buffer"));
    }

    #[test]
    fn constant_slot_becomes_a_direct_linked_call() {
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 1,
                entries: vec![LinkedFunction {
                    index: 0,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) #0 {\nentry:\n  %y = add i32 %x, 1, !range !0\n  ret i32 %y\n}\nattributes #0 = { nounwind }\n!0 = !{i32 0, i32 2}\n".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(ENTRY, "main", &linked).unwrap();
        assert!(specialized.contains("%value = call i32 @add_one(i32 41)"));
        assert!(specialized.contains("define i32 @add_one"));
        assert!(!specialized.contains("attributes #0 ="));
        assert!(!specialized.contains("!0 = !{i32 0, i32 2}"));
    }

    #[test]
    fn dynamic_slot_gets_an_authored_switch_dispatcher() {
        let entry = ENTRY
            .replace(
                "ptr addrspace(1) %table)",
                "ptr addrspace(1) %table, i32 %slot)",
            )
            .replace("i32 0)", "i32 %slot)");
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 1,
                entries: vec![LinkedFunction {
                    index: 0,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) { ret i32 %x }".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(&entry, "main", &linked).unwrap();
        assert!(specialized
            .contains("call i32 @metal2vulkan.linked.table.p1.dispatch.0(i32 %slot, i32 41)"));
        assert!(specialized.contains("switch i32 %metal2vulkan_slot"));
        assert!(specialized.contains("call i32 @add_one(i32 %arg0)"));
    }

    #[test]
    fn dynamic_dispatcher_contains_only_type_compatible_table_entries() {
        let entry = ENTRY
            .replace(
                "ptr addrspace(1) %table)",
                "ptr addrspace(1) %table, i32 %slot)",
            )
            .replace("i32 0)", "i32 %slot)");
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 2,
                entries: vec![
                    LinkedFunction {
                        index: 0,
                        symbol: "integer_function".into(),
                        module_ll: "define i32 @integer_function(i32 %x) { ret i32 %x }".into(),
                    },
                    LinkedFunction {
                        index: 1,
                        symbol: "float_function".into(),
                        module_ll: "define float @float_function(float %x) { ret float %x }".into(),
                    },
                ],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(&entry, "main", &linked).unwrap();
        assert!(specialized.contains("i32 0, label %case_0"));
        assert!(!specialized.contains("label %case_1"));
        assert!(!specialized.contains("define float @float_function"));
    }

    #[test]
    fn authored_table_size_and_nullness_are_not_placeholders() {
        let entry = ENTRY.replace(
            "%fp = call ptr",
            "%size = call i32 @air.get_size_visible_function_table(ptr addrspace(1) %table)\n  %is_null = call i1 @air.is_null_visible_function_table(ptr addrspace(1) %table)\n  %fp = call ptr",
        );
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 4,
                entries: vec![LinkedFunction {
                    index: 3,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) { ret i32 %x }".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        let entry = entry.replace("i32 0)", "i32 3)");
        let specialized = specialize_visible_function_tables(&entry, "main", &linked).unwrap();
        assert!(specialized.contains("%size = add i32 0, 4"));
        assert!(specialized.contains("%is_null = or i1 false, false"));
    }

    #[test]
    fn all_null_table_preserves_authored_capacity_and_null_slots() {
        let entry = r#"
define void @main(ptr addrspace(1) %table, i32 %slot) {
entry:
  %size = call i32 @air.get_size_visible_function_table(ptr addrspace(1) %table)
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %table, i32 %slot)
  %is_null = icmp eq ptr %fp, null
  ret void
}
"#;
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 0,
                size: 6,
                entries: vec![],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(entry, "main", &linked).unwrap();
        assert!(specialized.contains("%size = add i32 0, 6"));
        assert!(specialized.contains("%is_null = xor i1 false, true"));
    }

    #[test]
    fn dynamic_lookup_nullness_is_authored_slot_membership() {
        let entry = ENTRY
            .replace(
                "ptr addrspace(1) %table)",
                "ptr addrspace(1) %table, i32 %slot)",
            )
            .replace("i32 0)", "i32 %slot)")
            .replace(
                "%cast = bitcast ptr %fp to ptr",
                "%is_null = icmp eq ptr %fp, null\n  %cast = bitcast ptr %fp to ptr",
            );
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 8,
                entries: vec![
                    LinkedFunction {
                        index: 2,
                        symbol: "add_one".into(),
                        module_ll: "define i32 @add_one(i32 %x) { ret i32 %x }".into(),
                    },
                    LinkedFunction {
                        index: 7,
                        symbol: "add_two".into(),
                        module_ll: "define i32 @add_two(i32 %x) { ret i32 %x }".into(),
                    },
                ],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(&entry, "main", &linked).unwrap();
        assert!(specialized.contains("icmp eq i32 %slot, 2"));
        assert!(specialized.contains("icmp eq i32 %slot, 7"));
        assert!(specialized.contains("%is_null = xor i1 %metal2vulkan.table.present."));
        assert!(!specialized.contains("%is_null = icmp eq ptr %fp, null"));
    }

    #[test]
    fn visible_function_pointer_opaque_sentinel_probe_is_folded() {
        let entry = ENTRY
            .replace(
                "ptr addrspace(1) %table)",
                "ptr addrspace(1) %table, i32 %slot)",
            )
            .replace("i32 0)", "i32 %slot)")
            .replace(
                "%cast = bitcast ptr %fp to ptr",
                "%wide = ptrtoint ptr %fp to i64\n  %low = trunc i64 %wide to i32\n  %is_opaque = icmp eq i32 %low, 1\n  %cast = bitcast ptr %fp to ptr",
            );
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 4,
                entries: vec![LinkedFunction {
                    index: 3,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) { ret i32 %x }".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        let specialized = specialize_visible_function_tables(&entry, "main", &linked).unwrap();
        assert!(specialized.contains("%is_opaque = or i1 false, false"));
        assert!(!specialized.contains("%is_opaque = icmp eq i32 %low, 1"));
        assert!(specialized.contains("@metal2vulkan.linked.table.p1.dispatch.0"));
    }

    #[test]
    fn dynamic_slot_is_threaded_through_an_internal_helper_parameter() {
        let entry = r#"
define void @main(ptr addrspace(1) %output, ptr addrspace(1) %table, i32 %slot) {
entry:
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %table, i32 %slot)
  %typed = bitcast ptr %fp to ptr
  %value = call i32 @invoke(ptr %typed, i32 41)
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}

define internal i32 @invoke(ptr %callback, i32 %value) {
entry:
  %result = call i32 %callback(i32 %value)
  ret i32 %result
}
"#;
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 4,
                entries: vec![LinkedFunction {
                    index: 3,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) { ret i32 %x }".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        let traced = trace_visible_function_table_parameters(entry, "main", &[1]).unwrap();
        assert_eq!(
            traced["@main"],
            crate::fxhash::set_from(["%table".to_string()])
        );
        let specialized = specialize_visible_function_tables(entry, "main", &linked).unwrap();
        assert!(specialized.contains("call i32 @invoke(ptr %typed, i32 41, i32 %slot)"));
        assert!(specialized.contains(
            "define internal i32 @invoke(ptr %callback, i32 %value, i32 %metal2vulkan.table.param0.slot)"
        ));
        assert!(specialized.contains(
            "@metal2vulkan.linked.table.p1.dispatch.0(i32 %metal2vulkan.table.param0.slot, i32 %value)"
        ));
        assert!(!specialized.contains("call i32 %callback("));
    }

    #[test]
    fn table_parameter_trace_uses_the_specializers_internal_call_flow() {
        let entry = r#"
define void @main(ptr addrspace(1) %table) {
entry:
  %value = call i32 @invoke(ptr addrspace(1) %table)
  ret void
}
define internal i32 @invoke(ptr addrspace(1) %functions) {
entry:
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %functions, i32 0)
  %value = call i32 %fp()
  ret i32 %value
}
"#;
        let traced = trace_visible_function_table_parameters(entry, "main", &[0]).unwrap();
        assert_eq!(
            traced["@main"],
            crate::fxhash::set_from(["%table".to_string()])
        );
        assert_eq!(
            traced["@invoke"],
            crate::fxhash::set_from(["%functions".to_string()])
        );
    }

    #[test]
    fn linked_translation_emits_the_direct_function_and_no_table_descriptor() {
        let entry = format!(
            r#"{ENTRY}
declare ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1), i32)
!air.kernel = !{{!0}}
!0 = !{{ptr @main, !1, !2}}
!1 = !{{}}
!2 = !{{!3, !4}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"uint"}}
!4 = !{{i32 1, !"air.visible_function_table", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"visible_function_table"}}
"#
        );
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 1,
                entries: vec![LinkedFunction {
                    index: 0,
                    symbol: "add_one".into(),
                    module_ll: "define i32 @add_one(i32 %x) {\nentry:\n  %y = add i32 %x, 1\n  ret i32 %y\n}\n".into(),
                }],
            }],
            intersection_tables: vec![],
        };
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "metal2vulkan-linked-function-{}",
            std::process::id()
        )));
        let _ = std::fs::remove_dir_all(&scratch.0);
        std::fs::create_dir(&scratch.0).unwrap();
        let spv = crate::translate_sanitized_native_linked_with_options(
            &entry,
            crate::passes::Stage::Kernel,
            &scratch.0,
            crate::passes::TransformOptions::default(),
            &linked,
        )
        .unwrap();
        let asm = crate::disassemble(&spv).unwrap();
        assert!(asm.contains("OpIAdd"), "{asm}");
        assert!(!asm.contains("visible_function_table"), "{asm}");
        assert!(!asm.contains("Binding 1"), "{asm}");
    }

    #[test]
    fn direct_reference_linked_translation_emits_the_authored_definition() {
        let entry = r#"
define void @main(ptr addrspace(1) %output) {
entry:
  %value = call i32 @add_one.MTL_VISIBLE_FN_REF(i32 41)
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}
declare i32 @add_one.MTL_VISIBLE_FN_REF(i32) section "air.externally_defined"
!air.kernel = !{!0}
!air.visible_function_references = !{!4}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"uint"}
!4 = !{!"air.visible_function_reference", ptr @add_one.MTL_VISIBLE_FN_REF, !"add_one"}
"#;
        let linked = LinkedFunctionLinkage {
            visible_references: vec![LinkedFunctionReference {
                symbol: "add_one".into(),
                module_ll:
                    "define i32 @add_one(i32 %x) {\nentry:\n  %y = add i32 %x, 1\n  ret i32 %y\n}\n"
                        .into(),
            }],
            visible_tables: vec![],
            intersection_tables: vec![],
        };
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "metal2vulkan-direct-linked-function-{}",
            std::process::id()
        )));
        let _ = std::fs::remove_dir_all(&scratch.0);
        std::fs::create_dir(&scratch.0).unwrap();
        let spv = crate::translate_sanitized_native_linked_with_options(
            entry,
            crate::passes::Stage::Kernel,
            &scratch.0,
            crate::passes::TransformOptions::default(),
            &linked,
        )
        .unwrap();
        let asm = crate::disassemble(&spv).unwrap();
        assert!(asm.contains("OpIAdd"), "{asm}");
    }

    #[test]
    fn dynamic_linked_translation_emits_authored_slot_dispatch() {
        let entry = r#"
define void @main(ptr addrspace(1) %output, ptr addrspace(1) %table, ptr addrspace(2) %slot_buffer) {
entry:
  %slot = load i32, ptr addrspace(2) %slot_buffer, align 4
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %table, i32 %slot)
  %value = call i32 %fp(i32 40)
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}
declare ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1), i32)
!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"uint"}
!4 = !{i32 1, !"air.visible_function_table", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"visible_function_table"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_name", !"uint"}
"#;
        let linked = LinkedFunctionLinkage {
            visible_references: vec![],
            visible_tables: vec![LinkedFunctionTable {
                parameter_index: 1,
                size: 2,
                entries: vec![
                    LinkedFunction {
                        index: 0,
                        symbol: "add_one".into(),
                        module_ll: "define i32 @add_one(i32 %x) {\nentry:\n  %y = add i32 %x, 1\n  ret i32 %y\n}\n".into(),
                    },
                    LinkedFunction {
                        index: 1,
                        symbol: "add_two".into(),
                        module_ll: "define i32 @add_two(i32 %x) {\nentry:\n  %y = add i32 %x, 2\n  ret i32 %y\n}\n".into(),
                    },
                ],
            }],
            intersection_tables: vec![],
        };
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "metal2vulkan-linked-function-dynamic-{}",
            std::process::id()
        )));
        let _ = std::fs::remove_dir_all(&scratch.0);
        std::fs::create_dir(&scratch.0).unwrap();
        let spv = crate::translate_sanitized_native_linked_with_options(
            entry,
            crate::passes::Stage::Kernel,
            &scratch.0,
            crate::passes::TransformOptions::default(),
            &linked,
        )
        .unwrap();
        let asm = crate::disassemble(&spv).unwrap();
        assert!(asm.contains("OpSwitch"), "{asm}");
        assert!(!asm.contains("Binding 1"), "{asm}");
    }
}

pub fn rewrite_runtime_table_lookups(
    entry_ll: &str,
    entry_name: &str,
    parameter_indices: &[u32],
) -> Result<String, String> {
    if parameter_indices.is_empty() {
        return Ok(entry_ll.to_string());
    }
    let traced = trace_visible_function_table_parameters(entry_ll, entry_name, parameter_indices)?;
    let mut output = String::with_capacity(entry_ll.len() + 256);
    let mut current = None::<String>;
    let mut counter = 0usize;
    let mut rewrote = 0usize;
    for line in entry_ll.lines() {
        let trimmed = line.trim_start();
        if let Some(global) = definition_global(trimmed) {
            current = Some(global);
        } else if trimmed == "}" {
            current = None;
        }
        let lookup = trimmed
            .split_once(" = ")
            .filter(|(_, i)| i.contains("@air.get_function_pointer_visible_function_table("));
        if let (Some((result, instruction)), Some(function)) = (lookup, current.as_deref()) {
            let arguments = call_arguments(instruction)?;
            if arguments.len() < 2 {
                return Err(format!(
                    "runtime table lookup has {} arguments: {trimmed}",
                    arguments.len()
                ));
            }
            let table_arg = arguments[0].trim();
            let table_value = value_operand(table_arg);
            let is_table = traced
                .get(function)
                .is_some_and(|values| values.contains(table_value));
            if is_table {
                let table_ty = table_arg[..table_arg.len() - table_value.len()].trim();
                if !table_ty.starts_with("ptr") {
                    return Err(format!(
                        "runtime table operand is not an opaque pointer ({table_ty}): {trimmed}"
                    ));
                }
                let slot = arguments[1].trim();
                let k = counter;
                counter += 1;
                let indent = &line[..line.len() - trimmed.len()];
                output.push_str(&format!(
                    "{indent}%m2v.vft.p{k} = getelementptr inbounds i64, {table_arg}, {slot}\n\
                     {indent}%m2v.vft.id{k} = load i64, ptr addrspace(1) %m2v.vft.p{k}, align 8\n\
                     {indent}%m2v.vft.s{k} = trunc i64 %m2v.vft.id{k} to i32\n"
                ));
                let open = instruction
                    .find("@air.get_function_pointer_visible_function_table(")
                    .unwrap();
                let head = &instruction[..open];
                output.push_str(&format!(
                    "{indent}{} = {head}@air.get_function_pointer_visible_function_table({table_arg}, i32 %m2v.vft.s{k})\n",
                    result.trim()
                ));
                rewrote += 1;
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    if rewrote == 0 {
        return Err(
            "runtime visible function table: the entry declares a table but no lookup reads it"
                .into(),
        );
    }
    let mut result = String::with_capacity(output.len() + 256);
    for line in output.lines() {
        let mut l = line.to_string();
        if l.trim_start().starts_with('!') && l.contains("!\"air.visible_function_table\"") {
            let idx = l
                .split("!{i32 ")
                .nth(1)
                .and_then(|r| r.split(',').next())
                .and_then(|n| n.trim().parse::<u32>().ok());
            if idx.is_some_and(|i| parameter_indices.contains(&i)) {
                l = l.replacen("!\"air.visible_function_table\"", "!\"air.buffer\"", 1);
                let tn = "!\"air.arg_type_name\", !\"visible_function_table\"";
                if !l.contains(tn) {
                    return Err(format!(
                        "runtime table metadata has an unexpected shape: {line}"
                    ));
                }
                l = l.replacen(
                    tn,
                    "!\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 8, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\", !\"ulong\"",
                    1,
                );
            }
        }
        result.push_str(&l);
        result.push('\n');
    }
    Ok(result)
}

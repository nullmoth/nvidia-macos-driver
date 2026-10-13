pub(in crate::native) mod build;
pub(in crate::native) mod interp;
pub(in crate::native) mod shapes;

#[cfg(test)]
mod tests;

use crate::spirv_module::Module;
use interp::Outcome;

pub(in crate::native) const STEP_LIMIT: usize = 2_000_000;

pub(in crate::native) fn assert_construction_preserves_semantics(
    module: Module,
    arguments: &[&[u32]],
) -> Module {
    let before = arguments
        .iter()
        .map(|argument| {
            interp::run_module(&module, argument, STEP_LIMIT)
                .unwrap_or_else(|error| panic!("authored function on {argument:?}: {error}"))
        })
        .collect::<Vec<_>>();

    let mut constructed = module;
    crate::native::rewrites::construct_cfg_functions_module(
        &mut constructed,
        &crate::fxhash::HashSet::default(),
    )
    .expect("construction");

    for (argument, expected) in arguments.iter().zip(&before) {
        let actual = interp::run_module(&constructed, argument, STEP_LIMIT)
            .unwrap_or_else(|error| panic!("constructed function on {argument:?}: {error}"));
        assert_eq!(
            &actual, expected,
            "construction changed the result on {argument:?}"
        );
    }
    constructed
}

pub(in crate::native) fn nests(module: &Module) -> bool {
    let mut module = module.clone();
    let functions = module
        .functions
        .iter()
        .filter_map(|function| function.def.as_ref().and_then(|def| def.result_id))
        .collect::<crate::fxhash::HashSet<_>>();
    !crate::native::reloop_nest::structure_selected_functions(&mut module, &functions).is_empty()
}

pub(in crate::native) fn outcomes(module: &Module, arguments: &[&[u32]]) -> Vec<Outcome> {
    arguments
        .iter()
        .map(|argument| {
            interp::run_module(module, argument, STEP_LIMIT)
                .unwrap_or_else(|error| panic!("running on {argument:?}: {error}"))
        })
        .collect()
}

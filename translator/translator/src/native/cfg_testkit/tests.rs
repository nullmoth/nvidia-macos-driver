use super::{assert_construction_preserves_semantics, interp, nests, outcomes, shapes, STEP_LIMIT};
use crate::native::cfg_testkit::build::CfgBuilder;
use crate::native::cfg_testkit::interp::{Outcome, Value};

const ARGUMENTS: &[&[u32]] = &[&[0], &[1], &[2], &[3], &[7], &[16], &[31], &[1000]];

#[test]
fn the_interpreter_agrees_with_the_closed_form_of_a_counted_sum() {
    let module = counted_sum();
    for n in [0u32, 1, 2, 5, 17, 64] {
        let expected = (0..n).sum::<u32>();
        assert_eq!(
            interp::run_module(&module, &[n], STEP_LIMIT).expect("counted sum"),
            Outcome::Returned(Value::Int(expected)),
            "sum of 0..{n}"
        );
    }
}

#[test]
fn the_interpreter_reports_a_function_that_does_not_terminate() {
    let mut builder = CfgBuilder::new(1);
    let one = builder.constant(1);
    builder.block("entry");
    builder.branch("header");
    builder.block("header");
    let counter = builder.reserve_value();
    let seed = builder.parameter(0);
    let merged = builder.phi(&[(seed, "entry"), (counter, "header")]);
    builder.add_into(counter, merged, one);
    builder.branch("header");
    let module = builder.finish();

    let error = interp::run_module(&module, &[0], 10_000).expect_err("no exit edge");
    assert!(error.contains("does not terminate"), "{error}");
}

fn counted_sum() -> crate::spirv_module::Module {
    let mut builder = CfgBuilder::new(1);
    let zero = builder.constant(0);
    let one = builder.constant(1);

    builder.block("entry");
    builder.branch("header");

    builder.block("header");
    let total = builder.reserve_value();
    let index = builder.reserve_value();
    let limit = builder.parameter(0);
    let total_in = builder.phi(&[(zero, "entry"), (total, "body")]);
    let index_in = builder.phi(&[(zero, "entry"), (index, "body")]);
    let below = builder.less_than(index_in, limit);
    builder.branch_conditional(below, "body", "exit");

    builder.block("body");
    builder.add_into(total, total_in, index_in);
    builder.add_into(index, index_in, one);
    builder.branch("header");

    builder.block("exit");
    builder.return_value(total_in);

    builder.finish()
}

#[test]
fn generated_shapes_terminate_before_construction() {
    for seed in 0..64u64 {
        let shape = shapes::shape(seed, 4);
        let module = shapes::author(&shape);
        let results = outcomes(&module, ARGUMENTS);
        assert_eq!(results.len(), ARGUMENTS.len(), "seed {seed}");
    }
}

#[test]
fn generated_shapes_are_unstructured_enough_to_select_construction() {
    let mut selected = 0;
    for seed in 0..64u64 {
        let module = shapes::author(&shapes::shape(seed, 4));
        let function = &module.functions[0];
        if crate::native::rewrites::blocks_have_unowned_selection_header(&function.blocks)
            || crate::native::rewrites::function_has_unowned_backedge(function)
        {
            selected += 1;
        }
    }
    assert!(
        selected >= 60,
        "only {selected}/64 generated shapes need construction; the generator has stopped \
         producing unstructured control flow and the differential sweep is testing nothing"
    );
}

#[test]
fn construction_preserves_semantics_across_generated_shapes() {
    for seed in 0..64u64 {
        let shape = shapes::shape(seed, 4);
        let module = shapes::author(&shape);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_construction_preserves_semantics(module, ARGUMENTS);
        }))
        .unwrap_or_else(|_| {
            panic!(
                "construction lost the semantics of this shape:\n{}",
                shapes::describe(&shape)
            )
        });
    }
}

#[test]
fn construction_preserves_semantics_across_deeper_generated_shapes() {
    for seed in 1000..1032u64 {
        let shape = shapes::shape(seed, 6);
        let module = shapes::author(&shape);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_construction_preserves_semantics(module, ARGUMENTS);
        }))
        .unwrap_or_else(|_| {
            panic!(
                "construction lost the semantics of this shape:\n{}",
                shapes::describe(&shape)
            )
        });
    }
}

#[test]
fn a_break_out_of_two_constructs_names_its_destination() {
    let module = break_out_of_a_selection_inside_a_loop();
    assert!(
        nests(&module),
        "the nesting structurizer declined a break out of two constructs; the function falls back \
         to the whole-function dispatch this pass exists to avoid"
    );
    assert_construction_preserves_semantics(module, ARGUMENTS);
}

#[test]
fn most_generated_shapes_nest_instead_of_falling_back_to_the_state_machine() {
    let mut nested = 0;
    let mut total = 0;
    for (seeds, depth) in [(0..64u64, 4u32), (1000..1064, 6)] {
        for seed in seeds {
            total += 1;
            if nests(&shapes::author(&shapes::shape(seed, depth))) {
                nested += 1;
            }
        }
    }
    assert!(
        nested * 10 >= total * 7,
        "only {nested}/{total} generated reducible shapes nest; the rest fall back to the \
         whole-function dispatch that hangs a driver's shader compiler"
    );
}

fn break_out_of_a_selection_inside_a_loop() -> crate::spirv_module::Module {
    let mut builder = CfgBuilder::new(1);
    let (zero, one, four) = (
        builder.constant(0),
        builder.constant(1),
        builder.constant(4),
    );
    let bound = builder.constant(4034);
    let (three, five, six, seven, two) = (
        builder.constant(3),
        builder.constant(5),
        builder.constant(6),
        builder.constant(7),
        builder.constant(2),
    );

    let entering = builder.reserve_value();
    let latched = builder.reserve_value();

    builder.block("entry");
    let seed = builder.parameter(0);
    let start = builder.add(seed, one);
    builder.branch("header");

    builder.block("header");
    let merged = builder.phi(&[(start, "entry"), (latched, "latch")]);
    builder.add_into(entering, merged, three);
    let below = builder.less_than(entering, bound);
    builder.branch_conditional(below, "body", "exit");

    builder.block("body");
    let in_body = builder.add(entering, four);
    let masked = builder.bitwise_and(in_body, four);
    let d = builder.equal(masked, zero);
    builder.branch_conditional(d, "guard", "skip");

    builder.block("guard");
    let in_guard = builder.add(in_body, six);
    let low = builder.bitwise_and(in_guard, one);
    let e = builder.equal(low, zero);
    builder.branch_conditional(e, "exit", "latch");

    builder.block("skip");
    let in_skip = builder.add(in_body, seven);
    builder.branch("latch");

    builder.block("latch");
    let carried = builder.phi(&[(in_guard, "guard"), (in_skip, "skip")]);
    builder.add_into(latched, carried, five);
    builder.branch("header");

    builder.block("exit");
    let leaving = builder.phi(&[(entering, "header"), (in_guard, "guard")]);
    let result = builder.add(leaving, two);
    builder.return_value(result);

    builder.finish()
}

#[test]
fn construction_preserves_semantics_across_irreducible_shapes() {
    for depth in [3u32, 5] {
        for seed in 0..48u64 {
            let shape = shapes::irreducible_shape(seed, depth, 3);
            let module = shapes::author(&shape);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_construction_preserves_semantics(module, ARGUMENTS);
            }))
            .unwrap_or_else(|_| {
                panic!(
                    "construction lost the semantics of this shape:\n{}",
                    shapes::describe(&shape)
                )
            });
        }
    }
}

#[test]
fn irreducible_shapes_are_declined_by_the_nesting_structurizer() {
    let mut declined = 0;
    let mut total = 0;
    for depth in [3u32, 5] {
        for seed in 0..48u64 {
            total += 1;
            if !nests(&shapes::author(&shapes::irreducible_shape(seed, depth, 3))) {
                declined += 1;
            }
        }
    }
    assert!(
        declined * 2 >= total,
        "only {declined}/{total} shapes with interior cross edges reach the state machine"
    );
}

#[test]
fn a_pair_of_loop_phis_that_swap_still_swaps() {
    let mut builder = CfgBuilder::new(1);
    let (one, two, three, bound) = (
        builder.constant(1),
        builder.constant(2),
        builder.constant(3),
        builder.constant(64),
    );

    let x_next = builder.reserve_value();
    let y_next = builder.reserve_value();
    let i_next = builder.reserve_value();

    builder.block("entry");
    let seed = builder.parameter(0);
    let x0 = builder.add(seed, one);
    let y0 = builder.add(seed, two);
    builder.branch("header");

    builder.block("header");
    let x = builder.phi(&[(x0, "entry"), (y_next, "latch")]);
    let y = builder.phi(&[(y0, "entry"), (x_next, "latch")]);
    let i = builder.phi(&[(seed, "entry"), (i_next, "latch")]);
    let below = builder.less_than(i, bound);
    builder.branch_conditional(below, "latch", "exit");

    builder.block("latch");
    builder.add_into(x_next, x, three);
    builder.add_into(y_next, y, three);
    builder.add_into(i_next, i, one);
    builder.branch("header");

    builder.block("exit");
    let mixed = builder.bitwise_and(x, one);
    let combined = builder.add(y, mixed);
    builder.return_value(combined);

    assert_construction_preserves_semantics(builder.finish(), ARGUMENTS);
}

#[test]
fn generated_shapes_carry_the_phi_swap_on_real_back_edges() {
    let mut swapping = 0;
    let mut shapes_with_a_swap = 0;
    let mut total = 0;
    for (seeds, depth, crossings) in [(0..64u64, 4u32, 0u32), (0..48, 5, 3)] {
        for seed in seeds {
            total += 1;
            let edges = shapes::irreducible_shape(seed, depth, crossings).swapping_edges();
            swapping += edges;
            if edges > 0 {
                shapes_with_a_swap += 1;
            }
        }
    }
    assert!(
        shapes_with_a_swap * 2 >= total && swapping >= total,
        "only {shapes_with_a_swap}/{total} generated shapes have a back edge ({swapping} edges \
         in total); the swap the author hangs on them is not being exercised"
    );
}

#[test]
fn constant_folding_preserves_the_one_answer_a_constant_seeded_shape_has() {
    for depth in [3u32, 5] {
        for seed in 0..32u64 {
            for value in [0u32, 3] {
                let shape = shapes::irreducible_shape(seed, depth, 2);
                let (module, slot) = shapes::author_constant_seeded(&shape, value);
                let expected = interp::run_module_to_global(&module, slot, STEP_LIMIT)
                    .unwrap_or_else(|error| {
                        panic!("authored: {error}\n{}", shapes::describe(&shape))
                    });

                let mut folded_then_constructed = module.clone();
                crate::native::rewrites::prune_constant_cfg_module_if_changed(
                    &mut folded_then_constructed,
                );
                let _ = crate::native::rewrites::construct_cfg_functions_module(
                    &mut folded_then_constructed,
                    &crate::fxhash::HashSet::default(),
                );

                let mut constructed_then_folded = module;
                let _ = crate::native::rewrites::construct_cfg_functions_module(
                    &mut constructed_then_folded,
                    &crate::fxhash::HashSet::default(),
                );
                crate::native::rewrites::prune_constant_cfg_module_if_changed(
                    &mut constructed_then_folded,
                );

                for (order, module) in [
                    ("fold then construct", &folded_then_constructed),
                    ("construct then fold", &constructed_then_folded),
                ] {
                    let actual = interp::run_module_to_global(module, slot, STEP_LIMIT)
                        .unwrap_or_else(|error| {
                            panic!(
                                "{order} (value {value}): {error}\n{}",
                                shapes::describe(&shape)
                            )
                        });
                    assert_eq!(
                        actual,
                        expected,
                        "{order} (value {value}) changed the answer of this shape:\n{}",
                        shapes::describe(&shape)
                    );
                }
            }
        }
    }
}

#[test]
fn constant_folding_actually_removes_blocks_from_a_constant_seeded_shape() {
    let mut before = 0;
    let mut after = 0;
    for seed in 0..32u64 {
        let shape = shapes::irreducible_shape(seed, 5, 2);
        let (mut module, _) = shapes::author_constant_seeded(&shape, 3);
        before += module.functions[0].blocks.len();
        crate::native::rewrites::prune_constant_cfg_module_if_changed(&mut module);
        after += module.functions.first().map_or(0, |f| f.blocks.len());
    }
    assert!(
        after * 20 <= before * 19 && after > 0,
        "constant folding took {before} blocks to {after}; it is not deciding what it should, or \
         it decided the whole function away"
    );
}

#[test]
fn constructed_shapes_are_valid_spirv() {
    let tmp = std::env::temp_dir().join("m2v_cfg_testkit_val");
    for depth in [3u32, 5] {
        for crossings in [0u32, 2] {
            for seed in 0..16u64 {
                let shape = shapes::irreducible_shape(seed, depth, crossings);
                let (mut module, _) = shapes::author_constant_seeded(&shape, 3);
                let _ = crate::native::rewrites::construct_cfg_functions_module(
                    &mut module,
                    &crate::fxhash::HashSet::default(),
                );
                let bytes = assemble(&module);
                crate::tools::spirv_val_bytes(&bytes, &tmp).unwrap_or_else(|error| {
                    panic!(
                        "construction produced invalid SPIR-V: {error}\n{}",
                        shapes::describe(&shape)
                    )
                });
            }
        }
    }
}

#[test]
fn constructed_shapes_survive_a_binary_round_trip_unchanged() {
    for depth in [3u32, 5] {
        for seed in 0..24u64 {
            let shape = shapes::irreducible_shape(seed, depth, 2);
            let (mut module, slot) = shapes::author_constant_seeded(&shape, 3);
            let _ = crate::native::rewrites::construct_cfg_functions_module(
                &mut module,
                &crate::fxhash::HashSet::default(),
            );
            let expected = interp::run_module_to_global(&module, slot, STEP_LIMIT)
                .expect("constructed module runs");

            let reloaded = crate::spirv_module::load_bytes(assemble(&module))
                .unwrap_or_else(|error| panic!("reloading: {error:?}"));
            assert_eq!(
                reloaded.disassemble(),
                module.disassemble(),
                "a binary round trip changed this module:\n{}",
                shapes::describe(&shape)
            );
            assert_eq!(
                interp::run_module_to_global(&reloaded, slot, STEP_LIMIT).expect("reloaded runs"),
                expected,
                "a binary round trip changed what this module computes:\n{}",
                shapes::describe(&shape)
            );
        }
    }
}

fn early_return_inlined_past_its_merge() -> crate::spirv_module::Module {
    let mut builder = CfgBuilder::new(1);
    let one = builder.constant(1);
    let seven = builder.constant(7);
    let hundred = builder.constant(100);

    builder.block("entry");
    let argument = builder.parameter(0);
    let low_bit = builder.bitwise_and(argument, one);
    let taken = builder.equal(low_bit, one);
    builder.selection_merge("body");
    builder.branch_conditional(taken, "early", "body");

    builder.block("early");
    builder.branch("exit");

    builder.block("body");
    let computed = builder.add(argument, hundred);
    builder.branch("exit");

    builder.block("exit");
    let result = builder.phi(&[(seven, "early"), (computed, "body")]);
    builder.return_value(result);
    builder.finish()
}

#[test]
fn a_selection_arm_that_leaves_its_construct_is_reconstructed() {
    let module = early_return_inlined_past_its_merge();
    let before = construction_error(&module).expect("the authored nesting is badly nested");
    assert!(
        before.contains("enters a construct outside its header"),
        "{before}"
    );

    let constructed = assert_construction_preserves_semantics(module, ARGUMENTS);
    assert_eq!(
        construction_error(&constructed),
        None,
        "construction left the function badly nested"
    );
}

fn construction_error(module: &crate::spirv_module::Module) -> Option<String> {
    let value_types = module
        .all_inst_iter()
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<crate::fxhash::HashMap<_, _>>();
    module.functions.iter().find_map(|function| {
        crate::native::owned_cfg::owned_function_construction_error(function, &value_types)
    })
}

fn assemble(module: &crate::spirv_module::Module) -> Vec<u8> {
    module
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect()
}

#[test]
fn staged_loop_exit_preserves_edge_values() {
    let mut b = CfgBuilder::new(3);
    let zero = b.constant(0);
    let one = b.constant(1);
    let two = b.constant(2);
    let initial = b.constant(10);
    let start = b.parameter(0);
    let limit = b.parameter(1);
    let threshold = b.parameter(2);
    let next = b.reserve_value();
    let carried = b.reserve_value();
    b.block("entry");
    let entered = b.less_than(zero, start);
    b.branch_conditional(entered, "enter", "exit");
    b.block("enter");
    b.branch("header");
    b.block("header");
    let index = b.phi(&[(zero, "enter"), (next, "step")]);
    let acc = b.phi(&[(initial, "enter"), (carried, "step")]);
    let skip = b.equal(index, one);
    b.branch_conditional(skip, "step", "choose");
    b.block("choose");
    let fast = b.less_than(index, two);
    b.branch_conditional(fast, "step", "calculate");
    b.block("calculate");
    let updated = b.add(acc, one);
    let bounded = b.less_than(updated, threshold);
    b.branch_conditional(bounded, "step", "early");
    b.block("step");
    let value = b.phi(&[(acc, "header"), (acc, "choose"), (updated, "calculate")]);
    b.add_into(carried, value, zero);
    b.add_into(next, index, one);
    let again = b.less_than(next, limit);
    b.branch_conditional(again, "header", "late");
    b.block("early");
    b.branch("early_tail");
    b.block("early_tail");
    b.branch("exit");
    b.block("late");
    b.branch("exit");
    b.block("exit");
    let final_value = b.phi(&[(zero, "entry"), (updated, "early_tail"), (value, "late")]);
    let result = b.add(final_value, one);
    b.return_value(result);
    let module = b.finish();
    assert!(
        nests(&module),
        "a staged exit must not require the whole-function dispatcher"
    );
    let cases = (0..2)
        .flat_map(|start| {
            (1..9).flat_map(move |limit| (9..19).map(move |threshold| [start, limit, threshold]))
        })
        .collect::<Vec<_>>();
    let args = cases.iter().map(|case| case.as_slice()).collect::<Vec<_>>();
    for case in &cases {
        let mut expected = 0;
        if case[0] != 0 {
            expected = 10;
            for index in 0..case[1] {
                if index < 2 {
                    continue;
                }
                expected += 1;
                if expected >= case[2] {
                    break;
                }
            }
        }
        assert_eq!(
            interp::run_module(&module, case, STEP_LIMIT).expect("authored exit"),
            Outcome::Returned(Value::Int(expected + 1))
        );
    }
    assert_construction_preserves_semantics(module, &args);
}

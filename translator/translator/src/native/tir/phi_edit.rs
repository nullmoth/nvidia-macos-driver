use super::*;
use crate::native::ir::LlValue;

fn recompute_phi_uses(inst: &mut TirInst) {
    *inst.phi_incoming_values_mut() = None;
    inst.uses = None;
}

impl TirBlock {
    pub(in crate::native) fn rebuild_phi_incomings(&mut self, keep: impl Fn(&str) -> bool) {
        for inst in &mut self.insts {
            let Some((_, incoming)) = &inst.phi_incoming() else {
                continue;
            };
            let keep_idx: Vec<usize> = incoming
                .iter()
                .enumerate()
                .filter(|(_, (_, pred))| keep(pred))
                .map(|(i, _)| i)
                .collect();
            if keep_idx.len() == incoming.len() || keep_idx.is_empty() {
                continue;
            }
            if let Some((_, incoming)) = inst.phi_incoming_mut() {
                *incoming = keep_idx.iter().map(|&i| incoming[i].clone()).collect();
            }
            recompute_phi_uses(inst);
        }
    }

    pub(in crate::native) fn push_value_phi(
        &mut self,
        result: &str,
        ty: &LlType,
        incomings: &[(LlValue, String)],
    ) {
        let mut inst = TirInst {
            result: Some(result.to_string()),
            result_ty: Some(ty.clone()),
            uses: None,
            operands: Vec::new(),
            opcode: TirOpcode::Phi,
            data: Box::new(TirInstDetails {
                fast_math: false,
                float_math_mode: None,
                payload: TirInstData::Phi {
                    parse_error: None,
                    incoming: Some((ty.clone(), incomings.to_vec())),
                    incoming_values: None,
                },
            }),
        };
        recompute_phi_uses(&mut inst);
        self.insts.push(inst);
    }

    pub(in crate::native) fn expand_phi_predecessors(
        &mut self,
        rewrites: &HashMap<String, Vec<String>>,
    ) {
        for inst in &mut self.insts {
            let Some((_, incoming)) = &inst.phi_incoming() else {
                continue;
            };
            if !incoming.iter().any(|(_, pred)| rewrites.contains_key(pred)) {
                continue;
            }
            let plan: Vec<(usize, LlValue, String)> = incoming
                .iter()
                .enumerate()
                .flat_map(|(i, (value, pred))| match rewrites.get(pred) {
                    Some(new_preds) => new_preds
                        .iter()
                        .map(|new_pred| (i, value.clone(), new_pred.clone()))
                        .collect::<Vec<_>>(),
                    None => vec![(i, value.clone(), pred.clone())],
                })
                .collect();
            if let Some((_, inc)) = inst.phi_incoming_mut() {
                *inc = plan.into_iter().map(|(_, v, p)| (v, p)).collect();
            }
            recompute_phi_uses(inst);
        }
    }

    pub(in crate::native) fn append_phi_incoming(
        &mut self,
        result: &str,
        value: LlValue,
        pred: &str,
    ) {
        for inst in &mut self.insts {
            if inst.opcode == "phi" && inst.result.as_deref() == Some(result) {
                let Some((_, incoming)) = inst.phi_incoming_mut() else {
                    return;
                };
                incoming.push((value, pred.to_string()));
                recompute_phi_uses(inst);
                return;
            }
        }
    }

    pub(in crate::native) fn set_phi_incomings(
        &mut self,
        result: &str,
        incomings: &[(LlValue, String)],
    ) {
        for inst in &mut self.insts {
            if inst.opcode == "phi" && inst.result.as_deref() == Some(result) {
                let Some((_, existing)) = inst.phi_incoming_mut() else {
                    return;
                };
                *existing = incomings.to_vec();
                recompute_phi_uses(inst);
                return;
            }
        }
    }

    pub(in crate::native) fn duplicate_phi_incoming(
        &mut self,
        from: &str,
        to: &str,
        rename: &HashMap<String, String>,
    ) {
        for inst in &mut self.insts {
            let Some((_, incoming)) = inst.phi_incoming().as_ref() else {
                continue;
            };
            let Some(new_value) = incoming
                .iter()
                .find(|(_, p)| p == from)
                .map(|(v, _)| super::rename::renamed_llvalue(v, rename))
            else {
                continue;
            };
            if let Some((_, inc)) = inst.phi_incoming_mut() {
                inc.push((new_value.clone(), to.to_string()));
            }
            recompute_phi_uses(inst);
        }
    }

    pub(in crate::native) fn mirror_region_incomings(
        &mut self,
        region: &HashSet<String>,
        rename: &HashMap<String, String>,
    ) {
        for inst in &mut self.insts {
            let Some((ty, incoming)) = inst.phi_incoming().as_ref() else {
                continue;
            };
            let ty = ty.clone();
            let mut mirrored: Vec<(LlValue, String)> = Vec::with_capacity(incoming.len());
            let mut added = false;
            for (value, pred) in incoming {
                mirrored.push((value.clone(), pred.clone()));
                if region.contains(pred) {
                    mirrored.push((
                        super::rename::renamed_llvalue(value, rename),
                        super::rename::renamed_label(pred, rename),
                    ));
                    added = true;
                }
            }
            if !added {
                continue;
            }
            *inst.phi_incoming_mut() = Some((ty, mirrored));
            recompute_phi_uses(inst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::cfg::clone_crossarm::rebuild_phi;
    use crate::native::tir::lower_block_carrier;

    fn types() -> HashMap<String, LlType> {
        HashMap::default()
    }

    #[test]
    fn rebuild_phi_incomings_matches_relowered_lines() {
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["%r = phi i32 [ %a, %p1 ], [ %b, %p2 ]", "br label %exit"],
                &["%p1"],
            ),
            (
                &[
                    "%r = phi i32 [ %a, %p1 ], [ 0, %p3 ]",
                    "%s = phi float [ %c, %p1 ], [ %d, %p3 ]",
                    "ret void",
                ],
                &["%p1"],
            ),
            (
                &["%r = phi i32 [ %a, %p1 ], [ %b, %p2 ]", "br label %x"],
                &["%p1", "%p2"],
            ),
            (
                &[
                    "%t = add i32 %a, %b",
                    "%r = phi i32 [ %t, %p1 ], [ %b, %p2 ]",
                    "br label %x",
                ],
                &["%p1"],
            ),
        ];
        for (lines, keep) in cases {
            let keep_set: HashSet<&str> = keep.iter().copied().collect();
            let src: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            let mut carrier = lower_block_carrier("%blk", &src, &types()).unwrap();
            carrier.rebuild_phi_incomings(|pred| keep_set.contains(pred));

            let rewritten: Vec<String> = src
                .iter()
                .map(|l| {
                    rebuild_phi(l, |pred| keep_set.contains(pred)).unwrap_or_else(|| l.clone())
                })
                .collect();
            let expected = lower_block_carrier("%blk", &rewritten, &types()).unwrap();
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "typed rebuild_phi_incomings diverged from re-lower for {lines:?} keep {keep:?}"
            );
        }
    }

    #[test]
    fn mirror_region_incomings_matches_relowered_lines() {
        use crate::native::cfg::clone_crossarm::mirror_region_incomings as string_mirror;
        let cases: &[(&[&str], &[&str], &[(&str, &str)])] = &[
            (
                &["%r = phi i32 [ %a, %arm ], [ %b, %o ]", "br label %x"],
                &["%arm"],
                &[("%arm", "%arm.c"), ("%a", "%a.c")],
            ),
            (
                &[
                    "%r = phi i32 [ %a, %p1 ], [ 0, %p2 ]",
                    "%s = phi float [ %c, %p1 ], [ %d, %p2 ]",
                    "ret void",
                ],
                &["%p1", "%p2"],
                &[
                    ("%p1", "%p1.c"),
                    ("%p2", "%p2.c"),
                    ("%a", "%a.c"),
                    ("%c", "%c.c"),
                    ("%d", "%d.c"),
                ],
            ),
            (
                &["%r = phi i32 [ %a, %o1 ], [ %b, %o2 ]", "br label %x"],
                &["%arm"],
                &[("%arm", "%arm.c")],
            ),
        ];
        for (lines, region, rename_pairs) in cases {
            let region_set: HashSet<String> = region.iter().map(|s| s.to_string()).collect();
            let rename: HashMap<String, String> = rename_pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
            let src: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            let mut carrier = lower_block_carrier("%blk", &src, &types()).unwrap();
            carrier.mirror_region_incomings(&region_set, &rename);

            let rewritten: Vec<String> = src
                .iter()
                .map(|l| string_mirror(l, &region_set, &rename).unwrap_or_else(|| l.clone()))
                .collect();
            let expected = lower_block_carrier("%blk", &rewritten, &types()).unwrap();
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "typed mirror_region_incomings diverged from re-lower for {lines:?} region {region:?}"
            );
        }
    }

    #[test]
    fn push_value_phi_matches_relowered_line() {
        let cases: &[(&str, LlType, &[(LlValue, &str)])] = &[
            (
                "%r = phi i32 [ %a, %p1 ], [ %m, %M ]",
                LlType::Int(32),
                &[
                    (LlValue::Local("%a".to_string()), "%p1"),
                    (LlValue::Local("%m".to_string()), "%M"),
                ],
            ),
            (
                "%r = phi i32 [ %a, %p1 ], [ 0, %p2 ]",
                LlType::Int(32),
                &[
                    (LlValue::Local("%a".to_string()), "%p1"),
                    (LlValue::Int(0), "%p2"),
                ],
            ),
            (
                "%r = phi i32 [ %a, %p1 ], [ undef, %M ]",
                LlType::Int(32),
                &[
                    (LlValue::Local("%a".to_string()), "%p1"),
                    (LlValue::Undef, "%M"),
                ],
            ),
            (
                "%r = phi i1 [ true, %p1 ], [ false, %p2 ]",
                LlType::Int(1),
                &[(LlValue::Bool(true), "%p1"), (LlValue::Bool(false), "%p2")],
            ),
        ];
        for (phi_line, ty, incomings) in cases {
            let expected = lower_block_carrier(
                "%blk",
                &[phi_line.to_string(), "br label %x".to_string()],
                &types(),
            )
            .unwrap();
            let mut carrier =
                lower_block_carrier("%blk", &["br label %x".to_string()], &types()).unwrap();
            let owned: Vec<(LlValue, String)> = incomings
                .iter()
                .map(|(v, p)| (v.clone(), p.to_string()))
                .collect();
            carrier.push_value_phi("%r", ty, &owned);
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "push_value_phi diverged from re-lower for {phi_line:?}"
            );
        }
    }

    #[test]
    fn append_phi_incoming_matches_relowered_line() {
        let cases: &[(&str, &str, LlValue, &str)] = &[
            (
                "%r = phi i32 [ %a, %p1 ]",
                "%r = phi i32 [ %a, %p1 ], [ %m, %M ]",
                LlValue::Local("%m".to_string()),
                "%M",
            ),
            (
                "%r = phi i32 [ %a, %p1 ]",
                "%r = phi i32 [ %a, %p1 ], [ 0, %M ]",
                LlValue::Int(0),
                "%M",
            ),
            (
                "%r = phi i32 [ %a, %p1 ]",
                "%r = phi i32 [ %a, %p1 ], [ undef, %M ]",
                LlValue::Undef,
                "%M",
            ),
        ];
        for (orig, extended, value, pred) in cases {
            let expected = lower_block_carrier(
                "%blk",
                &[extended.to_string(), "br label %x".to_string()],
                &types(),
            )
            .unwrap();
            let mut carrier = lower_block_carrier(
                "%blk",
                &[orig.to_string(), "br label %x".to_string()],
                &types(),
            )
            .unwrap();
            carrier.append_phi_incoming("%r", value.clone(), pred);
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "append_phi_incoming diverged from re-lower for {extended:?}"
            );
        }
    }

    #[test]
    fn set_phi_incomings_matches_relowered_line() {
        let cases: &[(&str, &str, &[(LlValue, &str)])] = &[
            (
                "%r = phi i32 [ %a, %p1 ], [ %b, %p2 ]",
                "%r = phi i32 [ %a, %p1 ], [ %m, %M ]",
                &[
                    (LlValue::Local("%a".to_string()), "%p1"),
                    (LlValue::Local("%m".to_string()), "%M"),
                ],
            ),
            (
                "%r = phi i32 [ %a, %p1 ], [ %b, %p2 ]",
                "%r = phi i32 [ %m, %M ]",
                &[(LlValue::Local("%m".to_string()), "%M")],
            ),
        ];
        for (orig, rewritten, incomings) in cases {
            let expected = lower_block_carrier(
                "%blk",
                &[rewritten.to_string(), "br label %x".to_string()],
                &types(),
            )
            .unwrap();
            let mut carrier = lower_block_carrier(
                "%blk",
                &[orig.to_string(), "br label %x".to_string()],
                &types(),
            )
            .unwrap();
            let owned: Vec<(LlValue, String)> = incomings
                .iter()
                .map(|(v, p)| (v.clone(), p.to_string()))
                .collect();
            carrier.set_phi_incomings("%r", &owned);
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "set_phi_incomings diverged from re-lower for {rewritten:?}"
            );
        }
    }

    #[test]
    fn malformed_phi_incoming_is_none() {
        let carrier = lower_block_carrier(
            "%blk",
            &[
                "%r = phi <2 x i32> [ <2 x i32> <i32 %a, i32 %b> %p1 ], [ undef, %M ]".to_string(),
                "br label %x".to_string(),
            ],
            &types(),
        )
        .unwrap();
        let phi = &carrier.insts[0];
        assert_eq!(phi.opcode, "phi");
        assert!(
            phi.phi_incoming().is_none(),
            "a malformed phi incoming must not parse to a typed incoming list"
        );
    }

    #[test]
    fn aggregate_phi_incoming_parses() {
        let carrier = lower_block_carrier(
            "%blk",
            &[
                "%r = phi <2 x i32> [ <2 x i32> <i32 %a, i32 %b>, %p1 ], [ undef, %M ]".to_string(),
                "br label %x".to_string(),
            ],
            &types(),
        )
        .unwrap();
        let phi = &carrier.insts[0];
        assert_eq!(phi.opcode, "phi");
        let (_, incoming) = phi
            .phi_incoming()
            .as_ref()
            .expect("a typed vector-constant phi incoming parses");
        assert_eq!(incoming.len(), 2);
        assert_eq!(incoming[0].1, "%p1");
        assert_eq!(incoming[1].1, "%M");
    }

    #[test]
    fn duplicate_phi_incoming_matches_relowered_line() {
        let cases: &[(&[&str], &str, &str, &[(&str, &str)], &[&str])] = &[
            (
                &["%r = phi i32 [ %a, %arm ], [ %b, %o ]", "br label %x"],
                "%arm",
                "%arm.c",
                &[("%a", "%a.c")],
                &[
                    "%r = phi i32 [ %a, %arm ], [ %b, %o ], [ %a.c, %arm.c ]",
                    "br label %x",
                ],
            ),
            (
                &["%r = phi i32 [ %a, %o1 ], [ %b, %o2 ]", "ret void"],
                "%arm",
                "%arm.c",
                &[("%arm", "%arm.c")],
                &["%r = phi i32 [ %a, %o1 ], [ %b, %o2 ]", "ret void"],
            ),
            (
                &["%r = phi i32 [ 0, %arm ], [ %b, %o ]", "br label %x"],
                "%arm",
                "%arm.c",
                &[("%arm", "%arm.c")],
                &[
                    "%r = phi i32 [ 0, %arm ], [ %b, %o ], [ 0, %arm.c ]",
                    "br label %x",
                ],
            ),
        ];
        for (lines, from, to, rename_pairs, expected_lines) in cases {
            let rename: HashMap<String, String> = rename_pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
            let src: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            let mut carrier = lower_block_carrier("%blk", &src, &types()).unwrap();
            carrier.duplicate_phi_incoming(from, to, &rename);
            let expected_src: Vec<String> = expected_lines.iter().map(|s| s.to_string()).collect();
            let expected = lower_block_carrier("%blk", &expected_src, &types()).unwrap();
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "duplicate_phi_incoming diverged from re-lower for {lines:?}"
            );
        }
    }

    #[test]
    fn expand_phi_predecessors_matches_relowered_lines() {
        let cases: &[(&[&str], &[(&str, &[&str])], &[&str])] = &[
            (
                &["%r = phi i32 [ %a, %p1 ], [ %b, %p2 ]", "br label %x"],
                &[("%p1", &["%l0", "%l1"][..])],
                &[
                    "%r = phi i32 [ %a, %l0 ], [ %a, %l1 ], [ %b, %p2 ]",
                    "br label %x",
                ],
            ),
            (
                &["%r = phi i32 [ %a, %p1 ]", "ret void"],
                &[("%zzz", &["%q"][..])],
                &["%r = phi i32 [ %a, %p1 ]", "ret void"],
            ),
            (
                &[
                    "%r = phi i32 [ %a, %p1 ], [ 0, %p2 ]",
                    "%s = phi float [ %c, %p1 ], [ %d, %p2 ]",
                    "ret void",
                ],
                &[("%p2", &["%m"][..])],
                &[
                    "%r = phi i32 [ %a, %p1 ], [ 0, %m ]",
                    "%s = phi float [ %c, %p1 ], [ %d, %m ]",
                    "ret void",
                ],
            ),
        ];
        for (lines, rewrites_pairs, expected_lines) in cases {
            let rewrites: HashMap<String, Vec<String>> = rewrites_pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect();
            let src: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            let mut carrier = lower_block_carrier("%blk", &src, &types()).unwrap();
            carrier.expand_phi_predecessors(&rewrites);
            let expected_src: Vec<String> = expected_lines.iter().map(|s| s.to_string()).collect();
            let expected = lower_block_carrier("%blk", &expected_src, &types()).unwrap();
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "expand_phi_predecessors diverged from re-lower for {lines:?}"
            );
        }
    }
}

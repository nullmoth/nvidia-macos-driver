use super::{BlockRole, BodyBlock};

pub(in crate::native) fn bb(name: &str, lines: &[&str]) -> BodyBlock {
    bb_role(name, BlockRole::Normal, lines)
}

pub(in crate::native) fn bb_role(name: &str, role: BlockRole, lines: &[&str]) -> BodyBlock {
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    BodyBlock {
        name: name.to_string(),
        role,
        typed: crate::native::tir::lower_block_carrier(
            name,
            &lines,
            &crate::fxhash::HashMap::default(),
        )
        .map(Into::into),
    }
}

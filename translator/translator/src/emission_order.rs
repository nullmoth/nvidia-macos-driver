use crate::fxhash::HashSet;
use std::hash::Hash;

pub(crate) fn dedup_in_encounter_order<T: Copy + Eq + Hash>(
    items: impl IntoIterator<Item = T>,
) -> Vec<T> {
    let mut seen = HashSet::default();
    items
        .into_iter()
        .filter(|item| seen.insert(*item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_occurrence_wins_and_repeats_are_dropped() {
        assert_eq!(
            dedup_in_encounter_order([7u32, 3, 7, 9, 3, 3, 1]),
            vec![7, 3, 9, 1]
        );
    }

    #[test]
    fn an_empty_input_collects_to_nothing() {
        assert!(dedup_in_encounter_order(std::iter::empty::<u32>()).is_empty());
    }

    #[test]
    fn the_order_is_the_inputs_order_not_a_hash_order() {
        let items: Vec<u32> = (0..64).map(|i| i * 7 + 1).collect();
        for _ in 0..64 {
            assert_eq!(dedup_in_encounter_order(items.iter().copied()), items);
        }
    }
}

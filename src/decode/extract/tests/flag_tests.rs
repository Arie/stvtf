use super::*;

#[test]
fn flags_the_game_does_not_draw_are_not_reported_unless_carried() {
    let mut x = Extractor::default();
    // pd_watergate keeps two hidden power supply flags in the teams' bases.
    x.ents.flags.insert(5, Flag { at: Placed { team: 2, ..Placed::default() }, hidden: true, ..Flag::default() });
    x.ents.flags.insert(6, Flag { points: 3, ..Flag::default() });
    let flags = |x: &Extractor| -> Vec<u32> {
        x.snapshot()
            .objectives
            .iter()
            .filter_map(|o| match o {
                out::Objective::Flag { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    };
    assert_eq!(flags(&x), [6]);
}

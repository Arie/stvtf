use super::*;

fn point(capping: u8, in_zone: u8, members: u8) -> ControlPoint {
    let mut cp = ControlPoint { capping_team: capping, team_in_zone: in_zone, ..Default::default() };
    cp.cap_time[capping as usize] = 10.0;
    cp.members[capping as usize] = members;
    cp
}

#[test]
fn cap_rate_has_diminishing_returns_per_capper() {
    assert!((point(2, 2, 1).cap_rate() - 0.1).abs() < 1e-6);
    assert!((point(2, 2, 2).cap_rate() - 0.15).abs() < 1e-6);
    assert!((point(2, 2, 3).cap_rate() - 0.1833).abs() < 1e-3);
}

#[test]
fn contested_holds_and_empty_decays() {
    assert_eq!(point(2, 3, 2).cap_rate(), 0.0);
    let mut blocked = point(2, 2, 2);
    blocked.blocked = true;
    assert_eq!(blocked.cap_rate(), 0.0);
    assert!((point(2, 0, 0).cap_rate() + 1.0 / 90.0).abs() < 1e-6);
}

#[test]
fn progress_rolls_forward_when_the_rate_changes() {
    let mut cp = point(2, 2, 1);
    // 1 s capping alone, then a second capper joins for 1 s.
    cp.advance(0, DEFAULT_TICK_INTERVAL);
    cp.advance(67, DEFAULT_TICK_INTERVAL);
    cp.members[2] = 2;
    let p = cp.progress_at(134, DEFAULT_TICK_INTERVAL);
    assert!((p - (0.1005 + 0.15 * 1.005)).abs() < 0.01, "{p}");
    assert_eq!(point(0, 0, 0).progress_at(1000, DEFAULT_TICK_INTERVAL), 0.0);
}

#[test]
fn vaccinator_resistances() {
    assert_eq!(resist(RESIST_BULLET), Resist::Bullet);
    assert_eq!(resist(RESIST_BLAST), Resist::Blast);
    assert_eq!(resist(2), Resist::Fire);
}

#[test]
fn round_states() {
    let want = [
        (0, Round::Init),
        (1, Round::Pregame),
        (2, Round::StartGame),
        (3, Round::Preround),
        (4, Round::Running),
        (5, Round::TeamWin),
        (6, Round::Restart),
        (7, Round::Stalemate),
        (8, Round::GameOver),
        (9, Round::Bonus),
        (10, Round::BetweenRounds),
    ];
    for (raw, round) in want {
        assert_eq!(round_from_raw(raw), round, "{raw}");
    }
}

#[test]
fn chat_kinds() {
    let want = [
        (ChatMessageKind::ChatAll, ChatKind::ChatAll),
        (ChatMessageKind::ChatTeam, ChatKind::ChatTeam),
        (ChatMessageKind::ChatAllDead, ChatKind::ChatAllDead),
        (ChatMessageKind::ChatTeamDead, ChatKind::ChatTeamDead),
        (ChatMessageKind::ChatAllSpec, ChatKind::ChatAllSpec),
        (ChatMessageKind::NameChange, ChatKind::NameChange),
        (ChatMessageKind::Empty, ChatKind::Empty),
    ];
    for (kind, chat) in want {
        assert_eq!(chat_kind(kind), chat);
    }
}

#[test]
fn ubers_by_condition_the_stronger_first() {
    use PlayerCondition::{
        CritBoosted, Invulnerable, MedigunUberBlastResist, MedigunUberBulletResist, MedigunUberFireResist, MegaHeal,
    };
    let words = |conds: &[PlayerCondition]| cond_words(conds.iter().map(|&c| c as usize));
    let want = [
        (Invulnerable, Ubered::Uber),
        (MegaHeal, Ubered::Quickfix),
        (MedigunUberBulletResist, Ubered::VaccinatorBullet),
        (MedigunUberBlastResist, Ubered::VaccinatorBlast),
        (MedigunUberFireResist, Ubered::VaccinatorFire),
        (CritBoosted, Ubered::Kritz),
    ];
    for (cond, uber) in want {
        assert_eq!(ubered(&words(&[cond])), Some(uber));
    }
    assert_eq!(ubered(&words(&[])), None);
    assert_eq!(ubered(&words(&[CritBoosted, Invulnerable])), Some(Ubered::Uber));
}

#[test]
fn stalemate_and_overtime_are_moments() {
    use tf_demo_parser::demo::gameevent_gen::{TeamPlayOvertimeBeginEvent, TeamPlayRoundStalemateEvent};
    let stalemate = GameEvent::TeamPlayRoundStalemate(TeamPlayRoundStalemateEvent { reason: 0 });
    let overtime = GameEvent::TeamPlayOvertimeBegin(TeamPlayOvertimeBeginEvent {});
    assert_eq!(round_event(&stalemate, 5), Some(Line::Stalemate(out::Moment { tick: 5 })));
    assert_eq!(round_event(&overtime, 6), Some(Line::Overtime(out::Moment { tick: 6 })));
}

#[test]
fn building_kinds_by_number() {
    let want = [
        (0, BuildingKind::Dispenser),
        (1, BuildingKind::Teleporter),
        (2, BuildingKind::Sentry),
        (3, BuildingKind::Sapper),
        (4, BuildingKind::Unknown),
    ];
    for (object, kind) in want {
        assert_eq!(building_kind(object), kind, "{object}");
    }
}

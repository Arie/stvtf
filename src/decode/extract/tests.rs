use super::convert::*;
use super::events::*;
use super::state::*;
use super::*;
use crate::decode::modes::Placed;
use crate::output::{ChatColor, ChatKind, ChatPart, Condition, Resist, Ubered};
use tf_demo_parser::demo::data::game_state::{PlayerClassData, PlayerCondition, ProjectileType};
use tf_demo_parser::demo::gameevent_gen::GameEvent;
use tf_demo_parser::demo::message::PacketEntitiesMessage;
use tf_demo_parser::demo::message::packetentities::{EntityId, PacketEntity, UpdateType};
use tf_demo_parser::demo::message::usermessage::{ChatMessageKind, UserMessage};
use tf_demo_parser::demo::parser::analyser::Team;

fn say_text2(kind: ChatMessageKind, from: &str, text: &str) -> Vec<Line> {
    let mut x = Extractor::default();
    x.user_message(&UserMessage::SayText2(Box::new(tf_demo_parser::demo::message::usermessage::SayText2Message {
        client: EntityId::from(5u32),
        raw: 0,
        kind,
        from: Some(from.into()),
        text: text.into(),
    })));
    x.drain()
}

#[test]
fn a_chat_line_keeps_its_text() {
    let lines = say_text2(ChatMessageKind::ChatAll, "Korra", "xddxdx");
    assert!(matches!(&lines[..], [Line::Chat(c)] if c.text == "xddxdx"), "{lines:?}");
}

// #TF_Halloween_Underworld and the like name a player but have no text; they are not chat.
#[test]
fn a_game_message_without_text_is_no_chat_line() {
    assert_eq!(say_text2(ChatMessageKind::ChatAll, "Korra", ""), vec![]);
}

#[test]
fn strips_chat_colors() {
    assert_eq!(
        strip_colors("\u{1}\u{1}[STV Stats] \u{7}99CCFFBLU\u{1}'s medic DROPPED!\n"),
        "[STV Stats] BLU's medic DROPPED!"
    );
    assert_eq!(strip_colors("\u{8}FF0000FFred"), "red");
    assert_eq!(strip_colors("plain"), "plain");
}

#[test]
fn chat_colour_codes_become_spans() {
    let part = |color, text: &str| ChatPart { color, text: text.to_string() };
    let parts = color_parts("\u{1}\u{1}[STV Stats] \u{7}99ccffBLU\u{1}'s medic \u{3}DROPPED!\n").unwrap();
    assert_eq!(
        parts,
        [
            part(None, "[STV Stats] "),
            part(Some(ChatColor::Rgb([0x99, 0xCC, 0xFF])), "BLU"),
            part(None, "'s medic "),
            part(Some(ChatColor::Team), "DROPPED!")
        ]
    );
    assert!(color_parts("plain text").is_none());
    assert_eq!(color_parts("\u{8}FF0000AAred").unwrap(), [part(Some(ChatColor::Rgb([0xFF, 0, 0])), "red")]);
}

#[test]
fn team_numbers_out_of_range_are_no_team() {
    assert_eq!(team_num(2u8), out::Team::Red);
    assert_eq!(team_num(3u16), out::Team::Blue);
    assert_eq!(team_num(258u16), out::Team::Unassigned, "not truncated to red");
}

#[test]
fn a_reused_entity_slot_forgets_the_medic_charge() {
    let mut x = Extractor::default();
    x.ents.medic_charge.insert(5, Charge { charge: 100, releasing: true });
    x.forget_entity(5);
    assert!(x.ents.medic_charge.is_empty());
}

mod cond_tests {
    use super::*;

    fn words(conds: &[u8]) -> [u32; 6] {
        let mut w = [0u32; 6];
        for &c in conds {
            let n = c as usize;
            if n < 32 { w[0] |= 1 << n } else { w[n / 32 + 1] |= 1 << (n % 32) }
        }
        w
    }

    #[test]
    fn conditions_decode_by_bit() {
        let names = |w: &[u32; 6]| conditions(w).into_iter().map(Condition::name).collect::<Vec<_>>();
        assert_eq!(
            names(&words(&[1, 4, 32, 64, 130])),
            ["Zoomed", "Stealthed", "SpeedBoost", "StealthedUserBuff", "ImmuneToPushback"]
        );
        // m_nPlayerCond and _condition_bits both carry conditions 0-31.
        let mut w = [0u32; 6];
        w[1] = 1 << 5;
        assert_eq!(names(&w), ["Invulnerable"]);
    }

    #[test]
    fn uber_kinds_from_conditions() {
        assert_eq!(ubered(&words(&[5])), Some(Ubered::Uber));
        assert_eq!(ubered(&words(&[5, 8])), Some(Ubered::Uber));
        assert_eq!(ubered(&words(&[8])), None, "a leftover fade-out flag is not an über");
        assert_eq!(ubered(&words(&[11])), Some(Ubered::Kritz));
        assert_eq!(ubered(&words(&[28])), Some(Ubered::Quickfix));
        assert_eq!(ubered(&words(&[59])), Some(Ubered::VaccinatorBlast));
        assert_eq!(ubered(&words(&[23, 21])), None, "overheal is not an uber");
    }

    #[test]
    fn projectile_kinds() {
        let kind = projectile_kind;
        assert_eq!(kind(ProjectileType::Sticky, "CTFGrenadePipebombProjectile"), ProjectileKind::Sticky);
        assert_eq!(kind(ProjectileType::Unknown, "CTFProjectile_Arrow"), ProjectileKind::Arrow);
        assert_eq!(kind(ProjectileType::Unknown, "CTFProjectile_JarMilk"), ProjectileKind::MadMilk);
        assert_eq!(kind(ProjectileType::Unknown, "CTFProjectile_SpellBats"), ProjectileKind::Unknown);
    }

    #[test]
    fn condition_names_match_the_parser() {
        for n in 0..=PlayerCondition::MAX as u8 {
            let expected = PlayerCondition::try_from(n).map(|c| format!("{c:?}")).ok();
            assert_eq!(Condition::from_number(n).map(|c| c.name().to_string()), expected, "condition {n}");
        }
    }

    #[test]
    fn medigun_kinds() {
        assert_eq!(medigun_kind(29), MedigunKind::Medigun);
        assert_eq!(medigun_kind(211), MedigunKind::Medigun);
        assert_eq!(medigun_kind(35), MedigunKind::Kritzkrieg);
        assert_eq!(medigun_kind(411), MedigunKind::Quickfix);
        assert_eq!(medigun_kind(998), MedigunKind::Vaccinator);
    }
}

mod koth_tests {
    use super::*;

    #[test]
    fn koth_shows_each_teams_timer_and_not_old_ones() {
        let mut x = Extractor::default();
        let shown = RoundTimer { show: true, remaining: 180.0, paused: true, ..Default::default() };
        for idx in [5, 6, 7] {
            x.ents.timers.insert(idx, shown.clone());
        }
        x.koth_timers = [Some(6), Some(5)];
        let teams: Vec<out::Team> = x.snapshot().timers.iter().map(|t| t.team).collect();
        assert_eq!(teams, [out::Team::Blue, out::Team::Red]);
        x.koth_timers = [None; 2];
        assert_eq!(x.snapshot().timers.len(), 3);
    }
}

mod stopwatch_tests {
    use super::*;
    use crate::output::StopwatchState;

    /// Stopwatch with BLU (3 points) attacking RED (`red` points), its timer entity 9.
    fn stopwatch(state: i64, timer: &RoundTimer, red: i64) -> Option<out::Stopwatch> {
        let mut x = Extractor {
            round_state: Round::Running,
            stopwatch: StopwatchRules { on: true, state, timer: Some(9) },
            ..Default::default()
        };
        x.ents.teams.insert(1, TeamScore { team: 2, score: red, role: 1, ..Default::default() });
        x.ents.teams.insert(2, TeamScore { team: 3, score: 3, role: 2, ..Default::default() });
        x.ents.timers.insert(9, RoundTimer { stopwatch: true, ..timer.clone() });
        x.snapshot().stopwatch
    }

    #[test]
    fn setting_the_time_shows_it_as_of_the_last_capture() {
        let timer = RoundTimer { watching_caps: true, total: 92.84, end_time: 1.0, ..Default::default() };
        let sw = stopwatch(1, &timer, 0).unwrap();
        assert_eq!((sw.attackers, sw.setting, sw.state), (out::Team::Blue, true, StopwatchState::Running));
        assert_eq!((sw.time, sw.paused, sw.points), (92.8, true, 3), "the attackers' points");
    }

    #[test]
    fn beating_it_counts_down_with_the_points_still_to_get() {
        let timer = RoundTimer { end_time: 100.0, ..Default::default() };
        let sw = stopwatch(1, &timer, 4).unwrap();
        assert_eq!((sw.setting, sw.time, sw.paused, sw.points), (false, 100.0, false, 1));
        let sw = stopwatch(2, &RoundTimer { paused: true, ..timer }, 4).unwrap();
        assert_eq!((sw.state, sw.paused, sw.points), (StopwatchState::Overtime, true, 2), "one more than the time");
    }

    #[test]
    fn no_stopwatch_without_one_or_both_roles() {
        assert!(stopwatch(0, &RoundTimer::default(), 0).is_some());
        let mut x = Extractor {
            round_state: Round::Running,
            stopwatch: StopwatchRules { on: true, ..Default::default() },
            ..Default::default()
        };
        x.ents.teams.insert(2, TeamScore { team: 3, role: 2, ..Default::default() });
        assert_eq!(x.snapshot().stopwatch, None, "no defenders");
        x.ents.teams.insert(1, TeamScore { team: 2, role: 1, ..Default::default() });
        assert!(x.snapshot().stopwatch.is_some());
        x.round_state = Round::GameOver;
        assert_eq!(x.snapshot().stopwatch, None, "game over");
        x.round_state = Round::Running;
        x.stopwatch.on = false;
        assert_eq!(x.snapshot().stopwatch, None);
    }
}

mod flag_tests {
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
}

mod frame_tests {
    use super::*;
    use tf_demo_parser::demo::data::ServerTick;
    use tf_demo_parser::demo::message::packetentities::{BaselineIndex, EntityId};
    use tf_demo_parser::demo::packet::datatable::{ClassId, ServerClassName};

    fn update(id: u32, update_type: UpdateType) -> PacketEntity {
        PacketEntity {
            server_class: ClassId::from(1u16),
            entity_index: EntityId::from(id),
            props: Vec::new(),
            in_pvs: true,
            update_type,
            serial_number: 0,
            delay: None,
            delta: None,
            baseline_index: BaselineIndex::First,
        }
    }

    fn frame(x: &mut Extractor, tick: u32, delta: Option<u32>, entities: Vec<PacketEntity>) {
        x.server_tick = tick;
        for e in &entities {
            if e.update_type == UpdateType::Enter {
                let id = e.entity_index;
                x.gs.state.projectiles.entry(id).or_insert_with(|| {
                    Projectile::new(id, ClassId::from(1u16), &ServerClassName::from("CTFProjectile_Rocket"))
                });
            }
        }
        let pe = PacketEntitiesMessage {
            entities,
            removed_entities: Vec::new(),
            max_entries: 0,
            delta: delta.map(ServerTick::from),
            base_line: BaselineIndex::First,
            updated_base_line: false,
        };
        x.track_frame(&pe);
    }

    fn shown(x: &Extractor) -> Vec<u32> {
        let mut ids: Vec<u32> = x.gs.state.projectiles.keys().map(|k| u32::from(*k)).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn a_projectile_that_came_and_went_since_the_base_frame_is_gone() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![update(10, UpdateType::Enter)]);
        frame(&mut x, 101, Some(100), vec![update(11, UpdateType::Enter)]);
        frame(&mut x, 102, Some(100), vec![update(11, UpdateType::Enter)]);
        assert_eq!(shown(&x), [10, 11]);
        // Fired after frame 100 and exploded: no longer in the deltas from 100, and never deleted.
        frame(&mut x, 103, Some(100), vec![]);
        assert_eq!(shown(&x), [10]);
    }

    #[test]
    fn deletes_and_deltas_from_the_previous_frame() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![update(10, UpdateType::Enter), update(11, UpdateType::Enter)]);
        frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Delta)]);
        frame(&mut x, 102, Some(101), vec![update(11, UpdateType::Delete)]);
        assert_eq!(shown(&x), [10]);
        frame(&mut x, 103, Some(102), vec![]);
        assert_eq!(shown(&x), [10]);
    }

    #[test]
    fn game_mode_entities_and_flags_that_went_since_the_base_frame_are_gone() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![]);
        // A robot destruction core and a player destruction bottle, dropped and picked up since 100.
        x.modes.entity("CBonusPack", 12, std::iter::empty());
        x.ents.flags.insert(13, Flag::default());
        frame(&mut x, 101, Some(100), vec![update(12, UpdateType::Delta), update(13, UpdateType::Delta)]);
        assert_eq!(x.modes.objectives(&|_| None).len(), 1);
        frame(&mut x, 102, Some(100), vec![]);
        assert!(x.modes.objectives(&|_| None).is_empty() && x.ents.flags.is_empty());
    }

    #[test]
    fn without_the_base_frame_nothing_is_dropped() {
        let mut x = Extractor::default();
        frame(&mut x, 101, Some(100), vec![update(10, UpdateType::Enter)]);
        frame(&mut x, 102, Some(100), vec![]);
        assert_eq!(shown(&x), [10]);
    }

    #[test]
    fn tracking_picks_up_again_after_a_lost_base_frame() {
        let mut x = Extractor::default();
        frame(&mut x, 100, None, vec![update(10, UpdateType::Enter)]);
        // Frame 101 was lost; 102 deltas from it, 103 from 102.
        frame(&mut x, 102, Some(101), vec![update(11, UpdateType::Enter)]);
        assert_eq!(shown(&x), [10, 11]);
        frame(&mut x, 103, Some(102), vec![update(12, UpdateType::Enter)]);
        // 12 came and went since 102: frame 102 was recorded, so the delta from it drops 12.
        frame(&mut x, 104, Some(102), vec![]);
        assert_eq!(shown(&x), [10, 11]);
    }
}

mod medic_tests {
    use super::*;
    use tf_demo_parser::demo::data::game_state::Player;
    use tf_demo_parser::demo::message::packetentities::EntityId;
    use tf_demo_parser::demo::parser::analyser::UserInfo;

    fn with_medic(charge: u8, releasing: bool) -> Extractor {
        let mut x = Extractor::default();
        let mut p = Player::default();
        p.entity = EntityId::from(5u32);
        p.class = Class::Medic;
        p.state = PlayerState::Alive;
        p.class_data = PlayerClassData::default_for_class(Class::Medic);
        set_charge(&mut p, charge);
        p.info = Some(UserInfo {
            classes: tf_demo_parser::demo::parser::analyser::ClassList::default(),
            name: "medic".into(),
            user_id: 7u16.into(),
            steam_id: "BOT".into(),
            entity_id: EntityId::from(5u32),
            team: Team::Red,
        });
        x.gs.state.players.push(p);
        x.ents.mediguns.insert(9, Medigun { owner: Some(5), item: 35, releasing, ..Default::default() });
        let mut held = [None; MY_WEAPON_SLOTS];
        held[1] = Some(9);
        x.ents.my_weapons.insert(5, held);
        x
    }

    fn set_charge(p: &mut Player, to: u8) {
        if let PlayerClassData::Medic { charge, .. } = &mut p.class_data {
            *charge = to;
        }
    }

    fn step(x: &mut Extractor, charge: u8, releasing: bool) -> Vec<Line> {
        set_charge(&mut x.gs.state.players[0], charge);
        x.ents.mediguns.get_mut(&9).unwrap().releasing = releasing;
        x.medic_changes();
        x.drain()
    }

    #[test]
    fn the_weapon_list_props_are_known() {
        assert!(PROP_MY_WEAPONS.iter().all(|id| id.names().is_some()));
    }

    #[test]
    fn the_medigun_is_the_one_in_the_weapon_list() {
        let mut x = with_medic(0, false);
        x.ents.mediguns.insert(4, Medigun { owner: Some(5), item: 998, ..Default::default() });
        assert_eq!(x.medigun_of(5).map(|m| m.kind), Some(MedigunKind::Kritzkrieg));
        x.ents.my_weapons.get_mut(&5).unwrap()[1] = Some(4);
        assert_eq!(x.medigun_of(5).map(|m| m.kind), Some(MedigunKind::Vaccinator));
    }

    #[test]
    fn ready_once_when_full_and_ended_when_the_charge_runs_out() {
        let mut x = with_medic(90, false);
        let charge = out::MedicCharge { tick: 0, medic: PlayerId(7), medigun: Some(MedigunKind::Kritzkrieg) };
        assert!(step(&mut x, 95, false).is_empty(), "the first sighting only records the state");
        assert_eq!(step(&mut x, 100, false), [Line::UberReady(charge.clone())]);
        assert!(step(&mut x, 100, false).is_empty(), "ready only once");
        assert!(step(&mut x, 98, true).is_empty(), "deploying is the game's own uber event");
        assert_eq!(step(&mut x, 0, false), [Line::UberEnded(charge)]);
    }
}

mod sentry_tests {
    use super::*;

    #[test]
    fn aim_is_the_placement_turned_by_the_yaw_pose() {
        let s = |yaw, pose| Sentry { yaw, pose: [0.5, pose], ..Default::default() };
        assert_eq!(s(255.6, 0.5).aim(), 255.6);
        // Sampled while firing on a target 49 degrees left of the placement direction.
        assert!((s(255.6, 0.365).aim() - (255.6 + 48.6)).abs() < 0.1);
        assert!((s(10.0, 0.75).aim() - 280.0).abs() < 1e-3, "wraps around");
    }
}

mod timer_tests {
    use super::*;

    fn at(x: &mut Extractor, state: i64, transition: f32, seconds: f32) -> Option<f32> {
        x.round_state = round_from_raw(state);
        x.state_transition = transition;
        x.server_tick = (seconds / DEFAULT_TICK_INTERVAL).round() as u32;
        x.track_round_time();
        x.round_time().map(|t| (t * 10.0).round() / 10.0)
    }

    #[test]
    fn time_left_counts_down_from_the_match_start() {
        let mut x = Extractor::default();
        assert_eq!(x.time_left(), None, "no mp_timelimit");
        x.time_limit = Some(30.0);
        x.match_start = 200.0;
        x.server_tick = (500.0 / DEFAULT_TICK_INTERVAL) as u32;
        assert!((x.time_left().unwrap() - 1500.0).abs() < 0.1, "{:?}", x.time_left());
        x.server_tick = (3000.0 / DEFAULT_TICK_INTERVAL) as u32;
        assert_eq!(x.time_left(), Some(0.0));
    }

    #[test]
    fn round_time_runs_while_live_stops_at_the_win_and_resets() {
        let mut x = Extractor::default();
        assert_eq!(at(&mut x, 4, 100.0, 120.0), Some(20.0));
        assert_eq!(at(&mut x, 5, 160.0, 135.0), Some(35.0), "round won at 135 s");
        assert_eq!(at(&mut x, 5, 160.0, 150.0), Some(35.0), "frozen while the winners celebrate");
        assert_eq!(at(&mut x, 3, 170.0, 165.0), None, "pre-round of the next round");
        assert_eq!(at(&mut x, 4, 170.0, 180.0), Some(10.0));
    }

    #[test]
    fn timer_counts_down_in_server_time_unless_paused() {
        let mut t = RoundTimer { end_time: 100.0, remaining: 42.0, ..Default::default() };
        let at = |t: &RoundTimer, tick| t.remaining_at(tick, DEFAULT_TICK_INTERVAL);
        assert!((at(&t, 4000) - 40.0).abs() < 1e-3, "tick 4000 is 60 s of server time");
        assert_eq!(at(&t, 9000), 0.0);
        t.paused = true;
        assert_eq!(at(&t, 4000), 42.0);
    }
}

mod cap_tests {
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
        let words = |conds: &[PlayerCondition]| {
            let mut w = [0; COND_WORDS];
            for c in conds {
                let n = *c as usize;
                let word = if n < COND_BITS { 0 } else { n / COND_BITS + 1 };
                w[word] |= 1 << (n % COND_BITS);
            }
            w
        };
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
}

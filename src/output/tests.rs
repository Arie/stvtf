use super::*;
use serde_json::{Value, json};

fn value(line: &Line) -> Value {
    serde_json::to_value(line).unwrap()
}

fn back(v: &Value) -> Line {
    serde_json::from_value(v.clone()).unwrap()
}

#[test]
fn a_kill_is_written_as_documented() {
    let kill = Line::Kill(Kill {
        tick: 7,
        victim: PlayerId(3),
        attacker: Some(PlayerId(4)),
        assister: None,
        weapon: "tf_projectile_rocket".into(),
        crit: true,
        headshot: false,
        backstab: false,
        suicide: false,
        dominated: false,
        revenge: false,
        feign: false,
        airborne: true,
        victim_pos: Some([1.5, 2.0, -3.0]),
        attacker_pos: None,
    });
    let expected = json!({
        "type": "kill", "tick": 7, "victim": 3, "attacker": 4, "assister": null,
        "weapon": "tf_projectile_rocket", "crit": true, "headshot": false, "backstab": false,
        "suicide": false, "dominated": false, "revenge": false, "feign": false, "airborne": true,
        "victim_pos": [1.5, 2.0, -3.0], "attacker_pos": null,
    });
    assert_eq!(value(&kill), expected);
    assert_eq!(back(&expected), kill);
}

#[test]
fn statuses_are_tagged_by_state() {
    let connecting = Line::Status(Status::Connecting {
        server: "1.2.3.4:27020".into(),
        build: "11138802".into(),
        server_build: None,
    });
    let expected = json!({"type": "status", "state": "connecting", "server": "1.2.3.4:27020", "build": "11138802"});
    assert_eq!(value(&connecting), expected);
    assert_eq!(back(&expected), connecting);
    assert_eq!(value(&Line::Status(Status::MapChange)), json!({"type": "status", "state": "map_change"}));
    assert_eq!(value(&Line::Status(Status::Prespawn)), json!({"type": "status", "state": "prespawn"}));
    assert_eq!(Line::Status(Status::New).tick(), None);
    let relay = Line::Status(Status::Relay { listen: "0.0.0.0:27121".into() });
    assert_eq!(value(&relay), json!({"type": "status", "state": "relay", "listen": "0.0.0.0:27121"}));
}

#[expect(clippy::option_option, reason = "the field's own type")]
fn sentry(target: Option<Option<PlayerId>>) -> Building {
    Building {
        id: 492,
        kind: BuildingKind::Sentry,
        team: Team::Blue,
        level: 1,
        hp: 150,
        max_hp: 150,
        pos: [0.0, 0.0, 0.0],
        yaw: 90.0,
        builder: Some(PlayerId(5)),
        sapped: false,
        entrance: None,
        mini: Some(false),
        building: Some(false),
        aim: None,
        target,
        attacking: None,
    }
}

#[test]
fn a_sentry_without_a_target_differs_from_one_not_seen_aiming() {
    let idle = serde_json::to_value(sentry(Some(None))).unwrap();
    assert_eq!(idle["target"], Value::Null);
    assert!(idle.as_object().unwrap().contains_key("target"));
    assert_eq!(serde_json::from_value::<Building>(idle).unwrap().target, Some(None));

    let unseen = serde_json::to_value(sentry(None)).unwrap();
    assert!(!unseen.as_object().unwrap().contains_key("target"));
    assert_eq!(serde_json::from_value::<Building>(unseen).unwrap().target, None);

    let aiming = serde_json::to_value(sentry(Some(Some(PlayerId(9))))).unwrap();
    assert_eq!(serde_json::from_value::<Building>(aiming).unwrap().target, Some(Some(PlayerId(9))));
}

#[test]
fn conditions_go_by_name() {
    assert_eq!(Condition::from_number(5).map(Condition::name), Some("Invulnerable"));
    assert_eq!(Condition::from_number(89), None, "unused number");
    assert_eq!(Condition::from_number(200), None);
    let c = Condition::from_number(130).unwrap();
    assert_eq!(c.number(), 130);
    assert_eq!(c.to_string(), "ImmuneToPushback");
    assert_eq!(serde_json::to_value(c).unwrap(), json!("ImmuneToPushback"));
    assert_eq!(serde_json::from_value::<Condition>(json!("ImmuneToPushback")).unwrap(), c);
    assert!(serde_json::from_value::<Condition>(json!("Hovering")).is_err());
}

#[test]
fn stamped_lines_carry_the_receive_time_when_live() {
    let airblast = Line::Airblast(Airblast { tick: 9, player: PlayerId(2) });
    let live = Stamped { line: airblast.clone(), time: Some(1_700_000_000_000) };
    let expected = json!({"type": "airblast", "tick": 9, "player": 2, "time": 1_700_000_000_000u64});
    assert_eq!(serde_json::to_value(&live).unwrap(), expected);
    assert_eq!(serde_json::from_value::<Stamped>(expected).unwrap(), live);

    let offline = Stamped { line: airblast, time: None };
    let expected = json!({"type": "airblast", "tick": 9, "player": 2});
    assert_eq!(serde_json::to_value(&offline).unwrap(), expected);
    assert_eq!(serde_json::from_value::<Stamped>(expected).unwrap(), offline);
}

#[test]
fn lines_name_their_players_and_tick() {
    let cap = Line::PointCaptured(PointCaptured {
        tick: 40,
        point: 1,
        name: "#cp_mid".into(),
        team: Team::Red,
        cappers: vec![Some(PlayerId(3)), None, Some(PlayerId(8))],
    });
    assert_eq!(cap.players().collect::<Vec<_>>(), [PlayerId(3), PlayerId(8)]);
    assert_eq!(cap.tick(), Some(40));
    let medic = Line::MedicDeath(MedicDeath { tick: 1, medic: PlayerId(2), attacker: None, healing: 300, drop: true });
    assert_eq!(medic.players().collect::<Vec<_>>(), [PlayerId(2)]);
    let killed = Line::KilledCapper(KilledCapper {
        tick: 1,
        point: 0,
        killer: PlayerId(4),
        victim: PlayerId(5),
        assister: Some(PlayerId(6)),
    });
    assert_eq!(killed.players().collect::<Vec<_>>(), [PlayerId(4), PlayerId(5), PlayerId(6)]);
    assert_eq!(Line::Airblast(Airblast { tick: 1, player: PlayerId(7) }).players().collect::<Vec<_>>(), [PlayerId(7)]);
    assert_eq!(Line::Mark(Mark { tick: 1, pos: [0.0; 3] }).players().count(), 0);
}

#[test]
fn chat_colours_are_written_as_hex_or_team() {
    let parts = [
        ChatPart { color: None, text: "[STV] ".into() },
        ChatPart { color: Some(ChatColor::Rgb([0x99, 0xCC, 0x0F])), text: "BLU".into() },
        ChatPart { color: Some(ChatColor::Team), text: "won".into() },
    ];
    let expected = json!([{"c": null, "t": "[STV] "}, {"c": "99CC0F", "t": "BLU"}, {"c": "team", "t": "won"}]);
    assert_eq!(s(&parts), expected);
    assert_eq!(serde_json::from_value::<Vec<ChatPart>>(expected).unwrap(), parts);
    for bad in ["99cc0f", "99CC0", "GGGGGG", "red"] {
        assert!(serde_json::from_value::<ChatPart>(json!({"c": bad, "t": ""})).is_err(), "{bad}");
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one case per line type")]
fn game_mode_lines_are_written_as_documented() {
    let p = PlayerId;
    let cases = [
        (Line::PassGet(PassGet { tick: 1, player: p(3) }), json!({"type": "pass_get", "tick": 1, "player": 3})),
        (
            Line::PassFree(PassFree { tick: 1, player: p(3), attacker: None }),
            json!({"type": "pass_free", "tick": 1, "player": 3, "attacker": null}),
        ),
        (
            Line::PassCaught(PassCaught { tick: 1, passer: p(3), catcher: p(4), dist: 54.3, duration: 1.48 }),
            json!({"type": "pass_caught", "tick": 1, "passer": 3, "catcher": 4, "dist": 54.3, "duration": 1.48}),
        ),
        (
            Line::PassScore(PassScore { tick: 1, scorer: p(3), assister: Some(p(4)), points: 1 }),
            json!({"type": "pass_score", "tick": 1, "scorer": 3, "assister": 4, "points": 1}),
        ),
        (
            Line::PassStolen(PassStolen { tick: 1, victim: p(3), attacker: p(4) }),
            json!({"type": "pass_stolen", "tick": 1, "victim": 3, "attacker": 4}),
        ),
        (
            Line::PassBlocked(PassBlocked { tick: 1, owner: p(3), blocker: p(4) }),
            json!({"type": "pass_blocked", "tick": 1, "owner": 3, "blocker": 4}),
        ),
        (
            Line::BallCarrierKilled(BallCarrierKilled { tick: 1, attacker: Some(p(3)), victim: p(4), assister: None }),
            json!({"type": "ball_carrier_killed", "tick": 1, "attacker": 3, "victim": 4, "assister": null}),
        ),
        (
            Line::RobotKilled(RobotKilled { tick: 1, attacker: Some(p(3)), weapon: "solemn_vow".into() }),
            json!({"type": "robot_killed", "tick": 1, "attacker": 3, "weapon": "solemn_vow"}),
        ),
        (
            Line::PointsScored(PointsScored { tick: 1, player: p(3), method: ScoreMethod::ReactorStolen, amount: -5 }),
            json!({"type": "points_scored", "tick": 1, "player": 3, "method": "reactor_stolen", "amount": -5}),
        ),
        (
            Line::SpecialScore(PlayerMoment { tick: 1, player: p(3) }),
            json!({"type": "special_score", "tick": 1, "player": 3}),
        ),
        (
            Line::TeamLeaderKilled(TeamLeaderKilled { tick: 1, killer: Some(p(3)), victim: p(4) }),
            json!({"type": "team_leader_killed", "tick": 1, "killer": 3, "victim": 4}),
        ),
        (
            Line::RocketOpen(RocketOpen { tick: 1, team: Team::Red }),
            json!({"type": "rocket_open", "tick": 1, "team": "red"}),
        ),
        (
            Line::RoundSelected(RoundSelected { tick: 1, round: "round_a".into() }),
            json!({"type": "round_selected", "tick": 1, "round": "round_a"}),
        ),
        (
            Line::Teleported(Teleported { tick: 1, player: p(3), builder: Some(p(4)), dist: 2707.7 }),
            json!({"type": "teleported", "tick": 1, "player": 3, "builder": 4, "dist": 2707.7}),
        ),
        (
            Line::Stun(Stun { tick: 1, stunner: Some(p(3)), victim: p(4), capping: false, big: true }),
            json!({"type": "stun", "tick": 1, "stunner": 3, "victim": 4, "capping": false, "big": true}),
        ),
        (
            Line::Sapped(Sapped { tick: 1, player: p(3), owner: p(4), object: BuildingKind::Sentry, sapper: 1096 }),
            json!({"type": "sapped", "tick": 1, "player": 3, "owner": 4, "object": "sentry", "sapper": 1096}),
        ),
        (
            Line::BuildingRemoved(ObjectChange { tick: 1, player: p(3), object: BuildingKind::Teleporter, id: 165 }),
            json!({"type": "building_removed", "tick": 1, "player": 3, "object": "teleporter", "id": 165}),
        ),
        (
            Line::BuildingDetonated(ObjectChange { tick: 1, player: p(3), object: BuildingKind::Dispenser, id: 476 }),
            json!({"type": "building_detonated", "tick": 1, "player": 3, "object": "dispenser", "id": 476}),
        ),
        (
            Line::DetStickies(PlayerMoment { tick: 1, player: p(3) }),
            json!({"type": "det_stickies", "tick": 1, "player": 3}),
        ),
        (
            Line::CaptureStarted(CaptureStarted {
                tick: 1,
                point: 0,
                name: "#Arena_cap".into(),
                team: Team::Red,
                owner: Team::Unassigned,
                cappers: vec![Some(p(3)), None],
            }),
            json!({"type": "capture_started", "tick": 1, "point": 0, "name": "#Arena_cap", "team": "red",
                   "owner": "none", "cappers": [3, null]}),
        ),
        (
            Line::PointLocked(PointLocked { tick: 1, point: 0, name: "#cap".into(), locked: true }),
            json!({"type": "point_locked", "tick": 1, "point": 0, "name": "#cap", "locked": true}),
        ),
        (
            Line::NpcDamage(NpcDamage {
                tick: 1,
                id: 154,
                attacker: Some(p(3)),
                amount: 22,
                health: 77,
                crit: false,
                weapon_id: 58,
                boss: 0,
            }),
            json!({"type": "npc_damage", "tick": 1, "id": 154, "attacker": 3, "amount": 22, "health": 77,
                   "crit": false, "weapon_id": 58, "boss": 0}),
        ),
        (
            Line::HudText(HudText {
                tick: 1,
                channel: 2,
                pos: [0.65, 0.82],
                color: ChatColor::Rgb([0xFF, 0x55, 0x42]),
                hold: 1.0,
                text: "0%".into(),
            }),
            json!({"type": "hud_text", "tick": 1, "channel": 2, "pos": [0.65, 0.82], "color": "FF5542",
                   "hold": 1.0, "text": "0%"}),
        ),
        (
            Line::Hint(Hint { tick: 1, text: "Votes: 0/8".into() }),
            json!({"type": "hint", "tick": 1, "text": "Votes: 0/8"}),
        ),
        (
            Line::VoteStarted(VoteStarted {
                tick: 1,
                team: Team::Unassigned,
                caller: Some(p(3)),
                issue: "#TF_vote_nextlevel".into(),
                details: "pl_goldrush".into(),
                yes_no: true,
                target: None,
            }),
            json!({"type": "vote_started", "tick": 1, "team": "none", "caller": 3,
                   "issue": "#TF_vote_nextlevel", "details": "pl_goldrush", "yes_no": true, "target": null}),
        ),
        (
            Line::VotePassed(VotePassed {
                tick: 1,
                team: Team::Unassigned,
                issue: "#TF_vote_passed_nextlevel".into(),
                details: "pl_goldrush".into(),
            }),
            json!({"type": "vote_passed", "tick": 1, "team": "none", "issue": "#TF_vote_passed_nextlevel",
                   "details": "pl_goldrush"}),
        ),
        (
            Line::VoteFailed(VoteFailed { tick: 1, team: Team::Red, reason: 3 }),
            json!({"type": "vote_failed", "tick": 1, "team": "red", "reason": 3}),
        ),
        (
            Line::Upgrade(Upgrade { tick: 1, player: p(3), item: 30015, attribute: 327, cost: 50 }),
            json!({"type": "upgrade", "tick": 1, "player": 3, "item": 30015, "attribute": 327, "cost": 50}),
        ),
        (
            Line::BlastJump(BlastJump { tick: 1, player: p(3), kind: JumpKind::RocketPack, phase: JumpPhase::Start }),
            json!({"type": "blast_jump", "tick": 1, "player": 3, "kind": "rocket_pack", "phase": "start"}),
        ),
    ];
    for (line, expected) in cases {
        assert_eq!(value(&line), expected);
        assert_eq!(back(&expected), line);
        assert_eq!(line.tick(), Some(1));
    }
}

#[test]
fn game_mode_objectives_and_scores_are_written_as_documented() {
    let objectives = [
        Objective::Ball { id: 9, pos: [1.0, 2.0, 3.0], team: Team::Blue, carrier: Some(PlayerId(4)) },
        Objective::Goal { id: 10, pos: [0.0, -1758.1, -416.5], team: Team::Blue },
        Objective::Robot { id: 11, pos: [0.0; 3], team: Team::Red, hp: 70, max_hp: 330 },
        Objective::Tank { id: 12, pos: [0.0; 3], health: 0.99 },
        Objective::Core { id: 13, pos: [0.0; 3], team: Team::Blue },
        Objective::Revive { id: 14, pos: [0.0; 3], owner: Some(PlayerId(5)) },
        Objective::Money { id: 15, pos: [0.0; 3] },
    ];
    let expected = json!([
        {"kind": "ball", "id": 9, "pos": [1.0, 2.0, 3.0], "team": "blue", "carrier": 4},
        {"kind": "goal", "id": 10, "pos": [0.0, -1758.1, -416.5], "team": "blue"},
        {"kind": "robot", "id": 11, "pos": [0.0, 0.0, 0.0], "team": "red", "hp": 70, "max_hp": 330},
        {"kind": "tank", "id": 12, "pos": [0.0, 0.0, 0.0], "health": 0.99},
        {"kind": "core", "id": 13, "pos": [0.0, 0.0, 0.0], "team": "blue"},
        {"kind": "revive", "id": 14, "pos": [0.0, 0.0, 0.0], "owner": 5},
        {"kind": "money", "id": 15, "pos": [0.0, 0.0, 0.0]},
    ]);
    assert_eq!(s(&objectives), expected);
    assert_eq!(serde_json::from_value::<Vec<Objective>>(expected).unwrap(), objectives);
    let core = |points| Objective::Flag {
        id: 1,
        pos: [0.0; 3],
        team: Team::Red,
        status: FlagStatus::Home,
        carrier: None,
        returns_in: None,
        points,
    };
    assert!(s(core(None)).get("points").is_none(), "a CTF flag is written as before");
    assert_eq!(s(core(Some(5)))["points"], 5);
    let points = Points { red: 3, blue: 5, max: Some(45), red_leader: Some(PlayerId(4)), blue_leader: None };
    let expected = json!({"red": 3, "blue": 5, "max": 45, "red_leader": 4, "blue_leader": null});
    assert_eq!(s(&points), expected);
    let buster = MvmClass {
        name: "sentry_buster".into(),
        count: 1,
        active: false,
        giant: true,
        crit: false,
        support: false,
        mission: true,
    };
    let mvm = Mvm { wave: 2, waves: 5, enemies: 67, between_waves: false, money: 41, classes: vec![buster] };
    let expected = json!({"wave": 2, "waves": 5, "enemies": 67, "between_waves": false, "money": 41, "classes": [
        {"name": "sentry_buster", "count": 1, "active": false, "giant": true, "crit": false, "support": false, "mission": true}
    ]});
    assert_eq!(s(&mvm), expected);
    assert_eq!(serde_json::from_value::<Mvm>(expected).unwrap(), mvm);
}

#[test]
fn game_mode_lines_name_their_players() {
    let line = Line::BallCarrierKilled(BallCarrierKilled {
        tick: 1,
        attacker: Some(PlayerId(3)),
        victim: PlayerId(4),
        assister: Some(PlayerId(5)),
    });
    assert_eq!(line.players().collect::<Vec<_>>(), [PlayerId(3), PlayerId(4), PlayerId(5)]);
    let stun = Line::Stun(Stun { tick: 1, stunner: None, victim: PlayerId(4), capping: true, big: false });
    assert_eq!(stun.players().collect::<Vec<_>>(), [PlayerId(4)]);
    let started = Line::CaptureStarted(CaptureStarted {
        tick: 1,
        point: 1,
        name: String::new(),
        team: Team::Blue,
        owner: Team::Red,
        cappers: vec![Some(PlayerId(7)), Some(PlayerId(8))],
    });
    assert_eq!(started.players().collect::<Vec<_>>(), [PlayerId(7), PlayerId(8)]);
}

fn s(v: impl Serialize) -> Value {
    serde_json::to_value(v).unwrap()
}

#[test]
fn teams_and_kinds_use_the_documented_spellings() {
    assert_eq!(s(Team::Unassigned), json!("none"));
    assert_eq!(s(Class::Demoman), json!("demoman"));
    assert_eq!(s(ProjectileKind::ShortCircuitOrb), json!("ShortCircuitOrb"));
    assert_eq!(s(Ubered::VaccinatorBlast), json!("vaccinator_blast"));
    assert_eq!(s(Round::BetweenRounds), json!("between_rounds"));
    assert_eq!(s(BannerKind::BattalionsBackup), json!("battalions_backup"));
    assert_eq!(s(HealSource::OnHit), json!("on_hit"));
    assert_eq!(s(ChatKind::SayText), json!("SayText"));
}

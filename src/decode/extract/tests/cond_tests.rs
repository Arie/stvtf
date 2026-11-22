use super::*;

fn words(conds: &[usize]) -> [u32; COND_WORDS] {
    cond_words(conds.iter().copied())
}

#[test]
fn conditions_decode_by_bit() {
    let names = |w: &[u32; COND_WORDS]| conditions(w).into_iter().map(Condition::name).collect::<Vec<_>>();
    assert_eq!(
        names(&words(&[1, 4, 32, 64, 130])),
        ["Zoomed", "Stealthed", "SpeedBoost", "StealthedUserBuff", "ImmuneToPushback"]
    );
    // m_nPlayerCond and _condition_bits both carry conditions 0-31.
    let mut w = [0; COND_WORDS];
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

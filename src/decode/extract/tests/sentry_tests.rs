use super::*;

#[test]
fn aim_is_the_placement_turned_by_the_yaw_pose() {
    let s = |yaw, pose| Sentry { yaw, pose: [0.5, pose], ..Default::default() };
    assert_eq!(s(255.6, 0.5).aim(), 255.6);
    // Sampled while firing on a target 49 degrees left of the placement direction.
    assert!((s(255.6, 0.365).aim() - (255.6 + 48.6)).abs() < 0.1);
    assert!((s(10.0, 0.75).aim() - 280.0).abs() < 1e-3, "wraps around");
}

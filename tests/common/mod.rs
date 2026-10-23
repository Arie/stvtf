//! Shared by the integration tests.

use stvtf::Line;
use stvtf::output::Position;

/// The payloads of one kind of line: `of!(lines, Kill)` iterates over every `Kill` in `lines`.
macro_rules! of {
    ($lines:expr, $variant:ident) => {
        $lines.iter().filter_map(|l| match l {
            stvtf::Line::$variant(x) => Some(x),
            _ => None,
        })
    };
}
pub(crate) use of;

/// A demo file's output.
#[allow(dead_code, reason = "not every test decodes a demo")]
pub fn demo(name: &str, snapshot_hz: f32) -> Vec<Line> {
    let bytes = std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut out = Vec::new();
    stvtf::decode::demo_file(&bytes, snapshot_hz, &mut |l| out.push(l)).unwrap();
    out
}

#[allow(dead_code, reason = "not every test measures distances")]
pub fn dist(a: Position, b: Position) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

//! Positions, and numbers rounded as they are written.

/// A position in Hammer units, rounded to a tenth.
pub type Position = [f64; 3];

/// To a tenth: positions, angles, seconds.
pub fn round1(f: impl Into<f64>) -> f64 {
    (f.into() * 10.0).round() / 10.0
}

/// To a hundredth: progress, screen positions.
pub fn round2(f: impl Into<f64>) -> f64 {
    (f.into() * 100.0).round() / 100.0
}

/// To a thousandth: rates, fractions.
pub fn round3(f: impl Into<f64>) -> f64 {
    (f.into() * 1000.0).round() / 1000.0
}

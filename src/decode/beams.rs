//! Beams drawn in the world for the clients of a relay (`TE_BeamPoints`), as the game's own
//! temp entities: a room's strokes, seen in TF2.

use super::{Decoder, ModelsSent};
use crate::net::bits::BitBuf;
use crate::net::messages::SVC_TEMPENTITIES;
use tf_demo_parser::demo::message::tempentities::{EventInfo, TempEntitiesMessage};
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

/// Beams in one message: well under a datagram, as some go unreliably.
const PER_MESSAGE: usize = 16;
/// The longest a beam can last, and widest it can be, as the game sends them.
const MAX_LIFE: f32 = 25.5;
/// A beam's life goes in steps of this (8 bits up to 25.6 s), and one of 0 lasts forever: a life
/// is rounded up to a whole step, one at least.
const LIFE_STEP: f32 = 25.6 / 255.0;
const MAX_WIDTH: f32 = 127.0;

/// A straight line in the world, shown for `life` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beam {
    /// Where it starts.
    pub from: [f32; 3],
    /// Where it ends.
    pub to: [f32; 3],
    /// Its colour.
    pub rgb: [u8; 3],
    /// How long it shows, in seconds.
    pub life: f32,
    /// How wide it is, in units.
    pub width: f32,
    /// How fast its texture runs along it, from `from` to `to`; 0 stands still.
    pub speed: u8,
}

/// `life` as sent: rounded up to a whole step, one at least, never forever.
fn life(life: f32) -> f32 {
    ((life / LIFE_STEP).ceil().max(1.0) * LIFE_STEP).min(MAX_LIFE)
}

pub(crate) const BASE: &str = "DT_BaseBeam";
pub(crate) const POINTS: &str = "DT_TEBeamPoints";

impl Decoder {
    /// The model index beams are drawn with: the solid laser, with the message that loads it if
    /// the match did not (to send first; see `Tables::laser` for `before`), or else a beam sprite
    /// the match loaded.
    pub(crate) fn beam_model(&self, before: Option<u16>, sent: ModelsSent) -> Option<(u16, Option<BitBuf>)> {
        let laser = self.tables.laser(self.handler.get_parser_state(), before, sent);
        laser.or_else(|| Some((self.tables.beam_sprite()?, None)))
    }

    /// `was`, with the models these tables have: what the clients were sent (see `Tables::laser`).
    pub(crate) fn models_sent(&self, was: ModelsSent) -> ModelsSent {
        self.tables.models_sent(was)
    }

    /// `beams` as `svc_TempEntities` drawn with model `sprite`, or None when the demo has no beam
    /// class.
    pub(crate) fn beams(&self, beams: &[Beam], sprite: u16) -> Option<Vec<BitBuf>> {
        let state = self.handler.get_parser_state();
        let class = state.server_classes.iter().position(|c| c.name.as_str() == "CTEBeamPoints")?;
        let table = state.send_tables.get(class)?;
        let event = |b: &Beam| {
            let int = |v: i64| SendPropValue::Integer(v);
            let vec = |p: [f32; 3]| SendPropValue::Vector(Vector { x: p[0], y: p[1], z: p[2] });
            let [r, g, bl] = b.rgb.map(i64::from);
            let values = [
                (BASE, "m_nModelIndex", int(i64::from(sprite))),
                (BASE, "m_nHaloIndex", int(0)),
                (BASE, "m_nStartFrame", int(0)),
                (BASE, "m_nFrameRate", int(0)),
                (BASE, "m_fLife", SendPropValue::Float(life(b.life))),
                (BASE, "m_fWidth", SendPropValue::Float(b.width.clamp(0.0, MAX_WIDTH))),
                (BASE, "m_fEndWidth", SendPropValue::Float(b.width.clamp(0.0, MAX_WIDTH))),
                (BASE, "m_nFadeLength", int(0)),
                (BASE, "m_fAmplitude", SendPropValue::Float(0.0)),
                (BASE, "m_nSpeed", int(i64::from(b.speed))),
                (BASE, "r", int(r)),
                (BASE, "g", int(g)),
                (BASE, "b", int(bl)),
                (BASE, "a", int(255)),
                (BASE, "m_nFlags", int(0)),
                (POINTS, "m_vecStartPoint", vec(b.from)),
                (POINTS, "m_vecEndPoint", vec(b.to)),
            ];
            // Every prop, zeros too: the client decodes into the one object it keeps per class.
            let mut props = Vec::with_capacity(values.len());
            for (t, name, value) in values {
                let id = SendPropIdentifier::new(t, name);
                props.push(SendProp { index: super::props::prop_index(table, id)?, identifier: id, value });
            }
            props.sort_by_key(|p| p.index);
            Some(EventInfo { class_id: u16::try_from(class).ok()?.into(), fire_delay: 0.0, reliable: false, props })
        };
        beams
            .chunks(PER_MESSAGE)
            .map(|chunk| {
                let msg = TempEntitiesMessage { events: chunk.iter().map(event).collect::<Option<_>>()? };
                super::encode_message(SVC_TEMPENTITIES, &msg, state)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_life_is_never_forever_and_never_shorter() {
        assert!((life(0.0) - LIFE_STEP).abs() < 1e-6, "one step, not 0: forever");
        assert!((life(0.02) - LIFE_STEP).abs() < 1e-6);
        let two = life(2.0);
        assert!((2.0..2.0 + LIFE_STEP).contains(&two), "{two}");
        assert!((life(100.0) - MAX_LIFE).abs() < 1e-6);
    }
}

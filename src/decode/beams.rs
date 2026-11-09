//! Beams drawn in the world for the clients of a relay (`TE_BeamPoints`), as the game's own
//! temp entities: a room's strokes, seen in TF2.

use super::Decoder;
use crate::net::bits::{BitBuf, BitReader, BitWriter};
use crate::net::messages::TYPE_BITS;
use bitbuffer::{BitWriteStream, LittleEndian};
use tf_demo_parser::demo::message::tempentities::{EventInfo, TempEntitiesMessage};
use tf_demo_parser::demo::parser::Encode;
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

pub(crate) const SVC_TEMPENTITIES: u32 = 27;
/// Beams in one message: well under a datagram.
const PER_MESSAGE: usize = 32;
/// The longest a beam can last, and widest it can be, as the game sends them.
const MAX_LIFE: f32 = 25.5;
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
}

pub(crate) const BASE: &str = "DT_BaseBeam";
pub(crate) const POINTS: &str = "DT_TEBeamPoints";

impl Decoder {
    /// `beams` as `svc_TempEntities`, or None when the demo has no beam class or no sprite loaded
    /// to draw them with.
    pub(crate) fn beams(&self, beams: &[Beam]) -> Option<Vec<BitBuf>> {
        let state = self.handler.get_parser_state();
        let class = state.server_classes.iter().position(|c| c.name.as_str() == "CTEBeamPoints")?;
        let table = state.send_tables.get(class)?;
        let sprite = self.tables.beam_sprite()?;
        let event = |b: &Beam| {
            let int = |v: i64| SendPropValue::Integer(v);
            let vec = |p: [f32; 3]| SendPropValue::Vector(Vector { x: p[0], y: p[1], z: p[2] });
            let [r, g, bl] = b.rgb.map(i64::from);
            let values = [
                (BASE, "m_nModelIndex", int(i64::from(sprite))),
                (BASE, "m_nHaloIndex", int(0)),
                (BASE, "m_nStartFrame", int(0)),
                (BASE, "m_nFrameRate", int(0)),
                (BASE, "m_fLife", SendPropValue::Float(b.life.clamp(0.0, MAX_LIFE))),
                (BASE, "m_fWidth", SendPropValue::Float(b.width.clamp(0.0, MAX_WIDTH))),
                (BASE, "m_fEndWidth", SendPropValue::Float(b.width.clamp(0.0, MAX_WIDTH))),
                (BASE, "m_nFadeLength", int(0)),
                (BASE, "m_fAmplitude", SendPropValue::Float(0.0)),
                (BASE, "m_nSpeed", int(0)),
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
                let index = table.flattened_props.iter().position(|d| d.identifier == id)?;
                props.push(SendProp { index: u32::try_from(index).ok()?, identifier: id, value });
            }
            props.sort_by_key(|p| p.index);
            Some(EventInfo { class_id: u16::try_from(class).ok()?.into(), fire_delay: 0.0, reliable: false, props })
        };
        beams
            .chunks(PER_MESSAGE)
            .map(|chunk| {
                let msg = TempEntitiesMessage { events: chunk.iter().map(event).collect::<Option<_>>()? };
                let mut bytes = Vec::new();
                let nbits = {
                    let mut s = BitWriteStream::new(&mut bytes, LittleEndian);
                    msg.encode(&mut s, state).ok()?;
                    s.bit_len()
                };
                let mut w = BitWriter::new();
                w.write_bits(SVC_TEMPENTITIES, TYPE_BITS);
                w.write_buf(&BitReader::slice(&bytes, 0..nbits));
                Some(w.finish())
            })
            .collect()
    }
}

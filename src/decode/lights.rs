//! A light at a spot for the clients of a relay (`TE_DynamicLight`), the game's own temp entity:
//! a room's spotlight, seen in TF2. A client has room for one such light: the next takes its
//! place.

use super::Decoder;
use crate::net::bits::BitBuf;
use crate::net::messages::SVC_TEMPENTITIES;
use tf_demo_parser::demo::message::tempentities::{EventInfo, TempEntitiesMessage};
use tf_demo_parser::demo::sendprop::{SendProp, SendPropIdentifier, SendPropValue};
use tf_demo_parser::demo::vector::Vector;

pub(crate) const TABLE: &str = "DT_TEDynamicLight";
/// The smallest a light is, and the longest it lasts, as the game sends them.
const MIN_RADIUS: f32 = 10.0;
const MAX_RADIUS: f32 = 2560.0;
const MAX_LIFE: f32 = 25.5;

impl Decoder {
    /// A light at `at` in `rgb`, reaching `radius` units and falling off by `exponent`, for
    /// `life` seconds; or None when the demo has no light class.
    pub(crate) fn light(&self, at: [f32; 3], rgb: [u8; 3], radius: f32, exponent: u8, life: f32) -> Option<BitBuf> {
        let state = self.handler.get_parser_state();
        let class = state.server_classes.iter().position(|c| c.name.as_str() == "CTEDynamicLight")?;
        let table = state.send_tables.get(class)?;
        let int = |v: u8| SendPropValue::Integer(i64::from(v));
        let values = [
            ("m_vecOrigin", SendPropValue::Vector(Vector { x: at[0], y: at[1], z: at[2] })),
            ("r", int(rgb[0])),
            ("g", int(rgb[1])),
            ("b", int(rgb[2])),
            ("exponent", int(exponent.min(127))),
            ("m_fRadius", SendPropValue::Float(radius.clamp(MIN_RADIUS, MAX_RADIUS))),
            ("m_fTime", SendPropValue::Float(life.clamp(0.0, MAX_LIFE))),
            ("m_fDecay", SendPropValue::Float(0.0)),
        ];
        // Every prop, zeros too: the client decodes into the one object it keeps per class.
        let mut props = Vec::with_capacity(values.len());
        for (name, value) in values {
            let id = SendPropIdentifier::new(TABLE, name);
            props.push(SendProp { index: super::props::prop_index(table, id)?, identifier: id, value });
        }
        props.sort_by_key(|p| p.index);
        let event = EventInfo { class_id: u16::try_from(class).ok()?.into(), fire_delay: 0.0, reliable: false, props };
        super::encode_message(SVC_TEMPENTITIES, &TempEntitiesMessage { events: vec![event] }, state)
    }

    /// The light put out: the smallest, gone at once, in its place.
    pub(crate) fn no_light(&self) -> Option<BitBuf> {
        self.light([0.0; 3], [0; 3], MIN_RADIUS, 0, 0.0)
    }
}

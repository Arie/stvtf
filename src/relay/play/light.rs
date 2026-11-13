//! A room's spotlight in TF2: the game's dynamic light at a spot, shown again before it runs out
//! (and so to those who join), put out when the room has none. The clients have one such light:
//! each sent takes the last one's place.

use super::player::Player;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A light the clients in the game keep seeing, until replaced.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Light {
    /// Where it is.
    pub at: [f32; 3],
    /// Its colour.
    pub rgb: [u8; 3],
    /// How far it reaches, in units.
    pub radius: f32,
    /// How fast it falls off from its middle: more is brighter there.
    pub exponent: u8,
}

/// How often the light is shown again, and for how long each time: past the next showing at the
/// fastest a room plays (4×, the clients' clock with it).
const EVERY: Duration = Duration::from_secs(4);
const LIFE: f32 = 25.0;

/// The light as the clients were last given it, and when.
#[derive(Default)]
pub(super) struct Shown {
    light: Option<Light>,
    at: Option<Instant>,
}

impl Shown {
    /// The clients' clock went past what they were shown: the light is shown again at once.
    pub(super) fn again(&mut self) {
        self.at = None;
    }
}

impl Player<'_> {
    /// Gives the clients the room's `light`: at once and reliably when it changed (put out, when
    /// there is none now), again every [`EVERY`] while there is one.
    pub(super) fn show_light(&mut self, light: Option<Light>) {
        let Some(c) = self.cur.as_ref().filter(|c| c.started) else { return };
        let s = &mut self.shown_light;
        let changed = light != s.light;
        if !changed && (light.is_none() || s.at.is_some_and(|at| at.elapsed() < EVERY)) {
            return;
        }
        let msg = match light {
            Some(l) => c.dec.light(l.at, l.rgb, l.radius, l.exponent, LIFE),
            None => c.dec.no_light(),
        };
        let Some(msg) = msg else { return };
        let mut hub = self.relay.hub();
        if changed {
            hub.queue_to_active(&msg);
        } else {
            hub.send_to_active(&msg);
        }
        *s = Shown { light, at: Some(Instant::now()) };
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::upward_until;
    use crate::decode::lights::TABLE;
    use crate::net::messages::{SVC_TEMPENTITIES, TYPE_BITS};
    use tf_demo_parser::demo::message::Message;
    use tf_demo_parser::demo::sendprop::{SendPropIdentifier, SendPropValue};
    use tf_demo_parser::demo::vector::Vector;

    #[test]
    fn a_light_reads_back_as_the_games_own_and_put_out_is_the_smallest_gone_at_once() {
        let c = upward_until(|c| c.started);
        let read = |m: crate::net::bits::BitBuf| {
            let mut s = tf_demo_parser::Stream::from(bitbuffer::BitReadBuffer::new_owned(
                m.bytes().to_vec(),
                bitbuffer::LittleEndian,
            ));
            assert_eq!(s.read_int::<u32>(TYPE_BITS).unwrap(), SVC_TEMPENTITIES);
            let state = c.dec.parser_state_for_tests();
            let Message::TempEntities(te) =
                Message::from_type(tf_demo_parser::demo::message::MessageType::TempEntities, &mut s, state).unwrap()
            else {
                panic!()
            };
            assert_eq!(te.events.len(), 1);
            let props = te.events[0].props.clone();
            move |n: &str| {
                props.iter().find(|p| p.identifier == SendPropIdentifier::new(TABLE, n)).unwrap().value.clone()
            }
        };
        let float = |v: SendPropValue| if let SendPropValue::Float(f) = v { f } else { panic!("{v:?}") };
        let get = read(c.dec.light([100.0, -200.0, 300.0], [255, 128, 0], 500.0, 5, 25.0).expect("a light class"));
        assert_eq!(get("m_vecOrigin"), SendPropValue::Vector(Vector { x: 100.0, y: -200.0, z: 300.0 }));
        assert_eq!([get("r"), get("g"), get("b")], [255, 128, 0].map(SendPropValue::Integer));
        assert_eq!(get("exponent"), SendPropValue::Integer(5));
        assert!((float(get("m_fRadius")) - 500.0).abs() <= 10.0, "to the step it is sent in");
        assert!((float(get("m_fTime")) - 25.0).abs() <= 0.1);
        let out = read(c.dec.no_light().unwrap());
        assert!((float(out("m_fRadius")) - 10.0).abs() < 0.01);
        assert!(float(out("m_fTime")).abs() < 0.01, "gone at once");
    }
}

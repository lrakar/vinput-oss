//! The graphics cards of this PC, as Windows lists them.
//!
//! Local models can run on a graphics card of any make, with the Vulkan
//! build of the engine.

use std::sync::OnceLock;

/// A graphics card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// Its name, such as `AMD Radeon RX 7800 XT`.
    pub name: String,
    /// Memory of its own, in bytes. Cards built into the processor have little.
    pub memory: u64,
}

/// The card local models run on when the user wants a graphics card: the
/// one with the most memory of its own.
/// `None` if the PC has none, or Windows cannot say. Looked up once.
#[must_use]
pub fn card() -> Option<&'static Card> {
    static CARD: OnceLock<Option<Card>> = OnceLock::new();
    CARD.get_or_init(|| best(platform::cards())).as_ref()
}

fn best(cards: Vec<Card>) -> Option<Card> {
    cards.into_iter().max_by_key(|card| card.memory)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
    };

    use super::Card;

    /// Every graphics card, software renderers left out.
    pub(super) fn cards() -> Vec<Card> {
        let mut cards = Vec::new();
        // SAFETY: plain calls on a factory created here; each description is
        // copied out of the structure the adapter fills in.
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
                return cards;
            };
            let mut index = 0;
            while let Ok(adapter) = factory.EnumAdapters1(index) {
                index += 1;
                let Ok(about) = adapter.GetDesc1() else {
                    continue;
                };
                if about.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                let length = about
                    .Description
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(about.Description.len());
                cards.push(Card {
                    name: String::from_utf16_lossy(&about.Description[..length])
                        .trim()
                        .to_owned(),
                    memory: about.DedicatedVideoMemory as u64,
                });
            }
        }
        cards
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) fn cards() -> Vec<super::Card> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_with_the_most_memory_is_the_one_used() {
        let card = |name: &str, memory: u64| Card {
            name: name.to_owned(),
            memory,
        };
        let built_in = card("Intel UHD Graphics", 128 << 20);
        let radeon = card("AMD Radeon RX 7800 XT", 16 << 30);
        assert_eq!(best(vec![]), None);
        assert_eq!(best(vec![built_in.clone()]), Some(built_in.clone()));
        assert_eq!(best(vec![built_in, radeon.clone()]), Some(radeon));
    }
}

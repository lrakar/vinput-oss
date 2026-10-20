//! The menus of the tray icon: the builder every program of the family
//! shares (`putlab_core::menu`), and the one list only Vinput has, that of
//! the microphones.

pub(super) use putlab_core::menu::{Mark, Menu};
use vinput_core::audio::Microphone;

/// The first entry of a list of microphones: whichever one Windows uses by
/// default.
const DEFAULT_MICROPHONE: &str = "System default";

/// The list of microphones to choose from, with the one in `current`
/// marked, numbered from `first` on. The menu ids are `first` for the
/// default one and `first + 1 + index` for the connected ones.
pub(super) fn microphones(connected: &[Microphone], current: &str, first: usize) -> Option<Menu> {
    let mut menu = Menu::new()?;
    let mark = |chosen: bool| if chosen { Mark::Radio } else { Mark::None };
    menu.item(first, DEFAULT_MICROPHONE, mark(current.is_empty()));
    if !connected.is_empty() {
        menu.separator();
    }
    for (index, microphone) in connected.iter().enumerate() {
        menu.item(
            first + 1 + index,
            &microphone.name,
            mark(microphone.id == current),
        );
    }
    Some(menu)
}

/// The microphone a menu built by [`microphones`] from `first` on says was
/// picked, as its identifier: empty for the default one. `None` if nothing
/// was picked.
pub(super) fn picked_microphone(
    item: usize,
    connected: &[Microphone],
    first: usize,
) -> Option<String> {
    match item.checked_sub(first)? {
        0 => Some(String::new()),
        index => connected
            .get(index - 1)
            .map(|microphone| microphone.id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_microphone_item_maps_back_to_its_device() {
        let connected = [
            Microphone {
                id: "a".into(),
                name: "A".into(),
            },
            Microphone {
                id: "b".into(),
                name: "B".into(),
            },
        ];
        assert_eq!(picked_microphone(200, &connected, 200), Some(String::new()));
        assert_eq!(picked_microphone(201, &connected, 200), Some("a".into()));
        assert_eq!(picked_microphone(202, &connected, 200), Some("b".into()));
        // Nothing picked, or an item that is not a microphone.
        assert_eq!(picked_microphone(0, &connected, 200), None);
        assert_eq!(picked_microphone(203, &connected, 200), None);
        assert_eq!(picked_microphone(7, &connected, 200), None);
    }
}

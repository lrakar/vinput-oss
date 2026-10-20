//! The canned scenes of the hub's window.

use vinput_view::{
    Catalog, Hub, HubAsk, Pro, ProDoing, ProTier, ProWho, Product, ProductState, RunsOn, Status,
    View,
};

/// The names [`scene`] knows.
pub const SCENES: [&str; 11] = [
    "hub-apps",
    "hub-fresh",
    "hub-update",
    "hub-downloading",
    "hub-installing",
    "hub-failed",
    "hub-offline",
    "hub-signed-out",
    "hub-signing-in",
    "hub-account",
    "hub-no-plan",
];

const MEGABYTE: u64 = 1 << 20;

fn vinput(installed: Option<&str>, latest: &str) -> Product {
    Product {
        id: "vinput".to_owned(),
        name: "VinPUT".to_owned(),
        summary: "Type with your voice into any text field.".to_owned(),
        installed: installed.map(str::to_owned),
        latest: Some(latest.to_owned()),
        bytes: 3 * MEGABYTE + MEGABYTE / 3,
        soon: false,
        state: ProductState::Idle,
    }
}

fn voutput() -> Product {
    Product {
        id: "voutput".to_owned(),
        name: "Voutput".to_owned(),
        summary: "Hear any text you select, or a short summary of it.".to_owned(),
        installed: None,
        latest: None,
        bytes: 0,
        soon: true,
        state: ProductState::Idle,
    }
}

/// Someone signed in, with a plan and a part of its month used.
fn subscribed() -> Pro {
    Pro {
        who: ProWho::SignedIn,
        email: "ana@example.com".to_owned(),
        known: true,
        entitled: true,
        status: "active".to_owned(),
        tier: ProTier::Pro,
        // 1 November 2026 and the end of that month.
        period_end: 1_793_491_200,
        resets_at: 1_793_491_200,
        used_seconds: 7 * 3600 + 40 * 60,
        cap_seconds: 25 * 3600,
        doing: ProDoing::Nothing,
        ..Pro::default()
    }
}

/// The hub with Vinput at `installed`, `latest` on offer, and `pro` signed in.
fn hub(installed: Option<&str>, latest: &str, pro: Pro) -> View {
    let signed_in = (pro.who == ProWho::SignedIn).then(|| pro.email.clone());
    View {
        version: "0.1.0".to_owned(),
        status: Status {
            listening: false,
            model: signed_in,
            runs_on: RunsOn::Nowhere,
        },
        hub: Hub {
            catalog: Catalog::Ready,
            products: vec![vinput(installed, latest), voutput()],
        },
        pro,
        ..View::default()
    }
}

/// The view that puts the hub's window in the state `name` names.
#[must_use]
pub fn scene(name: &str) -> Option<View> {
    let mut view = hub(Some("0.4.8"), "0.4.8", subscribed());
    match name {
        "hub-apps" | "hub-account" => {}
        "hub-fresh" => view = hub(None, "0.4.8", Pro::default()),
        "hub-update" => view = hub(Some("0.4.8"), "0.4.9", subscribed()),
        "hub-downloading" => {
            view = hub(None, "0.4.8", Pro::default());
            view.hub.products[0].state = ProductState::Downloading {
                done: MEGABYTE + MEGABYTE / 2,
                total: 3 * MEGABYTE + MEGABYTE / 3,
            };
        }
        "hub-installing" => {
            view = hub(None, "0.4.8", Pro::default());
            view.hub.products[0].state = ProductState::Installing;
        }
        "hub-failed" => {
            view = hub(None, "0.4.8", Pro::default());
            view.hub.products[0].state = ProductState::Failed {
                reason: "The download does not match what was published. Nothing was installed."
                    .to_owned(),
            };
        }
        "hub-offline" => {
            view.hub.catalog = Catalog::Failed {
                reason: "This PC seems to be offline.".to_owned(),
            };
            view.hub.products.truncate(1);
            view.hub.products[0].latest = None;
        }
        "hub-signed-out" => view = hub(Some("0.4.8"), "0.4.8", Pro::default()),
        "hub-signing-in" => {
            let pro = Pro {
                who: ProWho::SigningIn,
                in_browser: true,
                ..Pro::default()
            };
            view = hub(Some("0.4.8"), "0.4.8", pro);
        }
        "hub-no-plan" => {
            let pro = Pro {
                entitled: false,
                status: String::new(),
                used_seconds: 0,
                cap_seconds: 0,
                period_end: 0,
                ..subscribed()
            };
            view = hub(Some("0.4.8"), "0.4.8", pro);
        }
        _ => return None,
    }
    Some(view)
}

fn find<'a>(view: &'a mut View, id: &str) -> Option<&'a mut Product> {
    view.hub
        .products
        .iter_mut()
        .find(|product| product.id == id)
}

/// Changes `view` the way the hub would after `ask`, as far as the picture
/// goes.
pub(super) fn apply(view: &mut View, ask: HubAsk) {
    match ask {
        HubAsk::Refresh => view.hub.catalog = Catalog::Ready,
        HubAsk::Install(id) => {
            if let Some(product) = find(view, &id) {
                product.state = ProductState::Downloading {
                    done: 0,
                    total: product.bytes,
                };
            }
        }
        HubAsk::Cancel(id) => {
            if let Some(product) = find(view, &id) {
                product.state = ProductState::Idle;
            }
        }
        HubAsk::Uninstall(id) => {
            if let Some(product) = find(view, &id) {
                product.installed = None;
            }
        }
        HubAsk::Open(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_scene_exists() {
        for name in SCENES {
            assert!(scene(name).is_some(), "no scene called {name}");
        }
        assert!(scene("hub-no-such").is_none());
    }
}

//! The `/settings/tokens` route: the connected server's auth tokens on a page of their own. It
//! shares [`SettingsState`] with `/settings`; the bar's `+` reaches the mounted page through
//! [`open_new_token`].
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use frust::{
    Component, Get, NavigatorController, View, WindowMetrics, component, on_cleanup, use_context,
};

use crate::AppState;
use crate::responsive::Responsive;
use crate::ui::palette::{Palette, active_theme};

use super::pages::{Actions, SettingsState, connected_row};
use super::widgets;

pub fn tokens_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(TokensScreen { overlays })
}

pub struct TokensScreen {
    overlays: NavigatorController<AppState>,
}

thread_local! {
    /// Tagged with its mount, so a cleanup withdraws only its own entry: an incoming page builds
    /// before the outgoing one tears down.
    static TOKENS_CHROME: RefCell<Option<(u64, Actions)>> = const { RefCell::new(None) };
}

static NEXT_CHROME_ID: AtomicU64 = AtomicU64::new(1);

/// The bar's `+`; a press with no tokens page mounted is ignored.
pub fn open_new_token(_state: &mut AppState) {
    let actions = TOKENS_CHROME.with(|cell| cell.borrow().as_ref().map(|(_, a)| a.clone()));
    if let Some(actions) = actions {
        actions.open_create_token_dialog();
    }
}

impl Component for TokensScreen {
    type State = SettingsState;

    fn init(&self) -> SettingsState {
        let state = SettingsState::mount(&self.overlays);
        let id = NEXT_CHROME_ID.fetch_add(1, Ordering::Relaxed);
        let published = state.actions.clone();
        TOKENS_CHROME.with(|cell| *cell.borrow_mut() = Some((id, published)));
        on_cleanup(move || {
            TOKENS_CHROME.with(|cell| {
                let mut published = cell.borrow_mut();
                if published
                    .as_ref()
                    .is_some_and(|(mounted, _)| *mounted == id)
                {
                    *published = None;
                }
            });
        });
        state
    }

    fn build(&self, state: &mut SettingsState) -> impl View<SettingsState> {
        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        let responsive = Responsive::of(use_context::<WindowMetrics>());

        let saved = state.actions.servers.controller.saved.get();
        let connected = state.actions.servers.connected_endpoint();
        state.reread_servers_on_change(connected.clone());
        let device_token =
            connected_row(&saved, connected.as_deref()).and_then(|row| row.token_name.clone());
        let tokens = state.token_frame(device_token.as_deref(), theme.motion.reduce_motion);

        widgets::page_tone(
            widgets::tokens_phone_page(&tokens, responsive.wide_clamp, &colors),
            &colors,
        )
    }
}

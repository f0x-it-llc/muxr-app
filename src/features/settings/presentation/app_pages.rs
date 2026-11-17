//! The `/settings/app` route: device-wide settings that open with or without a server. Like
//! `pages`, its gestures take `&mut AppSettingsState` and the sheets it pushes see
//! `&mut AppState`, so [`AppActions`] is a clonable bundle serving both sides.
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    AnyView, Component, Either, Get, GetUntracked, NavigatorController, Set, View, WindowMetrics,
    any, component, spawn_local, use_context,
};

use crate::AppState;
use crate::failure::AppFailure;
use crate::features::connect::presentation::widgets::getting_started::open_getting_started;
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::presentation::controller::{ProController, RestoreOutcome};
use crate::features::pro::presentation::{
    NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, UNAVAILABLE_COPY, UpgradeContext, show_upgrade_sheet,
};
use crate::features::settings::di::expect_preferences_repository;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, font_size_label, refresh_label,
};
use crate::responsive::Responsive;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;

use super::controller::AppSettingsController;
use super::widgets::{
    self, CardColumn, FontPicker, FontRow, Picker, PickerKind, RowAction, RowHost,
};

const STORE_BUSY_TOAST: &str = "A store operation is already running";

/// An estimate of everything outside the scroll band, not imported from `ui::sheet`'s private
/// geometry; overestimating only shortens the band.
const SHEET_SCROLL_RESERVE_PX: f64 = 220.0;
const SHEET_SCROLL_FLOOR_PX: f64 = 240.0;
/// The Glyph list-row estimate. The band is a snapshot of `WindowMetrics` when the sheet opens,
/// so a resize while it is open does not re-size it.
const PICK_ROW_ESTIMATE_PX: f64 = 48.0;

fn sheet_band_height(metrics: &WindowMetrics, rows_height: f64) -> f64 {
    (metrics.size.height - SHEET_SCROLL_RESERVE_PX)
        .max(SHEET_SCROLL_FLOOR_PX)
        .min(rows_height)
}

type Group = (CardColumn, &'static str, Vec<AnyView<AppSettingsState>>);

pub fn app_settings_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(AppSettingsScreen { overlays })
}

pub struct AppSettingsScreen {
    overlays: NavigatorController<AppState>,
}

pub struct AppSettingsState {
    pub actions: AppActions,
}

#[derive(Clone)]
pub struct AppActions {
    pub overlays: NavigatorController<AppState>,
    pub controller: Arc<AppSettingsController>,
    pub pro: Arc<ProController>,
}

impl Component for AppSettingsScreen {
    type State = AppSettingsState;

    fn init(&self) -> AppSettingsState {
        let controller = use_controller::<AppSettingsController, AppFailure>(|| {
            AppSettingsController::new(expect_preferences_repository())
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        // Refreshed on every mount, so the plan row and `connect`'s gate never disagree.
        let pro = expect_pro_controller();
        let pro_refresh = Arc::clone(&pro);
        spawn_local(async move { pro_refresh.refresh().await });

        AppSettingsState {
            actions: AppActions {
                overlays: self.overlays.clone(),
                controller,
                pro,
            },
        }
    }

    fn build(&self, state: &mut AppSettingsState) -> impl View<AppSettingsState> {
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();
        let restoring = state.actions.controller.restoring.get();
        let entitlement = state.actions.pro.entitlement.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        let responsive = Responsive::of(use_context::<WindowMetrics>());

        let mut groups: Vec<Group> = vec![
            (
                CardColumn::Left,
                "terminal",
                widgets::appearance_rows(&appearance, &colors),
            ),
            (
                CardColumn::Left,
                "behaviour",
                widgets::poll_rows(&poll, &colors),
            ),
            (
                CardColumn::Right,
                widgets::PRO_GROUP_LABEL,
                widgets::pro_rows(entitlement, restoring, &colors),
            ),
        ];
        groups.extend(servers_group());
        groups.push((CardColumn::Left, "about", widgets::about_rows(&colors)));

        if responsive.is_tablet() {
            return widgets::page_tone(
                Either::Left(widgets::card_grid(
                    groups,
                    responsive.is_full(),
                    None,
                    &colors,
                )),
                &colors,
            );
        }

        let mut body = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
        for (_, label, rows) in groups {
            body.push(any(widgets::group_label(label, &colors)));
            body.extend(rows);
        }
        widgets::page_tone(
            Either::Right(widgets::phone_scroller(body, responsive.wide_clamp)),
            &colors,
        )
    }
}

/// The seam for the device's saved servers, between `muxr pro` and `about` (the right column on
/// a Full tablet). It renders nothing yet.
fn servers_group() -> Option<Group> {
    None
}

impl RowHost for AppSettingsState {
    fn row_tapped(&mut self, action: RowAction) {
        match action {
            RowAction::Picker(kind) => self.actions.open_picker(kind),
            RowAction::UpgradeSheet => self.actions.open_upgrade_sheet(),
            RowAction::GettingStarted => open_getting_started(&self.actions.overlays),
            // Unreachable: this screen renders no such row, and an inert row carries no gesture.
            RowAction::Static | RowAction::AppSettings => {}
        }
    }
}

impl AppActions {
    /// The body re-runs on every navigator rebuild, so the check mark tracks the stored value.
    fn open_picker(&self, kind: PickerKind) {
        if kind == PickerKind::FontFamily {
            self.open_font_picker();
            return;
        }
        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let picker = actions.picker(kind);
            let picking = actions.clone();
            let sheet = sheet.clone();
            widgets::picker_sheet(&picker, &colors, move |_app: &mut AppState, index| {
                // Close first: a sheet pop lands at the next rebuild, so a second tap inside
                // that frame would pop the route out from under the picker.
                sheet.close();
                picking.pick(kind, index);
            })
        });
    }

    fn picker(&self, kind: PickerKind) -> Picker {
        let appearance = self.controller.appearance_untracked();
        let poll = self.controller.poll_untracked();
        let (options, selected): (Vec<String>, Option<usize>) = match kind {
            PickerKind::FontFamily => (Vec::new(), None),
            PickerKind::FontSize => (
                AppearancePrefs::FONT_SIZES_PX
                    .iter()
                    .map(|px| font_size_label(*px))
                    .collect(),
                AppearancePrefs::FONT_SIZES_PX
                    .iter()
                    .position(|px| *px == appearance.font_size_px),
            ),
            PickerKind::CursorStyle => (
                CursorStyle::ALL
                    .iter()
                    .map(|style| style.option_label().to_string())
                    .collect(),
                CursorStyle::ALL
                    .iter()
                    .position(|style| *style == appearance.cursor_style),
            ),
            PickerKind::LayoutRefresh => (
                PollPrefs::REFRESH_CHOICES_SECS
                    .iter()
                    .map(|secs| refresh_label(*secs))
                    .collect(),
                PollPrefs::REFRESH_CHOICES_SECS
                    .iter()
                    .position(|secs| *secs == poll.layout_refresh_secs),
            ),
        };
        let rows_height = options.len() as f64 * PICK_ROW_ESTIMATE_PX;
        let max_height =
            use_context::<WindowMetrics>().map(|metrics| sheet_band_height(&metrics, rows_height));
        Picker {
            title: kind.title(),
            options,
            selected,
            max_height,
        }
    }

    /// An index outside the current list is dropped, never clamped, since a guess is unchosen.
    fn pick(&self, kind: PickerKind, index: usize) {
        let controller = Arc::clone(&self.controller);
        match kind {
            // Unreachable: the font family persists through `pick_font_family`.
            PickerKind::FontFamily => {}
            PickerKind::FontSize => {
                let Some(px) = AppearancePrefs::FONT_SIZES_PX.get(index) else {
                    return;
                };
                let next = AppearancePrefs {
                    font_size_px: *px,
                    ..controller.appearance_untracked()
                };
                spawn_local(async move { controller.set_appearance(next).await });
            }
            PickerKind::CursorStyle => {
                let Some(style) = CursorStyle::ALL.get(index) else {
                    return;
                };
                let next = AppearancePrefs {
                    cursor_style: *style,
                    ..controller.appearance_untracked()
                };
                spawn_local(async move { controller.set_appearance(next).await });
            }
            PickerKind::LayoutRefresh => {
                let Some(secs) = PollPrefs::REFRESH_CHOICES_SECS.get(index) else {
                    return;
                };
                let next = PollPrefs {
                    layout_refresh_secs: *secs,
                    ..controller.poll_untracked()
                };
                spawn_local(async move { controller.set_poll_prefs(next).await });
            }
        }
    }

    /// The catalog loads here, never at startup; `load` is single-flight.
    fn open_font_picker(&self) {
        let catalog = crate::fonts::catalog::font_catalog();
        spawn_local(async move { catalog.load().await });

        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let picker = actions.font_picker();
            let sheet = sheet.clone();
            let selecting = actions.clone();
            let downloading = actions.clone();
            widgets::font_picker_sheet(
                &picker,
                &colors,
                move |_app: &mut AppState, family: String| {
                    sheet.close();
                    selecting.pick_font_family(family);
                },
                move |_app: &mut AppState, family: String| downloading.download_font(&family),
            )
        });
    }

    /// Every read is tracked: a catalog landing or a download state change re-renders the rows.
    fn font_picker(&self) -> FontPicker {
        let catalog = crate::fonts::catalog::font_catalog();
        let status = catalog.status();
        let stale = catalog.is_stale();
        let entries = catalog.entries();
        let selected = self.controller.appearance.get().font_family;

        let installed: Vec<FontRow> = crate::fonts::pickable_terminal_families()
            .into_iter()
            .map(|family| {
                let display = crate::fonts::display_name(&family);
                // Provenance, not name: a downloaded face whose display name matches its
                // family would otherwise read as "built-in".
                let meta = if crate::fonts::store::is_downloaded(&family) {
                    widgets::FONT_META_DOWNLOADED
                } else {
                    widgets::FONT_META_BUILT_IN
                };
                FontRow {
                    family,
                    display,
                    meta: meta.to_string(),
                    state: crate::fonts::catalog::FontState::Installed,
                    download_progress: None,
                }
            })
            .collect();

        let available: Vec<FontRow> = entries
            .iter()
            .filter_map(|entry| {
                let state = catalog.state_of(entry);
                if state == crate::fonts::catalog::FontState::Installed {
                    return None;
                }
                let download_progress = if state == crate::fonts::catalog::FontState::Downloading
                    && entry.total_bytes() > 0
                {
                    Some(catalog.progress_of_entry(entry).unwrap_or(0.0))
                } else {
                    None
                };
                Some(FontRow {
                    family: entry.family.clone(),
                    display: entry.display_name.clone(),
                    meta: format!("{} \u{b7} {}", entry.license, entry.size_label()),
                    state,
                    download_progress,
                })
            })
            .collect();

        let max_height = use_context::<WindowMetrics>()
            .map(|metrics| sheet_band_height(&metrics, f64::INFINITY));

        FontPicker {
            installed,
            available,
            selected,
            status,
            stale,
            max_height,
        }
    }

    fn pick_font_family(&self, family: String) {
        let controller = Arc::clone(&self.controller);
        let next = AppearancePrefs {
            font_family: family,
            ..controller.appearance_untracked()
        };
        spawn_local(async move { controller.set_appearance(next).await });
    }

    fn download_font(&self, family: &str) {
        let catalog = crate::fonts::catalog::font_catalog();
        let Some(entry) = catalog
            .entries_untracked()
            .iter()
            .find(|entry| entry.family == family)
            .cloned()
        else {
            return;
        };
        spawn_local(async move {
            if let Err(failure) = catalog.install(entry).await {
                app_toasts().raise(&failure);
            }
        });
    }

    pub fn toggle_bell(&self) {
        let controller = Arc::clone(&self.controller);
        let current = controller.poll_untracked();
        let next = PollPrefs {
            bell_notifications: !current.bell_notifications,
            ..current
        };
        spawn_local(async move { controller.set_poll_prefs(next).await });
    }

    pub fn restore_purchases(&self) {
        let pro = Arc::clone(&self.pro);
        let restoring = self.controller.restoring;
        spawn_local(async move {
            // Only a restore the controller accepts owns the flag, so a refused tap never clears
            // the one in flight. This check and `restore`'s own guard run in the same poll.
            let accepted = !pro.busy.get_untracked();
            if accepted {
                restoring.set(true);
            }
            let outcome = pro.restore().await;
            if accepted {
                restoring.set(false);
            }
            match outcome {
                RestoreOutcome::Pending => app_toasts().show(PENDING_TOAST),
                RestoreOutcome::NothingToRestore => app_toasts().show(NOTHING_TO_RESTORE_TOAST),
                RestoreOutcome::Unavailable => app_toasts().show(UNAVAILABLE_COPY),
                // The app already bridges the pro controller's failure sink to a toast.
                RestoreOutcome::Failed(_) => {}
                RestoreOutcome::Busy => app_toasts().show(STORE_BUSY_TOAST),
                RestoreOutcome::Restored => {}
            }
        });
    }

    /// `on_unlocked` is a no-op: this screen has no blocked dial to resume.
    pub fn open_upgrade_sheet(&self) {
        show_upgrade_sheet(
            &self.overlays,
            Arc::clone(&self.pro),
            UpgradeContext {
                on_unlocked: Rc::new(|| {}),
                server_limit: None,
            },
        );
    }
}

pub(super) fn restore_purchases(state: &mut AppSettingsState) {
    state.actions.restore_purchases();
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::WindowInsets;
    use kurbo::Size;

    fn window(height: f64) -> WindowMetrics {
        WindowMetrics::new(Size::new(800.0, height), 1.0, WindowInsets::default())
    }

    #[test]
    fn a_short_window_holds_the_band_at_its_floor() {
        let band = sheet_band_height(&window(390.0), f64::INFINITY);
        assert_eq!(band, SHEET_SCROLL_FLOOR_PX);
    }

    #[test]
    fn a_tall_window_leaves_the_reserve_and_never_outgrows_its_rows() {
        let metrics = window(1200.0);
        assert_eq!(
            sheet_band_height(&metrics, f64::INFINITY),
            1200.0 - SHEET_SCROLL_RESERVE_PX
        );
        assert_eq!(
            sheet_band_height(&metrics, 4.0 * PICK_ROW_ESTIMATE_PX),
            192.0
        );
    }
}

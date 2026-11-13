//! The Pro paywall bottom sheet, raised by the Connect server-limit gate and the Settings plan row.
//! Callbacks are `Rc<dyn Fn()>` taking no `&mut AppState`; every dismissal goes through one sheet handle, never a bare `pop`.
//! Store work runs in `spawn_local` bodies, which have no ambient reactive owner, so nothing is resolved from context inside them.

use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::text::TextStyle;
use frust::icons::LINK;
use frust::{
    AnyView, Axis, ButtonStyle, CrossAxisAlignment, EitherOf3, FlexChild, FlexView, Get,
    GetUntracked, IconSource, NavigatorController, SizedBox, View, any, button, icon, inflexible,
    row, spawn_local, text,
};
use frust_glyph::{dots_loader, skeleton};

use crate::AppState;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::domain::entities::{ProOffer, PurchaseOutcome};
use crate::features::pro::presentation::controller::{ProController, RestoreOutcome};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;

pub const UPGRADE_SHEET_TITLE: &str = "Muxr Pro";
pub const RESTORE_LABEL: &str = "Restore purchases";
pub const NOT_NOW_LABEL: &str = "Not now";
pub const UNAVAILABLE_COPY: &str = "Purchases aren't available on this device.";
/// Purchase taken but not settled: the sheet closes, but nothing is unlocked yet.
pub const PENDING_TOAST: &str = "Purchase pending — you'll be upgraded once it completes.";
/// About the account, not the device: a restore reaches the store account.
pub const NOTHING_TO_RESTORE_TOAST: &str = "No Muxr Pro purchase found for this account.";

const SUBTITLE_COPY: &str = "One-time purchase. Yours on this account for good.";
const MULTIPLE_SERVERS_COPY: &str = "Multiple servers";
const ALREADY_PRO_COPY: &str = "You're on Muxr Pro.";

const TITLE_GAP: f64 = spacing::XS;
const SECTION_GAP: f64 = spacing::XXL;
const ROW_GAP: f64 = spacing::SM;
const BENEFIT_MARK: f64 = spacing::XL;
const BENEFIT_MARK_GAP: f64 = spacing::BASE;
/// Button-sized so the sheet does not jump when the offer lands.
const OFFER_SKELETON_WIDTH: f64 = 180.0;
const OFFER_SKELETON_HEIGHT: f64 = 44.0;

#[derive(Clone)]
pub struct ServerLimitContext {
    pub to_forget: Vec<SavedServer>,
    pub on_forget_and_connect: Rc<dyn Fn()>,
}

#[derive(Clone)]
pub struct UpgradeContext {
    pub on_unlocked: Rc<dyn Fn()>,
    pub server_limit: Option<ServerLimitContext>,
}

/// Carries the store's localized price string, never a number this app formats.
pub fn upgrade_label(offer: &ProOffer) -> String {
    format!("Upgrade · {}", offer.display_price)
}

pub fn forget_and_connect_label(to_forget: &[SavedServer]) -> String {
    match to_forget {
        [only] => format!("Forget {} and connect", only.label),
        rows => format!("Forget {} saved servers and connect", rows.len()),
    }
}

/// The product lookup runs once at show time, not from the body, which re-runs on every navigator rebuild.
pub fn show_upgrade_sheet(
    overlays: &NavigatorController<AppState>,
    pro: Arc<ProController>,
    context: UpgradeContext,
) {
    if pro.offer.get_untracked().is_none() {
        let loading = Arc::clone(&pro);
        spawn_local(async move { loading.load_offer().await });
    }
    show_sheet(overlays, move |sheet| {
        upgrade_sheet_body(&pro, sheet, &context)
    });
}

fn upgrade_sheet_body(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
) -> impl View<AppState> + use<> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let entitled = pro.entitlement.get().is_pro();

    // erasure: keep accumulator into Vec<AnyView>
    let mut children = vec![
        any(text(UPGRADE_SHEET_TITLE).style(inked(&theme.type_scale.title_medium, colors.fg))),
        any(gap(TITLE_GAP)),
        any(text(SUBTITLE_COPY).style(inked(&theme.type_scale.body_small, colors.fg_dim))),
        any(gap(SECTION_GAP)),
        any(benefit_row(
            MULTIPLE_SERVERS_COPY,
            LINK,
            &theme.type_scale.body_medium,
            &colors,
        )),
        any(gap(SECTION_GAP)),
    ];

    if entitled {
        children.push(any(
            text(ALREADY_PRO_COPY).style(inked(&theme.type_scale.body_medium, colors.success))
        ));
        children.push(any(gap(SECTION_GAP)));
    } else {
        children.push(any(offer_action(
            pro,
            sheet,
            context,
            inked(&theme.type_scale.body_small, colors.fg_dim),
        )));
        children.push(any(gap(ROW_GAP)));
        children.push(any(restore_action(pro, sheet, context)));
        children.push(any(gap(ROW_GAP)));
        if let Some(limit) = &context.server_limit {
            children.push(any(forget_and_connect_action(sheet, limit)));
            children.push(any(gap(ROW_GAP)));
        }
    }
    children.push(any(not_now_action(sheet)));
    stretch_column(children)
}

fn offer_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
    unavailable_style: TextStyle,
) -> impl View<AppState> {
    let busy = pro.busy.get();
    match (pro.offer.get(), pro.offer_settled.get()) {
        (Some(offer), _) => EitherOf3::A(upgrade_action(pro, sheet, context, &offer, busy)),
        (None, false) => EitherOf3::B(
            row()
                .child(skeleton(OFFER_SKELETON_WIDTH, OFFER_SKELETON_HEIGHT))
                .child(SizedBox::<AppState>(Some(ROW_GAP), None))
                .child(dots_loader())
                .cross_axis(CrossAxisAlignment::Center),
        ),
        (None, true) => EitherOf3::C(text(UNAVAILABLE_COPY).style(unavailable_style)),
    }
}

/// `busy` is mirrored as both halves of disabled: lower-emphasis style and an inert handler.
fn upgrade_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
    offer: &ProOffer,
    busy: bool,
) -> impl View<AppState> + use<> {
    let pro = Arc::clone(pro);
    let sheet = sheet.clone();
    let on_unlocked = Rc::clone(&context.on_unlocked);
    button(upgrade_label(offer), move |_app: &mut AppState| {
        if busy {
            return;
        }
        let pro = Arc::clone(&pro);
        let sheet = sheet.clone();
        let on_unlocked = Rc::clone(&on_unlocked);
        spawn_local(async move {
            match pro.purchase().await {
                PurchaseOutcome::Purchased => {
                    sheet.close_then(move || on_unlocked());
                }
                PurchaseOutcome::Pending => {
                    app_toasts().show(PENDING_TOAST);
                    sheet.close();
                }
                // A dismissed store sheet is not an answer: this one stays up.
                PurchaseOutcome::Cancelled => {}
                PurchaseOutcome::Unavailable => app_toasts().show(UNAVAILABLE_COPY),
                // Every failure is announced once, here; the controller keeps the purchase off the failure bridge.
                PurchaseOutcome::Failed(message) => app_toasts().show(message),
            }
        });
    })
    .style(if busy {
        ButtonStyle::Ghost
    } else {
        ButtonStyle::Primary
    })
}

fn restore_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
) -> impl View<AppState> {
    let pro = Arc::clone(pro);
    let sheet = sheet.clone();
    let on_unlocked = Rc::clone(&context.on_unlocked);
    button(RESTORE_LABEL, move |_app: &mut AppState| {
        let pro = Arc::clone(&pro);
        let sheet = sheet.clone();
        let on_unlocked = Rc::clone(&on_unlocked);
        spawn_local(async move {
            match pro.restore().await {
                RestoreOutcome::Restored => {
                    sheet.close_then(move || on_unlocked());
                }
                RestoreOutcome::Pending => {
                    app_toasts().show(PENDING_TOAST);
                    sheet.close();
                }
                RestoreOutcome::NothingToRestore => app_toasts().show(NOTHING_TO_RESTORE_TOAST),
                RestoreOutcome::Unavailable => app_toasts().show(UNAVAILABLE_COPY),
                // A failure is announced once by the app's failure bridge; a refused overlap says nothing.
                RestoreOutcome::Failed(_) | RestoreOutcome::Busy => {}
            }
        });
    })
    .style(ButtonStyle::Ghost)
}

fn forget_and_connect_action(
    sheet: &SheetHandle,
    limit: &ServerLimitContext,
) -> impl View<AppState> {
    let sheet = sheet.clone();
    let retry = Rc::clone(&limit.on_forget_and_connect);
    button(
        forget_and_connect_label(&limit.to_forget),
        move |_app: &mut AppState| {
            let retry = Rc::clone(&retry);
            sheet.close_then(move || retry());
        },
    )
    .style(ButtonStyle::Danger)
}

fn not_now_action(sheet: &SheetHandle) -> impl View<AppState> {
    let sheet = sheet.clone();
    button(NOT_NOW_LABEL, move |_app: &mut AppState| {
        sheet.close();
    })
    .style(ButtonStyle::Ghost)
}

fn benefit_row(
    label: &str,
    mark: IconSource,
    style: &TextStyle,
    colors: &Palette,
) -> impl View<AppState> {
    row()
        .child(icon(mark).size(BENEFIT_MARK).color(colors.amber))
        .child(SizedBox::<AppState>(Some(BENEFIT_MARK_GAP), None))
        .child(text(label.to_string()).style(inked(style, colors.fg)))
        .cross_axis(CrossAxisAlignment::Center)
}

fn gap(height: f64) -> impl View<AppState> {
    SizedBox::<AppState>(None, Some(height))
}

fn stretch_column(children: Vec<AnyView<AppState>>) -> FlexView<AppState> {
    let children: Vec<FlexChild<AppState>> = children.into_iter().map(inflexible).collect();
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::connect::domain::entities::TrustMode;

    fn row(label: &str) -> SavedServer {
        SavedServer {
            host: "a.example".to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: label.to_string(),
            last_used_ms: 1_700_000_000_000,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        }
    }

    #[test]
    fn the_upgrade_label_carries_the_stores_own_price_string() {
        let offer = ProOffer {
            product_id: "muxr_pro".to_string(),
            title: "Muxr Pro".to_string(),
            description: "Unlimited servers".to_string(),
            display_price: "¥750".to_string(),
        };
        assert_eq!(upgrade_label(&offer), "Upgrade · ¥750");
    }

    #[test]
    fn one_row_to_forget_is_named_and_several_are_counted() {
        assert_eq!(
            forget_and_connect_label(&[row("Home rig")]),
            "Forget Home rig and connect"
        );
        assert_eq!(
            forget_and_connect_label(&[row("Home rig"), row("Office")]),
            "Forget 2 saved servers and connect"
        );
    }
}

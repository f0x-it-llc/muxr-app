//! The one Muxr Pro paywall: a bottom sheet over
//! [`crate::ui::sheet::show_sheet`], raised both by the Connect screen's
//! server-limit gate and by the Settings plan row.
//!
//! Page-agnostic, the `connect` widgets shape: it takes an
//! [`Arc<ProController>`] to read and act through, plus [`UpgradeContext`]'s
//! `Rc` callbacks — never a caller's `Actions` — so the same body composes
//! under either screen and headless in `tests/pro.rs`.
//!
//! Both callbacks are `Rc<dyn Fn()>`, taking **no** `&mut AppState`:
//! [`crate::ui::SheetHandle::close_then`] queues an `FnOnce()` that the Glyph
//! sheet's own `on_close` runs once the staged exit completes, and no
//! `&mut AppState` exists at that point. Neither caller needs one — Settings
//! passes a no-op, and the Connect gate retries through its own captured
//! controller and navigator handles.
//!
//! Every dismissal goes through that one handle, never a bare `pop`: a second
//! tap inside the close beat would otherwise pop the route *under* the sheet
//! (`crate::ui`'s one-shot dismissal gate). Purchased/Restore chain the
//! caller's follow-on onto the exit through `close_then` rather than calling
//! it beside `close`, so it lands after the sheet is actually gone.
//!
//! The store work runs in `frust::spawn_local` bodies capturing the
//! controller, the sheet handle and the callbacks — nothing is resolved from
//! context inside them, whose poller installs no ambient reactive owner.

use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::text::TextStyle;
use frust::icons::LINK;
use frust::{
    AnyView, Axis, ButtonStyle, CrossAxisAlignment, FlexChild, FlexView, Get, GetUntracked,
    IconSource, NavigatorController, SizedBox, any, button, icon, inflexible, spawn_local, text,
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

/// The sheet's own heading — the product, as both stores name it.
pub const UPGRADE_SHEET_TITLE: &str = "Muxr Pro";
/// The always-present second action: a purchase this account already owns.
pub const RESTORE_LABEL: &str = "Restore purchases";
/// The last action, and every dismissal's plain-language name.
pub const NOT_NOW_LABEL: &str = "Not now";
/// Shown in place of the Upgrade button once a lookup has come back with no
/// sellable product, and toasted when a purchase attempt reports the same.
pub const UNAVAILABLE_COPY: &str = "Purchases aren't available on this device.";
/// A purchase the store took but has not settled (bank approval, ask-to-buy):
/// the sheet closes, but nothing is unlocked yet.
pub const PENDING_TOAST: &str = "Purchase pending — you'll be upgraded once it completes.";
/// A restore that found nothing. Deliberately about the *account*, not the
/// device: a restore reaches the store account, not this install.
pub const NOTHING_TO_RESTORE_TOAST: &str = "No Muxr Pro purchase found for this account.";

/// The one-line promise under the title.
const SUBTITLE_COPY: &str = "One-time purchase. Yours on this account for good.";
/// The gated feature, named as the free tier's own rule names it.
const MULTIPLE_SERVERS_COPY: &str = "Multiple servers";
/// The whole body an entitled install gets in place of the offer.
const ALREADY_PRO_COPY: &str = "You're on Muxr Pro.";

/// Title-to-subtitle gap.
const TITLE_GAP: f64 = spacing::XS;
/// The gap between the sheet's three bands (copy, benefits, actions).
const SECTION_GAP: f64 = spacing::XXL;
/// Between stacked action buttons.
const ROW_GAP: f64 = spacing::SM;
/// A benefit row's mark, and the gap between it and the row's text.
const BENEFIT_MARK: f64 = spacing::XL;
const BENEFIT_MARK_GAP: f64 = spacing::BASE;
/// The placeholder standing in for the Upgrade button while the store is
/// still answering — button-sized, so the sheet does not jump when it lands.
const OFFER_SKELETON_WIDTH: f64 = 180.0;
const OFFER_SKELETON_HEIGHT: f64 = 44.0;

/// What the Connect gate adds when the free-tier server rule is what raised
/// this sheet: the rows a connect would have to forget, and the retry to run
/// once the sheet is gone.
#[derive(Clone)]
pub struct ServerLimitContext {
    /// Every remembered server that is not the one being connected to — the
    /// `ServerLimitVerdict::Blocked` payload, unchanged.
    pub to_forget: Vec<SavedServer>,
    /// Forget those rows and dial again. See the [module docs](self) for why
    /// it takes no state.
    pub on_forget_and_connect: Rc<dyn Fn()>,
}

/// Everything the sheet needs from whichever screen raised it.
#[derive(Clone)]
pub struct UpgradeContext {
    /// Run once Pro is actually unlocked — by a landed purchase or by a
    /// restore that found one. See the [module docs](self) for why it takes
    /// no state.
    pub on_unlocked: Rc<dyn Fn()>,
    /// `Some` only for the Connect gate; Settings raises the same sheet with
    /// `None` and gets no forget-and-connect action.
    pub server_limit: Option<ServerLimitContext>,
}

/// The Upgrade button's label, carrying the store's own localized price
/// string — never a number this app formats.
pub fn upgrade_label(offer: &ProOffer) -> String {
    format!("Upgrade · {}", offer.display_price)
}

/// The forget-and-connect action's label: one row is named, several are
/// counted (naming them all would not fit a button).
pub fn forget_and_connect_label(to_forget: &[SavedServer]) -> String {
    match to_forget {
        [only] => format!("Forget {} and connect", only.label),
        rows => format!("Forget {} saved servers and connect", rows.len()),
    }
}

/// Raise the paywall over `overlays`, kicking off the product lookup when
/// nothing has been looked up yet — once, at show time, rather than from the
/// body, which re-runs on every navigator rebuild.
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

/// The whole sheet body. Every controller read here is tracked, so a landed
/// offer, a flipped entitlement or the busy guard rebuilds this page.
fn upgrade_sheet_body(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
) -> AnyView<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let entitled = pro.entitlement.get().is_pro();

    let mut children = vec![
        any(text(UPGRADE_SHEET_TITLE).style(inked(&theme.type_scale.title_medium, colors.fg))),
        gap(TITLE_GAP),
        any(text(SUBTITLE_COPY).style(inked(&theme.type_scale.body_small, colors.fg_dim))),
        gap(SECTION_GAP),
        benefit_row(
            MULTIPLE_SERVERS_COPY,
            LINK,
            &theme.type_scale.body_medium,
            &colors,
        ),
        gap(SECTION_GAP),
    ];

    if entitled {
        children.push(any(
            text(ALREADY_PRO_COPY).style(inked(&theme.type_scale.body_medium, colors.success))
        ));
        children.push(gap(SECTION_GAP));
    } else {
        children.push(offer_action(
            pro,
            sheet,
            context,
            inked(&theme.type_scale.body_small, colors.fg_dim),
        ));
        children.push(gap(ROW_GAP));
        children.push(restore_action(pro, sheet, context));
        children.push(gap(ROW_GAP));
        if let Some(limit) = &context.server_limit {
            children.push(forget_and_connect_action(sheet, limit));
            children.push(gap(ROW_GAP));
        }
    }
    children.push(not_now_action(sheet));
    any(stretch_column(children))
}

/// The offer band's three states: still looking (a Glyph skeleton beside its
/// dots loader), nothing to sell (the refusal copy, and no button at all), or
/// the real Upgrade button.
fn offer_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
    unavailable_style: TextStyle,
) -> AnyView<AppState> {
    let busy = pro.busy.get();
    match (pro.offer.get(), pro.offer_settled.get()) {
        (Some(offer), _) => upgrade_action(pro, sheet, context, &offer, busy),
        (None, false) => any(FlexView::new(
            Axis::Horizontal,
            vec![
                inflexible(skeleton(OFFER_SKELETON_WIDTH, OFFER_SKELETON_HEIGHT)),
                inflexible(SizedBox::<AppState>(Some(ROW_GAP), None)),
                inflexible(dots_loader()),
            ],
        )
        .cross_axis(CrossAxisAlignment::Center)),
        (None, true) => any(text(UNAVAILABLE_COPY).style(unavailable_style)),
    }
}

/// The buy action. `busy` is the single-flight guard the controller already
/// enforces, mirrored here as both halves of "disabled": a lower-emphasis
/// style and an inert handler (`ui::dialogs`' own idiom).
fn upgrade_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
    offer: &ProOffer,
    busy: bool,
) -> AnyView<AppState> {
    let pro = Arc::clone(pro);
    let sheet = sheet.clone();
    let on_unlocked = Rc::clone(&context.on_unlocked);
    any(button(upgrade_label(offer), move |_app: &mut AppState| {
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
                // Every failure is this sheet's to announce, once — the
                // controller keeps the purchase off the app's failure bridge.
                PurchaseOutcome::Failed(message) => app_toasts().show(message),
            }
        });
    })
    .style(if busy {
        ButtonStyle::Ghost
    } else {
        ButtonStyle::Primary
    }))
}

/// Restore, which unlocks exactly when the store account already owns Pro —
/// otherwise it says so and leaves the sheet up to buy from.
fn restore_action(
    pro: &Arc<ProController>,
    sheet: &SheetHandle,
    context: &UpgradeContext,
) -> AnyView<AppState> {
    let pro = Arc::clone(pro);
    let sheet = sheet.clone();
    let on_unlocked = Rc::clone(&context.on_unlocked);
    any(button(RESTORE_LABEL, move |_app: &mut AppState| {
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
                // A failure is the app's failure bridge to announce, once; a
                // refused overlap says nothing at all, as a purchase's
                // `Cancelled` doesn't. Neither is an empty account.
                RestoreOutcome::Failed(_) | RestoreOutcome::Busy => {}
            }
        });
    })
    .style(ButtonStyle::Ghost))
}

/// The Connect gate's own way out: drop the rows the free tier cannot keep
/// and dial again, chained onto the sheet's exit.
fn forget_and_connect_action(sheet: &SheetHandle, limit: &ServerLimitContext) -> AnyView<AppState> {
    let sheet = sheet.clone();
    let retry = Rc::clone(&limit.on_forget_and_connect);
    any(button(
        forget_and_connect_label(&limit.to_forget),
        move |_app: &mut AppState| {
            let retry = Rc::clone(&retry);
            sheet.close_then(move || retry());
        },
    )
    .style(ButtonStyle::Danger))
}

/// The plain dismissal.
fn not_now_action(sheet: &SheetHandle) -> AnyView<AppState> {
    let sheet = sheet.clone();
    any(button(NOT_NOW_LABEL, move |_app: &mut AppState| {
        sheet.close();
    })
    .style(ButtonStyle::Ghost))
}

/// One benefit line: a `frust::icons` vector mark and its text.
fn benefit_row(
    label: &str,
    mark: IconSource,
    style: &TextStyle,
    colors: &Palette,
) -> AnyView<AppState> {
    any(FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(icon(mark).size(BENEFIT_MARK).color(colors.amber)),
            inflexible(SizedBox::<AppState>(Some(BENEFIT_MARK_GAP), None)),
            inflexible(text(label.to_string()).style(inked(style, colors.fg))),
        ],
    )
    .cross_axis(CrossAxisAlignment::Center))
}

/// A vertical spacer between bands.
fn gap(height: f64) -> AnyView<AppState> {
    any(SizedBox::<AppState>(None, Some(height)))
}

/// A column whose children each fill the sheet's width.
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

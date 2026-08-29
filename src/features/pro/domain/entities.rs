//! `pro` domain entities — the entitlement vocabulary, the store's offer and
//! purchase outcomes, and the free-tier server rule. Framework-free.
//!
//! [`ProFeature`] names what Pro unlocks; only [`ProFeature::MultipleServers`]
//! exists today, and `Notifications` joins it once that feature ships.
//!
//! [`server_limit_verdict`] answers in `connect`'s own [`SavedServer`] rows
//! rather than a copied shape: a `domain` → sibling-`domain` edge, the narrow
//! widening `docs/FEATURES_ARCHITECTURE.md` sanctions. A rule about how many
//! servers a free install may keep has to name the *rows* it would forget, and
//! one store owns those rows.

use crate::features::connect::domain::entities::SavedServer;

/// The one product this app sells, as both stores know it.
pub const PRO_PRODUCT_ID: &str = "muxr_pro";

/// What this install is entitled to. [`Entitlement::Pending`] is a purchase
/// the store has taken but not settled (bank approval, ask-to-buy): it grants
/// nothing yet, so every rule below treats it exactly as [`Entitlement::Free`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entitlement {
    Free,
    Pro,
    Pending,
}

impl Entitlement {
    /// Whether the Pro features are unlocked — `true` only for
    /// [`Entitlement::Pro`].
    pub fn is_pro(self) -> bool {
        matches!(self, Entitlement::Pro)
    }

    /// The badge text a screen prints for this state.
    pub fn label(self) -> &'static str {
        match self {
            Entitlement::Free => "Free",
            Entitlement::Pro => "Pro",
            Entitlement::Pending => "Pending",
        }
    }
}

/// A capability the paywall gates. Exhaustive by design: adding a member is
/// what forces every gate site to decide about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProFeature {
    /// Remembering more than one `muxrd` server at a time.
    MultipleServers,
}

impl ProFeature {
    /// Whether this feature needs a Pro entitlement.
    pub fn requires_pro(self) -> bool {
        match self {
            ProFeature::MultipleServers => true,
        }
    }
}

/// The store's own description of [`PRO_PRODUCT_ID`], as the paywall shows it.
/// [`Self::display_price`] is the store's localized string, never a number this
/// app formats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProOffer {
    pub product_id: String,
    pub title: String,
    pub description: String,
    pub display_price: String,
}

/// How a purchase attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurchaseOutcome {
    /// Bought and finalized — the entitlement is live.
    Purchased,
    /// Taken but not settled yet; nothing is unlocked until it is.
    Pending,
    /// The user dismissed the store sheet.
    Cancelled,
    /// The store cannot sell here (no billing service, unknown SKU, desktop).
    Unavailable,
    /// The store reported an error. Carries a short user-facing message only —
    /// never a token, receipt, or raw store payload.
    Failed(String),
}

/// What a restore found: the store's answer, or the fact that no store was
/// asked. A bare `Entitlement` cannot say the second, and reading `Free` off
/// a store that was never reached tells a paying user the account owns
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreResult {
    /// The store answered; this is what the account owns now.
    Found(Entitlement),
    /// No store to ask here (no billing service, a cache that would not
    /// open) — nothing was checked, so nothing is known about the account.
    Unavailable,
}

/// Whether a free install may connect to (and remember) one more server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerLimitVerdict {
    Allowed,
    /// Blocked until the listed rows are forgotten — every remembered server
    /// that is not the one being connected to.
    Blocked {
        to_forget: Vec<SavedServer>,
    },
}

/// The free tier's one-server rule: Pro is unlimited; a free install may keep
/// exactly the server it is connecting to. Remembering nothing yet is allowed
/// (the row is about to be written), as is reconnecting to the single row
/// already stored; anything else is [`ServerLimitVerdict::Blocked`], naming
/// every other row so the caller can offer to forget them.
///
/// [`Entitlement::Pending`] counts as free — an unsettled purchase unlocks
/// nothing.
pub fn server_limit_verdict(
    entitlement: Entitlement,
    saved: &[SavedServer],
    target_endpoint: &str,
) -> ServerLimitVerdict {
    if entitlement.is_pro() || saved.is_empty() {
        return ServerLimitVerdict::Allowed;
    }
    if saved.len() == 1 && saved[0].endpoint() == target_endpoint {
        return ServerLimitVerdict::Allowed;
    }
    ServerLimitVerdict::Blocked {
        to_forget: saved
            .iter()
            .filter(|server| server.endpoint() != target_endpoint)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::connect::domain::entities::TrustMode;

    const A: &str = "a.example:50051";
    const B: &str = "b.example:50051";

    fn row(endpoint: &str) -> SavedServer {
        let (host, port) = endpoint.rsplit_once(':').expect("host:port");
        SavedServer {
            host: host.to_string(),
            port: port.parse().expect("a port"),
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms: 1_700_000_000_000,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        }
    }

    fn forgets(verdict: &ServerLimitVerdict) -> Vec<String> {
        match verdict {
            ServerLimitVerdict::Allowed => Vec::new(),
            ServerLimitVerdict::Blocked { to_forget } => {
                to_forget.iter().map(SavedServer::endpoint).collect()
            }
        }
    }

    #[test]
    fn only_pro_is_pro_and_every_state_prints_its_own_badge() {
        assert!(Entitlement::Pro.is_pro());
        assert!(!Entitlement::Free.is_pro());
        assert!(!Entitlement::Pending.is_pro());
        assert_eq!(Entitlement::Free.label(), "Free");
        assert_eq!(Entitlement::Pro.label(), "Pro");
        assert_eq!(Entitlement::Pending.label(), "Pending");
    }

    #[test]
    fn the_multiple_servers_feature_is_gated() {
        assert!(ProFeature::MultipleServers.requires_pro());
    }

    #[test]
    fn pro_is_allowed_whatever_it_has_remembered() {
        let saved = vec![row(A), row(B)];
        assert_eq!(
            server_limit_verdict(Entitlement::Pro, &saved, "c.example:1"),
            ServerLimitVerdict::Allowed
        );
    }

    #[test]
    fn a_free_install_with_nothing_saved_is_allowed() {
        assert_eq!(
            server_limit_verdict(Entitlement::Free, &[], A),
            ServerLimitVerdict::Allowed
        );
    }

    #[test]
    fn a_free_install_may_reconnect_to_its_one_saved_server() {
        assert_eq!(
            server_limit_verdict(Entitlement::Free, &[row(A)], A),
            ServerLimitVerdict::Allowed
        );
    }

    #[test]
    fn a_free_install_is_blocked_on_a_second_server_and_told_what_to_forget() {
        let verdict = server_limit_verdict(Entitlement::Free, &[row(A)], B);
        assert_eq!(forgets(&verdict), vec![A.to_string()]);
    }

    #[test]
    fn a_blocked_verdict_never_lists_the_server_being_connected_to() {
        let verdict = server_limit_verdict(Entitlement::Free, &[row(A), row(B)], A);
        assert_eq!(forgets(&verdict), vec![B.to_string()]);
    }

    #[test]
    fn a_pending_purchase_is_judged_exactly_as_free() {
        assert_eq!(
            server_limit_verdict(Entitlement::Pending, &[row(A)], A),
            server_limit_verdict(Entitlement::Free, &[row(A)], A)
        );
        assert_eq!(
            server_limit_verdict(Entitlement::Pending, &[row(A)], B),
            server_limit_verdict(Entitlement::Free, &[row(A)], B)
        );
    }
}

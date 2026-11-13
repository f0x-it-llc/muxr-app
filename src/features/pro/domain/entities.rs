//! Entitlement vocabulary, store offer and purchase outcomes, and the free-tier server rule.
//! `server_limit_verdict` reads `connect`'s `SavedServer` rows: a sanctioned domain-to-domain edge.

use crate::features::connect::domain::entities::SavedServer;

pub const PRO_PRODUCT_ID: &str = "muxr_pro";

/// `Pending` is a purchase taken but not settled (bank approval, ask-to-buy): it grants nothing and counts as Free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entitlement {
    Free,
    Pro,
    Pending,
}

impl Entitlement {
    pub fn is_pro(self) -> bool {
        matches!(self, Entitlement::Pro)
    }

    pub fn label(self) -> &'static str {
        match self {
            Entitlement::Free => "Free",
            Entitlement::Pro => "Pro",
            Entitlement::Pending => "Pending",
        }
    }
}

/// Exhaustive by design: adding a member forces every gate site to decide about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProFeature {
    MultipleServers,
}

impl ProFeature {
    pub fn requires_pro(self) -> bool {
        match self {
            ProFeature::MultipleServers => true,
        }
    }
}

/// `display_price` is the store's localized string, never formatted by this app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProOffer {
    pub product_id: String,
    pub title: String,
    pub description: String,
    pub display_price: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurchaseOutcome {
    Purchased,
    Pending,
    Cancelled,
    Unavailable,
    /// Short user-facing message only; never a token, receipt, or raw store payload.
    Failed(String),
}

/// `Unavailable` means no store was asked; reading Free off it would tell a paying user the account owns nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreResult {
    Found(Entitlement),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerLimitVerdict {
    Allowed,
    Blocked { to_forget: Vec<SavedServer> },
}

/// Pro is unlimited; a free install may keep exactly the server it is connecting to.
/// `Blocked` names every other row so the caller can offer to forget them. `Pending` counts as free.
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

pub fn row_is_beyond_free_tier(
    entitlement: Entitlement,
    saved: &[SavedServer],
    endpoint: &str,
) -> bool {
    matches!(
        server_limit_verdict(entitlement, saved, endpoint),
        ServerLimitVerdict::Blocked { .. }
    )
}

/// Gate taken before any endpoint is known, so any remembered row blocks. `Pending` counts as free.
pub fn new_server_verdict(entitlement: Entitlement, saved: &[SavedServer]) -> ServerLimitVerdict {
    if entitlement.is_pro() || saved.is_empty() {
        return ServerLimitVerdict::Allowed;
    }
    ServerLimitVerdict::Blocked {
        to_forget: saved.to_vec(),
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
    fn a_free_install_with_one_row_tags_nothing() {
        assert!(!row_is_beyond_free_tier(Entitlement::Free, &[row(A)], A));
    }

    #[test]
    fn a_free_install_with_two_rows_tags_both() {
        let saved = vec![row(A), row(B)];
        assert!(row_is_beyond_free_tier(Entitlement::Free, &saved, A));
        assert!(row_is_beyond_free_tier(Entitlement::Free, &saved, B));
    }

    #[test]
    fn pro_and_an_empty_list_tag_nothing() {
        let saved = vec![row(A), row(B)];
        assert!(!row_is_beyond_free_tier(Entitlement::Pro, &saved, A));
        assert!(!row_is_beyond_free_tier(Entitlement::Free, &[], A));
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

    #[test]
    fn pro_may_add_a_server_whatever_it_has_remembered() {
        assert_eq!(
            new_server_verdict(Entitlement::Pro, &[row(A), row(B)]),
            ServerLimitVerdict::Allowed
        );
    }

    #[test]
    fn a_free_install_with_nothing_saved_may_add_its_first_server() {
        assert_eq!(
            new_server_verdict(Entitlement::Free, &[]),
            ServerLimitVerdict::Allowed
        );
    }

    #[test]
    fn a_free_install_with_one_saved_server_is_blocked_and_told_which_to_forget() {
        let verdict = new_server_verdict(Entitlement::Free, &[row(A)]);
        assert_eq!(forgets(&verdict), vec![A.to_string()]);
    }

    #[test]
    fn a_pending_purchase_is_judged_exactly_as_free_for_a_new_server_too() {
        assert_eq!(
            new_server_verdict(Entitlement::Pending, &[row(A)]),
            new_server_verdict(Entitlement::Free, &[row(A)])
        );
    }
}

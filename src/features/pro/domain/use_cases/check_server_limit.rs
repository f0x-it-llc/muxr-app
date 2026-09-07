//! [`check_server_limit`] — the free-tier server rule as a use case seam.
//!
//! A plain function rather than a `clean_signals::UseCase`, for the reason
//! `connect`'s two parses are: the verdict is pure and synchronous, with no
//! repository to inject and nothing to cancel. The rule itself lives on
//! [`server_limit_verdict`]; this is the name call sites reach for, so a later
//! rule that *does* need the store moves in here without any of them moving.

use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::domain::entities::{
    Entitlement, ServerLimitVerdict, new_server_verdict, server_limit_verdict,
};

/// Whether `entitlement` may connect to `target_endpoint` while `saved` is
/// remembered — see [`server_limit_verdict`].
pub fn check_server_limit(
    entitlement: Entitlement,
    saved: &[SavedServer],
    target_endpoint: &str,
) -> ServerLimitVerdict {
    server_limit_verdict(entitlement, saved, target_endpoint)
}

/// Whether `entitlement` may add a server it has not named yet, while
/// `saved` is remembered — see [`new_server_verdict`].
pub fn check_new_server_limit(
    entitlement: Entitlement,
    saved: &[SavedServer],
) -> ServerLimitVerdict {
    new_server_verdict(entitlement, saved)
}

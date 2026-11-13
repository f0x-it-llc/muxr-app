//! Pure synchronous free-tier rule, so a plain function rather than a `UseCase`.

use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::domain::entities::{
    Entitlement, ServerLimitVerdict, new_server_verdict, server_limit_verdict,
};

pub fn check_server_limit(
    entitlement: Entitlement,
    saved: &[SavedServer],
    target_endpoint: &str,
) -> ServerLimitVerdict {
    server_limit_verdict(entitlement, saved, target_endpoint)
}

pub fn check_new_server_limit(
    entitlement: Entitlement,
    saved: &[SavedServer],
) -> ServerLimitVerdict {
    new_server_verdict(entitlement, saved)
}

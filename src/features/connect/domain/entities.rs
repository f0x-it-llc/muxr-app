//! `connect` domain entities — the feature's plain-data types (framework-free,
//! per [`crate::features::connect::domain`]'s module doc).
//!
//! [`SavedServer`] is the one entity today: a `muxrd` endpoint the user has
//! connected to before, rendered by the Connect screen's saved-servers list
//! (`docs/redesign.html`'s `.saved-hdr`/`.saved-row` block) and tapped to refill
//! the form.
//!
//! # Trust-mode reuse (a documented layering tension)
//!
//! [`TrustMode`] is **not** re-minted here: it is
//! [`crate::connection::TrustMode`], the same enum
//! [`crate::connection::ConnectTarget`] carries, so a saved row's posture flows
//! into a connect request with no mapping layer. `crate::connection` is an
//! app-level module beside `crate::failure` (the composition root's own
//! vocabulary), *not* a sibling feature — but it is also not on
//! `docs/ARCHITECTURE.md`'s literal allow-list for `domain` (which names only
//! "this feature's own `domain` + `crate::failure`"), and the module hosting the
//! enum does import `frust::` for its controller half. The enum itself is a
//! `Copy`, three-variant, dependency-free data type; duplicating it here plus a
//! `From` mapping would buy a stricter reading of the rule at the cost of two
//! ways to spell one concept. Reuse wins; the allow-list is the thing that
//! wants widening.

pub use crate::connection::TrustMode;

/// A `muxrd` endpoint the user has connected to before.
///
/// Mock-era: nothing persists these yet (see
/// [`crate::features::connect::data::repositories`]), and `last_used` is a
/// pre-formatted human label rather than a timestamp — muxr has no clock
/// abstraction, and a `String` keeps this layer free of a date crate. A real
/// persistence layer swaps it for an instant and formats at the presentation
/// edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    /// The host to dial (no scheme, no port).
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// The TLS posture this endpoint was last trusted under.
    pub trust_mode: TrustMode,
    /// A human label for the rig ([`crate::connection::ConnectTarget::label`]'s
    /// source).
    pub label: String,
    /// When this endpoint was last connected to, pre-formatted (see the type
    /// docs' mock-era note).
    pub last_used: String,
    /// Whether the stored credential only grants read-only access — rendered as
    /// a badge on the row, and a real constraint once sessions can be mutated.
    pub read_only: bool,
}

impl SavedServer {
    /// `host:port`, the way both the row and
    /// [`crate::connection::ServerInfo::endpoint`] spell it. Also this row's
    /// identity for [`super::repositories::SavedServersRepository::remove`] —
    /// a host/port pair is unique per saved server.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_the_host_and_port_joined() {
        let server = SavedServer {
            host: "100.71.31.57".to_string(),
            port: 50051,
            trust_mode: TrustMode::TrustOnFirstUse,
            label: "Home rig".to_string(),
            last_used: "2 hours ago".to_string(),
            read_only: false,
        };
        assert_eq!(server.endpoint(), "100.71.31.57:50051");
    }
}

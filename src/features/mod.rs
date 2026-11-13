//! Feature slices, each `{domain, data, presentation}`. [`pro`] and [`servers`] are routeless:
//! `pro` owns the entitlement other features gate on, `servers` the server switcher's model.

pub mod connect;
pub mod pro;
pub mod servers;
pub mod sessions;
pub mod settings;
pub mod terminal;

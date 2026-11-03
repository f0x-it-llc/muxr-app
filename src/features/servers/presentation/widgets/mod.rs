//! The server switcher's widgets — page-agnostic, vectors only (no font
//! glyph), callbacks as `Rc<dyn Fn()>` the shell host closes over.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`faces`] | The non-live card faces: cached list, never / locked / failed centred glyph and two lines. The live face is the real shell page the switcher scales, so it is not drawn here |
//! | [`label_row`] | The row above a card: dot or lock, host, trust badge — used as `peer_card(face).label(row)` and `.live_label(row)` |
//! | [`bar`] | The stage's action bar over frust-glyph's `switcher_bar` and `switcher_chip` |

pub mod bar;
pub mod faces;
pub mod label_row;

pub use bar::{ADD_LABEL, CLOSE_LABEL, LOCK_LABEL, servers_bar};
pub use faces::server_face;
pub use label_row::label_row;

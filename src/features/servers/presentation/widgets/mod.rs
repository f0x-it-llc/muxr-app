//! The server switcher's widgets: page-agnostic, vectors only, callbacks as `Rc<dyn Fn()>` the shell host closes over.
//! [`faces`] (non-live card faces; the live face is the real shell page), [`label_row`] (dot or lock, host, trust badge), [`bar`] (the stage's action bar).

pub mod bar;
pub mod faces;
pub mod label_row;

pub use bar::{ADD_LABEL, CLOSE_LABEL, LOCK_LABEL, servers_bar};
pub use faces::server_face;
pub use label_row::label_row;

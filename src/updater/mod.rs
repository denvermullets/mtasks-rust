//! App self-update (JAIT project 320).
//!
//! Pure, in-process logic only: the Release manifest (`manifest`) and the
//! updater state machine (`machine`). The system edge (HTTP, ditto, codesign,
//! the swap) is carried out by the app from the machine's effects.

pub mod download;
pub mod edge;
pub mod machine;
pub mod manifest;
pub mod install;
pub mod staging;
pub mod swap;
pub mod verify;

//! Domain logic for the smart clock. No UI code lives here.

pub mod command;
pub mod store;

pub use store::{Store, StoreError};

//! Domain logic for the smart clock. No UI code lives here.

pub mod actions;
pub mod command;
pub mod model;
pub mod store;

pub use store::{Store, StoreError};

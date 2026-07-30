//! `greeting` presentation layer (huddle clean-architecture refactor, task
//! 07's `--arch clean-signals` scaffold): [`controllers::GreetingController`]
//! (the view model) and [`pages`] (the page-hosting functions
//! `crate::MuxrApp` — this scaffold's
//! composition root — delegates to). Imports domain only, never `data/`.

pub mod controllers;
pub mod pages;

pub use controllers::GreetingController;
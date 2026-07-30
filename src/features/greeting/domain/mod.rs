//! `greeting` domain — the [`GreetingRepository`] contract and its
//! [`LoadGreeting`] use case (huddle clean-architecture refactor's
//! `--arch clean-signals` scaffold). Imports nothing from `data/` or
//! `presentation/`, and no `frust` widget/reactive types — only
//! `crate::failure` plus `clean_signals`.

pub mod repositories;
pub mod use_cases;

pub use repositories::GreetingRepository;
pub use use_cases::LoadGreeting;
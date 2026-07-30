//! The `greeting` feature's use cases (huddle clean-architecture refactor's
//! `--arch clean-signals` scaffold) — one for now:
//! [`LoadGreeting`], the `<Verb><Object>`-named async loader.

pub mod load_greeting;

pub use load_greeting::LoadGreeting;
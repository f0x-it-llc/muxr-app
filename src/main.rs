//! Desktop preview entry point.
//!
//! Never hand-edit this file: `frust::app!` (in `lib.rs`) generates the
//! hidden `__frust_main` this one line calls — add screens/state to
//! `lib.rs` instead.

// On Android the app is driven by the JNI bridge (`frust::app!` in
// `lib.rs`), not this desktop preview binary — and `__frust_main` is
// compiled out for the Android target. Gate the desktop `main` accordingly,
// and provide an empty `main` on Android so this crate's bin target still
// compiles when `cargo ndk` builds every target for `aarch64-linux-android`.
#[cfg(not(target_os = "android"))]
fn main() {
    muxr::__frust_main();
}

#[cfg(target_os = "android")]
fn main() {}
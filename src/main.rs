//! Entry point only: `frust::app!` in `lib.rs` generates `__frust_main`.

// Android runs through the JNI bridge; `__frust_main` is compiled out there.
#[cfg(not(target_os = "android"))]
fn main() {
    muxr::__frust_main();
}

#[cfg(target_os = "android")]
fn main() {}

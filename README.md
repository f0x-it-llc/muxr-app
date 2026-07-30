# muxr

A Frust app created with `frust create`.

## Theming

This app uses the **Glyph design system** by default, which automatically
adapts to the device's light/dark preference. To opt back to Material or
Cupertino:

```rust
use frust::{Component, set_app_theme, Theme};

impl Component for MyAppApp {
    fn build(&self, state: &mut AppState) -> AnyView<AppState> {
        // Material 3 (M3)
        set_app_theme(Theme::m3_baseline());

        // Or Cupertino
        set_app_theme(Theme::cupertino_baseline());
    }
}
```

## Custom Fonts

By default, Frust includes IBM Plex Mono and Space Mono fonts. To drop bundled fonts:

```toml
# Cargo.toml
frust = { ..., default-features = false, features = ["..."] }
```

Then register your own at app startup:

```rust
use frust::register_app_fonts;

fn init() {
    register_app_fonts(&[
        ("MyFont", include_bytes!("../assets/MyFont.ttf")),
    ]);
}
```

## Theme Customization

Customize colors, motion, and spacing using the builder:

```rust
use frust::{ColorScheme, Theme};

let custom = Theme::builder()
    .light_scheme(ColorScheme {
        // ... customize roles
    })
    .build();
set_app_theme(custom);
```

See `frust-theme`'s docs in the Frust repo for the full token reference.
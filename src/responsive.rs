//! Tablet-aware responsive sizing rules.
//!
//! Responsive layout in Muxr uses the **shortest-side rule**: structure depends on
//! `min(width, height)` rather than orientation. This keeps layouts stable during
//! rotation — a device that rotates from portrait to landscape does not flip a
//! sidebar to a drawer or vice versa, even when the window's absolute dimensions
//! cross the breakpoint. This follows Material 3's
//! [window-size-class](https://m3.material.io/foundations/layout/applying-layout/window-size-classes)
//! design and is confirmed in this app by docs/tablet.html's size-class table.
//!
//! The 600 dp compact/medium boundary comes from Material 3 and the Android
//! framework (see Material 3 window-size-class docs and the docs/tablet.html
//! reference design). When `min(width, height) >= 600`, the device is treated as
//! a tablet, and broader layouts (e.g., a docked sidebar) may be used. When below
//! 600, layouts stay compact (stacked screens, drawer-based navigation).
//!
//! A resize that crosses a breakpoint causes a rebuild on the next frame. The
//! frame driver at `frust-shell-ios/src/app/frame.rs:348` runs every frame and
//! calls `RenderRoot::rebuild` on each vsync, and `frame_gate.rs:483` opens a
//! rebuild pass on size changes. Together, this means a layout change becomes
//! visible within one frame after the size change arrives.
//!
//! # Full-width band
//!
//! The `WidthBand` enum controls secondary layout within size classes: when
//! `width >= 840`, a "Full" band allows multi-column layouts to spread across
//! the whole screen. Below that, a "Collapsed" band keeps panels grouped or
//! overlaid. The band does not cause a navigation restructure or rotation-flipping
//! behavior — it only affects panel placement within the chosen structure.

/// Material 3 compact/medium window-size-class boundary
/// (<https://m3.material.io/foundations/layout/applying-layout/window-size-classes>).
pub const WIDE_BREAKPOINT_PX: f64 = 600.0;

/// Tablet size class threshold: the shortest side of the window.
///
/// When `min(width, height) >= TABLET_MIN_SHORT_SIDE_PX`, the device enters
/// tablet layout. Below this, layouts stay compact.
pub const TABLET_MIN_SHORT_SIDE_PX: f64 = 600.0;

/// Full-width band threshold: the width of the window.
///
/// When `width >= FULL_BAND_MIN_WIDTH_PX`, a "Full" band allows broader layouts.
/// Below this, a "Collapsed" band constrains panels.
pub const FULL_BAND_MIN_WIDTH_PX: f64 = 840.0;

/// Device size class, determined by the shortest side of the window.
///
/// Tablets and larger devices use structured layouts with sidebars or
/// multi-column grids. Compact phones constrain layout to a single primary
/// column or drawer-based navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeClass {
    /// Compact layout: `min(width, height) < 600.0`.
    Compact,
    /// Tablet layout: `min(width, height) >= 600.0`.
    Tablet,
}

/// Horizontal panel layout band.
///
/// Controls whether secondary panels (like a sidebar) occupy the full remaining
/// width or are constrained and centered. Does not trigger a navigation
/// restructure; only affects panel grouping and alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthBand {
    /// Panels are constrained or collapsed: `width < 840.0`.
    Collapsed,
    /// Panels spread to full width: `width >= 840.0`.
    Full,
}

/// Complete responsive layout rule: size class, width band, and legacy clamp flag.
///
/// Each screen reads `Responsive` computed from the current [`frust::WindowMetrics`]
/// and branches its layout accordingly. All three rules are independent:
/// - `class` determines structure (tablet sidebar vs. compact drawer)
/// - `band` determines panel width within that structure
/// - `wide_clamp` gates the legacy phone-landscape width clamp for backward
///   compatibility (same rule today: `width >= 600`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Responsive {
    /// Size class from the shortest side of the window.
    pub class: SizeClass,
    /// Width band for secondary panels.
    pub band: WidthBand,
    /// Legacy phone-landscape clamp: `width >= 600.0`.
    ///
    /// When true, screens clamp their width on phones in landscape. This flag
    /// preserves the current UI for phone users even as tablet layouts expand.
    pub wide_clamp: bool,
}

impl Responsive {
    /// Compute a [`Responsive`] rule from window metrics.
    ///
    /// When metrics are `None` (e.g., in a test harness with no shell or a
    /// desktop preview that has not reported size), this returns the compact,
    /// collapsed, non-clamped default (the same branch as a genuinely narrow
    /// phone in portrait).
    ///
    /// # Arguments
    ///
    /// * `metrics` - The current [`frust::WindowMetrics`], or `None` if no shell
    ///   has published size yet.
    pub fn of(metrics: Option<frust::WindowMetrics>) -> Responsive {
        match metrics {
            Some(m) => {
                let size = m.size;
                let min_side = size.width.min(size.height);
                let class = if min_side >= TABLET_MIN_SHORT_SIDE_PX {
                    SizeClass::Tablet
                } else {
                    SizeClass::Compact
                };
                let band = if size.width >= FULL_BAND_MIN_WIDTH_PX
                    && min_side >= TABLET_MIN_SHORT_SIDE_PX
                {
                    WidthBand::Full
                } else {
                    WidthBand::Collapsed
                };
                let wide_clamp = size.width >= WIDE_BREAKPOINT_PX;
                Responsive {
                    class,
                    band,
                    wide_clamp,
                }
            }
            None => Responsive {
                class: SizeClass::Compact,
                band: WidthBand::Collapsed,
                wide_clamp: false,
            },
        }
    }

    /// Check if this is a tablet-class layout.
    pub fn is_tablet(self) -> bool {
        self.class == SizeClass::Tablet
    }

    /// Check if this is a full-width band layout.
    pub fn is_full(self) -> bool {
        self.band == WidthBand::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::WindowMetrics;
    use frust::authoring::WindowInsets;
    use kurbo::Size;

    fn make_metrics(width: f64, height: f64) -> WindowMetrics {
        WindowMetrics::new(Size::new(width, height), 1.0, WindowInsets::default())
    }

    #[test]
    fn test_phone_portrait_390x844() {
        let r = Responsive::of(Some(make_metrics(390.0, 844.0)));
        assert_eq!(r.class, SizeClass::Compact);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(!r.wide_clamp);
    }

    #[test]
    fn test_phone_landscape_844x390() {
        let r = Responsive::of(Some(make_metrics(844.0, 390.0)));
        assert_eq!(r.class, SizeClass::Compact);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(r.wide_clamp);
    }

    #[test]
    fn test_exact_tablet_threshold_600x600() {
        let r = Responsive::of(Some(make_metrics(600.0, 600.0)));
        assert_eq!(r.class, SizeClass::Tablet);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(r.wide_clamp);
    }

    #[test]
    fn test_tablet_portrait_768x1024() {
        let r = Responsive::of(Some(make_metrics(768.0, 1024.0)));
        assert_eq!(r.class, SizeClass::Tablet);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(r.wide_clamp);
    }

    #[test]
    fn test_tablet_landscape_1024x720() {
        let r = Responsive::of(Some(make_metrics(1024.0, 720.0)));
        assert_eq!(r.class, SizeClass::Tablet);
        assert_eq!(r.band, WidthBand::Full);
        assert!(r.wide_clamp);
    }

    #[test]
    fn test_ipad_split_view_half_507x834() {
        let r = Responsive::of(Some(make_metrics(507.0, 834.0)));
        assert_eq!(r.class, SizeClass::Compact);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(!r.wide_clamp);
    }

    #[test]
    fn test_tablet_medium_678x834() {
        let r = Responsive::of(Some(make_metrics(678.0, 834.0)));
        assert_eq!(r.class, SizeClass::Tablet);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(r.wide_clamp);
    }

    #[test]
    fn test_none_metrics() {
        let r = Responsive::of(None);
        assert_eq!(r.class, SizeClass::Compact);
        assert_eq!(r.band, WidthBand::Collapsed);
        assert!(!r.wide_clamp);
    }

    #[test]
    fn test_is_tablet_true() {
        let r = Responsive {
            class: SizeClass::Tablet,
            band: WidthBand::Collapsed,
            wide_clamp: false,
        };
        assert!(r.is_tablet());
    }

    #[test]
    fn test_is_tablet_false() {
        let r = Responsive {
            class: SizeClass::Compact,
            band: WidthBand::Collapsed,
            wide_clamp: false,
        };
        assert!(!r.is_tablet());
    }

    #[test]
    fn test_is_full_true() {
        let r = Responsive {
            class: SizeClass::Tablet,
            band: WidthBand::Full,
            wide_clamp: true,
        };
        assert!(r.is_full());
    }

    #[test]
    fn test_is_full_false() {
        let r = Responsive {
            class: SizeClass::Compact,
            band: WidthBand::Collapsed,
            wide_clamp: false,
        };
        assert!(!r.is_full());
    }
}

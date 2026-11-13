//! The size-class rule every screen reads at the top of `build`. Structure follows the window's
//! shortest side, so rotation never flips it and an iPad Split View half reads as a phone; the
//! [`WidthBand`] only collapses panels within that structure.

/// Material 3 compact/medium window-size-class boundary.
pub const WIDE_BREAKPOINT_PX: f64 = 600.0;

/// Material 3's compact/medium boundary, as in `docs/tablet.html`.
pub const TABLET_MIN_SHORT_SIDE_PX: f64 = 600.0;

/// `docs/tablet.html`'s 840 band.
pub const FULL_BAND_MIN_WIDTH_PX: f64 = 840.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeClass {
    Compact,
    Tablet,
}

/// Always `Collapsed` on Compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthBand {
    Collapsed,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Responsive {
    pub class: SizeClass,
    pub band: WidthBand,
    /// Kept for phone landscape.
    pub wide_clamp: bool,
}

impl Responsive {
    /// `None` (no shell, headless tests) is Compact / Collapsed / no clamp.
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

    pub fn is_tablet(self) -> bool {
        self.class == SizeClass::Tablet
    }

    /// Implies Tablet.
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

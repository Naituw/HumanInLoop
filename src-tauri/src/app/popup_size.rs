//! Popup geometry recovery and filtering for shared size preferences.

use crate::config::PopupChannelConfig;

pub(super) const MIN_WIDTH: f64 = 420.0;
pub(super) const MIN_HEIGHT: f64 = 480.0;

pub(super) fn restored_size(config: &PopupChannelConfig) -> (f64, f64) {
    fn dimension(value: f64, minimum: f64, fallback: f64) -> f64 {
        if value.is_finite() && value > 0.0 && value <= i32::MAX as f64 {
            value.max(minimum)
        } else {
            fallback
        }
    }
    let defaults = PopupChannelConfig::default();
    (
        dimension(config.width, MIN_WIDTH, defaults.width),
        dimension(config.height, MIN_HEIGHT, defaults.height),
    )
}

#[derive(Default)]
pub(super) struct SizeMemory {
    last_size: Option<(f64, f64)>,
}

impl SizeMemory {
    pub(super) fn restoring(&mut self, size: (f64, f64)) {
        self.last_size = Some(size);
    }

    pub(super) fn observe(
        &mut self,
        physical: (u32, u32),
        scale: f64,
        eligible: bool,
    ) -> Option<(f64, f64)> {
        if !eligible || !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let size = (physical.0 as f64 / scale, physical.1 as f64 / scale);
        if !size.0.is_finite()
            || !size.1.is_finite()
            || size.0 < MIN_WIDTH
            || size.1 < MIN_HEIGHT
            || size.0 > i32::MAX as f64
            || size.1 > i32::MAX as f64
        {
            return None;
        }
        // Native DPI conversion rounds to physical pixels. A restore acknowledgement must
        // not rewrite shared preferences, even when another helper has since saved a size.
        let Some(previous) = self.last_size else {
            self.last_size = Some(size);
            return None;
        };
        let tolerance = 1.0 / scale;
        if (size.0 - previous.0).abs() <= tolerance && (size.1 - previous.1).abs() <= tolerance {
            return None;
        }
        self.last_size = Some(size);
        Some(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polluted_preferences_recover_and_valid_dimensions_survive() {
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            let mut config = PopupChannelConfig {
                width: invalid,
                height: 800.0,
                ..Default::default()
            };
            assert_eq!(restored_size(&config), (560.0, 800.0));
            config.width = 700.0;
            config.height = invalid;
            assert_eq!(restored_size(&config), (700.0, 620.0));
        }
        let config = PopupChannelConfig {
            width: 360.0,
            height: 360.0,
            ..Default::default()
        };
        assert_eq!(restored_size(&config), (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn warm_restore_does_not_overwrite_another_helpers_preference() {
        let mut warm = SizeMemory::default();
        warm.restoring((560.0, 620.0));
        assert_eq!(warm.observe((0, 0), 1.0, false), None);
        assert_eq!(warm.observe((560, 620), 1.0, false), None);
        // The active helper saved a different size while this helper was hidden.
        warm.restoring((701.0, 801.0));
        // A delayed construction event is ineligible when it differs from the live size.
        assert_eq!(warm.observe((560, 620), 1.0, false), None);
        assert_eq!(warm.observe((876, 1001), 1.25, true), None);
        assert_eq!(warm.observe((1000, 1125), 1.25, true), Some((800.0, 900.0)));
        assert_eq!(warm.observe((1000, 1125), 1.25, true), None);
    }

    #[test]
    fn minimize_maximize_and_invalid_events_preserve_normal_size() {
        let mut memory = SizeMemory::default();
        memory.restoring((560.0, 620.0));
        for physical in [(0, 0), (560, 0), (1, 620), (419, 479)] {
            assert_eq!(memory.observe(physical, 1.0, true), None);
        }
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(memory.observe((800, 900), scale, true), None);
        }
        assert_eq!(memory.observe((1920, 1080), 1.0, false), None);
        assert_eq!(memory.observe((560, 620), 1.0, true), None);
        assert_eq!(memory.observe((700, 800), 1.0, true), Some((700.0, 800.0)));
    }

    #[test]
    fn slow_single_pixel_drag_eventually_updates_preferences() {
        let mut memory = SizeMemory::default();
        memory.restoring((560.0, 620.0));
        assert_eq!(memory.observe((561, 620), 1.0, true), None);
        assert_eq!(memory.observe((562, 620), 1.0, true), Some((562.0, 620.0)));
    }
}

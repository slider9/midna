//! Trackpad haptics off macOS: there's no Force Touch trackpad to tap, so `ui.haptics` does
//! nothing. Same API as `src/haptics.rs`.

/// Nothing to install.
pub fn start() {}

/// No haptic engine.
pub fn tap() {}

/// `ui.haptics` changed (or the app connected).
pub fn sync(_enabled: bool) {}

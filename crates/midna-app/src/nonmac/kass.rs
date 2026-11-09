//! Kass dictation off macOS: Kass is a macOS app and its handshake travels over macOS
//! distributed notifications, so it is never detected here. Same API as `src/kass.rs`.

/// Kass never announces itself off macOS.
pub fn handshake_detected() -> bool {
    false
}

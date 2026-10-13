//! Opaque identifiers for devices and capture targets.

use std::fmt;

/// An opaque, backend-chosen identifier for a device or capture target: an audio
/// endpoint, a display or a window.
///
/// Only the backend that listed it can interpret it (on Windows, for example, a WASAPI
/// endpoint ID string or a monitor's device name). The engine stores and compares it,
/// never parses it. It is stable for as long as the backend can make it so; a device that
/// is unplugged and plugged back in may or may not keep its ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    /// Wraps a backend's identifier string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceId;

    #[test]
    fn round_trips_and_displays() {
        let id = DeviceId::new("{0.0.1.00000000}.{abc}");
        assert_eq!(id.as_str(), "{0.0.1.00000000}.{abc}");
        assert_eq!(id.to_string(), "{0.0.1.00000000}.{abc}");
        assert_eq!(id, DeviceId::new(String::from("{0.0.1.00000000}.{abc}")));
    }
}

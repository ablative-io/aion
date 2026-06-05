//// Canonical workflow duration quantities.

/// A workflow duration normalised to milliseconds for the FFI boundary.
pub opaque type Duration {
  Duration(milliseconds: Int)
}

/// Construct a duration from milliseconds.
pub fn milliseconds(value: Int) -> Duration {
  Duration(milliseconds: value)
}

/// Construct a duration from seconds.
pub fn seconds(value: Int) -> Duration {
  Duration(milliseconds: value * 1000)
}

/// Construct a duration from minutes.
pub fn minutes(value: Int) -> Duration {
  Duration(milliseconds: value * 60 * 1000)
}

/// Construct a duration from hours.
pub fn hours(value: Int) -> Duration {
  Duration(milliseconds: value * 60 * 60 * 1000)
}

/// Construct a duration from days.
pub fn days(value: Int) -> Duration {
  Duration(milliseconds: value * 24 * 60 * 60 * 1000)
}

/// Return the canonical millisecond representation used at the FFI boundary.
pub fn to_milliseconds(duration: Duration) -> Int {
  duration.milliseconds
}

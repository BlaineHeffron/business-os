//! Shared QuickBooks Online API constants.

/// QBO minor versions below 75 are discontinued; keep every read/write
/// request explicit so provider behavior does not drift with Intuit defaults.
pub const QBO_MINOR_VERSION: u16 = 75;

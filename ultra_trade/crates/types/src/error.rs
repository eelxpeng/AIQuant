use core::fmt;

/// What went wrong at a boundary.
///
/// Three variants because the spec names three distinct conditions and each
/// needs a test that reaches it (Constitution IV). No variant carries a
/// payload: the crate is `no_std`, so an error cannot quietly grow a `String`,
/// and nothing on an error path allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueError {
    /// The value cannot be represented at all.
    OutOfRange,
    /// The operation's result left the representable range.
    Overflow,
    /// A tick or lot step was zero or negative.
    InvalidStep,
}

impl ValueError {
    const fn message(self) -> &'static str {
        match self {
            ValueError::OutOfRange => "value is not representable",
            ValueError::Overflow => "operation left the representable range",
            ValueError::InvalidStep => "rounding step must be greater than zero",
        }
    }
}

impl fmt::Display for ValueError {
    /// Writes a `&'static str`, so this allocates nothing — but it is still
    /// formatting, and the hot path must not call it (Constitution VI).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl core::error::Error for ValueError {}

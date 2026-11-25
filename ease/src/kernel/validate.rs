//! Untrusted data from u-mode and validation

use ease_abi::FileAccess;

pub(crate) struct Untrusted<T>(T);

impl<T> Untrusted<T> {
    pub(crate) const fn new(val: T) -> Self {
        Self(val)
    }

    pub(crate) fn raw(self) -> T {
        self.0
    }
}

pub(crate) trait Validate<T>: Sized {
    /// Type returned from a conversion error
    type Error;

    /// Performs the validation
    fn validate(value: Untrusted<T>) -> Result<Self, Self::Error>;
}

impl Validate<usize> for FileAccess {
    type Error = ease_abi::Error;

    /// Validate the usize against [ease_abi::FileAccess] bits using the
    /// try_from associated function
    fn validate(value: Untrusted<usize>) -> Result<Self, Self::Error> {
        FileAccess::try_from(value.raw())
    }
}

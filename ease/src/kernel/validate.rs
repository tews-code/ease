//! Untrusted data from u-mode and validation

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

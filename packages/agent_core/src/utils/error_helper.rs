use std::fmt::Debug;

pub trait ErrorHelper<T: Debug> {
    fn with_error<F: FnOnce(&T)>(self, f: F) -> Self;
    fn take_error<F: FnOnce(T)>(self, f: F);
}

impl<R, E: Debug> ErrorHelper<E> for Result<R, E> {
    fn with_error<F: FnOnce(&E)>(self, f: F) -> Self {
        match self {
            Ok(ok) => Ok(ok),
            Err(error) => {
                f(&error);
                Err(error)
            }
        }
    }

    fn take_error<F: FnOnce(E)>(self, f: F) {
        if let Err(error) = self {
            f(error);
        }
    }
}
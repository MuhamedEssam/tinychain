//! Provides generic datatypes used across multiple Tinychain sub-crates.
//!
//! This library is a part of Tinychain: [http://github.com/haydnv/tinychain](http://github.com/haydnv/tinychain)

use std::fmt;
use std::pin::Pin;

use futures::Stream;

use tc_error::TCResult;

pub mod id;
pub mod map;
pub mod time;
pub mod tuple;

pub use id::*;
pub use map::*;
pub use time::*;
pub use tuple::*;

/// A pinned TryStream with error type TCError
pub type TCTryStream<'a, T> = Pin<Box<dyn Stream<Item = TCResult<T>> + Send + Unpin + 'a>>;

/// A generic class trait
pub trait Class: fmt::Display + Sized {}

/// A generic native (i.e. implemented in Rust) class trait
pub trait NativeClass: Class {
    /// Given a fully qualified path, return this class, or a subclass.
    ///
    /// Example:
    /// ```no_run
    /// assert_eq!(
    ///     Number::from_path("/state/scalar/value/number/int/32"),
    ///     NumberType::Int(IntType::I32));
    /// ```
    fn from_path(path: &[PathSegment]) -> Option<Self>;

    /// Returns the fully-qualified path of this class.
    fn path(&self) -> TCPathBuf;
}

/// A generic instance trait
pub trait Instance: Send + Sync {
    /// The [`Class`] type of this instance
    type Class: Class;

    /// Returns the [`Class]` of this instance.
    fn class(&self) -> Self::Class;
}

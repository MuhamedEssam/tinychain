use std::fmt;

pub mod id;
pub mod map;
pub mod time;
pub mod tuple;

pub use id::*;
pub use map::*;
pub use time::*;
pub use tuple::*;

pub trait Class: fmt::Display + Sized {
    type Instance;
}

pub trait NativeClass: Class {
    fn from_path(path: &[PathSegment]) -> Option<Self>;

    fn path(&self) -> TCPathBuf;
}

pub trait Instance {
    type Class: Class;

    fn class(&self) -> Self::Class;
}
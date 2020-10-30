use std::fmt;
use std::sync::Arc;

use futures::TryFutureExt;

use crate::auth::Auth;
use crate::class::{Class, Instance, NativeClass, State, TCBoxTryFuture, TCType};
use crate::error::{self, TCResult};
use crate::scalar::{self, label, Link, TCPath, Value};
use crate::transaction::Txn;

mod class;
mod instance;

pub use class::{InstanceClass, InstanceClassType};
pub use instance::ObjectInstance;

#[derive(Clone, Eq, PartialEq)]
pub enum ObjectType {
    Class(InstanceClassType),
    Instance(InstanceClass),
}

impl ObjectType {
    pub fn post(path: TCPath, data: scalar::Object) -> TCResult<Object> {
        println!("ObjectType::post {} <- {}", path, data);

        if path == Self::prefix() {
            InstanceClass::post(path, data).map(Object::Instance)
        } else if path.starts_with(&InstanceClassType::prefix()) {
            InstanceClassType::post(path, data).map(Object::Class)
        } else {
            Err(error::not_found(path))
        }
    }
}

impl Class for ObjectType {
    type Instance = Object;
}

impl NativeClass for ObjectType {
    fn from_path(path: &TCPath) -> TCResult<ObjectType> {
        let suffix = path.from_path(&Self::prefix())?;

        if suffix.is_empty() {
            Ok(ObjectType::Instance(InstanceClass::default()))
        } else if &suffix == "/class" {
            Ok(ObjectType::Class(InstanceClassType))
        } else {
            Err(error::not_found(suffix))
        }
    }

    fn prefix() -> TCPath {
        TCType::prefix().join(label("object").into())
    }
}

impl From<ObjectType> for Link {
    fn from(ot: ObjectType) -> Link {
        match ot {
            ObjectType::Class(ict) => ict.into(),
            ObjectType::Instance(ic) => ic.into(),
        }
    }
}

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Class(ict) => write!(f, "{}", ict),
            Self::Instance(ic) => write!(f, "{}", ic),
        }
    }
}

#[derive(Clone)]
pub enum Object {
    Class(InstanceClass),
    Instance(ObjectInstance),
}

impl Object {
    pub fn get(
        &self,
        txn: Arc<Txn>,
        path: TCPath,
        key: Value,
        auth: Auth,
    ) -> TCBoxTryFuture<State> {
        Box::pin(async move {
            match self {
                Self::Class(ic) => {
                    ic.get(txn, path, key, auth)
                        .map_ok(Object::Instance)
                        .map_ok(State::Object)
                        .await
                }
                Self::Instance(instance) => instance.get(txn, path, key, auth).await,
            }
        })
    }
}

impl Instance for Object {
    type Class = ObjectType;

    fn class(&self) -> ObjectType {
        match self {
            Self::Class(ic) => ObjectType::Class(ic.class()),
            Self::Instance(i) => ObjectType::Instance(i.class()),
        }
    }
}

impl From<InstanceClass> for Object {
    fn from(ic: InstanceClass) -> Object {
        Object::Class(ic)
    }
}

impl From<ObjectInstance> for Object {
    fn from(oi: ObjectInstance) -> Object {
        Object::Instance(oi)
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Class(ict) => write!(f, "{}", ict),
            Self::Instance(ic) => write!(f, "{}", ic),
        }
    }
}
use std::convert::{TryFrom, TryInto};
use std::fmt;

use destream::{en, EncodeMap};

use transact::IntoView;

use crate::state::State;
use crate::txn::{FileEntry, Txn};

use super::{InstanceClass, Object};

#[derive(Clone)]
pub struct InstanceExt<T: generic::Instance> {
    parent: Box<T>,
    class: InstanceClass,
}

impl<T: generic::Instance> InstanceExt<T> {
    pub fn new(parent: T, class: InstanceClass) -> InstanceExt<T> {
        InstanceExt {
            parent: Box::new(parent),
            class,
        }
    }

    pub fn try_into<E, O: generic::Instance + TryFrom<T, Error = E>>(
        self,
    ) -> Result<InstanceExt<O>, E> {
        let class = self.class;
        let parent = (*self.parent).try_into()?;

        Ok(InstanceExt {
            parent: Box::new(parent),
            class,
        })
    }
}

impl<T: generic::Instance> generic::Instance for InstanceExt<T> {
    type Class = InstanceClass;

    fn class(&self) -> Self::Class {
        self.class.clone()
    }
}

impl<'en, T: generic::Instance + en::IntoStream<'en> + 'en> en::IntoStream<'en> for InstanceExt<T> {
    fn into_stream<E: en::Encoder<'en>>(self, encoder: E) -> Result<E::Ok, E::Error> {
        let mut map = encoder.encode_map(Some(1))?;
        map.encode_entry(self.class.extends().to_string(), self.parent)?;
        map.end()
    }
}

impl<'en> IntoView<'en, FileEntry> for InstanceExt<State> {
    type Txn = Txn;
    type View = InstanceView;

    fn into_view(self, txn: Txn) -> InstanceView {
        InstanceView {
            instance: self,
            txn,
        }
    }
}

impl<T: generic::Instance> From<InstanceExt<T>> for State
where
    State: From<T>,
{
    fn from(instance: InstanceExt<T>) -> State {
        let parent = Box::new((*instance.parent).into());
        let class = instance.class;
        let instance = InstanceExt { parent, class };
        State::Object(Object::Instance(instance))
    }
}

impl<T: generic::Instance> fmt::Display for InstanceExt<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} Object", generic::Instance::class(self))
    }
}

pub struct InstanceView {
    instance: InstanceExt<State>,
    txn: Txn,
}

impl<'en> en::IntoStream<'en> for InstanceView {
    fn into_stream<E: en::Encoder<'en>>(self, encoder: E) -> Result<E::Ok, E::Error> {
        let mut map = encoder.encode_map(Some(1))?;

        map.encode_entry(
            self.instance.class.extends().to_string(),
            self.instance.parent.into_view(self.txn),
        )?;

        map.end()
    }
}

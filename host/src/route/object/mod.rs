use tcgeneric::PathSegment;

use crate::object::{InstanceClass, InstanceExt, Object, ObjectType};
use crate::state::State;

use super::{GetHandler, Handler, Route};

mod instance;

impl Route for ObjectType {
    fn route<'a>(&'a self, _path: &'a [PathSegment]) -> Option<Box<dyn Handler<'a> + 'a>> {
        None
    }
}

struct ClassHandler<'a> {
    class: &'a InstanceClass,
}

impl<'a> Handler<'a> for ClassHandler<'a> {
    fn get<'b>(self: Box<Self>) -> Option<GetHandler<'a, 'b>>
    where
        'b: 'a,
    {
        Some(Box::new(|_txn, key| {
            Box::pin(async move {
                let parent = State::from(key);
                Ok(State::Object(
                    InstanceExt::new(parent, self.class.clone()).into(),
                ))
            })
        }))
    }
}

impl Route for InstanceClass {
    fn route<'a>(&'a self, path: &'a [PathSegment]) -> Option<Box<dyn Handler<'a> + 'a>> {
        if path.is_empty() {
            Some(Box::new(ClassHandler { class: self }))
        } else if let Some(attribute) = self.proto().get(&path[0]) {
            attribute.route(&path[1..])
        } else {
            None
        }
    }
}

impl Route for Object {
    fn route<'a>(&'a self, path: &'a [PathSegment]) -> Option<Box<dyn Handler<'a> + 'a>> {
        match self {
            Self::Class(class) => class.route(path),
            Self::Instance(instance) => instance.route(path),
        }
    }
}

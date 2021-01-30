use std::iter;

use futures::future;

use generic::PathSegment;

use crate::state::scalar::op::*;
use crate::state::scalar::{Scalar, Value};
use crate::state::State;
use crate::txn::*;

use super::*;

struct OpHandler<'a> {
    op_def: &'a OpDef,
}

impl<'a> Handler<'a> for OpHandler<'a> {
    fn get(self: Box<Self>) -> Option<GetHandler<'a>> {
        let handle: GetHandler<'a> = match self.op_def {
            OpDef::Get((_, op_def)) if op_def.is_empty() => {
                let handle = |_: &'a Txn, _: Value| {
                    let result: GetFuture<'a> = Box::pin(future::ready(Ok(State::default())));
                    result
                };

                Box::new(handle)
            }
            OpDef::Get(get_op) => {
                let (key_name, op_def) = get_op.clone();

                let handle = move |txn: &'a Txn, key: Value| {
                    let capture = op_def.last().unwrap().0.clone();
                    let op_def =
                        iter::once((key_name, Scalar::Value(key))).chain(op_def.into_iter());
                    let executor = Executor::new(txn, op_def);
                    let result: GetFuture<'a> = Box::pin(executor.capture(capture));
                    result
                };

                Box::new(handle)
            }
            _ => unimplemented!(),
        };

        Some(handle)
    }
}

impl Route for OpDef {
    fn route<'a>(&'a self, path: &[PathSegment]) -> Option<Box<dyn Handler<'a> + 'a>> {
        if path.is_empty() {
            Some(Box::new(OpHandler { op_def: self }))
        } else {
            None
        }
    }
}
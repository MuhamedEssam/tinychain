use std::collections::HashSet;
use std::iter::FromIterator;

use futures::stream::{FuturesUnordered, StreamExt};

use error::*;
use generic::{Id, Map};

use crate::state::scalar::reference::Refer;
use crate::state::State;
use crate::txn::Txn;

#[derive(Clone)]
pub struct Executor<'a> {
    txn: &'a Txn,
    state: Map<State>,
}

impl<'a> Executor<'a> {
    pub fn new<S: Into<State>, I: IntoIterator<Item = (Id, S)>>(txn: &'a Txn, iter: I) -> Self {
        let state = iter.into_iter().map(|(id, s)| (id, s.into())).collect();

        Self { txn, state }
    }

    pub async fn capture(mut self, capture: Id) -> TCResult<State> {
        while self.resolve_id(&capture)?.is_ref() {
            let mut pending = Vec::with_capacity(self.state.len());
            let mut unvisited = Vec::with_capacity(self.state.len());
            unvisited.push(capture.clone());

            while let Some(id) = unvisited.pop() {
                let state = self.resolve_id(&capture)?;

                if state.is_ref() {
                    let mut deps = HashSet::new();
                    state.requires(&mut deps);

                    let mut ready = true;
                    for dep_id in deps.into_iter() {
                        if self.resolve_id(&dep_id)?.is_ref() {
                            ready = false;
                            unvisited.push(dep_id);
                        }
                    }

                    if ready {
                        pending.push(id);
                    }
                }
            }

            if pending.is_empty() && self.resolve_id(&capture)?.is_ref() {
                return Err(TCError::bad_request(
                    "Cannot resolve all dependencies of",
                    capture,
                ));
            }

            let mut providers = FuturesUnordered::from_iter(
                pending
                    .into_iter()
                    .map(|id| async { (id, Err(TCError::not_implemented("State::resolve"))) }),
            );

            while let Some((id, r)) = providers.next().await {
                match r {
                    Ok(state) => {
                        self.state.insert(id, state);
                    }
                    Err(cause) => return Err(cause.consume(format!("Error resolving {}", id))),
                }
            }
        }

        self.state
            .remove(&capture)
            .ok_or_else(|| TCError::not_found(capture))
    }

    fn resolve_id(&'_ self, id: &Id) -> TCResult<&'_ State> {
        self.state.get(id).ok_or_else(|| TCError::not_found(id))
    }
}

use futures::Stream;

use crate::auth::Auth;
use crate::error;
use crate::state::State;
use crate::transaction::*;
use crate::value::link::TCPath;
use crate::value::{TCResult, Value, ValueId};

pub fn get(endpoint: &TCPath, _id: Value) -> TCResult<State> {
    Err(error::not_found(endpoint))
}

pub async fn post<S: Stream<Item = (ValueId, Value)>>(
    _endpoint: &TCPath,
    _op: S,
    _auth: &Auth,
) -> TCResult<TxnContext> {
    Err(error::not_implemented())
}

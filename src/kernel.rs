use std::convert::TryInto;
use std::sync::Arc;

use futures::stream;
use futures::TryFutureExt;

use crate::auth::Auth;
use crate::class::{NativeClass, State, TCResult};
use crate::collection::class::{CollectionClass, CollectionType};
use crate::error;
use crate::object::ObjectType;
use crate::scalar::*;
use crate::transaction::Txn;

const ERR_TXN_REQUIRED: &str = "Collection requires a transaction context";

pub async fn get(path: &TCPath, id: Value, txn: Option<Arc<Txn>>) -> TCResult<State> {
    println!("kernel::get {}", path);

    let suffix = path.from_path(&label("sbin").into())?;
    if suffix.is_empty() {
        return Err(error::unsupported("Cannot access /sbin directly"));
    }

    match suffix[0].as_str() {
        "chain" => {
            let _txn = txn.ok_or_else(|| error::unsupported(ERR_TXN_REQUIRED))?;
            Err(error::not_implemented("Instantiate Chain"))
        }
        "collection" => {
            let txn = txn.ok_or_else(|| error::unsupported(ERR_TXN_REQUIRED))?;
            let ctype = CollectionType::from_path(path)?;
            ctype.get(txn, id).map_ok(State::Collection).await
        }
        "error" => Err(error::get(path, id.try_into()?)),
        "value" => {
            let dtype = ValueType::from_path(path)?;
            dtype.try_cast(id).map(Scalar::Value).map(State::Scalar)
        }
        "transact" => Err(error::method_not_allowed(suffix)),
        other => Err(error::not_found(other)),
    }
}

pub async fn post(txn: Arc<Txn>, path: TCPath, data: Scalar, auth: Auth) -> TCResult<State> {
    println!("kernel::post {}", path);

    if &path == "/sbin/transact" {
        if data.matches::<Vec<(ValueId, Scalar)>>() {
            let values: Vec<(ValueId, Scalar)> = data.opt_cast_into().unwrap();
            txn.execute(stream::iter(values), auth).await
        } else if data.matches::<OpRef>() {
            Err(error::not_implemented("Resolve OpRef"))
        } else {
            Ok(State::Scalar(data))
        }
    } else if path.starts_with(&ObjectType::prefix()) {
        let data = data.try_into()?;
        ObjectType::post(path, data).map(State::Object)
    } else {
        Err(error::not_found(path))
    }
}

use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::join_all;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::error;
use crate::host::Host;
use crate::internal::block::Store;
use crate::internal::cache::{Map, Queue};
use crate::state::{State, Transactable};
use crate::value::*;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct TransactionId {
    timestamp: u128, // nanoseconds since Unix epoch
    nonce: u16,
}

impl TransactionId {
    fn new(timestamp: u128) -> TransactionId {
        let nonce: u16 = rand::thread_rng().gen();
        TransactionId { timestamp, nonce }
    }
}

impl From<Bytes> for TransactionId {
    fn from(b: Bytes) -> TransactionId {
        TransactionId {
            timestamp: u128::from_be_bytes(b[..16].try_into().expect("Bad transaction timestamp")),
            nonce: u16::from_be_bytes(b[16..].try_into().expect("Bad transaction nonce")),
        }
    }
}

impl PartialOrd for TransactionId {
    fn partial_cmp(&self, other: &TransactionId) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TransactionId {
    fn cmp(&self, other: &TransactionId) -> std::cmp::Ordering {
        if self.timestamp == other.timestamp {
            self.nonce.cmp(&other.nonce)
        } else {
            self.timestamp.cmp(&other.timestamp)
        }
    }
}

impl Into<Link> for TransactionId {
    fn into(self) -> Link {
        Link::to(&format!("/{}-{}", self.timestamp, self.nonce)).unwrap()
    }
}

impl Into<String> for TransactionId {
    fn into(self) -> String {
        format!("{}-{}", self.timestamp, self.nonce)
    }
}

impl Into<Bytes> for TransactionId {
    fn into(self) -> Bytes {
        Bytes::from(
            [
                &self.timestamp.to_be_bytes()[..],
                &self.nonce.to_be_bytes()[..],
            ]
            .concat(),
        )
    }
}

impl Into<Bytes> for &TransactionId {
    fn into(self) -> Bytes {
        Bytes::from(
            [
                &self.timestamp.to_be_bytes()[..],
                &self.nonce.to_be_bytes()[..],
            ]
            .concat(),
        )
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.timestamp, self.nonce)
    }
}

fn calc_deps(
    value_id: ValueId,
    op: Op,
    state: &mut HashMap<ValueId, State>,
    queue: &Queue<(ValueId, Op)>,
) -> TCResult<()> {
    if let Op::Post { requires, .. } = &op {
        let mut required_value_ids: Vec<(String, ValueId)> = vec![];
        for (id, provider) in requires {
            match provider {
                TCValue::Op(dep) => {
                    calc_deps(id.clone(), dep.clone(), state, queue)?;
                    required_value_ids.push((id.clone(), id.clone()));
                }
                TCValue::Ref(r) => {
                    required_value_ids.push((id.clone(), r.value_id()));
                }
                value => {
                    if state.contains_key(id) {
                        return Err(error::bad_request("Duplicate values provided for", id));
                    }

                    state.insert(id.clone(), State::Value(value.clone()));
                    required_value_ids.push((id.clone(), id.clone()));
                }
            }
        }
    }

    queue.push((value_id, op));
    Ok(())
}

pub struct Transaction {
    host: Arc<Host>,
    id: TransactionId,
    context: Arc<Store>,
    state: Map<ValueId, State>,
    queue: Queue<(ValueId, Op)>,
    mutated: Queue<Arc<dyn Transactable>>,
}

impl Transaction {
    pub fn new(host: Arc<Host>, root: Arc<Store>) -> TCResult<Arc<Transaction>> {
        let id = TransactionId::new(host.time());
        let context = root.create(&id.clone().into())?;

        println!();
        println!("Transaction::new");

        Ok(Arc::new(Transaction {
            host,
            id,
            context,
            state: Map::new(),
            queue: Queue::new(),
            mutated: Queue::new(),
        }))
    }

    pub fn of(host: Arc<Host>, root: Arc<Store>, op: Op) -> TCResult<Arc<Transaction>> {
        let id = TransactionId::new(host.time());
        let context = root.create(&id.clone().into())?;

        let mut state: HashMap<ValueId, State> = HashMap::new();
        let queue: Queue<(ValueId, Op)> = Queue::new();
        calc_deps(String::from("_"), op, &mut state, &queue)?;
        queue.reverse();

        println!();
        println!("Transaction::of");

        Ok(Arc::new(Transaction {
            host,
            id,
            context,
            state: state.into_iter().collect(),
            queue,
            mutated: Queue::new(),
        }))
    }

    fn extend(
        self: &Arc<Self>,
        context: Arc<Store>,
        required: HashMap<ValueId, TCValue>,
    ) -> Arc<Transaction> {
        Arc::new(Transaction {
            host: self.host.clone(),
            id: self.id.clone(),
            context,
            state: required
                .iter()
                .map(|(k, v)| (k.to_string(), v.into()))
                .collect(),
            queue: Queue::new(),
            mutated: Queue::new(),
        })
    }

    pub fn context(self: &Arc<Self>) -> Arc<Store> {
        self.context.clone()
    }

    pub fn id(self: &Arc<Self>) -> TransactionId {
        self.id.clone()
    }

    pub async fn commit(self: &Arc<Self>) {
        println!("commit!");
        let mut tasks = Vec::with_capacity(self.mutated.len());
        while let Some(state) = self.mutated.pop() {
            tasks.push(async move { state.commit(&self.id).await });
        }
        join_all(tasks).await;
    }

    pub async fn execute<'a>(
        self: &Arc<Self>,
        capture: HashSet<ValueId>,
    ) -> TCResult<HashMap<ValueId, State>> {
        while let Some((value_id, op)) = self.queue.pop() {
            let state = match op {
                Op::Get { subject, key } => match subject {
                    Subject::Link(l) => self.get(&l, *key).await,
                    Subject::Ref(r) => match self.state.get(&r.value_id()) {
                        Some(s) => s.get(self.clone(), *key).await,
                        None => Err(error::bad_request(
                            "Required value not provided",
                            r.value_id(),
                        )),
                    },
                },
                Op::Put {
                    subject,
                    key,
                    value,
                } => match subject {
                    Subject::Link(l) => self.put(l, *key, self.resolve_val(*value)?).await,
                    Subject::Ref(r) => {
                        let subject = self.resolve(&r.value_id())?;
                        let value = self.resolve_val(*value)?;
                        subject.put(self.clone(), *key, value.try_into()?).await
                    }
                },
                Op::Post {
                    subject,
                    action,
                    requires,
                } => {
                    let mut deps: HashMap<ValueId, TCValue> = HashMap::new();
                    for (dest_id, id) in requires {
                        let dep = self.resolve_val(id)?;
                        deps.insert(dest_id, dep.try_into()?);
                    }

                    let subcontext = Link::to(&format!("/{}", value_id))?;
                    let txn = self.extend(self.context.create(&subcontext)?, deps);
                    match subject {
                        Some(r) => {
                            let subject = self.resolve(&r.value_id())?;
                            subject.post(txn, &action).await
                        }
                        None => self.host.post(txn, &action).await,
                    }
                }
            };

            self.state.insert(value_id, state?);
        }

        let mut results: HashMap<ValueId, State> = HashMap::new();
        for value_id in capture {
            match self.state.get(&value_id) {
                Some(state) => {
                    results.insert(value_id, state);
                }
                None => {
                    return Err(error::bad_request(
                        "There is no such value to capture",
                        value_id,
                    ));
                }
            }
        }

        Ok(results)
    }

    fn resolve(self: &Arc<Self>, id: &ValueId) -> TCResult<State> {
        match self.state.get(id) {
            Some(s) => Ok(s),
            None => Err(error::bad_request("Required value not provided", id)),
        }
    }

    fn resolve_val(self: &Arc<Self>, value: TCValue) -> TCResult<State> {
        match value {
            TCValue::Ref(r) => self.resolve(&r.value_id()),
            _ => Ok(value.into()),
        }
    }

    pub fn mutate(self: &Arc<Self>, state: Arc<dyn Transactable>) {
        self.mutated.push(state)
    }

    pub fn require(self: &Arc<Self>, value_id: &str) -> TCResult<TCValue> {
        match self.state.get(&value_id.to_string()) {
            Some(response) => match response {
                State::Value(value) => Ok(value),
                other => Err(error::bad_request(
                    &format!("Required value {} is not serializable", value_id),
                    other,
                )),
            },
            None => Err(error::bad_request("No value was provided for", value_id)),
        }
    }

    pub async fn get(self: &Arc<Self>, path: &Link, key: TCValue) -> TCResult<State> {
        println!("txn::get {} {}", path, key);
        self.host.get(self.clone(), path, key).await
    }

    pub async fn put(self: &Arc<Self>, path: Link, key: TCValue, state: State) -> TCResult<State> {
        println!("txn::put {} {}", path, key);
        self.host.put(self.clone(), path, key, state).await
    }

    pub async fn post(
        self: &Arc<Self>,
        path: &Link,
        args: Vec<(ValueId, TCValue)>,
    ) -> TCResult<State> {
        println!("txn::post {} {:?}", path, args);
        let txn = self.extend(self.context.clone(), args.into_iter().collect());
        self.host.post(txn, path).await
    }
}

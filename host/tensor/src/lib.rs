use std::convert::TryFrom;
use std::fmt;
use std::iter::FromIterator;
use std::marker::PhantomData;
use std::pin::Pin;

use afarray::Array;
use async_trait::async_trait;
use destream::{de, en};
use futures::{Future, TryFutureExt};
use log::debug;
use number_general::{Number, NumberType};

use tc_error::*;
use tc_transact::fs::{Dir, File};
use tc_transact::{IntoView, Transaction, TxnId};
use tcgeneric::{
    label, path_label, Class, Instance, NativeClass, PathLabel, PathSegment, TCPathBuf, Tuple,
};

pub use bounds::{Bounds, Shape};
pub use dense::{BlockListFile, DenseAccess, DenseAccessor, DenseTensor};

mod bounds;
mod dense;
#[allow(dead_code)]
mod transform;

const PREFIX: PathLabel = path_label(&["state", "collection", "tensor"]);

pub type Schema = (NumberType, Shape);

pub type Coord = Vec<u64>;

type Read<'a> = Pin<Box<dyn Future<Output = TCResult<(Coord, Number)>> + Send + 'a>>;

pub trait ReadValueAt<D: Dir> {
    type Txn: Transaction<D>;

    fn read_value_at<'a>(&'a self, txn: &'a Self::Txn, coord: Coord) -> Read<'a>;
}

pub trait TensorAccess: Send {
    fn dtype(&self) -> NumberType;

    fn ndim(&self) -> usize;

    fn shape(&'_ self) -> &'_ Shape;

    fn size(&self) -> u64;
}

pub trait TensorInstance<D: Dir>: TensorIO<D> + TensorTransform<D> + Send + Sync {
    type Dense: TensorInstance<D>;

    fn into_dense(self) -> Self::Dense;
}

#[async_trait]
pub trait TensorIO<D: Dir>: TensorAccess + Sized {
    type Txn: Transaction<D>;

    async fn read_value(&self, txn: &Self::Txn, coord: Coord) -> TCResult<Number>;

    async fn write_value(&self, txn_id: TxnId, bounds: Bounds, value: Number) -> TCResult<()>;

    async fn write_value_at(&self, txn_id: TxnId, coord: Coord, value: Number) -> TCResult<()>;
}

pub trait TensorTransform<D: Dir>: TensorAccess + Sized {
    type Slice: TensorInstance<D>;

    fn slice(&self, bounds: bounds::Bounds) -> TCResult<Self::Slice>;
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum TensorType {
    Dense,
}

impl Class for TensorType {}

impl NativeClass for TensorType {
    fn from_path(path: &[PathSegment]) -> Option<Self> {
        if path.len() == 4 && &path[..3] == &PREFIX[..] {
            match path[3].as_str() {
                "dense" => Some(Self::Dense),
                "sparse" => todo!(),
                _ => None,
            }
        } else {
            None
        }
    }

    fn path(&self) -> TCPathBuf {
        match self {
            Self::Dense => TCPathBuf::from(PREFIX).append(label("dense")),
        }
    }
}

impl fmt::Display for TensorType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("type Tensor")
    }
}

#[derive(Clone)]
pub enum Tensor<F: File<Array>, D: Dir, T: Transaction<D>> {
    Dense(DenseTensor<F, D, T, DenseAccessor<F, D, T>>),
}

impl<F: File<Array>, D: Dir, T: Transaction<D>> Instance for Tensor<F, D, T> {
    type Class = TensorType;

    fn class(&self) -> Self::Class {
        match self {
            Self::Dense(_) => TensorType::Dense,
        }
    }
}

impl<F: File<Array>, D: Dir, T: Transaction<D>> TensorAccess for Tensor<F, D, T> {
    fn dtype(&self) -> NumberType {
        match self {
            Self::Dense(dense) => dense.dtype(),
        }
    }

    fn ndim(&self) -> usize {
        match self {
            Self::Dense(dense) => dense.ndim(),
        }
    }

    fn shape(&self) -> &Shape {
        match self {
            Self::Dense(dense) => dense.shape(),
        }
    }

    fn size(&self) -> u64 {
        match self {
            Self::Dense(dense) => dense.size(),
        }
    }
}

#[async_trait]
impl<F: File<Array>, D: Dir, T: Transaction<D>> TensorIO<D> for Tensor<F, D, T> {
    type Txn = T;

    async fn read_value(&self, txn: &Self::Txn, coord: Coord) -> TCResult<Number> {
        match self {
            Self::Dense(dense) => dense.read_value(txn, coord).await,
        }
    }

    async fn write_value(&self, txn_id: TxnId, bounds: Bounds, value: Number) -> TCResult<()> {
        match self {
            Self::Dense(dense) => dense.write_value(txn_id, bounds, value).await,
        }
    }

    async fn write_value_at(&self, txn_id: TxnId, coord: Coord, value: Number) -> TCResult<()> {
        debug!(
            "Tensor::write_value_at {}, {}",
            Tuple::<u64>::from_iter(coord.to_vec()),
            value
        );

        match self {
            Self::Dense(dense) => dense.write_value_at(txn_id, coord, value).await,
        }
    }
}

impl<F: File<Array>, D: Dir, T: Transaction<D>, B: DenseAccess<F, D, T>>
    From<DenseTensor<F, D, T, B>> for Tensor<F, D, T>
{
    fn from(dense: DenseTensor<F, D, T, B>) -> Self {
        Self::Dense(dense.into_inner().accessor().into())
    }
}

#[async_trait]
impl<F: File<Array>, D: Dir, T: Transaction<D>> de::FromStream for Tensor<F, D, T>
where
    <D as Dir>::FileClass: From<TensorType> + Send,
    F: TryFrom<<D as Dir>::File, Error = TCError>,
{
    type Context = T;

    async fn from_stream<De: de::Decoder>(txn: T, decoder: &mut De) -> Result<Self, De::Error> {
        decoder.decode_map(TensorVisitor::new(txn)).await
    }
}

struct TensorVisitor<F, D, T> {
    txn: T,
    dir: PhantomData<D>,
    file: PhantomData<F>,
}

impl<F, D, T> TensorVisitor<F, D, T> {
    fn new(txn: T) -> Self {
        Self {
            txn,
            dir: PhantomData,
            file: PhantomData,
        }
    }
}

#[async_trait]
impl<F: File<Array>, D: Dir, T: Transaction<D>> de::Visitor for TensorVisitor<F, D, T>
where
    <D as Dir>::FileClass: From<TensorType> + Send,
    F: TryFrom<<D as Dir>::File, Error = TCError>,
{
    type Value = Tensor<F, D, T>;

    fn expecting() -> &'static str {
        "a Tensor"
    }

    async fn visit_map<A: de::MapAccess>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let classpath = map
            .next_key::<TCPathBuf>(())
            .await?
            .ok_or_else(|| de::Error::custom("missing Tensor class"))?;

        let class = TensorType::from_path(&classpath)
            .ok_or_else(|| de::Error::invalid_type(classpath, "a Tensor class"))?;

        match class {
            TensorType::Dense => {
                map.next_value::<DenseTensor<F, D, T, BlockListFile<F, D, T>>>(self.txn)
                    .map_ok(Tensor::from)
                    .await
            }
        }
    }
}

#[async_trait]
impl<'en, F: File<Array>, D: Dir, T: Transaction<D>> IntoView<'en, D> for Tensor<F, D, T> {
    type Txn = T;
    type View = TensorView<'en>;

    async fn into_view(self, txn: T) -> TCResult<Self::View> {
        match self {
            Tensor::Dense(tensor) => tensor.into_view(txn).map_ok(TensorView::Dense).await,
        }
    }
}

pub enum TensorView<'en> {
    Dense(dense::DenseTensorView<'en>),
}

impl<'en> en::IntoStream<'en> for TensorView<'en> {
    fn into_stream<E: en::Encoder<'en>>(self, encoder: E) -> Result<E::Ok, E::Error> {
        match self {
            Self::Dense(view) => view.into_stream(encoder),
        }
    }
}

impl<F: File<Array>, D: Dir, T: Transaction<D>> fmt::Display for Tensor<F, D, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a Tensor")
    }
}
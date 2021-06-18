use std::convert::{TryFrom, TryInto};
use std::marker::PhantomData;
use std::ops::{Add, Div, Mul, Sub};
use std::pin::Pin;

use afarray::Array;
use async_trait::async_trait;
use destream::{de, en};
use futures::future::{self, TryFutureExt};
use futures::stream::{self, Stream, StreamExt, TryStreamExt};
use log::debug;

use tc_btree::{BTreeType, Node};
use tc_error::*;
use tc_transact::fs::{CopyFrom, Dir, File, Hash, Persist};
use tc_transact::{IntoView, Transaction, TxnId};
use tc_value::{Number, NumberClass, NumberType, ValueType};
use tcgeneric::{NativeClass, TCBoxTryFuture, TCTryStream};

use super::dense::{BlockListSparse, DenseTensor};
use super::{
    Bounds, Coord, Phantom, Schema, Shape, Tensor, TensorAccess, TensorIO, TensorInstance,
    TensorMath, TensorReduce, TensorTransform, TensorType,
};

use crate::dense::PER_BLOCK;
use access::*;
pub use access::{DenseToSparse, SparseAccess, SparseAccessor};
pub use table::SparseTable;

mod access;
mod combine;
mod table;

pub type SparseRow = (Coord, Number);
pub type SparseStream<'a> = Pin<Box<dyn Stream<Item = TCResult<SparseRow>> + Send + Unpin + 'a>>;

#[derive(Clone)]
pub struct SparseTensor<FD, FS, D, T, A> {
    accessor: A,
    phantom: Phantom<FD, FS, D, T>,
}

impl<FD, FS, D, T, A> SparseTensor<FD, FS, D, T, A> {
    pub fn into_inner(self) -> A {
        self.accessor
    }
}

impl<FD, FS, D, T, A> SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
{
    fn combine<R: SparseAccess<FD, FS, D, T>>(
        self,
        other: SparseTensor<FD, FS, D, T, R>,
        combinator: fn(Number, Number) -> Number,
        dtype: NumberType,
    ) -> TCResult<SparseTensor<FD, FS, D, T, SparseCombinator<FD, FS, D, T, A, R>>> {
        if self.shape() != other.shape() {
            return Err(TCError::unsupported(format!(
                "Cannot combine Tensors of different shapes: {}, {}",
                self.shape(),
                other.shape()
            )));
        }

        let accessor = SparseCombinator::new(self.accessor, other.accessor, combinator, dtype)?;

        Ok(SparseTensor {
            accessor,
            phantom: self.phantom,
        })
    }
}

impl<FD, FS, D, T, A> TensorAccess for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
{
    fn dtype(&self) -> NumberType {
        self.accessor.dtype()
    }

    fn ndim(&self) -> usize {
        self.accessor.ndim()
    }

    fn shape(&self) -> &Shape {
        self.accessor.shape()
    }

    fn size(&self) -> u64 {
        self.accessor.size()
    }
}

impl<FD, FS, D, T, A> TensorInstance for SparseTensor<FD, FS, D, T, A> {
    type Dense = DenseTensor<FD, FS, D, T, BlockListSparse<FD, FS, D, T, A>>;
    type Sparse = Self;

    fn into_dense(self) -> Self::Dense {
        BlockListSparse::from(self.into_inner()).into()
    }

    fn into_sparse(self) -> Self::Sparse {
        self
    }
}

#[async_trait]
impl<FD, FS, D, T, A> TensorIO<D> for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
{
    type Txn = T;

    async fn read_value(self, txn: Self::Txn, coord: Coord) -> TCResult<Number> {
        self.accessor
            .read_value_at(txn, coord)
            .map_ok(|(_, value)| value)
            .await
    }

    async fn write_value(&self, txn_id: TxnId, mut bounds: Bounds, value: Number) -> TCResult<()> {
        bounds.normalize(self.shape());
        debug!("SparseTensor::write_value {} to bounds, {}", value, bounds);
        stream::iter(bounds.affected())
            .inspect(|coord| debug!("SparseTensor::write_value {:?} <- {}", coord, value))
            .map(|coord| self.accessor.write_value(txn_id, coord, value))
            .buffer_unordered(num_cpus::get())
            .try_fold((), |_, _| future::ready(Ok(())))
            .await
    }

    async fn write_value_at(&self, txn_id: TxnId, coord: Coord, value: Number) -> TCResult<()> {
        self.accessor.write_value(txn_id, coord, value).await
    }
}

impl<FD, FS, D, T, L, R> TensorMath<D, SparseTensor<FD, FS, D, T, R>>
    for SparseTensor<FD, FS, D, T, L>
where
    FD: File<Array>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    L: SparseAccess<FD, FS, D, T>,
    R: SparseAccess<FD, FS, D, T>,
{
    type Combine = SparseTensor<FD, FS, D, T, SparseCombinator<FD, FS, D, T, L, R>>;

    fn add(self, other: SparseTensor<FD, FS, D, T, R>) -> TCResult<Self::Combine> {
        let dtype = Ord::max(self.dtype(), other.dtype());
        self.combine(other, Number::add, dtype)
    }

    fn div(self, other: SparseTensor<FD, FS, D, T, R>) -> TCResult<Self::Combine> {
        let dtype = Ord::max(self.dtype(), other.dtype());
        self.combine(other, Number::div, dtype)
    }

    fn mul(self, other: SparseTensor<FD, FS, D, T, R>) -> TCResult<Self::Combine> {
        let dtype = Ord::max(self.dtype(), other.dtype());
        self.combine(other, Number::mul, dtype)
    }

    fn sub(self, other: SparseTensor<FD, FS, D, T, R>) -> TCResult<Self::Combine> {
        let dtype = Ord::max(self.dtype(), other.dtype());
        self.combine(other, Number::sub, dtype)
    }
}

impl<FD, FS, D, T, A> TensorMath<D, Tensor<FD, FS, D, T>> for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
    D::FileClass: From<TensorType>,
{
    type Combine = Tensor<FD, FS, D, T>;

    fn add(self, other: Tensor<FD, FS, D, T>) -> TCResult<Self::Combine> {
        match other {
            Tensor::Sparse(sparse) => self.add(sparse).map(Tensor::from),
            Tensor::Dense(dense) => self.into_dense().add(dense).map(Tensor::from),
        }
    }

    fn div(self, other: Tensor<FD, FS, D, T>) -> TCResult<Self::Combine> {
        match other {
            Tensor::Sparse(sparse) => self.div(sparse).map(Tensor::from),
            Tensor::Dense(dense) => self.into_dense().div(dense).map(Tensor::from),
        }
    }

    fn mul(self, other: Tensor<FD, FS, D, T>) -> TCResult<Self::Combine> {
        match other {
            Tensor::Sparse(sparse) => self.mul(sparse).map(Tensor::from),
            Tensor::Dense(dense) => self.into_dense().mul(dense).map(Tensor::from),
        }
    }

    fn sub(self, other: Tensor<FD, FS, D, T>) -> TCResult<Self::Combine> {
        match other {
            Tensor::Sparse(sparse) => self.sub(sparse).map(Tensor::from),
            Tensor::Dense(dense) => self.into_dense().sub(dense).map(Tensor::from),
        }
    }
}

impl<FD, FS, D, T, A> TensorReduce<D> for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
    D::FileClass: From<TensorType>,
    Self: TensorInstance,
    <Self as TensorInstance>::Dense: TensorReduce<D, Txn = T> + Send + Sync,
{
    type Txn = T;
    type Reduce = SparseTensor<FD, FS, D, T, SparseReduce<FD, FS, D, T>>;

    fn product(self, axis: usize) -> TCResult<Self::Reduce> {
        let accessor = SparseReduce::new(
            self.accessor.accessor(),
            axis,
            SparseTensor::<FD, FS, D, T, SparseAccessor<FD, FS, D, T>>::product_all,
        )?;

        Ok(SparseTensor::from(accessor))
    }

    fn product_all(&self, txn: T) -> TCBoxTryFuture<Number> {
        Box::pin(async move { self.clone().into_dense().product_all(txn).await })
    }

    fn sum(self, axis: usize) -> TCResult<Self::Reduce> {
        let accessor = SparseReduce::new(
            self.accessor.accessor(),
            axis,
            SparseTensor::<FD, FS, D, T, SparseAccessor<FD, FS, D, T>>::sum_all,
        )?;

        Ok(SparseTensor::from(accessor))
    }

    fn sum_all(&self, txn: T) -> TCBoxTryFuture<Number> {
        Box::pin(async move {
            let mut sum = self.dtype().zero();
            let mut filled = self.accessor.clone().filled(txn).await?;
            let mut buffer = Vec::with_capacity(PER_BLOCK);
            while let Some((_coord, value)) = filled.try_next().await? {
                buffer.push(value);

                if buffer.len() == PER_BLOCK {
                    sum += Array::from(buffer.to_vec()).sum();
                    buffer.clear()
                }
            }

            if !buffer.is_empty() {
                sum += Array::from(buffer).sum();
            }

            Ok(sum)
        })
    }
}

impl<FD, FS, D, T, A> TensorTransform for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
    D::FileClass: From<TensorType>,
{
    type Broadcast = SparseTensor<FD, FS, D, T, SparseBroadcast<FD, FS, D, T, A>>;
    type Cast = SparseTensor<FD, FS, D, T, SparseCast<FD, FS, D, T, A>>;
    type Expand = SparseTensor<FD, FS, D, T, SparseExpand<FD, FS, D, T, A>>;
    type Slice = SparseTensor<FD, FS, D, T, A::Slice>;
    type Transpose = SparseTensor<FD, FS, D, T, A::Transpose>;

    fn broadcast(self, shape: Shape) -> TCResult<Self::Broadcast> {
        let accessor = SparseBroadcast::new(self.accessor, shape)?;
        Ok(accessor.into())
    }

    fn cast_into(self, dtype: NumberType) -> TCResult<Self::Cast> {
        let accessor = SparseCast::new(self.accessor, dtype);
        Ok(accessor.into())
    }

    fn expand_dims(self, axis: usize) -> TCResult<Self::Expand> {
        let accessor = SparseExpand::new(self.accessor, axis)?;
        Ok(accessor.into())
    }

    fn slice(self, bounds: Bounds) -> TCResult<Self::Slice> {
        let accessor = self.accessor.slice(bounds)?;
        Ok(accessor.into())
    }

    fn transpose(self, permutation: Option<Vec<usize>>) -> TCResult<Self::Transpose> {
        let accessor = self.accessor.transpose(permutation)?;
        Ok(accessor.into())
    }
}

#[async_trait]
impl<FD, FS, D, T, A> CopyFrom<D, SparseTensor<FD, FS, D, T, A>>
    for SparseTensor<FD, FS, D, T, SparseTable<FD, FS, D, T>>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
    D::FileClass: From<BTreeType> + From<TensorType>,
{
    async fn copy_from(
        instance: SparseTensor<FD, FS, D, T, A>,
        store: Self::Store,
        txn: Self::Txn,
    ) -> TCResult<Self> {
        SparseTable::copy_from(instance, store, txn)
            .map_ok(Self::from)
            .await
    }
}

#[async_trait]
impl<'en, FD, FS, D, T, A> Hash<'en, D> for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
{
    type Item = SparseRow;
    type Txn = T;

    async fn hashable(&'en self, txn: &'en Self::Txn) -> TCResult<TCTryStream<'en, SparseRow>> {
        self.accessor.clone().filled(txn.clone()).await
    }
}

#[async_trait]
impl<FD, FS, D, T> Persist<D> for SparseTensor<FD, FS, D, T, SparseTable<FD, FS, D, T>>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    D::FileClass: From<BTreeType> + From<TensorType>,
{
    type Schema = Schema;
    type Store = D;
    type Txn = T;

    fn schema(&self) -> &Self::Schema {
        self.accessor.schema()
    }

    async fn load(txn: &Self::Txn, schema: Self::Schema, store: Self::Store) -> TCResult<Self> {
        SparseTable::load(txn, schema, store)
            .map_ok(Self::from)
            .await
    }
}

impl<FD, FS, D, T, A> From<A> for SparseTensor<FD, FS, D, T, A> {
    fn from(accessor: A) -> Self {
        Self {
            accessor,
            phantom: Phantom::default(),
        }
    }
}

#[async_trait]
impl<'en, FD, FS, D, T, A> IntoView<'en, D> for SparseTensor<FD, FS, D, T, A>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node>,
    D: Dir,
    T: Transaction<D>,
    A: SparseAccess<FD, FS, D, T>,
    D::FileClass: From<TensorType>,
{
    type Txn = T;
    type View = SparseTensorView<'en>;

    async fn into_view(self, txn: Self::Txn) -> TCResult<Self::View> {
        Ok(SparseTensorView {
            shape: self.shape().to_vec(),
            dtype: self.dtype().into(),
            filled: self.accessor.filled(txn).await?,
        })
    }
}

#[async_trait]
impl<FD, FS, D, T> de::FromStream for SparseTensor<FD, FS, D, T, SparseTable<FD, FS, D, T>>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    D::FileClass: From<BTreeType> + From<TensorType>,
{
    type Context = T;

    async fn from_stream<De: de::Decoder>(txn: T, decoder: &mut De) -> Result<Self, De::Error> {
        decoder.decode_seq(SparseTensorVisitor::new(txn)).await
    }
}

struct SparseTensorVisitor<FD, FS, D, T> {
    txn: T,
    dense: PhantomData<FD>,
    sparse: PhantomData<FS>,
    dir: PhantomData<D>,
}

impl<FD, FS, D, T> SparseTensorVisitor<FD, FS, D, T> {
    fn new(txn: T) -> Self {
        Self {
            txn,
            dense: PhantomData,
            sparse: PhantomData,
            dir: PhantomData,
        }
    }
}

#[async_trait]
impl<FD, FS, D, T> de::Visitor for SparseTensorVisitor<FD, FS, D, T>
where
    FD: File<Array> + TryFrom<D::File, Error = TCError>,
    FS: File<Node> + TryFrom<D::File, Error = TCError>,
    D: Dir,
    T: Transaction<D>,
    D::FileClass: From<BTreeType> + From<TensorType>,
{
    type Value = SparseTensor<FD, FS, D, T, SparseTable<FD, FS, D, T>>;

    fn expecting() -> &'static str {
        "a SparseTensor"
    }

    async fn visit_seq<A: de::SeqAccess>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let schema = seq.next_element::<(Vec<u64>, ValueType)>(()).await?;
        let (shape, dtype) = schema.ok_or_else(|| de::Error::invalid_length(0, "tensor schema"))?;
        let shape = Shape::from(shape);
        let dtype = dtype.try_into().map_err(de::Error::custom)?;

        let txn_id = *self.txn.id();
        let table = SparseTable::create(self.txn.context(), txn_id, (shape, dtype))
            .map_err(de::Error::custom)
            .await?;

        if let Some(table) = seq
            .next_element::<SparseTable<FD, FS, D, T>>((table.clone(), txn_id))
            .await?
        {
            Ok(SparseTensor::from(table))
        } else {
            Ok(SparseTensor::from(table))
        }
    }
}

pub struct SparseTensorView<'en> {
    shape: Vec<u64>,
    dtype: ValueType,
    filled: SparseStream<'en>,
}

impl<'en> en::IntoStream<'en> for SparseTensorView<'en> {
    fn into_stream<E: en::Encoder<'en>>(self, encoder: E) -> Result<E::Ok, E::Error> {
        let schema = (self.shape.to_vec(), self.dtype.path());
        let filled = en::SeqStream::from(self.filled);
        (schema, filled).into_stream(encoder)
    }
}

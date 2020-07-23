use std::convert::TryInto;
use std::iter;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;

use arrayfire as af;
use async_trait::async_trait;
use futures::future;
use futures::stream::{self, FuturesOrdered, Stream, StreamExt, TryStreamExt};
use itertools::Itertools;

use crate::error;
use crate::state::file::block::{BlockId, BlockOwned};
use crate::state::file::File;
use crate::transaction::{Txn, TxnId};
use crate::value::class::{ComplexType, FloatType, NumberType};
use crate::value::class::{Impl, NumberClass};
use crate::value::{Number, TCResult};

use super::array::*;
use super::base::*;
use super::bounds::*;
use super::sparse::SparseTensorView;
use super::stream::{ValueBlockStream, ValueStream};

const BLOCK_SIZE: usize = 1_000_000;

#[async_trait]
pub trait DenseTensorView: TensorView + 'static {
    type BlockStream: Stream<Item = TCResult<Array>> + Send + Unpin;
    type ValueStream: Stream<Item = TCResult<Number>> + Send;

    fn block_stream(self: Arc<Self>, txn_id: TxnId) -> Self::BlockStream;

    fn value_stream(self: Arc<Self>, txn_id: TxnId, bounds: Bounds) -> Self::ValueStream;

    async fn write<T: DenseTensorView>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Arc<T>,
    ) -> TCResult<()>;

    async fn write_value(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCResult<()>;
}

#[async_trait]
impl<T: DenseTensorView, O: DenseTensorView> DenseTensorBoolean<O> for T {
    async fn and(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.and(&r?)).await
    }

    async fn or(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.or(&r?)).await
    }

    async fn xor(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.xor(&r?)).await
    }
}

#[async_trait]
impl<T: DenseTensorView + Slice, O: DenseTensorView> DenseTensorCompare<O> for T {
    async fn equals(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.equals(&r?)).await
    }

    async fn gt(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.gt(&r?)).await
    }

    async fn gte(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.gte(&r?)).await
    }

    async fn lt(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.lt(&r?)).await
    }

    async fn lte(self: Arc<Self>, other: Arc<O>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        BlockTensor::combine(txn, self, other, |(l, r)| l?.lte(&r?)).await
    }
}

#[async_trait]
impl<T: DenseTensorView + Slice> DenseTensorUnary for T
where
    <T as Slice>::Slice: DenseTensorUnary,
{
    async fn as_dtype(
        self: Arc<Self>,
        txn: Arc<Txn>,
        dtype: NumberType,
    ) -> TCResult<Arc<BlockTensor>> {
        let shape = self.shape().clone();
        let per_block = per_block(dtype);
        let source = self
            .block_stream(txn.id().clone())
            .map(move |data| data.and_then(|d| d.into_type(dtype.clone())));
        let values = ValueStream::new(source);
        let blocks = ValueBlockStream::new(values, dtype, per_block);
        BlockTensor::from_blocks(txn, shape, dtype, blocks).await
    }

    async fn copy(self: Arc<Self>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        let shape = self.shape().clone();
        let dtype = self.dtype();
        let blocks = self.block_stream(txn.id().clone());
        BlockTensor::from_blocks(txn, shape, dtype, blocks).await
    }

    async fn abs(self: Arc<Self>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        let shape = self.shape().clone();
        let txn_id = txn.id().clone();

        use NumberType::*;
        match self.dtype() {
            Bool => BlockTensor::from_blocks(txn, shape, Bool, self.block_stream(txn_id)).await,
            UInt(u) => {
                BlockTensor::from_blocks(txn, shape, u.into(), self.block_stream(txn_id)).await
            }
            Complex(c) => match c {
                ComplexType::C32 => {
                    let dtype = FloatType::F32.into();
                    let source = self.block_stream(txn_id).map(|d| d?.abs());
                    let per_block = per_block(dtype);
                    let values = ValueStream::new(source);
                    let blocks = ValueBlockStream::new(values, dtype, per_block);
                    BlockTensor::from_blocks(txn, shape, dtype, blocks).await
                }
                ComplexType::C64 => {
                    let dtype = FloatType::F64.into();
                    let source = self.block_stream(txn_id).map(|d| d?.abs());
                    let per_block = per_block(dtype);
                    let values = ValueStream::new(source);
                    let blocks = ValueBlockStream::new(values, dtype, per_block);
                    BlockTensor::from_blocks(txn, shape, dtype, blocks).await
                }
            },
            dtype => {
                let blocks = self.block_stream(txn_id).map(|d| d?.abs());
                BlockTensor::from_blocks(txn, shape, dtype, blocks).await
            }
        }
    }

    async fn sum(self: Arc<Self>, txn: Arc<Txn>, axis: usize) -> TCResult<Arc<BlockTensor>> {
        if axis >= self.ndim() {
            return Err(error::bad_request("Axis out of range", axis));
        }

        let txn_id = txn.id().clone();
        let mut shape = self.shape().clone();
        shape.remove(axis);
        let summed = BlockTensor::constant(txn.clone(), shape, self.dtype().zero()).await?;

        if axis == 0 {
            reduce_axis0(self)
                .then(|r| async {
                    let (bounds, slice) = r?;
                    let value = slice.sum_all(txn_id.clone()).await?;
                    summed
                        .clone()
                        .write_value(txn_id.clone(), bounds, value)
                        .await
                })
                .fold(Ok(()), |_, r| future::ready(r))
                .await?;
        } else {
            reduce_axis(self, axis)
                .then(|r| async {
                    let (bounds, slice) = r?;
                    let value = slice.sum(txn.clone().subcontext_tmp().await?, 0).await?;
                    summed.clone().write(txn_id.clone(), bounds, value).await
                })
                .fold(Ok(()), |_, r| future::ready(r))
                .await?;
        }

        Ok(summed)
    }

    async fn sum_all(self: Arc<Self>, txn_id: TxnId) -> TCResult<Number> {
        let mut sum = self.dtype().zero();
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            sum = sum + block?.sum();
        }

        Ok(sum)
    }

    async fn product(self: Arc<Self>, txn: Arc<Txn>, axis: usize) -> TCResult<Arc<BlockTensor>> {
        if axis >= self.ndim() {
            return Err(error::bad_request("Axis out of range", axis));
        }

        let txn_id = txn.id().clone();
        let mut shape = self.shape().clone();
        shape.remove(axis);
        let product = BlockTensor::constant(txn.clone(), shape, self.dtype().zero()).await?;

        if axis == 0 {
            reduce_axis0(self)
                .then(|r| async {
                    let (bounds, slice) = r?;
                    let value = slice.product_all(txn_id.clone()).await?;
                    product
                        .clone()
                        .write_value(txn_id.clone(), bounds, value)
                        .await
                })
                .fold(Ok(()), |_, r| future::ready(r))
                .await?;
        } else {
            reduce_axis(self, axis)
                .then(|r| async {
                    let (bounds, slice) = r?;
                    let value = slice
                        .product(txn.clone().subcontext_tmp().await?, 0)
                        .await?;
                    product.clone().write(txn_id.clone(), bounds, value).await
                })
                .fold(Ok(()), |_, r| future::ready(r))
                .await?;
        }

        Ok(product)
    }

    async fn product_all(self: Arc<Self>, txn_id: TxnId) -> TCResult<Number> {
        let mut product = self.dtype().one();
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            product = product * block?.product();
        }

        Ok(product)
    }

    async fn not(self: Arc<Self>, txn: Arc<Txn>) -> TCResult<Arc<BlockTensor>> {
        let blocks = self
            .clone()
            .as_dtype(txn.clone(), NumberType::Bool)
            .await?
            .block_stream(txn.id().clone())
            .map(|c| Ok(c?.not()));

        BlockTensor::from_blocks(txn, self.shape().clone(), NumberType::Bool, blocks).await
    }
}

pub struct BlockTensor {
    file: Arc<File<Array>>,
    dtype: NumberType,
    shape: Shape,
    per_block: usize,
    coord_bounds: Vec<u64>,
}

impl BlockTensor {
    async fn combine<
        L: DenseTensorView,
        R: DenseTensorView,
        C: FnMut((TCResult<Array>, TCResult<Array>)) -> TCResult<Array> + Send,
    >(
        txn: Arc<Txn>,
        left: Arc<L>,
        right: Arc<R>,
        combinator: C,
    ) -> TCResult<Arc<BlockTensor>> {
        compatible(&left, &right)?;

        let shape = left.shape().clone();
        let dtype = left.dtype();
        let blocks = left
            .block_stream(txn.id().clone())
            .zip(right.block_stream(txn.id().clone()))
            .map(combinator);

        BlockTensor::from_blocks(txn, shape, dtype, blocks).await
    }

    pub async fn constant(
        txn: Arc<Txn>,
        shape: Shape,
        value: Number,
    ) -> TCResult<Arc<BlockTensor>> {
        let per_block = BLOCK_SIZE / value.class().size();
        let size = shape.size();

        let value_clone = value.clone();
        let blocks = (0..(size / per_block as u64))
            .map(move |_| Ok(Array::constant(value_clone.clone(), per_block)));
        let trailing_len = (size % (per_block as u64)) as usize;
        if trailing_len > 0 {
            let blocks = blocks.chain(iter::once(Ok(Array::constant(value.clone(), trailing_len))));
            BlockTensor::from_blocks(txn, shape, value.class(), stream::iter(blocks)).await
        } else {
            BlockTensor::from_blocks(txn, shape, value.class(), stream::iter(blocks)).await
        }
    }

    pub async fn from_blocks<S: Stream<Item = TCResult<Array>> + Send + Unpin>(
        txn: Arc<Txn>,
        shape: Shape,
        dtype: NumberType,
        blocks: S,
    ) -> TCResult<Arc<BlockTensor>> {
        let file = txn
            .context()
            .create_tensor(txn.id().clone(), "block_tensor".parse()?)
            .await?;

        blocks
            .enumerate()
            .map(|(i, r)| r.map(|block| (BlockId::from(i), block)))
            .map_ok(|(id, block)| file.create_block(txn.id().clone(), id, block))
            .try_buffer_unordered(2)
            .try_fold((), |_, _| future::ready(Ok(())))
            .await?;

        let coord_bounds = (0..shape.len())
            .map(|axis| shape[axis + 1..].iter().product())
            .collect();

        Ok(Arc::new(BlockTensor {
            dtype,
            shape,
            file,
            per_block: per_block(dtype),
            coord_bounds,
        }))
    }

    pub async fn from_sparse<S: SparseTensorView + Slice>(
        txn: Arc<Txn>,
        sparse: Arc<S>,
    ) -> TCResult<Arc<BlockTensor>>
    where
        <S as Slice>::Slice: SparseTensorView,
    {
        let shape = sparse.shape().clone();
        let dtype = sparse.dtype();
        let blocks = Self::sparse_into_blocks(txn.id().clone(), sparse);
        BlockTensor::from_blocks(txn, shape, dtype, Box::pin(blocks)).await
    }

    pub fn sparse_into_blocks<S: SparseTensorView + Slice>(
        txn_id: TxnId,
        sparse: Arc<S>,
    ) -> impl Stream<Item = TCResult<Array>>
    where
        <S as Slice>::Slice: SparseTensorView,
    {
        let shape = sparse.shape().clone();
        let dtype = sparse.dtype();
        let per_block = per_block(dtype);
        let ndim = shape.len();
        let size = shape.size();

        let coord_bounds: Vec<u64> = (0..shape.len())
            .map(|axis| shape[axis + 1..].iter().product())
            .collect();

        let from_offsets = (0..size).step_by(per_block);
        let to_offsets =
            (per_block as u64..((size % per_block as u64) + per_block as u64)).step_by(per_block);

        let from_limit = Bounds::all(sparse.shape()).affected().step_by(per_block);
        let mut to_limit = Bounds::all(sparse.shape())
            .affected()
            .step_by(per_block)
            .chain(iter::once(shape.to_vec()));
        to_limit.next();

        stream::iter(from_limit.zip(to_limit))
            .map(Bounds::from)
            .map(move |bounds| sparse.clone().slice(bounds))
            .and_then(move |slice| slice.filled(txn_id.clone()))
            .and_then(|filled| async {
                let values: Vec<(Vec<u64>, Number)> = filled.collect().await;
                Ok(values)
            })
            .zip(stream::iter(from_offsets.zip(to_offsets)))
            .map(|(r, offset)| r.map(|values| (values, offset)))
            .and_then(move |(mut values, (from_offset, to_offset))| {
                let coord_bounds =
                    af::Array::new(&coord_bounds, af::Dim4::new(&[ndim as u64, 1, 1, 1]));

                async move {
                    let mut block =
                        Array::constant(dtype.zero(), (to_offset - from_offset) as usize);

                    if values.is_empty() {
                        return Ok(block);
                    }

                    let (mut coords, values): (Vec<Vec<u64>>, Vec<Number>) =
                        values.drain(..).unzip();
                    let coords: Vec<u64> = coords.drain(..).flatten().collect();
                    let coords_dim = af::Dim4::new(&[ndim as u64, values.len() as u64, 1, 1]);
                    let mut coords: af::Array<u64> = af::Array::new(&coords, coords_dim);
                    coords *= af::tile(&coord_bounds, coords_dim);
                    let mut coords = af::sum(&coords, 1);
                    coords -=
                        af::constant(from_offset, af::Dim4::new(&[values.len() as u64, 1, 1, 1]));

                    let values = Array::try_from_values(values, dtype)?;
                    block.set(coords, &values)?;
                    Ok(block)
                }
            })
    }

    fn blocks(self: Arc<Self>, txn_id: TxnId) -> impl Stream<Item = TCResult<BlockOwned<Array>>> {
        stream::iter(0..(self.size() / self.per_block as u64))
            .map(BlockId::from)
            .then(move |block_id| self.file.clone().get_block_owned(txn_id.clone(), block_id))
    }
}

impl TensorView for BlockTensor {
    fn dtype(&self) -> NumberType {
        self.dtype
    }

    fn ndim(&self) -> usize {
        self.shape.len()
    }

    fn shape(&'_ self) -> &'_ Shape {
        &self.shape
    }

    fn size(&self) -> u64 {
        self.shape.size()
    }
}

#[async_trait]
impl AnyAll for BlockTensor {
    async fn all(self: Arc<Self>, txn_id: TxnId) -> TCResult<bool> {
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            if !block?.all() {
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn any(self: Arc<Self>, txn_id: TxnId) -> TCResult<bool> {
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            if !block?.any() {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

#[async_trait]
impl DenseTensorView for BlockTensor {
    type BlockStream = Pin<Box<dyn Stream<Item = TCResult<Array>> + Send>>;
    type ValueStream = Pin<Box<dyn Stream<Item = TCResult<Number>> + Send>>;

    fn block_stream(self: Arc<Self>, txn_id: TxnId) -> Self::BlockStream {
        Box::pin(self.blocks(txn_id).map(|r| r.map(|block| block.clone())))
    }

    fn value_stream(self: Arc<Self>, txn_id: TxnId, bounds: Bounds) -> Self::ValueStream {
        if bounds == self.shape().all() {
            return Box::pin(ValueStream::new(self.block_stream(txn_id)));
        }

        assert!(self.shape().contains_bounds(&bounds));
        let mut selected = FuturesOrdered::new();

        let ndim = bounds.ndim();

        let coord_bounds = af::Array::new(
            &self.coord_bounds,
            af::Dim4::new(&[self.ndim() as u64, 1, 1, 1]),
        );
        let per_block = self.per_block;

        for coords in &bounds.affected().chunks(self.per_block) {
            let (block_ids, af_indices, af_offsets, num_coords) =
                coord_block(coords, &coord_bounds, per_block, ndim);

            let this = self.clone();
            let txn_id = txn_id.clone();

            selected.push(async move {
                let mut start = 0.0f64;
                let mut values = vec![];
                for block_id in block_ids {
                    let (block_offsets, new_start) =
                        block_offsets(&af_indices, &af_offsets, num_coords, start, block_id);

                    match this.file.clone().get_block(&txn_id, block_id.into()).await {
                        Ok(block) => {
                            let array: &Array = block.deref().try_into().unwrap();
                            values.extend(array.get(block_offsets));
                        }
                        Err(cause) => return stream::iter(vec![Err(cause)]),
                    }

                    start = new_start;
                }

                let values: Vec<TCResult<Number>> = values.drain(..).map(Ok).collect();
                stream::iter(values)
            });
        }

        Box::pin(selected.flatten())
    }

    async fn write<T: DenseTensorView>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Arc<T>,
    ) -> TCResult<()> {
        if !self.shape().contains_bounds(&bounds) {
            return Err(error::bad_request("Bounds out of bounds", bounds));
        }

        let ndim = bounds.ndim();

        let coord_bounds = af::Array::new(
            &self.coord_bounds,
            af::Dim4::new(&[self.ndim() as u64, 1, 1, 1]),
        );
        let per_block = self.per_block;

        stream::iter(bounds.affected())
            .chunks(self.per_block)
            .zip(value.block_stream(txn_id.clone()))
            .map(|(coords, block)| {
                let (block_ids, af_indices, af_offsets, num_coords) =
                    coord_block(coords.into_iter(), &coord_bounds, per_block, ndim);

                let this = self.clone();
                let txn_id = txn_id.clone();

                async move {
                    let values = block?;
                    let mut start = 0.0f64;
                    for block_id in block_ids {
                        let (block_offsets, new_start) =
                            block_offsets(&af_indices, &af_offsets, num_coords, start, block_id);

                        let mut block = this
                            .file
                            .get_block(&txn_id, block_id.into())
                            .await?
                            .upgrade()
                            .await?;
                        block.deref_mut().set(block_offsets, &values)?;
                        start = new_start;
                    }

                    Ok(())
                }
            })
            .buffer_unordered(2)
            .try_fold((), |_, _| future::ready(Ok(())))
            .await
    }

    async fn write_value(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Number,
    ) -> TCResult<()> {
        if !self.shape().contains_bounds(&bounds) {
            return Err(error::bad_request("Bounds out of bounds", bounds));
        }

        let ndim = bounds.ndim();

        let coord_bounds = af::Array::new(
            &self.coord_bounds,
            af::Dim4::new(&[self.ndim() as u64, 1, 1, 1]),
        );
        let per_block = self.per_block;

        stream::iter(bounds.affected())
            .chunks(self.per_block)
            .map(|coords| {
                let (block_ids, af_indices, af_offsets, num_coords) =
                    coord_block(coords.into_iter(), &coord_bounds, per_block, ndim);

                let this = self.clone();
                let value = value.clone();
                let txn_id = txn_id.clone();

                Ok(async move {
                    let mut start = 0.0f64;
                    for block_id in block_ids {
                        let value = value.clone();
                        let (block_offsets, new_start) =
                            block_offsets(&af_indices, &af_offsets, num_coords, start, block_id);

                        let mut block = this
                            .file
                            .get_block(&txn_id, block_id.into())
                            .await?
                            .upgrade()
                            .await?;
                        let value = Array::constant(value, (new_start - start) as usize);
                        block.deref_mut().set(block_offsets, &value)?;
                        start = new_start;
                    }

                    Ok(())
                })
            })
            .try_buffer_unordered(2)
            .fold(Ok(()), |_, r| future::ready(r))
            .await
    }
}

impl Slice for BlockTensor {
    type Slice = TensorSlice<BlockTensor>;

    fn slice(self: Arc<Self>, bounds: Bounds) -> TCResult<Arc<Self::Slice>> {
        Ok(Arc::new(TensorSlice::new(self, bounds)?))
    }
}

#[async_trait]
impl<T: Rebase + Slice + 'static> DenseTensorView for T
where
    <Self as Rebase>::Source: DenseTensorView,
{
    type BlockStream = ValueBlockStream<Self::ValueStream>;
    type ValueStream = <<Self as Rebase>::Source as DenseTensorView>::ValueStream;

    fn block_stream(self: Arc<Self>, txn_id: TxnId) -> Self::BlockStream {
        let dtype = self.source().dtype();
        let bounds = self.shape().all();
        ValueBlockStream::new(self.value_stream(txn_id, bounds), dtype, per_block(dtype))
    }

    fn value_stream(self: Arc<Self>, txn_id: TxnId, bounds: Bounds) -> Self::ValueStream {
        assert!(self.shape().contains_bounds(&bounds));
        self.source()
            .value_stream(txn_id, self.invert_bounds(bounds))
    }

    async fn write<O: DenseTensorView>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Arc<O>,
    ) -> TCResult<()> {
        self.source()
            .write(txn_id, self.invert_bounds(bounds), value)
            .await
    }

    async fn write_value(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Number,
    ) -> TCResult<()> {
        self.source()
            .write_value(txn_id, self.invert_bounds(bounds), value)
            .await
    }
}

#[async_trait]
impl AnyAll for TensorSlice<BlockTensor> {
    async fn all(self: Arc<Self>, txn_id: TxnId) -> TCResult<bool> {
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            if !block?.all() {
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn any(self: Arc<Self>, txn_id: TxnId) -> TCResult<bool> {
        let mut blocks = self.block_stream(txn_id);
        while let Some(block) = blocks.next().await {
            if !block?.any() {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

pub fn per_block(dtype: NumberType) -> usize {
    BLOCK_SIZE / dtype.size()
}

fn compatible<L: TensorView, R: TensorView>(l: &Arc<L>, r: &Arc<R>) -> TCResult<()> {
    if l.shape() != r.shape() {
        Err(error::bad_request(
            "Can't compare shapes (try broadcasting first)",
            format!("{} != {}", l.shape(), r.shape()),
        ))
    } else if l.dtype() != r.dtype() {
        Err(error::bad_request(
            "Can't compare data types (try casting first)",
            format!("{} != {}", l.dtype(), r.dtype()),
        ))
    } else {
        Ok(())
    }
}

fn block_offsets(
    af_indices: &af::Array<u64>,
    af_offsets: &af::Array<u64>,
    num_coords: u64,
    start: f64,
    block_id: u64,
) -> (af::Array<u64>, f64) {
    let num_to_update = af::sum_all(&af::eq(
        af_indices,
        &af::constant(block_id, af_indices.dims()),
        false,
    ))
    .0;
    let block_offsets = af::index(
        af_offsets,
        &[
            af::Seq::new(block_id as f64, block_id as f64, 1.0f64),
            af::Seq::new(start, (start + num_to_update) - 1.0f64, 1.0f64),
        ],
    );
    let block_offsets = af::moddims(&block_offsets, af::Dim4::new(&[num_coords as u64, 1, 1, 1]));

    (block_offsets, (start + num_to_update))
}

fn coord_block<I: Iterator<Item = Vec<u64>>>(
    coords: I,
    coord_bounds: &af::Array<u64>,
    per_block: usize,
    ndim: usize,
) -> (Vec<u64>, af::Array<u64>, af::Array<u64>, u64) {
    let coords: Vec<u64> = coords.flatten().collect();
    let num_coords = coords.len() / ndim;
    let af_coords_dim = af::Dim4::new(&[num_coords as u64, ndim as u64, 1, 1]);
    let af_coords = af::Array::new(&coords, af_coords_dim) * af::tile(coord_bounds, af_coords_dim);
    let af_coords = af::sum(&af_coords, 1);
    let af_per_block = af::constant(
        per_block as u64,
        af::Dim4::new(&[1, num_coords as u64, 1, 1]),
    );
    let af_offsets = af_coords.copy() % af_per_block.copy();
    let af_indices = af_coords / af_per_block;
    let af_block_ids = af::set_unique(&af_indices, true);

    let mut block_ids: Vec<u64> = Vec::with_capacity(af_block_ids.elements());
    af_block_ids.host(&mut block_ids);
    (block_ids, af_indices, af_offsets, num_coords as u64)
}

fn reduce_axis0<T: DenseTensorView + Slice>(
    source: Arc<T>,
) -> impl Stream<Item = TCResult<(Bounds, Arc<<T as Slice>::Slice>)>> {
    assert!(source.shape().len() > 1);
    let shape: Shape = source.shape()[1..].to_vec().into();
    let axis_bounds = source.shape().all()[0].clone();
    stream::iter(shape.all().affected()).map(move |coord| {
        let source_bounds: Bounds = (axis_bounds.clone(), coord.clone()).into();
        let slice = source.clone().slice(source_bounds)?;
        Ok((coord.into(), slice))
    })
}

fn reduce_axis<T: DenseTensorView + Slice>(
    source: Arc<T>,
    axis: usize,
) -> impl Stream<Item = TCResult<(Bounds, Arc<<T as Slice>::Slice>)>> {
    let prefix_range: Shape = source.shape()[0..axis].to_vec().into();
    stream::iter(prefix_range.all().affected()).map(move |coord| {
        let bounds: Bounds = coord.into();
        let slice = source.clone().slice(bounds.clone())?;
        Ok((bounds, slice))
    })
}

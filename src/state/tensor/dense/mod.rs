use std::convert::TryInto;
use std::iter;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use arrayfire as af;
use futures::future::{self, TryFutureExt};
use futures::stream::{self, Stream, StreamExt, TryStreamExt};
use itertools::Itertools;

use crate::error;
use crate::state::file::block::BlockId;
use crate::state::file::File;
use crate::transaction::{Txn, TxnId};
use crate::value::class::{Impl, NumberClass, NumberImpl, NumberType};
use crate::value::{Number, TCBoxTryFuture, TCResult, TCStream, TCTryStream};

use super::bounds::{Bounds, Shape};
use super::*;

pub mod array;

use array::Array;

const PER_BLOCK: usize = 131_072; // = 1 mibibyte / 64 bits

const ERR_BROADCAST_WRITE: &str = "Cannot write to a broadcasted tensor since it is not a \
bijection of its source. Consider copying the broadcast, or writing directly to the source Tensor.";
const ERR_CORRUPT: &str = "DenseTensor corrupted! Please file a bug report.";

trait BlockList: TensorView + 'static {
    fn block_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Array>> {
        Box::pin(async move {
            let dtype = self.dtype();
            let blocks = self
                .value_stream(txn_id)
                .await?
                .chunks(PER_BLOCK)
                .map(|values| values.into_iter().collect::<TCResult<Vec<Number>>>())
                .and_then(move |values| future::ready(Array::try_from_values(values, dtype)));

            let blocks: TCTryStream<Array> = Box::pin(blocks);
            Ok(blocks)
        })
    }

    fn value_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let values = self
                .block_stream(txn_id)
                .await?
                .and_then(|array| future::ready(Ok(array.into_values())))
                .map_ok(|mut values| values.drain(..).map(Ok).collect::<Vec<TCResult<Number>>>())
                .map_ok(stream::iter)
                .try_flatten();

            let values: TCTryStream<Number> = Box::pin(values);
            Ok(values)
        })
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>>;

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number>;

    fn read_value_at_owned<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        coord: Vec<u64>,
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move { self.read_value_at(&txn_id, &coord).await })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()>;

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()>;
}

#[derive(Clone)]
pub struct BlockListFile {
    file: Arc<File<Array>>,
    dtype: NumberType,
    shape: Shape,
    coord_bounds: Vec<u64>,
}

impl BlockListFile {
    pub async fn constant(txn: Arc<Txn>, shape: Shape, value: Number) -> TCResult<BlockListFile> {
        let size = shape.size();

        let value_clone = value.clone();
        let blocks = (0..(size / PER_BLOCK as u64))
            .map(move |_| Ok(Array::constant(value_clone.clone(), PER_BLOCK)));
        let trailing_len = (size % (PER_BLOCK as u64)) as usize;
        if trailing_len > 0 {
            let blocks = blocks.chain(iter::once(Ok(Array::constant(value.clone(), trailing_len))));
            BlockListFile::from_blocks(txn, shape, value.class(), stream::iter(blocks)).await
        } else {
            BlockListFile::from_blocks(txn, shape, value.class(), stream::iter(blocks)).await
        }
    }

    pub async fn from_blocks<S: Stream<Item = TCResult<Array>> + Send + Unpin>(
        txn: Arc<Txn>,
        shape: Shape,
        dtype: NumberType,
        blocks: S,
    ) -> TCResult<BlockListFile> {
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

        Ok(BlockListFile {
            dtype,
            shape,
            file,
            coord_bounds,
        })
    }
}

impl TensorView for BlockListFile {
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

impl BlockList for BlockListFile {
    fn block_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Array>> {
        Box::pin(async move {
            let block_stream = Box::pin(
                stream::iter(0..(self.size() / PER_BLOCK as u64))
                    .map(BlockId::from)
                    .then(move |block_id| {
                        self.file.clone().get_block_owned(txn_id.clone(), block_id)
                    }),
            );
            let block_stream =
                block_stream.and_then(|block| future::ready(Ok(block.deref().clone())));
            let block_stream: TCTryStream<Array> = Box::pin(block_stream);
            Ok(block_stream)
        })
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            if bounds == self.shape().all() {
                return self.value_stream(txn_id).await;
            }

            if !self.shape.contains_bounds(&bounds) {
                return Err(error::bad_request("Invalid bounds", bounds));
            }

            let ndim = bounds.ndim();

            let coord_bounds = af::Array::new(
                &self.coord_bounds,
                af::Dim4::new(&[self.ndim() as u64, 1, 1, 1]),
            );

            let selected =
                stream::iter(bounds.affected())
                    .chunks(PER_BLOCK)
                    .then(move |mut coords| {
                        let (block_ids, af_indices, af_offsets, num_coords) =
                            coord_block(coords.drain(..), &coord_bounds, PER_BLOCK, ndim);

                        let this = self.clone();
                        let txn_id = txn_id.clone();

                        Box::pin(async move {
                            let mut start = 0.0f64;
                            let mut values = vec![];
                            for block_id in block_ids {
                                let (block_offsets, new_start) = block_offsets(
                                    &af_indices,
                                    &af_offsets,
                                    num_coords,
                                    start,
                                    block_id,
                                );

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
                        })
                    });

            let selected: TCTryStream<Number> = Box::pin(selected.flatten());
            Ok(selected)
        })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        Box::pin(async move {
            if !self.shape().contains_bounds(&bounds) {
                return Err(error::bad_request("Bounds out of bounds", bounds));
            }

            let ndim = bounds.ndim();

            let coord_bounds = af::Array::new(
                &self.coord_bounds,
                af::Dim4::new(&[self.ndim() as u64, 1, 1, 1]),
            );

            stream::iter(bounds.affected())
                .chunks(PER_BLOCK)
                .map(|coords| {
                    let (block_ids, af_indices, af_offsets, num_coords) =
                        coord_block(coords.into_iter(), &coord_bounds, PER_BLOCK, ndim);

                    let this = self.clone();
                    let value = value.clone();
                    let txn_id = txn_id.clone();

                    Ok(async move {
                        let mut start = 0.0f64;
                        for block_id in block_ids {
                            let value = value.clone();
                            let (block_offsets, new_start) = block_offsets(
                                &af_indices,
                                &af_offsets,
                                num_coords,
                                start,
                                block_id,
                            );

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
        })
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move {
            if !self.shape().contains_coord(coord) {
                let coord: Vec<String> = coord.iter().map(|c| c.to_string()).collect();
                return Err(error::bad_request(
                    "Coordinate is out of bounds",
                    coord.join(", "),
                ));
            }

            let offset: u64 = self
                .coord_bounds
                .iter()
                .zip(coord.iter())
                .map(|(d, x)| d * x)
                .sum();
            let block_id: u64 = offset / PER_BLOCK as u64;
            let block = self.file.get_block(txn_id, block_id.into()).await?;
            block
                .deref()
                .get_value((offset % PER_BLOCK as u64) as usize)
                .ok_or_else(|| error::internal(ERR_CORRUPT))
        })
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        Box::pin(async move {
            if !self.shape().contains_coord(&coord) {
                return Err(error::bad_request(
                    "Invalid coordinate",
                    format!("[{}]", coord.iter().map(|x| x.to_string()).join(", ")),
                ));
            } else if value.class() != self.dtype() {
                return Err(error::bad_request(
                    "Wrong class for tensor value",
                    value.class(),
                ));
            }

            let offset: u64 = self
                .coord_bounds
                .iter()
                .zip(coord.iter())
                .map(|(d, x)| d * x)
                .sum();
            let block_id: u64 = offset / PER_BLOCK as u64;
            let mut block = self
                .file
                .get_block(&txn_id, block_id.into())
                .await?
                .upgrade()
                .await?;
            block
                .deref_mut()
                .set_value((offset % PER_BLOCK as u64) as usize, value)
        })
    }
}

struct BlockListBroadcast {
    source: Arc<dyn BlockList>,
    rebase: transform::Broadcast,
}

impl TensorView for BlockListBroadcast {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.source.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.rebase.shape()
    }

    fn size(&self) -> u64 {
        self.source.size()
    }
}

impl BlockList for BlockListBroadcast {
    fn value_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        let bounds = Bounds::all(self.shape());
        self.slice_value_stream(txn_id, bounds)
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let source_bounds = self.rebase.invert_bounds(bounds);
            let source_coords = source_bounds.affected();
            let rebase = self.rebase.clone();
            let values = self
                .source
                .clone()
                .slice_value_stream(txn_id, source_bounds)
                .await?
                .zip(stream::iter(source_coords))
                .map(move |(value, coord)| {
                    let broadcast = rebase.invert_bounds(coord.into());
                    stream::iter(iter::repeat(value).take(broadcast.size() as usize))
                })
                .flatten();

            let values: TCTryStream<Number> = Box::pin(values);
            Ok(values)
        })
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move {
            let coord = self.rebase.invert_coord(coord);
            self.source.read_value_at(txn_id, &coord).await
        })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        _txn_id: TxnId,
        _bounds: Bounds,
        _number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        Box::pin(future::ready(Err(error::unsupported(ERR_BROADCAST_WRITE))))
    }

    fn write_value_at<'a>(
        &'a self,
        _txn_id: TxnId,
        _coord: Vec<u64>,
        _value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        Box::pin(future::ready(Err(error::unsupported(ERR_BROADCAST_WRITE))))
    }
}

struct BlockListCast {
    source: Arc<dyn BlockList>,
    dtype: NumberType,
}

impl TensorView for BlockListCast {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.source.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.source.shape()
    }

    fn size(&self) -> u64 {
        self.source.size()
    }
}

impl BlockList for BlockListCast {
    fn block_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Array>> {
        Box::pin(async move {
            let dtype = self.dtype;
            let blocks: TCStream<TCResult<Array>> =
                self.source.clone().block_stream(txn_id).await?;
            let cast = blocks.and_then(move |array| future::ready(array.into_type(dtype)));
            let cast: TCTryStream<Array> = Box::pin(cast);
            Ok(cast)
        })
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let dtype = self.dtype;
            let value_stream = self
                .source
                .clone()
                .slice_value_stream(txn_id, bounds)
                .await?
                .map_ok(move |value| value.into_type(dtype));

            let value_stream: TCTryStream<Number> = Box::pin(value_stream);
            Ok(value_stream)
        })
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        let dtype = self.dtype;
        Box::pin(
            self.source
                .read_value_at(txn_id, coord)
                .map_ok(move |value| value.into_type(dtype)),
        )
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        self.source.clone().write_value(txn_id, bounds, number)
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        self.source.write_value_at(txn_id, coord, value)
    }
}

struct BlockListExpand {
    source: Arc<dyn BlockList>,
    rebase: transform::Expand,
}

impl TensorView for BlockListExpand {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.source.ndim() + 1
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.rebase.shape()
    }

    fn size(&self) -> u64 {
        self.shape().size()
    }
}

impl BlockList for BlockListExpand {
    fn block_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Array>> {
        self.source.clone().block_stream(txn_id)
    }

    fn value_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        self.source.clone().value_stream(txn_id)
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        let bounds = self.rebase.invert_bounds(bounds);
        self.source.clone().slice_value_stream(txn_id, bounds)
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move {
            let coord = self.rebase.invert_coord(coord);
            self.source.read_value_at(txn_id, &coord).await
        })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let bounds = self.rebase.invert_bounds(bounds);
        self.source.clone().write_value(txn_id, bounds, number)
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let coord = self.rebase.invert_coord(&coord);
        self.source.write_value_at(txn_id, coord, value)
    }
}

struct BlockListSlice {
    source: Arc<dyn BlockList>,
    rebase: transform::Slice,
}

impl TensorView for BlockListSlice {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.rebase.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.rebase.shape()
    }

    fn size(&self) -> u64 {
        self.rebase.size()
    }
}

impl BlockList for BlockListSlice {
    fn value_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        self.source
            .clone()
            .slice_value_stream(txn_id, self.rebase.bounds().clone())
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        self.source
            .clone()
            .slice_value_stream(txn_id, self.rebase.invert_bounds(bounds))
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move {
            let coord = self.rebase.invert_coord(coord);
            self.source.read_value_at(txn_id, &coord).await
        })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let bounds = self.rebase.invert_bounds(bounds);
        self.source.clone().write_value(txn_id, bounds, number)
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let coord = self.rebase.invert_coord(&coord);
        self.source.write_value_at(txn_id, coord, value)
    }
}

struct BlockListSparse {
    source: SparseTensor,
    coord_index: Vec<u64>,
}

impl BlockListSparse {
    fn new(source: SparseTensor) -> BlockListSparse {
        let shape = source.shape().to_vec();
        let coord_index: Vec<u64> = (0..source.ndim())
            .map(|x| shape[x + 1..].iter().product())
            .collect();

        BlockListSparse {
            source,
            coord_index,
        }
    }
}

impl TensorView for BlockListSparse {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.source.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.source.shape()
    }

    fn size(&self) -> u64 {
        self.source.size()
    }
}

impl BlockList for BlockListSparse {
    fn block_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Array>> {
        Box::pin(async move {
            let dtype = self.dtype();
            let ndim = self.ndim();
            let source = self.source.clone();
            let shape = self.source.shape().to_vec();
            let coord_index = self.coord_index.to_vec();

            let block_offsets = ((PER_BLOCK as u64)..self.size()).step_by(PER_BLOCK);
            let block_stream = stream::iter(block_offsets)
                .map(|offset| (offset - PER_BLOCK as u64, offset))
                .map(move |(start, end)| {
                    let start: Vec<u64> = coord_index
                        .iter()
                        .zip(shape.iter())
                        .map(|(index, dim)| (start / index) % dim)
                        .collect();

                    let end: Vec<u64> = coord_index
                        .iter()
                        .zip(shape.iter())
                        .map(|(index, dim)| (end / index) % dim)
                        .collect();

                    (start, end)
                })
                .then(move |(start, end)| {
                    let source = source.clone();
                    let txn_id = txn_id.clone();

                    Box::pin(async move {
                        let mut filled: Vec<(Vec<u64>, Number)> = source
                            .filled_range(txn_id, start, end)
                            .await?
                            .collect()
                            .await;

                        let mut block = Array::constant(dtype.zero(), PER_BLOCK);
                        if filled.is_empty() {
                            return Ok(block);
                        }

                        let (mut coords, values): (Vec<Vec<u64>>, Vec<Number>) =
                            filled.drain(..).unzip();
                        let coords: Vec<u64> = coords.drain(..).flatten().collect();
                        let coords = af::Array::new(
                            &coords,
                            af::Dim4::new(&[ndim as u64, coords.len() as u64, 1, 1]),
                        );
                        let values = Array::try_from_values(values, dtype)?;
                        block.set(coords, &values)?;

                        Ok(block)
                    })
                });

            let block_stream: TCTryStream<Array> = Box::pin(block_stream);
            Ok(block_stream)
        })
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let source = self.source.clone().slice(bounds)?;
            let blocks = Arc::new(BlockListSparse::new(source));
            blocks.value_stream(txn_id).await
        })
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        self.source.read_value(txn_id, coord)
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        Box::pin(async move {
            self.source
                .clone()
                .write_value(txn_id, bounds, number)
                .await
        })
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        self.source.write_value_at(txn_id, coord, value)
    }
}

struct BlockListTranspose {
    source: Arc<dyn BlockList>,
    rebase: transform::Transpose,
}

impl TensorView for BlockListTranspose {
    fn dtype(&self) -> NumberType {
        self.source.dtype()
    }

    fn ndim(&self) -> usize {
        self.source.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.rebase.shape()
    }

    fn size(&self) -> u64 {
        self.source.size()
    }
}

impl BlockList for BlockListTranspose {
    fn value_stream<'a>(self: Arc<Self>, txn_id: TxnId) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let values = stream::iter(Bounds::all(self.shape()).affected())
                .then(move |coord| self.clone().read_value_at_owned(txn_id.clone(), coord));
            let values: TCTryStream<Number> = Box::pin(values);
            Ok(values)
        })
    }

    fn slice_value_stream<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
    ) -> TCBoxTryFuture<'a, TCTryStream<Number>> {
        Box::pin(async move {
            let values = stream::iter(bounds.affected())
                .then(move |coord| self.clone().read_value_at_owned(txn_id.clone(), coord));
            let values: TCTryStream<Number> = Box::pin(values);
            Ok(values)
        })
    }

    fn read_value_at<'a>(
        &'a self,
        txn_id: &'a TxnId,
        coord: &'a [u64],
    ) -> TCBoxTryFuture<'a, Number> {
        Box::pin(async move {
            let coord = self.rebase.invert_coord(coord);
            self.source.read_value_at(txn_id, &coord).await
        })
    }

    fn write_value<'a>(
        self: Arc<Self>,
        txn_id: TxnId,
        bounds: Bounds,
        number: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let bounds = self.rebase.invert_bounds(bounds);
        self.source.clone().write_value(txn_id, bounds, number)
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        let coord = self.rebase.invert_coord(&coord);
        self.source.write_value_at(txn_id, coord, value)
    }
}

#[derive(Clone)]
pub struct DenseTensor {
    blocks: Arc<dyn BlockList>,
}

impl DenseTensor {
    pub fn from_sparse(sparse: SparseTensor) -> DenseTensor {
        let blocks = Arc::new(BlockListSparse::new(sparse));
        DenseTensor { blocks }
    }

    pub async fn value_stream(&self, txn_id: TxnId) -> TCResult<TCStream<TCResult<Number>>> {
        self.blocks.clone().value_stream(txn_id).await
    }
}

impl TensorView for DenseTensor {
    fn dtype(&self) -> NumberType {
        self.blocks.dtype()
    }

    fn ndim(&self) -> usize {
        self.blocks.ndim()
    }

    fn shape(&'_ self) -> &'_ Shape {
        self.blocks.shape()
    }

    fn size(&self) -> u64 {
        self.blocks.size()
    }
}

#[async_trait]
impl TensorBoolean for DenseTensor {
    async fn all(&self, _txn_id: TxnId) -> TCResult<bool> {
        Err(error::not_implemented())
    }

    async fn any(&self, _txn_id: TxnId) -> TCResult<bool> {
        Err(error::not_implemented())
    }

    async fn and(&self, _other: &Self) -> TCResult<Self> {
        Err(error::not_implemented())
    }

    async fn not(&self) -> TCResult<Self> {
        Err(error::not_implemented())
    }

    async fn or(&self, _other: &Self) -> TCResult<Self> {
        Err(error::not_implemented())
    }

    async fn xor(&self, _other: &Self) -> TCResult<Self> {
        Err(error::not_implemented())
    }
}

impl TensorIO for DenseTensor {
    fn read_value<'a>(&'a self, txn_id: &'a TxnId, coord: &'a [u64]) -> TCBoxTryFuture<Number> {
        self.blocks.read_value_at(txn_id, coord)
    }

    fn write_value(
        &'_ self,
        txn_id: TxnId,
        bounds: Bounds,
        value: Number,
    ) -> TCBoxTryFuture<'_, ()> {
        self.blocks.clone().write_value(txn_id, bounds, value)
    }

    fn write_value_at<'a>(
        &'a self,
        txn_id: TxnId,
        coord: Vec<u64>,
        value: Number,
    ) -> TCBoxTryFuture<'a, ()> {
        self.blocks.write_value_at(txn_id, coord, value)
    }
}

impl TensorTransform for DenseTensor {
    fn as_type(&self, dtype: NumberType) -> TCResult<Self> {
        if dtype == self.dtype() {
            return Ok(self.clone());
        }

        let blocks = Arc::new(BlockListCast {
            source: self.blocks.clone(),
            dtype,
        });

        Ok(DenseTensor { blocks })
    }

    fn broadcast(&self, shape: Shape) -> TCResult<Self> {
        if &shape == self.shape() {
            return Ok(self.clone());
        }

        let rebase = transform::Broadcast::new(self.shape().clone(), shape)?;
        let blocks = Arc::new(BlockListBroadcast {
            source: self.blocks.clone(),
            rebase,
        });

        Ok(DenseTensor { blocks })
    }

    fn expand_dims(&self, axis: usize) -> TCResult<Self> {
        let rebase = transform::Expand::new(self.shape().clone(), axis)?;
        let blocks = Arc::new(BlockListExpand {
            source: self.blocks.clone(),
            rebase,
        });

        Ok(DenseTensor { blocks })
    }

    fn slice(&self, bounds: Bounds) -> TCResult<Self> {
        if bounds == Bounds::all(self.shape()) {
            return Ok(self.clone());
        }

        let rebase = transform::Slice::new(self.shape().clone(), bounds)?;
        let blocks = Arc::new(BlockListSlice {
            source: self.blocks.clone(),
            rebase,
        });

        Ok(DenseTensor { blocks })
    }

    fn transpose(&self, permutation: Option<Vec<usize>>) -> TCResult<Self> {
        if permutation == Some((0..self.ndim()).collect()) {
            return Ok(self.clone());
        }

        let rebase = transform::Transpose::new(self.shape().clone(), permutation)?;
        let blocks = Arc::new(BlockListTranspose {
            source: self.blocks.clone(),
            rebase,
        });

        Ok(DenseTensor { blocks })
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
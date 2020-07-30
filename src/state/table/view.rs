use std::collections::{HashMap, HashSet};
use std::convert::{TryFrom, TryInto};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future;
use futures::stream::{self, Stream, StreamExt, TryStreamExt};

use crate::error;
use crate::state::btree::{BTree, BTreeRange};
use crate::transaction::{Txn, TxnId};
use crate::value::{TCResult, TCStream, Value, ValueId};

use super::index::TableBase;
use super::schema::{Bounds, Column, Row, Schema};
use super::{Selection, Table};

#[derive(Clone)]
pub struct Aggregate {
    source: Box<Table>,
    columns: Vec<ValueId>,
}

impl Aggregate {
    pub async fn new(source: Table, txn_id: TxnId, columns: Vec<ValueId>) -> TCResult<Aggregate> {
        let source = Box::new(source.order_by(&txn_id, columns.to_vec(), false).await?);
        Ok(Aggregate { source, columns })
    }
}

#[async_trait]
impl Selection for Aggregate {
    type Stream = TCStream<Vec<Value>>;

    async fn group_by(&self, _txn_id: TxnId, _columns: Vec<ValueId>) -> TCResult<Aggregate> {
        Err(error::unsupported("It doesn't make sense to aggregate an aggregate table view; consider aggregating the source table directly"))
    }

    async fn order_by(
        &self,
        txn_id: &TxnId,
        columns: Vec<ValueId>,
        reverse: bool,
    ) -> TCResult<Table> {
        let source = Box::new(self.source.order_by(txn_id, columns, reverse).await?);
        Ok(Aggregate {
            source,
            columns: self.columns.to_vec(),
        }
        .into())
    }

    fn reversed(&self) -> TCResult<Table> {
        let columns = self.columns.to_vec();
        let reversed = self
            .source
            .reversed()
            .map(Box::new)
            .map(|source| Aggregate { source, columns })?;
        Ok(reversed.into())
    }

    fn schema(&'_ self) -> &'_ Schema {
        self.source.schema()
    }

    async fn stream(&self, txn_id: TxnId) -> TCResult<Self::Stream> {
        let first = self.source.stream(txn_id.clone()).await?.next().await;
        let first = if let Some(first) = first {
            first
        } else {
            return Ok(Box::pin(stream::empty()));
        };

        let left =
            stream::once(future::ready(first)).chain(self.source.stream(txn_id.clone()).await?);
        let right = self.source.stream(txn_id).await?;
        let aggregate = left.zip(right).filter_map(|(l, r)| {
            if l == r {
                future::ready(None)
            } else {
                future::ready(Some(r))
            }
        });

        Ok(Box::pin(aggregate))
    }

    async fn validate_bounds(&self, _txn_id: &TxnId, _bounds: &Bounds) -> TCResult<()> {
        Err(error::unsupported("Table aggregate does not support slicing, consider aggregating a slice of the source table"))
    }

    async fn validate_order(&self, txn_id: &TxnId, order: &[ValueId]) -> TCResult<()> {
        self.source.validate_order(txn_id, order).await
    }
}

#[derive(Clone)]
pub struct ColumnSelection {
    source: Box<Table>,
    schema: Schema,
    columns: Vec<ValueId>,
    indices: Vec<usize>,
}

impl<T: Into<Table>> TryFrom<(T, Vec<ValueId>)> for ColumnSelection {
    type Error = error::TCError;

    fn try_from(params: (T, Vec<ValueId>)) -> TCResult<ColumnSelection> {
        let (source, columns) = params;
        let source: Table = source.into();

        let column_set: HashSet<&ValueId> = columns.iter().collect();
        if column_set.len() != columns.len() {
            return Err(error::bad_request(
                "Tried to select duplicate column",
                columns
                    .iter()
                    .map(|name| name.to_string())
                    .collect::<Vec<String>>()
                    .join(", "),
            ));
        }

        let mut indices: Vec<usize> = Vec::with_capacity(columns.len());
        let mut schema: Vec<Column> = Vec::with_capacity(columns.len());
        let mut source_columns: HashMap<ValueId, Column> = source.schema().clone().into();

        for (i, name) in columns.iter().enumerate() {
            let column = source_columns
                .remove(name)
                .ok_or_else(|| error::not_found(name))?;
            indices.push(i);
            schema.push(column);
        }

        Ok(ColumnSelection {
            source: Box::new(source),
            schema: Schema::new(vec![], schema),
            columns,
            indices,
        })
    }
}

#[async_trait]
impl Selection for ColumnSelection {
    type Stream = TCStream<Vec<Value>>;

    async fn count(&self, txn_id: TxnId) -> TCResult<u64> {
        self.source.clone().count(txn_id).await
    }

    async fn order_by(
        &self,
        txn_id: &TxnId,
        order: Vec<ValueId>,
        reverse: bool,
    ) -> TCResult<Table> {
        self.validate_order(txn_id, &order).await?;

        let source = self
            .source
            .order_by(txn_id, order, reverse)
            .await
            .map(Box::new)?;

        Ok(ColumnSelection {
            source,
            schema: self.schema.clone(),
            columns: self.columns.to_vec(),
            indices: self.indices.to_vec(),
        }
        .into())
    }

    fn reversed(&self) -> TCResult<Table> {
        self.source
            .reversed()?
            .select(self.columns.to_vec())
            .map(|s| s.into())
    }

    fn schema(&'_ self) -> &'_ Schema {
        &self.schema
    }

    async fn stream(&self, txn_id: TxnId) -> TCResult<Self::Stream> {
        let indices = self.indices.to_vec();
        let selected = self.source.clone().stream(txn_id).await?.map(move |row| {
            let selection: Vec<Value> = indices.iter().map(|i| row[*i].clone()).collect();
            selection
        });

        Ok(Box::pin(selected))
    }

    async fn validate_bounds(&self, txn_id: &TxnId, bounds: &Bounds) -> TCResult<()> {
        let bounds_columns: HashSet<&ValueId> = bounds.keys().collect();
        let selected: HashSet<&ValueId> = self.schema.column_names();
        let mut unknown: HashSet<&&ValueId> = selected.difference(&bounds_columns).collect();
        if !unknown.is_empty() {
            let unknown: Vec<String> = unknown.drain().map(|c| c.to_string()).collect();
            return Err(error::bad_request(
                "Tried to slice by unselected columns",
                unknown.join(", "),
            ));
        }

        self.source.validate_bounds(txn_id, bounds).await
    }

    async fn validate_order(&self, txn_id: &TxnId, order: &[ValueId]) -> TCResult<()> {
        let order_columns: HashSet<&ValueId> = order.iter().collect();
        let selected: HashSet<&ValueId> = self.schema().column_names();
        let mut unknown: HashSet<&&ValueId> = selected.difference(&order_columns).collect();
        if !unknown.is_empty() {
            let unknown: Vec<String> = unknown.drain().map(|c| c.to_string()).collect();
            return Err(error::bad_request(
                "Tried to order by unselected columns",
                unknown.join(", "),
            ));
        }

        self.source.validate_order(txn_id, order).await
    }
}

#[derive(Clone)]
pub struct IndexSlice {
    source: Arc<BTree>,
    schema: Schema,
    bounds: Bounds,
    range: BTreeRange,
    reverse: bool,
}

impl IndexSlice {
    pub fn all(source: Arc<BTree>, schema: Schema, reverse: bool) -> IndexSlice {
        IndexSlice {
            source,
            schema,
            bounds: Bounds::all(),
            range: BTreeRange::all(),
            reverse,
        }
    }

    pub fn new(source: Arc<BTree>, schema: Schema, bounds: Bounds) -> TCResult<IndexSlice> {
        assert!(source.schema() == &schema.clone().into());
        schema.validate_bounds(&bounds)?;

        let range: BTreeRange = bounds.clone().try_into_btree_range(&schema)?;

        Ok(IndexSlice {
            source,
            schema,
            bounds,
            range,
            reverse: false,
        })
    }

    pub fn into_reversed(mut self) -> IndexSlice {
        self.reverse = !self.reverse;
        self
    }

    pub fn slice_index(&self, bounds: Bounds) -> TCResult<IndexSlice> {
        let schema = self.schema();
        let outer = self.bounds.clone().try_into_btree_range(schema)?;
        let inner = bounds.clone().try_into_btree_range(schema)?;
        if outer.contains(&inner, schema.data_types())? {
            let mut slice = self.clone();
            slice.bounds = bounds;
            Ok(slice)
        } else {
            Err(error::bad_request(
                &format!("IndexSlice with bounds {} does not contain", &self.bounds),
                bounds,
            ))
        }
    }
}

#[async_trait]
impl Selection for IndexSlice {
    type Stream = TCStream<Vec<Value>>;

    async fn count(&self, txn_id: TxnId) -> TCResult<u64> {
        self.source
            .clone()
            .len(txn_id, self.range.clone().into())
            .await
    }

    async fn delete(self, txn_id: TxnId) -> TCResult<()> {
        self.source.delete(&txn_id, self.range.into()).await
    }

    async fn order_by(
        &self,
        _txn_id: &TxnId,
        order: Vec<ValueId>,
        reverse: bool,
    ) -> TCResult<Table> {
        if self.schema.starts_with(&order) {
            if reverse {
                self.reversed()
            } else {
                Ok(self.clone().into())
            }
        } else {
            let order: Vec<String> = order.iter().map(String::from).collect();
            Err(error::bad_request(
                &format!("Index with schema {} does not support order", &self.schema),
                order.join(", "),
            ))
        }
    }

    fn reversed(&self) -> TCResult<Table> {
        Ok(self.clone().into_reversed().into())
    }

    fn schema(&'_ self) -> &'_ Schema {
        &self.schema
    }

    async fn stream(&self, txn_id: TxnId) -> TCResult<Self::Stream> {
        self.source
            .clone()
            .slice(txn_id.clone(), self.range.clone().into())
            .await
    }

    async fn update(self, txn: Arc<Txn>, value: Row) -> TCResult<()> {
        self.source
            .update(
                txn.id(),
                &self.range.into(),
                &self.schema.row_into_values(value, true)?,
            )
            .await
    }

    async fn validate_bounds(&self, _txn_id: &TxnId, bounds: &Bounds) -> TCResult<()> {
        let schema = self.schema();
        let outer = self.bounds.clone().try_into_btree_range(schema)?;
        let inner = bounds.clone().try_into_btree_range(schema)?;
        outer.contains(&inner, schema.data_types()).map(|_| ())
    }

    async fn validate_order(&self, _txn_id: &TxnId, order: &[ValueId]) -> TCResult<()> {
        if self.schema.starts_with(order) {
            Ok(())
        } else {
            let order: Vec<String> = order.iter().map(String::from).collect();
            Err(error::bad_request(
                &format!("Index with schema {} does not support order", &self.schema),
                order.join(", "),
            ))
        }
    }
}

#[derive(Clone)]
pub struct Limited {
    source: Box<Table>,
    limit: usize,
}

impl TryFrom<(Table, u64)> for Limited {
    type Error = error::TCError;

    fn try_from(params: (Table, u64)) -> TCResult<Limited> {
        let (source, limit) = params;
        let limit: usize = limit.try_into().map_err(|_| {
            error::internal("This host architecture does not support a 64-bit stream limit")
        })?;

        Ok(Limited {
            source: Box::new(source),
            limit,
        })
    }
}

#[async_trait]
impl Selection for Limited {
    type Stream = TCStream<Vec<Value>>;

    async fn count(&self, txn_id: TxnId) -> TCResult<u64> {
        let source_count = self.source.count(txn_id).await?;
        Ok(u64::min(source_count, self.limit as u64))
    }

    async fn delete(self, txn_id: TxnId) -> TCResult<()> {
        let source = self.source.clone();
        let schema = source.schema().clone();
        self.stream(txn_id.clone())
            .await?
            .map(|row| schema.values_into_row(row))
            .map_ok(|row| source.delete_row(&txn_id, row))
            .try_buffer_unordered(2)
            .try_fold((), |_, _| future::ready(Ok(())))
            .await
    }

    async fn order_by(
        &self,
        _txn_id: &TxnId,
        _order: Vec<ValueId>,
        _reverse: bool,
    ) -> TCResult<Table> {
        Err(error::unsupported("Cannot order a limited selection, consider ordering the source or indexing the selection"))
    }

    fn reversed(&self) -> TCResult<Table> {
        Err(error::unsupported(
            "Cannot reverse a limited selection, consider reversing a slice before limiting",
        ))
    }

    fn schema(&'_ self) -> &'_ Schema {
        self.source.schema()
    }

    async fn stream(&self, txn_id: TxnId) -> TCResult<Self::Stream> {
        let rows = self.source.clone().stream(txn_id).await?;

        Ok(Box::pin(rows.take(self.limit)))
    }

    async fn validate_bounds(&self, txn_id: &TxnId, bounds: &Bounds) -> TCResult<()> {
        self.source.validate_bounds(txn_id, bounds).await
    }

    async fn validate_order(&self, _txn_id: &TxnId, _order: &[ValueId]) -> TCResult<()> {
        Err(error::unsupported("Cannot order a limited selection, consider ordering the source or indexing the selection"))
    }

    async fn update(self, txn: Arc<Txn>, value: Row) -> TCResult<()> {
        let source = self.source.clone();
        let schema = source.schema().clone();
        let txn_id = txn.id().clone();
        self.stream(txn_id.clone())
            .await?
            .map(|row| schema.values_into_row(row))
            .map_ok(|row| source.update_row(txn_id.clone(), row, value.clone()))
            .try_buffer_unordered(2)
            .try_fold((), |_, _| future::ready(Ok(())))
            .await
    }
}

#[derive(Clone)]
pub struct TableSlice {
    table: TableBase,
    bounds: Bounds,
    reversed: bool,
}

impl TableSlice {
    pub async fn new(table: TableBase, txn_id: &TxnId, bounds: Bounds) -> TCResult<TableSlice> {
        table.validate_bounds(txn_id, &bounds).await?;

        Ok(TableSlice {
            table,
            bounds,
            reversed: false,
        })
    }
}

#[async_trait]
impl Selection for TableSlice {
    type Stream = Pin<Box<dyn Stream<Item = Vec<Value>> + Send + Sync + Unpin>>;

    async fn count(&self, txn_id: TxnId) -> TCResult<u64> {
        let index = self.table.supporting_index(&txn_id, &self.bounds).await?;
        index
            .slice(&txn_id, self.bounds.clone())
            .await?
            .count(txn_id)
            .await
    }

    async fn delete(self, txn_id: TxnId) -> TCResult<()> {
        let schema = self.schema().clone();
        self.stream(txn_id.clone())
            .await?
            .map(|row| schema.values_into_row(row))
            .map_ok(|row| self.delete_row(&txn_id, row))
            .try_buffer_unordered(2)
            .fold(Ok(()), |_, r| future::ready(r))
            .await
    }

    async fn delete_row(&self, txn_id: &TxnId, row: Row) -> TCResult<()> {
        self.table.delete_row(txn_id, row).await
    }

    async fn order_by(
        &self,
        txn_id: &TxnId,
        order: Vec<ValueId>,
        reverse: bool,
    ) -> TCResult<Table> {
        self.table.order_by(txn_id, order, reverse).await
    }

    fn reversed(&self) -> TCResult<Table> {
        let mut selection = self.clone();
        selection.reversed = true;
        Ok(selection.into())
    }

    fn schema(&'_ self) -> &'_ Schema {
        self.table.schema()
    }

    async fn slice(&self, txn_id: &TxnId, bounds: Bounds) -> TCResult<Table> {
        self.validate_bounds(txn_id, &bounds).await?;
        self.table.slice(txn_id, bounds).await
    }

    async fn stream(&self, txn_id: TxnId) -> TCResult<Self::Stream> {
        let left = Arc::new(self.table.primary().clone());
        let right = self.table.supporting_index(&txn_id, &self.bounds).await?;
        right.validate_bounds(&txn_id, &self.bounds).await?;

        let rows = right
            .stream(txn_id.clone())
            .await?
            .then(move |key| left.clone().get_by_key(txn_id.clone(), key))
            .filter(|row| future::ready(row.is_some()))
            .map(|row| row.unwrap());

        Ok(Box::pin(rows))
    }

    async fn validate_bounds(&self, txn_id: &TxnId, bounds: &Bounds) -> TCResult<()> {
        let index = self.table.supporting_index(txn_id, &self.bounds).await?;
        index
            .validate_schema_bounds(self.bounds.clone(), bounds.clone())
            .map(|_| ())
    }

    async fn validate_order(&self, txn_id: &TxnId, order: &[ValueId]) -> TCResult<()> {
        self.table.validate_order(txn_id, order).await
    }

    async fn update(self, txn: Arc<Txn>, value: Row) -> TCResult<()> {
        let txn_id = txn.id().clone();
        let schema = self.schema().clone();
        self.stream(txn_id.clone())
            .await?
            .map(|row| schema.values_into_row(row))
            .map_ok(|row| self.update_row(txn_id.clone(), row, value.clone()))
            .try_buffer_unordered(2)
            .try_fold((), |_, _| future::ready(Ok(())))
            .await
    }

    async fn update_row(&self, txn_id: TxnId, row: Row, value: Row) -> TCResult<()> {
        self.table.update_row(txn_id, row, value).await
    }
}

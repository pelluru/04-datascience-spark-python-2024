// Copyright 2024 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::marker::PhantomData;
use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use risingwave_common::bitmap::Bitmap;
use risingwave_common::hash::VirtualNode;
use risingwave_hummock_sdk::key::{TableKey, TableKeyRange};
use risingwave_hummock_sdk::HummockReadEpoch;

use crate::error::StorageResult;
use crate::storage_value::StorageValue;
use crate::store::*;

/// A panic state store. If a workload is fully in-memory, we can use this state store to
/// ensure that no data is stored in the state store and no serialization will happen.
#[derive(Clone, Default)]
pub struct PanicStateStore;

impl StateStoreRead for PanicStateStore {
    type ChangeLogIter = PanicStateStoreIter<StateStoreReadLogItem>;
    type Iter = PanicStateStoreIter<StateStoreKeyedRow>;
    type RevIter = PanicStateStoreIter<StateStoreKeyedRow>;

    #[allow(clippy::unused_async)]
    async fn get_keyed_row(
        &self,
        _key: TableKey<Bytes>,
        _epoch: u64,
        _read_options: ReadOptions,
    ) -> StorageResult<Option<StateStoreKeyedRow>> {
        panic!("should not read from the state store!");
    }

    #[allow(clippy::unused_async)]
    async fn iter(
        &self,
        _key_range: TableKeyRange,
        _epoch: u64,
        _read_options: ReadOptions,
    ) -> StorageResult<Self::Iter> {
        panic!("should not read from the state store!");
    }

    #[allow(clippy::unused_async)]
    async fn rev_iter(
        &self,
        _key_range: TableKeyRange,
        _epoch: u64,
        _read_options: ReadOptions,
    ) -> StorageResult<Self::RevIter> {
        panic!("should not read from the state store!");
    }

    async fn iter_log(
        &self,
        _epoch_range: (u64, u64),
        _key_range: TableKeyRange,
        _options: ReadLogOptions,
    ) -> StorageResult<Self::ChangeLogIter> {
        unimplemented!()
    }
}

impl StateStoreWrite for PanicStateStore {
    fn ingest_batch(
        &self,
        _kv_pairs: Vec<(TableKey<Bytes>, StorageValue)>,
        _delete_ranges: Vec<(Bound<Bytes>, Bound<Bytes>)>,
        _write_options: WriteOptions,
    ) -> StorageResult<usize> {
        panic!("should not write to the state store!");
    }
}

impl LocalStateStore for PanicStateStore {
    type Iter<'a> = PanicStateStoreIter<StateStoreKeyedRow>;
    type RevIter<'a> = PanicStateStoreIter<StateStoreKeyedRow>;

    #[allow(clippy::unused_async)]
    async fn get(
        &self,
        _key: TableKey<Bytes>,
        _read_options: ReadOptions,
    ) -> StorageResult<Option<Bytes>> {
        panic!("should not operate on the panic state store!");
    }

    #[allow(clippy::unused_async)]
    async fn iter(
        &self,
        _key_range: TableKeyRange,
        _read_options: ReadOptions,
    ) -> StorageResult<Self::Iter<'_>> {
        panic!("should not operate on the panic state store!");
    }

    #[allow(clippy::unused_async)]
    async fn rev_iter(
        &self,
        _key_range: TableKeyRange,
        _read_options: ReadOptions,
    ) -> StorageResult<Self::RevIter<'_>> {
        panic!("should not operate on the panic state store!");
    }

    fn insert(
        &mut self,
        _key: TableKey<Bytes>,
        _new_val: Bytes,
        _old_val: Option<Bytes>,
    ) -> StorageResult<()> {
        panic!("should not operate on the panic state store!");
    }

    fn delete(&mut self, _key: TableKey<Bytes>, _old_val: Bytes) -> StorageResult<()> {
        panic!("should not operate on the panic state store!");
    }

    #[allow(clippy::unused_async)]
    async fn flush(&mut self) -> StorageResult<usize> {
        panic!("should not operate on the panic state store!");
    }

    fn epoch(&self) -> u64 {
        panic!("should not operate on the panic state store!");
    }

    fn is_dirty(&self) -> bool {
        panic!("should not operate on the panic state store!");
    }

    #[allow(clippy::unused_async)]
    async fn init(&mut self, _epoch: InitOptions) -> StorageResult<()> {
        panic!("should not operate on the panic state store!");
    }

    fn seal_current_epoch(&mut self, _next_epoch: u64, _opts: SealCurrentEpochOptions) {
        panic!("should not operate on the panic state store!")
    }

    #[allow(clippy::unused_async)]
    async fn try_flush(&mut self) -> StorageResult<()> {
        panic!("should not operate on the panic state store!");
    }

    fn update_vnode_bitmap(&mut self, _vnodes: Arc<Bitmap>) -> Arc<Bitmap> {
        panic!("should not operate on the panic state store!");
    }

    fn get_table_watermark(&self, _vnode: VirtualNode) -> Option<Bytes> {
        panic!("should not operate on the panic state store!");
    }
}

impl StateStore for PanicStateStore {
    type Local = Self;

    #[allow(clippy::unused_async)]
    async fn try_wait_epoch(
        &self,
        _epoch: HummockReadEpoch,
        _options: TryWaitEpochOptions,
    ) -> StorageResult<()> {
        panic!("should not wait epoch from the panic state store!");
    }

    #[allow(clippy::unused_async)]
    async fn new_local(&self, _option: NewLocalOptions) -> Self::Local {
        panic!("should not call new local from the panic state store");
    }
}

pub struct PanicStateStoreIter<T: IterItem>(PhantomData<T>);

impl<T: IterItem> StateStoreIter<T> for PanicStateStoreIter<T> {
    async fn try_next(&mut self) -> StorageResult<Option<T::ItemRef<'_>>> {
        panic!("should not call next on panic state store iter")
    }
}

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

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::time::Duration;

use await_tree::InstrumentAwait;
use governor::clock::MonotonicClock;
use governor::{Quota, RateLimiter};
use itertools::Itertools;
use risingwave_common::array::StreamChunk;
use risingwave_common::bail;
use risingwave_common::row::Row;
use risingwave_connector::error::ConnectorError;
use risingwave_connector::source::{BoxSourceChunkStream, SourceColumnDesc, SplitId};
use risingwave_pb::plan_common::additional_column::ColumnType;
use risingwave_pb::plan_common::AdditionalColumn;
pub use state_table_handler::*;

mod executor_core;
pub use executor_core::StreamSourceCore;

mod fs_source_executor;
#[expect(deprecated)]
pub use fs_source_executor::*;
mod source_executor;
pub use source_executor::*;
mod source_backfill_executor;
pub use source_backfill_executor::*;
mod list_executor;
pub use list_executor::*;
mod fetch_executor;
pub use fetch_executor::*;

mod source_backfill_state_table;
pub use source_backfill_state_table::BackfillStateTableHandler;

pub mod state_table_handler;
use futures_async_stream::try_stream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_retry::strategy::{jitter, ExponentialBackoff};

use crate::executor::error::StreamExecutorError;
use crate::executor::{Barrier, Message};

/// Receive barriers from barrier manager with the channel, error on channel close.
#[try_stream(ok = Message, error = StreamExecutorError)]
pub async fn barrier_to_message_stream(mut rx: UnboundedReceiver<Barrier>) {
    while let Some(barrier) = rx.recv().instrument_await("receive_barrier").await {
        yield Message::Barrier(barrier);
    }
    bail!("barrier reader closed unexpectedly");
}

pub fn get_split_offset_mapping_from_chunk(
    chunk: &StreamChunk,
    split_idx: usize,
    offset_idx: usize,
) -> Option<HashMap<SplitId, String>> {
    let mut split_offset_mapping = HashMap::new();
    // All rows (including those visible or invisible) will be used to update the source offset.
    for i in 0..chunk.capacity() {
        let (_, row, _) = chunk.row_at(i);
        let split_id = row.datum_at(split_idx).unwrap().into_utf8().into();
        let offset = row.datum_at(offset_idx).unwrap().into_utf8();
        split_offset_mapping.insert(split_id, offset.to_owned());
    }
    Some(split_offset_mapping)
}

pub fn get_split_offset_col_idx(
    column_descs: &[SourceColumnDesc],
) -> (Option<usize>, Option<usize>) {
    let mut split_idx = None;
    let mut offset_idx = None;
    for (idx, column) in column_descs.iter().enumerate() {
        match column.additional_column {
            AdditionalColumn {
                column_type: Some(ColumnType::Partition(_) | ColumnType::Filename(_)),
            } => {
                split_idx = Some(idx);
            }
            AdditionalColumn {
                column_type: Some(ColumnType::Offset(_)),
            } => {
                offset_idx = Some(idx);
            }
            _ => (),
        }
    }
    (split_idx, offset_idx)
}

pub fn prune_additional_cols(
    chunk: &StreamChunk,
    split_idx: usize,
    offset_idx: usize,
    column_descs: &[SourceColumnDesc],
) -> StreamChunk {
    chunk.project(
        &(0..chunk.dimension())
            .filter(|&idx| {
                (idx != split_idx && idx != offset_idx) || column_descs[idx].is_visible()
            })
            .collect_vec(),
    )
}

#[try_stream(ok = StreamChunk, error = ConnectorError)]
pub async fn apply_rate_limit(stream: BoxSourceChunkStream, rate_limit_rps: Option<u32>) {
    if rate_limit_rps == Some(0) {
        // block the stream until the rate limit is reset
        let future = futures::future::pending::<()>();
        future.await;
        unreachable!();
    }

    let limiter = rate_limit_rps.map(|limit| {
        tracing::info!(rate_limit = limit, "rate limit applied");
        RateLimiter::direct_with_clock(
            Quota::per_second(NonZeroU32::new(limit).unwrap()),
            &MonotonicClock,
        )
    });

    #[for_await]
    for chunk in stream {
        let chunk = chunk?;
        let chunk_size = chunk.capacity();

        if rate_limit_rps.is_none() || chunk_size == 0 {
            // no limit, or empty chunk
            yield chunk;
            continue;
        }

        let limiter = limiter.as_ref().unwrap();
        let limit = rate_limit_rps.unwrap() as usize;

        let required_permits: usize = chunk.compute_rate_limit_chunk_permits(limit);
        if required_permits <= limit {
            let n = NonZeroU32::new(required_permits as u32).unwrap();
            // `InsufficientCapacity` should never happen because we have check the cardinality
            limiter.until_n_ready(n).await.unwrap();
            yield chunk;
        } else {
            // Cut the chunk into smaller chunks
            for chunk in chunk.split(limit) {
                let n =
                    NonZeroU32::new(chunk.compute_rate_limit_chunk_permits(limit) as u32).unwrap();
                // chunks split should have effective chunk size <= limit
                limiter.until_n_ready(n).await.unwrap();
                yield chunk;
            }
        }
    }
}

pub fn get_infinite_backoff_strategy() -> impl Iterator<Item = Duration> {
    const BASE_DELAY: Duration = Duration::from_secs(1);
    const BACKOFF_FACTOR: u64 = 2;
    const MAX_DELAY: Duration = Duration::from_secs(10);
    ExponentialBackoff::from_millis(BASE_DELAY.as_millis() as u64)
        .factor(BACKOFF_FACTOR)
        .max_delay(MAX_DELAY)
        .map(jitter)
}
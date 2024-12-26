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
use std::future::IntoFuture;
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::anyhow;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, Stream, TryFutureExt};
use iceberg::io::{
    FileIOBuilder, FileMetadata, FileRead, S3_ACCESS_KEY_ID, S3_REGION, S3_SECRET_ACCESS_KEY,
};
use iceberg::{Error, ErrorKind};
use itertools::Itertools;
use opendal::layers::{LoggingLayer, RetryLayer};
use opendal::services::S3;
use opendal::Operator;
use parquet::arrow::async_reader::AsyncFileReader;
use parquet::arrow::{parquet_to_arrow_schema, ParquetRecordBatchStreamBuilder, ProjectionMask};
use parquet::file::metadata::{FileMetaData, ParquetMetaData, ParquetMetaDataReader};
use risingwave_common::array::arrow::arrow_schema_udf::{DataType as ArrowDateType, IntervalUnit};
use risingwave_common::array::arrow::IcebergArrowConvert;
use risingwave_common::array::StreamChunk;
use risingwave_common::catalog::ColumnId;
use risingwave_common::types::DataType as RwDataType;
use risingwave_common::util::tokio_util::compat::FuturesAsyncReadCompatExt;
use url::Url;

use crate::error::ConnectorResult;
use crate::parser::ParquetParser;
use crate::source::{Column, SourceColumnDesc};

pub struct ParquetFileReader<R: FileRead> {
    meta: FileMetadata,
    r: R,
}

impl<R: FileRead> ParquetFileReader<R> {
    pub fn new(meta: FileMetadata, r: R) -> Self {
        Self { meta, r }
    }
}

impl<R: FileRead> AsyncFileReader for ParquetFileReader<R> {
    fn get_bytes(&mut self, range: Range<usize>) -> BoxFuture<'_, parquet::errors::Result<Bytes>> {
        Box::pin(
            self.r
                .read(range.start as _..range.end as _)
                .map_err(|err| parquet::errors::ParquetError::External(Box::new(err))),
        )
    }

    fn get_metadata(&mut self) -> BoxFuture<'_, parquet::errors::Result<Arc<ParquetMetaData>>> {
        async move {
            let reader = ParquetMetaDataReader::new();
            let size = self.meta.size as usize;
            let meta = reader.load_and_finish(self, size).await?;

            Ok(Arc::new(meta))
        }
        .boxed()
    }
}

pub async fn create_parquet_stream_builder(
    s3_region: String,
    s3_access_key: String,
    s3_secret_key: String,
    location: String,
) -> Result<ParquetRecordBatchStreamBuilder<ParquetFileReader<impl FileRead>>, anyhow::Error> {
    let mut props = HashMap::new();
    props.insert(S3_REGION, s3_region.clone());
    props.insert(S3_ACCESS_KEY_ID, s3_access_key.clone());
    props.insert(S3_SECRET_ACCESS_KEY, s3_secret_key.clone());

    let file_io_builder = FileIOBuilder::new("s3");
    let file_io = file_io_builder
        .with_props(props.into_iter())
        .build()
        .map_err(|e| anyhow!(e))?;
    let parquet_file = file_io.new_input(&location).map_err(|e| anyhow!(e))?;

    let parquet_metadata = parquet_file.metadata().await.map_err(|e| anyhow!(e))?;
    let parquet_reader = parquet_file.reader().await.map_err(|e| anyhow!(e))?;
    let parquet_file_reader = ParquetFileReader::new(parquet_metadata, parquet_reader);

    ParquetRecordBatchStreamBuilder::new(parquet_file_reader)
        .await
        .map_err(|e| anyhow!(e))
}

pub fn new_s3_operator(
    s3_region: String,
    s3_access_key: String,
    s3_secret_key: String,
    bucket: String,
) -> ConnectorResult<Operator> {
    // Create s3 builder.
    let mut builder = S3::default().bucket(&bucket).region(&s3_region);
    builder = builder.secret_access_key(&s3_access_key);
    builder = builder.secret_access_key(&s3_secret_key);
    builder = builder.endpoint(&format!(
        "https://{}.s3.{}.amazonaws.com",
        bucket, s3_region
    ));

    let op: Operator = Operator::new(builder)?
        .layer(LoggingLayer::default())
        .layer(RetryLayer::default())
        .finish();

    Ok(op)
}

pub fn extract_bucket_and_file_name(location: &str) -> ConnectorResult<(String, String)> {
    let url = Url::parse(location)?;
    let bucket = url
        .host_str()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::DataInvalid,
                format!("Invalid s3 url: {}, missing bucket", location),
            )
        })?
        .to_owned();
    let prefix = format!("s3://{}/", bucket);
    let file_name = location[prefix.len()..].to_string();
    Ok((bucket, file_name))
}

pub async fn list_s3_directory(
    s3_region: String,
    s3_access_key: String,
    s3_secret_key: String,
    dir: String,
) -> Result<Vec<String>, anyhow::Error> {
    let (bucket, file_name) = extract_bucket_and_file_name(&dir)?;
    let prefix = format!("s3://{}/", bucket);
    if dir.starts_with(&prefix) {
        let mut builder = S3::default();
        builder = builder
            .region(&s3_region)
            .access_key_id(&s3_access_key)
            .secret_access_key(&s3_secret_key)
            .bucket(&bucket);
        builder = builder.endpoint(&format!(
            "https://{}.s3.{}.amazonaws.com",
            bucket, s3_region
        ));
        let op = Operator::new(builder)?
            .layer(RetryLayer::default())
            .finish();

        op.list(&file_name)
            .await
            .map_err(|e| anyhow!(e))
            .map(|list| {
                list.into_iter()
                    .map(|entry| prefix.clone() + entry.path())
                    .collect()
            })
    } else {
        Err(Error::new(
            ErrorKind::DataInvalid,
            format!("Invalid s3 url: {}, should start with {}", dir, prefix),
        ))?
    }
}

/// Extracts valid column indices from a Parquet file schema based on the user's requested schema.
///
/// This function is used for column pruning of Parquet files. It calculates the intersection
/// between the columns in the currently read Parquet file and the schema provided by the user.
/// This is useful for reading a `RecordBatch` with the appropriate `ProjectionMask`, ensuring that
/// only the necessary columns are read.
///
/// # Parameters
/// - `columns`: A vector of `Column` representing the user's requested schema.
/// - `metadata`: A reference to `FileMetaData` containing the schema and metadata of the Parquet file.
///
/// # Returns
/// - A `ConnectorResult<Vec<usize>>`, which contains the indices of the valid columns in the
///   Parquet file schema that match the requested schema. If an error occurs during processing,
///   it returns an appropriate error.
pub fn extract_valid_column_indices(
    rw_columns: Vec<Column>,
    metadata: &FileMetaData,
) -> ConnectorResult<Vec<usize>> {
    let parquet_column_names = metadata
        .schema_descr()
        .columns()
        .iter()
        .map(|c| c.name())
        .collect_vec();

    let converted_arrow_schema =
        parquet_to_arrow_schema(metadata.schema_descr(), metadata.key_value_metadata())
            .map_err(anyhow::Error::from)?;

    let valid_column_indices: Vec<usize> = rw_columns
    .iter()
    .filter_map(|column| {
        parquet_column_names
            .iter()
            .position(|&name| name == column.name)
            .and_then(|pos| {
                let arrow_data_type: &risingwave_common::array::arrow::arrow_schema_udf::DataType = converted_arrow_schema.field(pos).data_type();
                let rw_data_type: &risingwave_common::types::DataType = &column.data_type;

                if is_parquet_schema_match_source_schema(arrow_data_type, rw_data_type) {
                    Some(pos)
                } else {
                    None
                }
            })
    })
    .collect();
    Ok(valid_column_indices)
}

/// Reads a specified Parquet file and converts its content into a stream of chunks.
pub async fn read_parquet_file(
    op: Operator,
    file_name: String,
    rw_columns: Option<Vec<Column>>,
    parser_columns: Option<Vec<SourceColumnDesc>>,
    batch_size: usize,
    offset: usize,
) -> ConnectorResult<
    Pin<Box<dyn Stream<Item = Result<StreamChunk, crate::error::ConnectorError>> + Send>>,
> {
    let mut reader: tokio_util::compat::Compat<opendal::FuturesAsyncReader> = op
        .reader_with(&file_name)
        .into_future() // Unlike `rustc`, `try_stream` seems require manual `into_future`.
        .await?
        .into_futures_async_read(..)
        .await?
        .compat();
    let parquet_metadata = reader.get_metadata().await.map_err(anyhow::Error::from)?;

    let file_metadata = parquet_metadata.file_metadata();
    let projection_mask = match rw_columns {
        Some(columns) => {
            let column_indices = extract_valid_column_indices(columns, file_metadata)?;
            ProjectionMask::leaves(file_metadata.schema_descr(), column_indices)
        }
        None => ProjectionMask::all(),
    };

    // For the Parquet format, we directly convert from a record batch to a stream chunk.
    // Therefore, the offset of the Parquet file represents the current position in terms of the number of rows read from the file.
    let record_batch_stream = ParquetRecordBatchStreamBuilder::new(reader)
        .await?
        .with_batch_size(batch_size)
        .with_projection(projection_mask)
        .with_offset(offset)
        .build()?;
    let converted_arrow_schema = parquet_to_arrow_schema(
        file_metadata.schema_descr(),
        file_metadata.key_value_metadata(),
    )
    .map_err(anyhow::Error::from)?;
    let columns = match parser_columns {
        Some(columns) => columns,
        None => converted_arrow_schema
            .fields
            .iter()
            .enumerate()
            .map(|(index, field_ref)| {
                let data_type = IcebergArrowConvert.type_from_field(field_ref).unwrap();
                SourceColumnDesc::simple(
                    field_ref.name().clone(),
                    data_type,
                    ColumnId::new(index as i32),
                )
            })
            .collect(),
    };
    let parquet_parser = ParquetParser::new(columns, file_name, offset)?;
    let msg_stream: Pin<
        Box<dyn Stream<Item = Result<StreamChunk, crate::error::ConnectorError>> + Send>,
    > = parquet_parser.into_stream(record_batch_stream);
    Ok(msg_stream)
}

pub async fn get_parquet_fields(
    op: Operator,
    file_name: String,
) -> ConnectorResult<risingwave_common::array::arrow::arrow_schema_udf::Fields> {
    let mut reader: tokio_util::compat::Compat<opendal::FuturesAsyncReader> = op
        .reader_with(&file_name)
        .into_future() // Unlike `rustc`, `try_stream` seems require manual `into_future`.
        .await?
        .into_futures_async_read(..)
        .await?
        .compat();
    let parquet_metadata = reader.get_metadata().await.map_err(anyhow::Error::from)?;

    let file_metadata = parquet_metadata.file_metadata();
    let converted_arrow_schema = parquet_to_arrow_schema(
        file_metadata.schema_descr(),
        file_metadata.key_value_metadata(),
    )
    .map_err(anyhow::Error::from)?;
    let fields: risingwave_common::array::arrow::arrow_schema_udf::Fields =
        converted_arrow_schema.fields;
    Ok(fields)
}

/// This function checks whether the schema of a Parquet file matches the user defined schema.
/// It handles the following special cases:
/// - Arrow's `timestamp(_, None)` types (all four time units) match with RisingWave's `TimeStamp` type.
/// - Arrow's `timestamp(_, Some)` matches with RisingWave's `TimeStamptz` type.
/// - Since RisingWave does not have an `UInt` type:
///   - Arrow's `UInt8` matches with RisingWave's `Int16`.
///   - Arrow's `UInt16` matches with RisingWave's `Int32`.
///   - Arrow's `UInt32` matches with RisingWave's `Int64`.
///   - Arrow's `UInt64` matches with RisingWave's `Decimal`.
/// - Arrow's `Float16` matches with RisingWave's `Float32`.
fn is_parquet_schema_match_source_schema(
    arrow_data_type: &ArrowDateType,
    rw_data_type: &RwDataType,
) -> bool {
    matches!(
        (arrow_data_type, rw_data_type),
        (ArrowDateType::Boolean, RwDataType::Boolean)
            | (
                ArrowDateType::Int8 | ArrowDateType::Int16 | ArrowDateType::UInt8,
                RwDataType::Int16
            )
            | (
                ArrowDateType::Int32 | ArrowDateType::UInt16,
                RwDataType::Int32
            )
            | (
                ArrowDateType::Int64 | ArrowDateType::UInt32,
                RwDataType::Int64
            )
            | (
                ArrowDateType::UInt64 | ArrowDateType::Decimal128(_, _),
                RwDataType::Decimal
            )
            | (ArrowDateType::Decimal256(_, _), RwDataType::Int256)
            | (
                ArrowDateType::Float16 | ArrowDateType::Float32,
                RwDataType::Float32
            )
            | (ArrowDateType::Float64, RwDataType::Float64)
            | (ArrowDateType::Timestamp(_, None), RwDataType::Timestamp)
            | (
                ArrowDateType::Timestamp(_, Some(_)),
                RwDataType::Timestamptz
            )
            | (ArrowDateType::Date32, RwDataType::Date)
            | (
                ArrowDateType::Time32(_) | ArrowDateType::Time64(_),
                RwDataType::Time
            )
            | (
                ArrowDateType::Interval(IntervalUnit::MonthDayNano),
                RwDataType::Interval
            )
            | (
                ArrowDateType::Utf8 | ArrowDateType::LargeUtf8,
                RwDataType::Varchar
            )
            | (
                ArrowDateType::Binary | ArrowDateType::LargeBinary,
                RwDataType::Bytea
            )
            | (ArrowDateType::List(_), RwDataType::List(_))
            | (ArrowDateType::Struct(_), RwDataType::Struct(_))
            | (ArrowDateType::Map(_, _), RwDataType::Map(_))
    )
}

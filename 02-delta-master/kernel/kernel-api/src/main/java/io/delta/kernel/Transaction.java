/*
 * Copyright (2023) The Delta Lake Project Authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
package io.delta.kernel;

import static io.delta.kernel.internal.DeltaErrors.dataSchemaMismatch;
import static io.delta.kernel.internal.DeltaErrors.partitionColumnMissingInData;
import static io.delta.kernel.internal.TransactionImpl.getStatisticsColumns;
import static io.delta.kernel.internal.data.TransactionStateRow.*;
import static io.delta.kernel.internal.util.PartitionUtils.getTargetDirectory;
import static io.delta.kernel.internal.util.PartitionUtils.validateAndSanitizePartitionValues;
import static io.delta.kernel.internal.util.Preconditions.checkArgument;
import static io.delta.kernel.internal.util.SchemaUtils.findColIndex;

import io.delta.kernel.annotation.Evolving;
import io.delta.kernel.data.*;
import io.delta.kernel.engine.Engine;
import io.delta.kernel.exceptions.ConcurrentWriteException;
import io.delta.kernel.expressions.Literal;
import io.delta.kernel.internal.DataWriteContextImpl;
import io.delta.kernel.internal.IcebergCompatV2Utils;
import io.delta.kernel.internal.actions.AddFile;
import io.delta.kernel.internal.actions.SingleAction;
import io.delta.kernel.internal.fs.Path;
import io.delta.kernel.types.StructType;
import io.delta.kernel.utils.*;
import java.net.URI;
import java.util.List;
import java.util.Map;

/**
 * Represents a transaction to mutate a Delta table.
 *
 * @since 3.2.0
 */
@Evolving
public interface Transaction {
  /**
   * Get the schema of the table. If the connector is adding any data to the table through this
   * transaction, it should have the same schema as the table schema.
   */
  StructType getSchema(Engine engine);

  /**
   * Get the list of logical names of the partition columns. This helps the connector to do physical
   * partitioning of the data before asking the Kernel to stage the data per partition.
   */
  List<String> getPartitionColumns(Engine engine);

  /**
   * Get the state of the transaction. The state helps Kernel do the transformations to logical data
   * according to the Delta protocol and table features enabled on the table. The engine should use
   * this at the data writer task to transform the logical data that the engine wants to write to
   * the table in to physical data that goes in data files using {@link
   * Transaction#transformLogicalData(Engine, Row, CloseableIterator, Map)}
   */
  Row getTransactionState(Engine engine);

  /**
   * Commit the transaction including the data action rows generated by {@link
   * Transaction#generateAppendActions}.
   *
   * @param engine {@link Engine} instance.
   * @param dataActions Iterable of data actions to commit. These data actions are generated by the
   *     {@link Transaction#generateAppendActions(Engine, Row, CloseableIterator,
   *     DataWriteContext)}. The {@link CloseableIterable} allows the Kernel to access the list of
   *     actions multiple times (in case of retries to resolve the conflicts due to other writers to
   *     the table). Kernel provides a in-memory based implementation of {@link CloseableIterable}
   *     with utility API {@link CloseableIterable#inMemoryIterable(CloseableIterator)}
   * @return {@link TransactionCommitResult} status of the successful transaction.
   * @throws ConcurrentWriteException when the transaction has encountered a non-retryable conflicts
   *     or exceeded the maximum number of retries reached. The connector needs to rerun the query
   *     on top of the latest table state and retry the transaction.
   */
  TransactionCommitResult commit(Engine engine, CloseableIterable<Row> dataActions)
      throws ConcurrentWriteException;

  /**
   * Given the logical data that needs to be written to the table, convert it into the required
   * physical data depending upon the table Delta protocol and features enabled on the table. Kernel
   * takes care of adding any additional column or removing existing columns that doesn't need to be
   * in physical data files. All these transformations are driven by the Delta protocol and table
   * features enabled on the table.
   *
   * <p>The given data should belong to exactly one partition. It is the job of the connector to do
   * partitioning of the data before calling the API. Partition values are provided as map of column
   * name to partition value (as {@link Literal}). If the table is an un-partitioned table, then map
   * should be empty.
   *
   * @param engine {@link Engine} instance to use.
   * @param transactionState The transaction state
   * @param dataIter Iterator of logical data (with schema same as the table schema) to transform to
   *     physical data. All the data n this iterator should belong to one physical partition and it
   *     should also include the partition data.
   * @param partitionValues The partition values for the data. If the table is un-partitioned, the
   *     map should be empty
   * @return Iterator of physical data to write to the data files.
   */
  static CloseableIterator<FilteredColumnarBatch> transformLogicalData(
      Engine engine,
      Row transactionState,
      CloseableIterator<FilteredColumnarBatch> dataIter,
      Map<String, Literal> partitionValues) {

    // Note: `partitionValues` are not used as of now in this API, but taking the partition
    // values as input forces the connector to not pass data from multiple partitions this
    // API in a single call.
    StructType tableSchema = getLogicalSchema(engine, transactionState);
    List<String> partitionColNames = getPartitionColumnsList(transactionState);
    validateAndSanitizePartitionValues(tableSchema, partitionColNames, partitionValues);

    // TODO: add support for:
    // - enforcing the constraints
    // - generating the default value columns
    // - generating the generated columns

    boolean isIcebergCompatV2Enabled = isIcebergCompatV2Enabled(transactionState);

    // TODO: set the correct schema once writing into column mapping enabled table is supported.
    String tablePath = getTablePath(transactionState);
    return dataIter.map(
        filteredBatch -> {
          if (isIcebergCompatV2Enabled) {
            // don't remove the partition columns for iceberg compat v2 enabled tables
            return filteredBatch;
          }

          ColumnarBatch data = filteredBatch.getData();
          if (!data.getSchema().equals(tableSchema)) {
            throw dataSchemaMismatch(tablePath, tableSchema, data.getSchema());
          }
          for (String partitionColName : partitionColNames) {
            int partitionColIndex = findColIndex(data.getSchema(), partitionColName);
            if (partitionColIndex < 0) {
              throw partitionColumnMissingInData(tablePath, partitionColName);
            }
            data = data.withDeletedColumnAt(partitionColIndex);
          }
          return new FilteredColumnarBatch(data, filteredBatch.getSelectionVector());
        });
  }

  /**
   * Get the context for writing data into a table. The context tells the connector where the data
   * should be written. For partitioned table context is generated per partition. So, the connector
   * should call this API for each partition. For un-partitioned table, the context is same for all
   * the data.
   *
   * @param engine {@link Engine} instance to use.
   * @param transactionState The transaction state
   * @param partitionValues The partition values for the data. If the table is un-partitioned, the
   *     map should be empty
   * @return {@link DataWriteContext} containing metadata about where and how the data for partition
   *     should be written.
   */
  static DataWriteContext getWriteContext(
      Engine engine, Row transactionState, Map<String, Literal> partitionValues) {
    StructType tableSchema = getLogicalSchema(engine, transactionState);
    List<String> partitionColNames = getPartitionColumnsList(transactionState);

    partitionValues =
        validateAndSanitizePartitionValues(tableSchema, partitionColNames, partitionValues);

    String targetDirectory =
        getTargetDirectory(getTablePath(transactionState), partitionColNames, partitionValues);

    return new DataWriteContextImpl(
        targetDirectory, partitionValues, getStatisticsColumns(engine, transactionState));
  }

  /**
   * For given data files, generate Delta actions that can be committed in a transaction. These data
   * files are the result of writing the data returned by {@link Transaction#transformLogicalData}
   * with the context returned by {@link Transaction#getWriteContext}.
   *
   * @param engine {@link Engine} instance.
   * @param transactionState State of the transaction.
   * @param fileStatusIter Iterator of row objects representing each data file written. When {@code
   *     delta.icebergCompatV2} is enabled, each data file status should contain {@link
   *     DataFileStatistics} with at least the {@link DataFileStatistics#getNumRecords()} field set.
   * @param dataWriteContext The context used when writing the data files given in {@code
   *     fileStatusIter}
   * @return {@link CloseableIterator} of {@link Row} representing the actions to commit using
   *     {@link Transaction#commit}.
   */
  static CloseableIterator<Row> generateAppendActions(
      Engine engine,
      Row transactionState,
      CloseableIterator<DataFileStatus> fileStatusIter,
      DataWriteContext dataWriteContext) {
    checkArgument(
        dataWriteContext instanceof DataWriteContextImpl,
        "DataWriteContext is not created by the `Transaction.getWriteContext()`");

    boolean isIcebergCompatV2Enabled = isIcebergCompatV2Enabled(transactionState);
    URI tableRoot = new Path(getTablePath(transactionState)).toUri();
    return fileStatusIter.map(
        dataFileStatus -> {
          if (isIcebergCompatV2Enabled) {
            IcebergCompatV2Utils.validDataFileStatus(dataFileStatus);
          }
          Row addFileRow =
              AddFile.convertDataFileStatus(
                  tableRoot,
                  dataFileStatus,
                  ((DataWriteContextImpl) dataWriteContext).getPartitionValues(),
                  true /* dataChange */);
          return SingleAction.createAddFileSingleAction(addFileRow);
        });
  }
}
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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use itertools::Itertools;
use pgwire::pg_response::{PgResponse, StatementType};
use risingwave_common::catalog::{ColumnCatalog, Engine};
use risingwave_common::hash::VnodeCount;
use risingwave_common::types::DataType;
use risingwave_common::util::column_index_mapping::ColIndexMapping;
use risingwave_common::{bail, bail_not_implemented};
use risingwave_connector::sink::catalog::SinkCatalog;
use risingwave_pb::catalog::{Source, Table};
use risingwave_pb::ddl_service::TableJobType;
use risingwave_pb::stream_plan::stream_node::PbNodeBody;
use risingwave_pb::stream_plan::{ProjectNode, StreamFragmentGraph};
use risingwave_sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, DataType as AstDataType, Ident, ObjectName,
    Statement, StructField, TableConstraint,
};
use risingwave_sqlparser::parser::Parser;

use super::create_source::schema_has_schema_registry;
use super::create_table::{generate_stream_graph_for_replace_table, ColumnIdGenerator};
use super::util::SourceSchemaCompatExt;
use super::{HandlerArgs, RwPgResponse};
use crate::catalog::root_catalog::SchemaPath;
use crate::catalog::table_catalog::TableType;
use crate::error::{ErrorCode, Result, RwError};
use crate::expr::{Expr, ExprImpl, InputRef, Literal};
use crate::handler::create_sink::{fetch_incoming_sinks, insert_merger_to_union_with_project};
use crate::handler::create_table::bind_table_constraints;
use crate::session::SessionImpl;
use crate::{Binder, TableCatalog};

/// Used in auto schema change process
pub async fn get_new_table_definition_for_cdc_table(
    session: &Arc<SessionImpl>,
    table_name: ObjectName,
    new_columns: &[ColumnCatalog],
) -> Result<(Statement, Arc<TableCatalog>)> {
    let original_catalog = fetch_table_catalog_for_alter(session.as_ref(), &table_name)?;

    // Retrieve the original table definition and parse it to AST.
    let [mut definition]: [_; 1] = Parser::parse_sql(&original_catalog.definition)
        .context("unable to parse original table definition")?
        .try_into()
        .unwrap();

    let Statement::CreateTable {
        columns: original_columns,
        format_encode,
        constraints,
        ..
    } = &mut definition
    else {
        panic!("unexpected statement: {:?}", definition);
    };

    assert!(
        format_encode.is_none(),
        "source schema should be None for CDC table"
    );

    if bind_table_constraints(constraints)?.is_empty() {
        // For table created by `create table t (*)` the constraint is empty, we need to
        // retrieve primary key names from original table catalog if available
        let pk_names: Vec<_> = original_catalog
            .pk
            .iter()
            .map(|x| original_catalog.columns[x.column_index].name().to_owned())
            .collect();

        constraints.push(TableConstraint::Unique {
            name: None,
            columns: pk_names.iter().map(Ident::new_unchecked).collect(),
            is_primary: true,
        });
    }

    let orig_column_catalog: HashMap<String, ColumnCatalog> = HashMap::from_iter(
        original_catalog
            .columns()
            .iter()
            .map(|col| (col.name().to_owned(), col.clone())),
    );

    // update the original columns with new version columns
    let mut new_column_defs = vec![];
    for new_col in new_columns {
        // if the column exists in the original catalog, use it to construct the column definition.
        // since we don't support altering the column type right now
        if let Some(original_col) = orig_column_catalog.get(new_col.name()) {
            let ty = to_ast_data_type(original_col.data_type())?;
            new_column_defs.push(ColumnDef::new(original_col.name().into(), ty, None, vec![]));
        } else {
            let ty = to_ast_data_type(new_col.data_type())?;
            new_column_defs.push(ColumnDef::new(new_col.name().into(), ty, None, vec![]));
        }
    }
    *original_columns = new_column_defs;

    Ok((definition, original_catalog))
}

fn to_ast_data_type(ty: &DataType) -> Result<AstDataType> {
    match ty {
        DataType::Boolean => Ok(AstDataType::Boolean),
        DataType::Int16 => Ok(AstDataType::SmallInt),
        DataType::Int32 => Ok(AstDataType::Int),
        DataType::Int64 => Ok(AstDataType::BigInt),
        DataType::Float32 => Ok(AstDataType::Real),
        DataType::Float64 => Ok(AstDataType::Double),
        // TODO: handle precision and scale for decimal
        DataType::Decimal => Ok(AstDataType::Decimal(None, None)),
        DataType::Date => Ok(AstDataType::Date),
        DataType::Varchar => Ok(AstDataType::Varchar),
        DataType::Time => Ok(AstDataType::Time(false)),
        DataType::Timestamp => Ok(AstDataType::Timestamp(false)),
        DataType::Timestamptz => Ok(AstDataType::Timestamp(true)),
        DataType::Interval => Ok(AstDataType::Interval),
        DataType::Jsonb => Ok(AstDataType::Jsonb),
        DataType::Bytea => Ok(AstDataType::Bytea),
        DataType::List(item_ty) => Ok(AstDataType::Array(Box::new(to_ast_data_type(item_ty)?))),
        DataType::Struct(fields) => {
            let fields = fields
                .iter()
                .map(|(name, ty)| {
                    Ok::<StructField, RwError>(StructField {
                        name: name.into(),
                        data_type: to_ast_data_type(ty)?,
                    })
                })
                .try_collect()?;
            Ok(AstDataType::Struct(fields))
        }
        DataType::Serial | DataType::Int256 | DataType::Map(_) => {
            Err(anyhow!("unsupported data type: {:?}", ty).context("to_ast_data_type"))?
        }
    }
}

pub async fn get_replace_table_plan(
    session: &Arc<SessionImpl>,
    table_name: ObjectName,
    new_definition: Statement,
    old_catalog: &Arc<TableCatalog>,
    new_version_columns: Option<Vec<ColumnCatalog>>, // only provided in auto schema change
) -> Result<(
    Option<Source>,
    Table,
    StreamFragmentGraph,
    ColIndexMapping,
    TableJobType,
)> {
    // Create handler args as if we're creating a new table with the altered definition.
    let handler_args = HandlerArgs::new(session.clone(), &new_definition, Arc::from(""))?;
    let col_id_gen = ColumnIdGenerator::new_alter(old_catalog);
    let Statement::CreateTable {
        columns,
        constraints,
        source_watermarks,
        append_only,
        on_conflict,
        with_version_column,
        wildcard_idx,
        cdc_table_info,
        format_encode,
        include_column_options,
        engine,
        ..
    } = new_definition
    else {
        panic!("unexpected statement type: {:?}", new_definition);
    };

    let format_encode = format_encode
        .clone()
        .map(|format_encode| format_encode.into_v2_with_warning());

    let engine = match engine {
        risingwave_sqlparser::ast::Engine::Hummock => Engine::Hummock,
        risingwave_sqlparser::ast::Engine::Iceberg => Engine::Iceberg,
    };

    let (mut graph, table, source, job_type) = generate_stream_graph_for_replace_table(
        session,
        table_name,
        old_catalog,
        format_encode,
        handler_args.clone(),
        col_id_gen,
        columns.clone(),
        wildcard_idx,
        constraints,
        source_watermarks,
        append_only,
        on_conflict,
        with_version_column,
        cdc_table_info,
        new_version_columns,
        include_column_options,
        engine,
    )
    .await?;

    // Calculate the mapping from the original columns to the new columns.
    let col_index_mapping = ColIndexMapping::new(
        old_catalog
            .columns()
            .iter()
            .map(|old_c| {
                table.columns.iter().position(|new_c| {
                    new_c.get_column_desc().unwrap().column_id == old_c.column_id().get_id()
                })
            })
            .collect(),
        table.columns.len(),
    );

    let incoming_sink_ids: HashSet<_> = old_catalog.incoming_sinks.iter().copied().collect();

    let target_columns = table
        .columns
        .iter()
        .map(|col| ColumnCatalog::from(col.clone()))
        .filter(|col| !col.is_rw_timestamp_column())
        .collect_vec();

    for sink in fetch_incoming_sinks(session, &incoming_sink_ids)? {
        hijack_merger_for_target_table(
            &mut graph,
            &target_columns,
            &sink,
            Some(&sink.unique_identity()),
        )?;
    }

    // Set some fields ourselves so that the meta service does not need to maintain them.
    let mut table = table;
    table.incoming_sinks = incoming_sink_ids.iter().copied().collect();
    table.maybe_vnode_count = VnodeCount::set(old_catalog.vnode_count()).to_protobuf();

    Ok((source, table, graph, col_index_mapping, job_type))
}

pub(crate) fn hijack_merger_for_target_table(
    graph: &mut StreamFragmentGraph,
    target_columns: &[ColumnCatalog],
    sink: &SinkCatalog,
    uniq_identify: Option<&str>,
) -> Result<()> {
    let mut sink_columns = sink.original_target_columns.clone();
    if sink_columns.is_empty() {
        // This is due to the fact that the value did not exist in earlier versions,
        // which means no schema changes such as `ADD/DROP COLUMN` have been made to the table.
        // Therefore the columns of the table at this point are `original_target_columns`.
        // This value of sink will be filled on the meta.
        sink_columns = target_columns.to_vec();
    }

    let mut i = 0;
    let mut j = 0;
    let mut exprs = Vec::new();

    while j < target_columns.len() {
        if i < sink_columns.len() && sink_columns[i].data_type() == target_columns[j].data_type() {
            exprs.push(ExprImpl::InputRef(Box::new(InputRef {
                data_type: sink_columns[i].data_type().clone(),
                index: i,
            })));

            i += 1;
            j += 1;
        } else {
            exprs.push(ExprImpl::Literal(Box::new(Literal::new(
                None,
                target_columns[j].data_type().clone(),
            ))));

            j += 1;
        }
    }

    let pb_project = PbNodeBody::Project(Box::new(ProjectNode {
        select_list: exprs.iter().map(|expr| expr.to_expr_proto()).collect(),
        ..Default::default()
    }));

    for fragment in graph.fragments.values_mut() {
        if let Some(node) = &mut fragment.node {
            insert_merger_to_union_with_project(node, &pb_project, uniq_identify);
        }
    }

    Ok(())
}

/// Handle `ALTER TABLE [ADD|DROP] COLUMN` statements. The `operation` must be either `AddColumn` or
/// `DropColumn`.
pub async fn handle_alter_table_column(
    handler_args: HandlerArgs,
    table_name: ObjectName,
    operation: AlterTableOperation,
) -> Result<RwPgResponse> {
    let session = handler_args.session;
    let original_catalog = fetch_table_catalog_for_alter(session.as_ref(), &table_name)?;

    if !original_catalog.incoming_sinks.is_empty() && original_catalog.has_generated_column() {
        return Err(RwError::from(ErrorCode::BindError(
            "Alter a table with incoming sink and generated column has not been implemented."
                .to_owned(),
        )));
    }

    // Retrieve the original table definition and parse it to AST.
    let [mut definition]: [_; 1] = Parser::parse_sql(&original_catalog.definition)
        .context("unable to parse original table definition")?
        .try_into()
        .unwrap();
    let Statement::CreateTable {
        columns,
        format_encode,
        ..
    } = &mut definition
    else {
        panic!("unexpected statement: {:?}", definition);
    };

    let format_encode = format_encode
        .clone()
        .map(|format_encode| format_encode.into_v2_with_warning());

    let fail_if_has_schema_registry = || {
        if let Some(format_encode) = &format_encode
            && schema_has_schema_registry(format_encode)
        {
            Err(ErrorCode::NotSupported(
                "alter table with schema registry".to_owned(),
                "try `ALTER TABLE .. FORMAT .. ENCODE .. (...)` instead".to_owned(),
            ))
        } else {
            Ok(())
        }
    };

    if columns.is_empty() {
        Err(ErrorCode::NotSupported(
            "alter a table with empty column definitions".to_owned(),
            "Please recreate the table with column definitions.".to_owned(),
        ))?
    }

    if !original_catalog.incoming_sinks.is_empty()
        && matches!(operation, AlterTableOperation::DropColumn { .. })
    {
        return Err(ErrorCode::InvalidInputSyntax(
            "dropping columns in target table of sinks is not supported".to_owned(),
        ))?;
    }

    match operation {
        AlterTableOperation::AddColumn {
            column_def: new_column,
        } => {
            fail_if_has_schema_registry()?;

            // Duplicated names can actually be checked by `StreamMaterialize`. We do here for
            // better error reporting.
            let new_column_name = new_column.name.real_value();
            if columns
                .iter()
                .any(|c| c.name.real_value() == new_column_name)
            {
                Err(ErrorCode::InvalidInputSyntax(format!(
                    "column \"{new_column_name}\" of table \"{table_name}\" already exists"
                )))?
            }

            if new_column
                .options
                .iter()
                .any(|x| matches!(x.option, ColumnOption::GeneratedColumns(_)))
            {
                Err(ErrorCode::InvalidInputSyntax(
                    "alter table add generated columns is not supported".to_owned(),
                ))?
            }

            // Add the new column to the table definition if it is not created by `create table (*)` syntax.
            columns.push(new_column);
        }

        AlterTableOperation::DropColumn {
            column_name,
            if_exists,
            cascade,
        } => {
            if cascade {
                bail_not_implemented!(issue = 6903, "drop column cascade");
            }

            // Check if the column to drop is referenced by any generated columns.
            for column in original_catalog.columns() {
                if column_name.real_value() == column.name() && !column.is_generated() {
                    fail_if_has_schema_registry()?;
                }

                if let Some(expr) = column.generated_expr() {
                    let expr = ExprImpl::from_expr_proto(expr)?;
                    let refs = expr.collect_input_refs(original_catalog.columns().len());
                    for idx in refs.ones() {
                        let refed_column = &original_catalog.columns()[idx];
                        if refed_column.name() == column_name.real_value() {
                            bail!(format!(
                                "failed to drop column \"{}\" because it's referenced by a generated column \"{}\"",
                                column_name,
                                column.name()
                            ))
                        }
                    }
                }
            }

            // Locate the column by name and remove it.
            let column_name = column_name.real_value();
            let removed_column = columns
                .extract_if(|c| c.name.real_value() == column_name)
                .at_most_one()
                .ok()
                .unwrap();

            if removed_column.is_some() {
                // PASS
            } else if if_exists {
                return Ok(PgResponse::builder(StatementType::ALTER_TABLE)
                    .notice(format!(
                        "column \"{}\" does not exist, skipping",
                        column_name
                    ))
                    .into());
            } else {
                Err(ErrorCode::InvalidInputSyntax(format!(
                    "column \"{}\" of table \"{}\" does not exist",
                    column_name, table_name
                )))?
            }
        }

        _ => unreachable!(),
    };

    let (source, table, graph, col_index_mapping, job_type) =
        get_replace_table_plan(&session, table_name, definition, &original_catalog, None).await?;

    let catalog_writer = session.catalog_writer()?;

    catalog_writer
        .replace_table(source, table, graph, col_index_mapping, job_type)
        .await?;
    Ok(PgResponse::empty_result(StatementType::ALTER_TABLE))
}

pub fn fetch_table_catalog_for_alter(
    session: &SessionImpl,
    table_name: &ObjectName,
) -> Result<Arc<TableCatalog>> {
    let db_name = session.database();
    let (schema_name, real_table_name) =
        Binder::resolve_schema_qualified_name(db_name, table_name.clone())?;
    let search_path = session.config().search_path();
    let user_name = &session.auth_context().user_name;

    let schema_path = SchemaPath::new(schema_name.as_deref(), &search_path, user_name);

    let original_catalog = {
        let reader = session.env().catalog_reader().read_guard();
        let (table, schema_name) =
            reader.get_created_table_by_name(db_name, schema_path, &real_table_name)?;

        match table.table_type() {
            TableType::Table => {}

            _ => Err(ErrorCode::InvalidInputSyntax(format!(
                "\"{table_name}\" is not a table or cannot be altered"
            )))?,
        }

        session.check_privilege_for_drop_alter(schema_name, &**table)?;

        table.clone()
    };

    Ok(original_catalog)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use risingwave_common::catalog::{DEFAULT_DATABASE_NAME, DEFAULT_SCHEMA_NAME, ROWID_PREFIX};
    use risingwave_common::types::DataType;

    use crate::catalog::root_catalog::SchemaPath;
    use crate::test_utils::LocalFrontend;

    #[tokio::test]
    async fn test_add_column_handler() {
        let frontend = LocalFrontend::new(Default::default()).await;
        let session = frontend.session_ref();
        let schema_path = SchemaPath::Name(DEFAULT_SCHEMA_NAME);

        let sql = "create table t (i int, r real);";
        frontend.run_sql(sql).await.unwrap();

        let get_table = || {
            let catalog_reader = session.env().catalog_reader().read_guard();
            catalog_reader
                .get_created_table_by_name(DEFAULT_DATABASE_NAME, schema_path, "t")
                .unwrap()
                .0
                .clone()
        };

        let table = get_table();

        let columns: HashMap<_, _> = table
            .columns
            .iter()
            .map(|col| (col.name(), (col.data_type().clone(), col.column_id())))
            .collect();

        // Alter the table.
        let sql = "alter table t add column s text;";
        frontend.run_sql(sql).await.unwrap();

        let altered_table = get_table();

        let altered_columns: HashMap<_, _> = altered_table
            .columns
            .iter()
            .map(|col| (col.name(), (col.data_type().clone(), col.column_id())))
            .collect();

        // Check the new column.
        assert_eq!(columns.len() + 1, altered_columns.len());
        assert_eq!(altered_columns["s"].0, DataType::Varchar);

        // Check the old columns and IDs are not changed.
        assert_eq!(columns["i"], altered_columns["i"]);
        assert_eq!(columns["r"], altered_columns["r"]);
        assert_eq!(columns[ROWID_PREFIX], altered_columns[ROWID_PREFIX]);

        // Check the version is updated.
        assert_eq!(
            table.version.as_ref().unwrap().version_id + 1,
            altered_table.version.as_ref().unwrap().version_id
        );
        assert_eq!(
            table.version.as_ref().unwrap().next_column_id.next(),
            altered_table.version.as_ref().unwrap().next_column_id
        );
    }
}
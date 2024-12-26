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

use super::*;

impl CatalogController {
    async fn alter_database_name(
        &self,
        database_id: DatabaseId,
        name: &str,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;
        check_database_name_duplicate(name, &txn).await?;

        let active_model = database::ActiveModel {
            database_id: Set(database_id),
            name: Set(name.to_owned()),
        };
        let database = active_model.update(&txn).await?;

        let obj = Object::find_by_id(database_id)
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found("database", database_id))?;

        txn.commit().await?;

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::Database(ObjectModel(database, obj).into()),
            )
            .await;
        Ok(version)
    }

    async fn alter_schema_name(
        &self,
        schema_id: SchemaId,
        name: &str,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;

        let obj = Object::find_by_id(schema_id)
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found("schema", schema_id))?;
        check_schema_name_duplicate(name, obj.database_id.unwrap(), &txn).await?;

        let active_model = schema::ActiveModel {
            schema_id: Set(schema_id),
            name: Set(name.to_owned()),
        };
        let schema = active_model.update(&txn).await?;

        txn.commit().await?;

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::Schema(ObjectModel(schema, obj).into()),
            )
            .await;
        Ok(version)
    }

    pub async fn alter_name(
        &self,
        object_type: ObjectType,
        object_id: ObjectId,
        object_name: &str,
    ) -> MetaResult<NotificationVersion> {
        if object_type == ObjectType::Database {
            return self.alter_database_name(object_id as _, object_name).await;
        } else if object_type == ObjectType::Schema {
            return self.alter_schema_name(object_id as _, object_name).await;
        }

        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;
        let obj: PartialObject = Object::find_by_id(object_id)
            .into_partial_model()
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found(object_type.as_str(), object_id))?;
        assert_eq!(obj.obj_type, object_type);
        check_relation_name_duplicate(
            object_name,
            obj.database_id.unwrap(),
            obj.schema_id.unwrap(),
            &txn,
        )
        .await?;

        // rename relation.
        let (mut to_update_relations, old_name) =
            rename_relation(&txn, object_type, object_id, object_name).await?;
        // rename referring relation name.
        to_update_relations.extend(
            rename_relation_refer(&txn, object_type, object_id, object_name, &old_name).await?,
        );

        txn.commit().await?;

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::RelationGroup(PbRelationGroup {
                    relations: to_update_relations,
                }),
            )
            .await;

        Ok(version)
    }

    pub async fn alter_swap_rename(
        &self,
        object_type: ObjectType,
        object_id: ObjectId,
        dst_object_id: ObjectId,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;
        let dst_name: String = match object_type {
            ObjectType::Table => Table::find_by_id(dst_object_id)
                .select_only()
                .column(table::Column::Name)
                .into_tuple()
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    MetaError::catalog_id_not_found(object_type.as_str(), dst_object_id)
                })?,
            ObjectType::Source => Source::find_by_id(dst_object_id)
                .select_only()
                .column(source::Column::Name)
                .into_tuple()
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    MetaError::catalog_id_not_found(object_type.as_str(), dst_object_id)
                })?,
            ObjectType::Sink => Sink::find_by_id(dst_object_id)
                .select_only()
                .column(sink::Column::Name)
                .into_tuple()
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    MetaError::catalog_id_not_found(object_type.as_str(), dst_object_id)
                })?,
            ObjectType::View => View::find_by_id(dst_object_id)
                .select_only()
                .column(view::Column::Name)
                .into_tuple()
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    MetaError::catalog_id_not_found(object_type.as_str(), dst_object_id)
                })?,
            ObjectType::Subscription => Subscription::find_by_id(dst_object_id)
                .select_only()
                .column(subscription::Column::Name)
                .into_tuple()
                .one(&txn)
                .await?
                .ok_or_else(|| {
                    MetaError::catalog_id_not_found(object_type.as_str(), dst_object_id)
                })?,
            _ => {
                return Err(MetaError::permission_denied(format!(
                    "swap rename not supported for object type: {:?}",
                    object_type
                )));
            }
        };

        // rename relations.
        let (mut to_update_relations, src_name) =
            rename_relation(&txn, object_type, object_id, &dst_name).await?;
        let (to_update_relations2, _) =
            rename_relation(&txn, object_type, dst_object_id, &src_name).await?;
        to_update_relations.extend(to_update_relations2);
        // rename referring relation name.
        to_update_relations.extend(
            rename_relation_refer(&txn, object_type, object_id, &dst_name, &src_name).await?,
        );
        to_update_relations.extend(
            rename_relation_refer(&txn, object_type, dst_object_id, &src_name, &dst_name).await?,
        );

        txn.commit().await?;

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::RelationGroup(PbRelationGroup {
                    relations: to_update_relations,
                }),
            )
            .await;

        Ok(version)
    }

    pub async fn alter_non_shared_source(
        &self,
        pb_source: PbSource,
    ) -> MetaResult<NotificationVersion> {
        let source_id = pb_source.id as SourceId;
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;

        let original_version: i64 = Source::find_by_id(source_id)
            .select_only()
            .column(source::Column::Version)
            .into_tuple()
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found("source", source_id))?;
        if original_version + 1 != pb_source.version as i64 {
            return Err(MetaError::permission_denied(
                "source version is stale".to_owned(),
            ));
        }

        let source: source::ActiveModel = pb_source.clone().into();
        source.update(&txn).await?;
        txn.commit().await?;

        let version = self
            .notify_frontend_relation_info(
                NotificationOperation::Update,
                PbRelationInfo::Source(pb_source),
            )
            .await;
        Ok(version)
    }

    pub async fn alter_owner(
        &self,
        object_type: ObjectType,
        object_id: ObjectId,
        new_owner: UserId,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;
        ensure_user_id(new_owner, &txn).await?;

        let obj = Object::find_by_id(object_id)
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found(object_type.as_str(), object_id))?;
        if obj.owner_id == new_owner {
            return Ok(IGNORED_NOTIFICATION_VERSION);
        }
        let mut obj = obj.into_active_model();
        obj.owner_id = Set(new_owner);
        let obj = obj.update(&txn).await?;

        let mut relations = vec![];
        match object_type {
            ObjectType::Database => {
                let db = Database::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("database", object_id))?;

                txn.commit().await?;

                let version = self
                    .notify_frontend(
                        NotificationOperation::Update,
                        NotificationInfo::Database(ObjectModel(db, obj).into()),
                    )
                    .await;
                return Ok(version);
            }
            ObjectType::Schema => {
                let schema = Schema::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("schema", object_id))?;

                txn.commit().await?;

                let version = self
                    .notify_frontend(
                        NotificationOperation::Update,
                        NotificationInfo::Schema(ObjectModel(schema, obj).into()),
                    )
                    .await;
                return Ok(version);
            }
            ObjectType::Table => {
                let table = Table::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("table", object_id))?;

                // associated source.
                if let Some(associated_source_id) = table.optional_associated_source_id {
                    let src_obj = object::ActiveModel {
                        oid: Set(associated_source_id as _),
                        owner_id: Set(new_owner),
                        ..Default::default()
                    }
                    .update(&txn)
                    .await?;
                    let source = Source::find_by_id(associated_source_id)
                        .one(&txn)
                        .await?
                        .ok_or_else(|| {
                            MetaError::catalog_id_not_found("source", associated_source_id)
                        })?;
                    relations.push(PbRelationInfo::Source(ObjectModel(source, src_obj).into()));
                }

                // indexes.
                let (index_ids, mut table_ids): (Vec<IndexId>, Vec<TableId>) =
                    if table.table_type == TableType::Table {
                        Index::find()
                            .select_only()
                            .columns([index::Column::IndexId, index::Column::IndexTableId])
                            .filter(index::Column::PrimaryTableId.eq(object_id))
                            .into_tuple::<(IndexId, TableId)>()
                            .all(&txn)
                            .await?
                            .into_iter()
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };
                relations.push(PbRelationInfo::Table(ObjectModel(table, obj).into()));

                // internal tables.
                let internal_tables: Vec<TableId> = Table::find()
                    .select_only()
                    .column(table::Column::TableId)
                    .filter(
                        table::Column::BelongsToJobId
                            .is_in(table_ids.iter().cloned().chain(std::iter::once(object_id))),
                    )
                    .into_tuple()
                    .all(&txn)
                    .await?;
                table_ids.extend(internal_tables);

                if !index_ids.is_empty() || !table_ids.is_empty() {
                    Object::update_many()
                        .col_expr(
                            object::Column::OwnerId,
                            SimpleExpr::Value(Value::Int(Some(new_owner))),
                        )
                        .filter(
                            object::Column::Oid
                                .is_in(index_ids.iter().cloned().chain(table_ids.iter().cloned())),
                        )
                        .exec(&txn)
                        .await?;
                }

                if !table_ids.is_empty() {
                    let table_objs = Table::find()
                        .find_also_related(Object)
                        .filter(table::Column::TableId.is_in(table_ids))
                        .all(&txn)
                        .await?;
                    for (table, table_obj) in table_objs {
                        relations.push(PbRelationInfo::Table(
                            ObjectModel(table, table_obj.unwrap()).into(),
                        ));
                    }
                }
                // FIXME: frontend will update index/primary table from cache, requires apply updates of indexes after tables.
                if !index_ids.is_empty() {
                    let index_objs = Index::find()
                        .find_also_related(Object)
                        .filter(index::Column::IndexId.is_in(index_ids))
                        .all(&txn)
                        .await?;
                    for (index, index_obj) in index_objs {
                        relations.push(PbRelationInfo::Index(
                            ObjectModel(index, index_obj.unwrap()).into(),
                        ));
                    }
                }
            }
            ObjectType::Source => {
                let source = Source::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("source", object_id))?;
                let is_shared = source.is_shared();
                relations.push(PbRelationInfo::Source(ObjectModel(source, obj).into()));

                // Note: For non-shared source, we don't update their state tables, which
                // belongs to the MV.
                if is_shared {
                    update_internal_tables(
                        &txn,
                        object_id,
                        object::Column::OwnerId,
                        Value::Int(Some(new_owner)),
                        &mut relations,
                    )
                    .await?;
                }
            }
            ObjectType::Sink => {
                let sink = Sink::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("sink", object_id))?;
                relations.push(PbRelationInfo::Sink(ObjectModel(sink, obj).into()));

                update_internal_tables(
                    &txn,
                    object_id,
                    object::Column::OwnerId,
                    Value::Int(Some(new_owner)),
                    &mut relations,
                )
                .await?;
            }
            ObjectType::Subscription => {
                let subscription = Subscription::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("subscription", object_id))?;
                relations.push(PbRelationInfo::Subscription(
                    ObjectModel(subscription, obj).into(),
                ));
            }
            ObjectType::View => {
                let view = View::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("view", object_id))?;
                relations.push(PbRelationInfo::View(ObjectModel(view, obj).into()));
            }
            _ => unreachable!("not supported object type: {:?}", object_type),
        };

        txn.commit().await?;

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::RelationGroup(PbRelationGroup {
                    relations: relations
                        .into_iter()
                        .map(|relation| PbRelation {
                            relation_info: Some(relation),
                        })
                        .collect(),
                }),
            )
            .await;
        Ok(version)
    }

    pub async fn alter_schema(
        &self,
        object_type: ObjectType,
        object_id: ObjectId,
        new_schema: SchemaId,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let txn = inner.db.begin().await?;
        ensure_object_id(ObjectType::Schema, new_schema, &txn).await?;

        let obj = Object::find_by_id(object_id)
            .one(&txn)
            .await?
            .ok_or_else(|| MetaError::catalog_id_not_found(object_type.as_str(), object_id))?;
        if obj.schema_id == Some(new_schema) {
            return Ok(IGNORED_NOTIFICATION_VERSION);
        }
        let database_id = obj.database_id.unwrap();

        let mut relations = vec![];
        match object_type {
            ObjectType::Table => {
                let table = Table::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("table", object_id))?;
                check_relation_name_duplicate(&table.name, database_id, new_schema, &txn).await?;
                let (associated_src_id, table_type) =
                    (table.optional_associated_source_id, table.table_type);

                let mut obj = obj.into_active_model();
                obj.schema_id = Set(Some(new_schema));
                let obj = obj.update(&txn).await?;
                relations.push(PbRelationInfo::Table(ObjectModel(table, obj).into()));

                // associated source.
                if let Some(associated_source_id) = associated_src_id {
                    let src_obj = object::ActiveModel {
                        oid: Set(associated_source_id as _),
                        schema_id: Set(Some(new_schema)),
                        ..Default::default()
                    }
                    .update(&txn)
                    .await?;
                    let source = Source::find_by_id(associated_source_id)
                        .one(&txn)
                        .await?
                        .ok_or_else(|| {
                            MetaError::catalog_id_not_found("source", associated_source_id)
                        })?;
                    relations.push(PbRelationInfo::Source(ObjectModel(source, src_obj).into()));
                }

                // indexes.
                let (index_ids, (index_names, mut table_ids)): (
                    Vec<IndexId>,
                    (Vec<String>, Vec<TableId>),
                ) = if table_type == TableType::Table {
                    Index::find()
                        .select_only()
                        .columns([
                            index::Column::IndexId,
                            index::Column::Name,
                            index::Column::IndexTableId,
                        ])
                        .filter(index::Column::PrimaryTableId.eq(object_id))
                        .into_tuple::<(IndexId, String, TableId)>()
                        .all(&txn)
                        .await?
                        .into_iter()
                        .map(|(id, name, t_id)| (id, (name, t_id)))
                        .unzip()
                } else {
                    (vec![], (vec![], vec![]))
                };

                // internal tables.
                let internal_tables: Vec<TableId> = Table::find()
                    .select_only()
                    .column(table::Column::TableId)
                    .filter(
                        table::Column::BelongsToJobId
                            .is_in(table_ids.iter().cloned().chain(std::iter::once(object_id))),
                    )
                    .into_tuple()
                    .all(&txn)
                    .await?;
                table_ids.extend(internal_tables);

                if !index_ids.is_empty() || !table_ids.is_empty() {
                    for index_name in index_names {
                        check_relation_name_duplicate(&index_name, database_id, new_schema, &txn)
                            .await?;
                    }

                    Object::update_many()
                        .col_expr(
                            object::Column::SchemaId,
                            SimpleExpr::Value(Value::Int(Some(new_schema))),
                        )
                        .filter(
                            object::Column::Oid
                                .is_in(index_ids.iter().cloned().chain(table_ids.iter().cloned())),
                        )
                        .exec(&txn)
                        .await?;
                }

                if !table_ids.is_empty() {
                    let table_objs = Table::find()
                        .find_also_related(Object)
                        .filter(table::Column::TableId.is_in(table_ids))
                        .all(&txn)
                        .await?;
                    for (table, table_obj) in table_objs {
                        relations.push(PbRelationInfo::Table(
                            ObjectModel(table, table_obj.unwrap()).into(),
                        ));
                    }
                }
                if !index_ids.is_empty() {
                    let index_objs = Index::find()
                        .find_also_related(Object)
                        .filter(index::Column::IndexId.is_in(index_ids))
                        .all(&txn)
                        .await?;
                    for (index, index_obj) in index_objs {
                        relations.push(PbRelationInfo::Index(
                            ObjectModel(index, index_obj.unwrap()).into(),
                        ));
                    }
                }
            }
            ObjectType::Source => {
                let source = Source::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("source", object_id))?;
                check_relation_name_duplicate(&source.name, database_id, new_schema, &txn).await?;
                let is_shared = source.is_shared();

                let mut obj = obj.into_active_model();
                obj.schema_id = Set(Some(new_schema));
                let obj = obj.update(&txn).await?;
                relations.push(PbRelationInfo::Source(ObjectModel(source, obj).into()));

                // Note: For non-shared source, we don't update their state tables, which
                // belongs to the MV.
                if is_shared {
                    update_internal_tables(
                        &txn,
                        object_id,
                        object::Column::SchemaId,
                        Value::Int(Some(new_schema)),
                        &mut relations,
                    )
                    .await?;
                }
            }
            ObjectType::Sink => {
                let sink = Sink::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("sink", object_id))?;
                check_relation_name_duplicate(&sink.name, database_id, new_schema, &txn).await?;

                let mut obj = obj.into_active_model();
                obj.schema_id = Set(Some(new_schema));
                let obj = obj.update(&txn).await?;
                relations.push(PbRelationInfo::Sink(ObjectModel(sink, obj).into()));

                update_internal_tables(
                    &txn,
                    object_id,
                    object::Column::SchemaId,
                    Value::Int(Some(new_schema)),
                    &mut relations,
                )
                .await?;
            }
            ObjectType::Subscription => {
                let subscription = Subscription::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("subscription", object_id))?;
                check_relation_name_duplicate(&subscription.name, database_id, new_schema, &txn)
                    .await?;

                let mut obj = obj.into_active_model();
                obj.schema_id = Set(Some(new_schema));
                let obj = obj.update(&txn).await?;
                relations.push(PbRelationInfo::Subscription(
                    ObjectModel(subscription, obj).into(),
                ));
            }
            ObjectType::View => {
                let view = View::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("view", object_id))?;
                check_relation_name_duplicate(&view.name, database_id, new_schema, &txn).await?;

                let mut obj = obj.into_active_model();
                obj.schema_id = Set(Some(new_schema));
                let obj = obj.update(&txn).await?;
                relations.push(PbRelationInfo::View(ObjectModel(view, obj).into()));
            }
            ObjectType::Function => {
                let function = Function::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("function", object_id))?;

                let mut pb_function: PbFunction = ObjectModel(function, obj).into();
                pb_function.schema_id = new_schema as _;
                check_function_signature_duplicate(&pb_function, &txn).await?;

                object::ActiveModel {
                    oid: Set(object_id),
                    schema_id: Set(Some(new_schema)),
                    ..Default::default()
                }
                .update(&txn)
                .await?;

                txn.commit().await?;
                let version = self
                    .notify_frontend(
                        NotificationOperation::Update,
                        NotificationInfo::Function(pb_function),
                    )
                    .await;
                return Ok(version);
            }
            ObjectType::Connection => {
                let connection = Connection::find_by_id(object_id)
                    .one(&txn)
                    .await?
                    .ok_or_else(|| MetaError::catalog_id_not_found("connection", object_id))?;

                let mut pb_connection: PbConnection = ObjectModel(connection, obj).into();
                pb_connection.schema_id = new_schema as _;
                check_connection_name_duplicate(&pb_connection, &txn).await?;

                object::ActiveModel {
                    oid: Set(object_id),
                    schema_id: Set(Some(new_schema)),
                    ..Default::default()
                }
                .update(&txn)
                .await?;

                txn.commit().await?;
                let version = self
                    .notify_frontend(
                        NotificationOperation::Update,
                        NotificationInfo::Connection(pb_connection),
                    )
                    .await;
                return Ok(version);
            }
            _ => unreachable!("not supported object type: {:?}", object_type),
        }

        txn.commit().await?;
        let version = self
            .notify_frontend(
                Operation::Update,
                Info::RelationGroup(PbRelationGroup {
                    relations: relations
                        .into_iter()
                        .map(|relation_info| PbRelation {
                            relation_info: Some(relation_info),
                        })
                        .collect_vec(),
                }),
            )
            .await;
        Ok(version)
    }

    pub async fn alter_secret(
        &self,
        pb_secret: PbSecret,
        secret_plain_payload: Vec<u8>,
    ) -> MetaResult<NotificationVersion> {
        let inner = self.inner.write().await;
        let owner_id = pb_secret.owner as _;
        let txn = inner.db.begin().await?;
        ensure_user_id(owner_id, &txn).await?;
        ensure_object_id(ObjectType::Database, pb_secret.database_id as _, &txn).await?;
        ensure_object_id(ObjectType::Schema, pb_secret.schema_id as _, &txn).await?;

        ensure_object_id(ObjectType::Secret, pb_secret.id as _, &txn).await?;
        let secret: secret::ActiveModel = pb_secret.clone().into();
        Secret::update(secret).exec(&txn).await?;

        txn.commit().await?;

        // Notify the compute and frontend node plain secret
        let mut secret_plain = pb_secret;
        secret_plain.value.clone_from(&secret_plain_payload);

        LocalSecretManager::global().update_secret(secret_plain.id, secret_plain_payload);
        self.env
            .notification_manager()
            .notify_compute_without_version(Operation::Update, Info::Secret(secret_plain.clone()));

        let version = self
            .notify_frontend(
                NotificationOperation::Update,
                NotificationInfo::Secret(secret_plain),
            )
            .await;

        Ok(version)
    }
}

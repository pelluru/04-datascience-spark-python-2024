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

mod jni_catalog;
mod mock_catalog;
mod storage_catalog;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use iceberg::io::{S3_ACCESS_KEY_ID, S3_ENDPOINT, S3_REGION, S3_SECRET_ACCESS_KEY};
use icelake::catalog::{
    load_iceberg_base_catalog_config, BaseCatalogConfig, CATALOG_NAME, CATALOG_TYPE,
};
use risingwave_common::bail;
use serde_derive::Deserialize;
use serde_with::serde_as;
use url::Url;
use with_options::WithOptions;

use crate::deserialize_optional_bool_from_string;
use crate::error::ConnectorResult;

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, WithOptions)]
pub struct IcebergCommon {
    // Catalog type supported by iceberg, such as "storage", "rest".
    // If not set, we use "storage" as default.
    #[serde(rename = "catalog.type")]
    pub catalog_type: Option<String>,
    #[serde(rename = "s3.region")]
    pub region: Option<String>,
    #[serde(rename = "s3.endpoint")]
    pub endpoint: Option<String>,
    #[serde(rename = "s3.access.key")]
    pub access_key: Option<String>,
    #[serde(rename = "s3.secret.key")]
    pub secret_key: Option<String>,
    /// Path of iceberg warehouse, only applicable in storage catalog.
    #[serde(rename = "warehouse.path")]
    pub warehouse_path: Option<String>,
    /// Catalog name, can be omitted for storage catalog, but
    /// must be set for other catalogs.
    #[serde(rename = "catalog.name")]
    pub catalog_name: Option<String>,
    /// URI of iceberg catalog, only applicable in rest catalog.
    #[serde(rename = "catalog.uri")]
    pub catalog_uri: Option<String>,
    #[serde(rename = "database.name")]
    pub database_name: Option<String>,
    /// Full name of table, must include schema name.
    #[serde(rename = "table.name")]
    pub table_name: String,
    /// Credential for accessing iceberg catalog, only applicable in rest catalog.
    /// A credential to exchange for a token in the OAuth2 client credentials flow.
    #[serde(rename = "catalog.credential")]
    pub credential: Option<String>,
    /// token for accessing iceberg catalog, only applicable in rest catalog.
    /// A Bearer token which will be used for interaction with the server.
    #[serde(rename = "catalog.token")]
    pub token: Option<String>,
    /// `oauth2-server-uri` for accessing iceberg catalog, only applicable in rest catalog.
    /// Token endpoint URI to fetch token from if the Rest Catalog is not the authorization server.
    #[serde(rename = "catalog.oauth2-server-uri")]
    pub oauth2_server_uri: Option<String>,
    /// scope for accessing iceberg catalog, only applicable in rest catalog.
    /// Additional scope for OAuth2.
    #[serde(rename = "catalog.scope")]
    pub scope: Option<String>,

    #[serde(
        rename = "s3.path.style.access",
        default,
        deserialize_with = "deserialize_optional_bool_from_string"
    )]
    pub path_style_access: Option<bool>,
    /// enable config load currently is used by iceberg engine, so it only support jdbc catalog.
    #[serde(default, deserialize_with = "deserialize_optional_bool_from_string")]
    pub enable_config_load: Option<bool>,
}

impl IcebergCommon {
    pub fn catalog_type(&self) -> &str {
        self.catalog_type.as_deref().unwrap_or("storage")
    }

    pub fn catalog_name(&self) -> String {
        self.catalog_name
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "risingwave".to_owned())
    }

    /// For both V1 and V2.
    fn build_jni_catalog_configs(
        &self,
        java_catalog_props: &HashMap<String, String>,
    ) -> ConnectorResult<(BaseCatalogConfig, HashMap<String, String>)> {
        let mut iceberg_configs = HashMap::new();

        let base_catalog_config = {
            let catalog_type = self.catalog_type().to_owned();

            iceberg_configs.insert(CATALOG_TYPE.to_owned(), catalog_type.clone());
            iceberg_configs.insert(CATALOG_NAME.to_owned(), self.catalog_name());

            if let Some(region) = &self.region {
                // icelake
                iceberg_configs.insert("iceberg.table.io.region".to_owned(), region.clone());
                // iceberg-rust
                iceberg_configs.insert(format!("iceberg.table.io.{}", S3_REGION), region.clone());
            }

            if let Some(endpoint) = &self.endpoint {
                iceberg_configs.insert("iceberg.table.io.endpoint".to_owned(), endpoint.clone());

                // iceberg-rust
                iceberg_configs.insert(
                    format!("iceberg.table.io.{}", S3_ENDPOINT),
                    endpoint.clone(),
                );
            }

            // icelake
            if let Some(access_key) = &self.access_key {
                iceberg_configs.insert(
                    "iceberg.table.io.access_key_id".to_owned(),
                    access_key.clone(),
                );
            }
            if let Some(secret_key) = &self.secret_key {
                iceberg_configs.insert(
                    "iceberg.table.io.secret_access_key".to_owned(),
                    secret_key.clone(),
                );
            }

            // iceberg-rust
            if let Some(access_key) = &self.access_key {
                iceberg_configs.insert(
                    format!("iceberg.table.io.{}", S3_ACCESS_KEY_ID),
                    access_key.clone(),
                );
            }
            if let Some(secret_key) = &self.secret_key {
                iceberg_configs.insert(
                    format!("iceberg.table.io.{}", S3_SECRET_ACCESS_KEY),
                    secret_key.clone(),
                );
            }

            match &self.warehouse_path {
                Some(warehouse_path) => {
                    let (bucket, _) = {
                        let url = Url::parse(warehouse_path);
                        if url.is_err() && catalog_type == "rest" {
                            // If the warehouse path is not a valid URL, it could be a warehouse name in rest catalog
                            // so we allow it to pass here.
                            (None, None)
                        } else {
                            let url = url.with_context(|| {
                                format!("Invalid warehouse path: {}", warehouse_path)
                            })?;
                            let bucket = url
                                .host_str()
                                .with_context(|| {
                                    format!(
                                        "Invalid s3 path: {}, bucket is missing",
                                        warehouse_path
                                    )
                                })?
                                .to_owned();
                            let root = url.path().trim_start_matches('/').to_owned();
                            (Some(bucket), Some(root))
                        }
                    };

                    if let Some(bucket) = bucket {
                        iceberg_configs.insert("iceberg.table.io.bucket".to_owned(), bucket);
                    }
                }
                None => {
                    if catalog_type != "rest" {
                        bail!("`warehouse.path` must be set in {} catalog", &catalog_type);
                    }
                }
            }
            let enable_config_load = self.enable_config_load.unwrap_or(false);
            iceberg_configs.insert(
                "iceberg.table.io.disable_config_load".to_owned(),
                (!enable_config_load).to_string(),
            );

            load_iceberg_base_catalog_config(&iceberg_configs)?
        };

        // Prepare jni configs, for details please see https://iceberg.apache.org/docs/latest/aws/
        let mut java_catalog_configs = HashMap::new();
        {
            if let Some(uri) = self.catalog_uri.as_deref() {
                java_catalog_configs.insert("uri".to_owned(), uri.to_owned());
            }

            if let Some(warehouse_path) = &self.warehouse_path {
                java_catalog_configs.insert("warehouse".to_owned(), warehouse_path.clone());
            }
            java_catalog_configs.extend(java_catalog_props.clone());

            // Currently we only support s3, so let's set it to s3
            java_catalog_configs.insert(
                "io-impl".to_owned(),
                "org.apache.iceberg.aws.s3.S3FileIO".to_owned(),
            );

            // suppress log of S3FileIO like: Unclosed S3FileIO instance created by...
            java_catalog_configs.insert("init-creation-stacktrace".to_owned(), "false".to_owned());

            if let Some(region) = &self.region {
                java_catalog_configs.insert("client.region".to_owned(), region.clone());
            }
            if let Some(endpoint) = &self.endpoint {
                java_catalog_configs.insert("s3.endpoint".to_owned(), endpoint.clone());
            }

            if let Some(access_key) = &self.access_key {
                java_catalog_configs.insert("s3.access-key-id".to_owned(), access_key.clone());
            }
            if let Some(secret_key) = &self.secret_key {
                java_catalog_configs.insert("s3.secret-access-key".to_owned(), secret_key.clone());
            }

            if let Some(path_style_access) = self.path_style_access {
                java_catalog_configs.insert(
                    "s3.path-style-access".to_owned(),
                    path_style_access.to_string(),
                );
            }

            match self.catalog_type.as_deref() {
                Some("rest") => {
                    if let Some(credential) = &self.credential {
                        java_catalog_configs.insert("credential".to_owned(), credential.clone());
                    }
                    if let Some(token) = &self.token {
                        java_catalog_configs.insert("token".to_owned(), token.clone());
                    }
                    if let Some(oauth2_server_uri) = &self.oauth2_server_uri {
                        java_catalog_configs
                            .insert("oauth2-server-uri".to_owned(), oauth2_server_uri.clone());
                    }
                    if let Some(scope) = &self.scope {
                        java_catalog_configs.insert("scope".to_owned(), scope.clone());
                    }
                }
                Some("glue") => {
                    java_catalog_configs.insert(
                        "client.credentials-provider".to_owned(),
                        "com.risingwave.connector.catalog.GlueCredentialProvider".to_owned(),
                    );
                    // Use S3 ak/sk and region as glue ak/sk and region by default.
                    // TODO: use different ak/sk and region for s3 and glue.
                    if let Some(access_key) = &self.access_key {
                        java_catalog_configs.insert(
                            "client.credentials-provider.glue.access-key-id".to_owned(),
                            access_key.clone(),
                        );
                    }
                    if let Some(secret_key) = &self.secret_key {
                        java_catalog_configs.insert(
                            "client.credentials-provider.glue.secret-access-key".to_owned(),
                            secret_key.clone(),
                        );
                    }
                    if let Some(region) = &self.region {
                        java_catalog_configs.insert("client.region".to_owned(), region.clone());
                        java_catalog_configs.insert(
                            "glue.endpoint".to_owned(),
                            format!("https://glue.{}.amazonaws.com", region),
                        );
                    }
                }
                _ => {}
            }
        }

        Ok((base_catalog_config, java_catalog_configs))
    }
}

/// icelake
mod v1 {
    use anyhow::anyhow;
    use icelake::catalog::{load_catalog, CatalogRef};
    use icelake::{Table, TableIdentifier};

    use super::*;

    impl IcebergCommon {
        pub fn full_table_name(&self) -> ConnectorResult<TableIdentifier> {
            let ret = if let Some(database_name) = &self.database_name {
                TableIdentifier::new(vec![database_name, &self.table_name])
            } else {
                TableIdentifier::new(vec![&self.table_name])
            };

            Ok(ret.context("Failed to create table identifier")?)
        }

        fn build_iceberg_configs(&self) -> ConnectorResult<HashMap<String, String>> {
            let mut iceberg_configs = HashMap::new();

            let catalog_type = self.catalog_type().to_owned();

            iceberg_configs.insert(CATALOG_NAME.to_owned(), self.catalog_name());

            match catalog_type.as_str() {
                "storage" => {
                    iceberg_configs.insert(
                        format!("iceberg.catalog.{}.warehouse", self.catalog_name()),
                        self.warehouse_path.clone().ok_or_else(|| {
                            anyhow!("`warehouse.path` must be set in storage catalog")
                        })?,
                    );
                    iceberg_configs.insert(CATALOG_TYPE.to_owned(), "storage".into());
                }
                "rest_rust" => {
                    let uri = self
                        .catalog_uri
                        .clone()
                        .with_context(|| "`catalog.uri` must be set in rest catalog".to_owned())?;
                    iceberg_configs
                        .insert(format!("iceberg.catalog.{}.uri", self.catalog_name()), uri);
                    iceberg_configs.insert(CATALOG_TYPE.to_owned(), "rest".into());
                }
                _ => {
                    bail!(
                        "Unsupported catalog type: {}, only support `storage` and `rest`",
                        catalog_type
                    );
                }
            }

            if let Some(region) = &self.region {
                iceberg_configs.insert("iceberg.table.io.region".to_owned(), region.clone());
            }

            if let Some(endpoint) = &self.endpoint {
                iceberg_configs.insert("iceberg.table.io.endpoint".to_owned(), endpoint.clone());
            }

            if let Some(access_key) = &self.access_key {
                iceberg_configs.insert(
                    "iceberg.table.io.access_key_id".to_owned(),
                    access_key.clone(),
                );
            }
            if let Some(secret_key) = &self.secret_key {
                iceberg_configs.insert(
                    "iceberg.table.io.secret_access_key".to_owned(),
                    secret_key.clone(),
                );
            }

            if let Some(path_style_access) = self.path_style_access {
                iceberg_configs.insert(
                    "iceberg.table.io.enable_virtual_host_style".to_owned(),
                    (!path_style_access).to_string(),
                );
            }

            match &self.warehouse_path {
                Some(warehouse_path) => {
                    let (bucket, root) = {
                        let url = Url::parse(warehouse_path).with_context(|| {
                            format!("Invalid warehouse path: {}", warehouse_path)
                        })?;
                        let bucket = url
                            .host_str()
                            .with_context(|| {
                                format!("Invalid s3 path: {}, bucket is missing", warehouse_path)
                            })?
                            .to_owned();
                        let root = url.path().trim_start_matches('/').to_owned();
                        (bucket, root)
                    };

                    iceberg_configs.insert("iceberg.table.io.bucket".to_owned(), bucket);

                    // Only storage catalog should set this.
                    if catalog_type == "storage" {
                        iceberg_configs.insert("iceberg.table.io.root".to_owned(), root);
                    }
                }
                None => {
                    if catalog_type == "storage" {
                        bail!("`warehouse.path` must be set in storage catalog");
                    }
                }
            }

            let enable_config_load = self.enable_config_load.unwrap_or(false);
            iceberg_configs.insert(
                "iceberg.table.io.disable_config_load".to_owned(),
                (!enable_config_load).to_string(),
            );

            Ok(iceberg_configs)
        }

        /// TODO: remove the arguments and put them into `IcebergCommon`. Currently the handling in source and sink are different, so pass them separately to be safer.
        pub async fn create_catalog(
            &self,
            java_catalog_props: &HashMap<String, String>,
        ) -> ConnectorResult<CatalogRef> {
            match self.catalog_type() {
                "storage" | "rest_rust" => {
                    let iceberg_configs = self.build_iceberg_configs()?;
                    let catalog = load_catalog(&iceberg_configs).await?;
                    Ok(catalog)
                }
                catalog_type
                    if catalog_type == "hive"
                        || catalog_type == "jdbc"
                        || catalog_type == "glue"
                        || catalog_type == "rest" =>
                {
                    // Create java catalog
                    let (base_catalog_config, java_catalog_props) =
                        self.build_jni_catalog_configs(java_catalog_props)?;
                    let catalog_impl = match catalog_type {
                        "hive" => "org.apache.iceberg.hive.HiveCatalog",
                        "jdbc" => "org.apache.iceberg.jdbc.JdbcCatalog",
                        "glue" => "org.apache.iceberg.aws.glue.GlueCatalog",
                        "rest" => "org.apache.iceberg.rest.RESTCatalog",
                        _ => unreachable!(),
                    };

                    jni_catalog::JniCatalog::build_catalog(
                        base_catalog_config,
                        self.catalog_name(),
                        catalog_impl,
                        java_catalog_props,
                    )
                }
                "mock" => Ok(Arc::new(mock_catalog::MockCatalog {})),
                _ => {
                    bail!(
                    "Unsupported catalog type: {}, only support `storage`, `rest`, `hive`, `jdbc`, `glue`",
                    self.catalog_type()
                )
                }
            }
        }

        /// TODO: remove the arguments and put them into `IcebergCommon`. Currently the handling in source and sink are different, so pass them separately to be safer.
        pub async fn load_table(
            &self,
            java_catalog_props: &HashMap<String, String>,
        ) -> ConnectorResult<Table> {
            let catalog = self
                .create_catalog(java_catalog_props)
                .await
                .context("Unable to load iceberg catalog")?;

            let table_id = self
                .full_table_name()
                .context("Unable to parse table name")?;

            catalog.load_table(&table_id).await.map_err(Into::into)
        }
    }
}

/// iceberg-rust
mod v2 {
    use anyhow::anyhow;
    use iceberg::spec::TableMetadata;
    use iceberg::table::Table as TableV2;
    use iceberg::{Catalog as CatalogV2, TableIdent};
    use iceberg_catalog_glue::{AWS_ACCESS_KEY_ID, AWS_REGION_NAME, AWS_SECRET_ACCESS_KEY};

    use super::*;

    impl IcebergCommon {
        pub fn full_table_name_v2(&self) -> ConnectorResult<TableIdent> {
            let ret = if let Some(database_name) = &self.database_name {
                TableIdent::from_strs(vec![database_name, &self.table_name])
            } else {
                TableIdent::from_strs(vec![&self.table_name])
            };

            Ok(ret.context("Failed to create table identifier")?)
        }

        /// TODO: remove the arguments and put them into `IcebergCommon`. Currently the handling in source and sink are different, so pass them separately to be safer.
        pub async fn create_catalog_v2(
            &self,
            java_catalog_props: &HashMap<String, String>,
        ) -> ConnectorResult<Arc<dyn CatalogV2>> {
            match self.catalog_type() {
                "storage" => {
                    let config = storage_catalog::StorageCatalogConfig::builder()
                        .warehouse(self.warehouse_path.clone().ok_or_else(|| {
                            anyhow!("`warehouse.path` must be set in storage catalog")
                        })?)
                        .access_key(self.access_key.clone().ok_or_else(|| {
                            anyhow!("`s3.access.key` must be set in storage catalog")
                        })?)
                        .secret_key(self.secret_key.clone().ok_or_else(|| {
                            anyhow!("`s3.secret.key` must be set in storage catalog")
                        })?)
                        .region(self.region.clone())
                        .endpoint(self.endpoint.clone())
                        .build();
                    let catalog = storage_catalog::StorageCatalog::new(config)?;
                    Ok(Arc::new(catalog))
                }
                "rest_rust" => {
                    let mut iceberg_configs = HashMap::new();
                    if let Some(region) = &self.region {
                        iceberg_configs.insert(S3_REGION.to_owned(), region.clone());
                    }
                    if let Some(endpoint) = &self.endpoint {
                        iceberg_configs.insert(S3_ENDPOINT.to_owned(), endpoint.clone());
                    }
                    if let Some(access_key) = &self.access_key {
                        iceberg_configs.insert(S3_ACCESS_KEY_ID.to_owned(), access_key.clone());
                    }
                    if let Some(secret_key) = &self.secret_key {
                        iceberg_configs.insert(S3_SECRET_ACCESS_KEY.to_owned(), secret_key.clone());
                    }
                    if let Some(credential) = &self.credential {
                        iceberg_configs.insert("credential".to_owned(), credential.clone());
                    }
                    if let Some(token) = &self.token {
                        iceberg_configs.insert("token".to_owned(), token.clone());
                    }
                    if let Some(oauth2_server_uri) = &self.oauth2_server_uri {
                        iceberg_configs
                            .insert("oauth2-server-uri".to_owned(), oauth2_server_uri.clone());
                    }
                    if let Some(scope) = &self.scope {
                        iceberg_configs.insert("scope".to_owned(), scope.clone());
                    }

                    let config_builder = iceberg_catalog_rest::RestCatalogConfig::builder()
                        .uri(self.catalog_uri.clone().with_context(|| {
                            "`catalog.uri` must be set in rest catalog".to_owned()
                        })?)
                        .props(iceberg_configs);

                    let config = match &self.warehouse_path {
                        Some(warehouse_path) => {
                            config_builder.warehouse(warehouse_path.clone()).build()
                        }
                        None => config_builder.build(),
                    };
                    let catalog = iceberg_catalog_rest::RestCatalog::new(config);
                    Ok(Arc::new(catalog))
                }
                "glue" => {
                    let mut iceberg_configs = HashMap::new();
                    // glue
                    if let Some(region) = &self.region {
                        iceberg_configs.insert(AWS_REGION_NAME.to_owned(), region.clone());
                    }
                    if let Some(access_key) = &self.access_key {
                        iceberg_configs.insert(AWS_ACCESS_KEY_ID.to_owned(), access_key.clone());
                    }
                    if let Some(secret_key) = &self.secret_key {
                        iceberg_configs
                            .insert(AWS_SECRET_ACCESS_KEY.to_owned(), secret_key.clone());
                    }
                    // s3
                    if let Some(region) = &self.region {
                        iceberg_configs.insert(S3_REGION.to_owned(), region.clone());
                    }
                    if let Some(endpoint) = &self.endpoint {
                        iceberg_configs.insert(S3_ENDPOINT.to_owned(), endpoint.clone());
                    }
                    if let Some(access_key) = &self.access_key {
                        iceberg_configs.insert(S3_ACCESS_KEY_ID.to_owned(), access_key.clone());
                    }
                    if let Some(secret_key) = &self.secret_key {
                        iceberg_configs.insert(S3_SECRET_ACCESS_KEY.to_owned(), secret_key.clone());
                    }
                    let config_builder = iceberg_catalog_glue::GlueCatalogConfig::builder()
                        .warehouse(self.warehouse_path.clone().ok_or_else(|| {
                            anyhow!("`warehouse.path` must be set in glue catalog")
                        })?)
                        .props(iceberg_configs);
                    let config = if let Some(uri) = self.catalog_uri.as_deref() {
                        config_builder.uri(uri.to_owned()).build()
                    } else {
                        config_builder.build()
                    };
                    let catalog = iceberg_catalog_glue::GlueCatalog::new(config).await?;
                    Ok(Arc::new(catalog))
                }
                catalog_type
                    if catalog_type == "hive"
                        || catalog_type == "jdbc"
                        || catalog_type == "rest" =>
                {
                    // Create java catalog
                    let (base_catalog_config, java_catalog_props) =
                        self.build_jni_catalog_configs(java_catalog_props)?;
                    let catalog_impl = match catalog_type {
                        "hive" => "org.apache.iceberg.hive.HiveCatalog",
                        "jdbc" => "org.apache.iceberg.jdbc.JdbcCatalog",
                        "rest" => "org.apache.iceberg.rest.RESTCatalog",
                        _ => unreachable!(),
                    };

                    jni_catalog::JniCatalog::build_catalog_v2(
                        base_catalog_config,
                        self.catalog_name(),
                        catalog_impl,
                        java_catalog_props,
                    )
                }
                "mock" => Ok(Arc::new(mock_catalog::MockCatalog {})),
                _ => {
                    bail!(
                    "Unsupported catalog type: {}, only support `storage`, `rest`, `hive`, `jdbc`, `glue`",
                    self.catalog_type()
                )
                }
            }
        }

        /// TODO: remove the arguments and put them into `IcebergCommon`. Currently the handling in source and sink are different, so pass them separately to be safer.
        pub async fn load_table_v2(
            &self,
            java_catalog_props: &HashMap<String, String>,
        ) -> ConnectorResult<TableV2> {
            let catalog = self
                .create_catalog_v2(java_catalog_props)
                .await
                .context("Unable to load iceberg catalog")?;

            let table_id = self
                .full_table_name_v2()
                .context("Unable to parse table name")?;

            catalog.load_table(&table_id).await.map_err(Into::into)
        }

        pub async fn load_table_v2_with_metadata(
            &self,
            metadata: TableMetadata,
            java_catalog_props: &HashMap<String, String>,
        ) -> ConnectorResult<TableV2> {
            match self.catalog_type() {
                "storage" => {
                    let config = storage_catalog::StorageCatalogConfig::builder()
                        .warehouse(self.warehouse_path.clone().ok_or_else(|| {
                            anyhow!("`warehouse.path` must be set in storage catalog")
                        })?)
                        .access_key(self.access_key.clone().ok_or_else(|| {
                            anyhow!("`s3.access.key` must be set in storage catalog")
                        })?)
                        .secret_key(self.secret_key.clone().ok_or_else(|| {
                            anyhow!("`s3.secret.key` must be set in storage catalog")
                        })?)
                        .region(self.region.clone())
                        .endpoint(self.endpoint.clone())
                        .build();
                    let storage_catalog = storage_catalog::StorageCatalog::new(config)?;

                    let table_id = self
                        .full_table_name_v2()
                        .context("Unable to parse table name")?;

                    Ok(iceberg::table::Table::builder()
                        .metadata(metadata)
                        .identifier(table_id)
                        .file_io(storage_catalog.file_io().clone())
                        // Only support readonly table for storage catalog now.
                        .readonly(true)
                        .build()?)
                }
                _ => self.load_table_v2(java_catalog_props).await,
            }
        }
    }
}

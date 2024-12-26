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

use pgwire::pg_response::{PgResponse, StatementType};
use risingwave_sqlparser::ast::{DropMode, ObjectName};

use super::RwPgResponse;
use crate::binder::Binder;
use crate::catalog::CatalogError;
use crate::error::{ErrorCode, Result};
use crate::handler::HandlerArgs;

pub async fn handle_drop_user(
    handler_args: HandlerArgs,
    user_name: ObjectName,
    if_exists: bool,
    mode: Option<DropMode>,
) -> Result<RwPgResponse> {
    let session = handler_args.session;
    if mode.is_some() {
        return Err(ErrorCode::BindError("Drop user not support drop mode".to_owned()).into());
    }

    let user_name = Binder::resolve_user_name(user_name)?;
    let user_info_reader = session.env().user_info_reader();
    let user_id = user_info_reader
        .read_guard()
        .get_user_by_name(&user_name)
        .map(|u| u.id);
    match user_id {
        Some(user_id) => {
            let user_info_writer = session.user_info_writer()?;
            user_info_writer.drop_user(user_id).await?;
        }
        None => {
            return if if_exists {
                Ok(PgResponse::builder(StatementType::DROP_USER)
                    .notice(format!("user \"{}\" does not exist, skipping", user_name))
                    .into())
            } else {
                Err(CatalogError::NotFound("user", user_name).into())
            };
        }
    }

    Ok(PgResponse::empty_result(StatementType::DROP_USER))
}

#[cfg(test)]
mod tests {
    use crate::test_utils::LocalFrontend;

    #[tokio::test]
    async fn test_drop_user() {
        let frontend = LocalFrontend::new(Default::default()).await;
        let session = frontend.session_ref();
        let user_info_reader = session.env().user_info_reader();

        frontend.run_sql("CREATE USER user").await.unwrap();
        assert!(user_info_reader
            .read_guard()
            .get_user_by_name("user")
            .is_some());

        frontend.run_sql("DROP USER user").await.unwrap();
        assert!(user_info_reader
            .read_guard()
            .get_user_by_name("user")
            .is_none());
    }
}

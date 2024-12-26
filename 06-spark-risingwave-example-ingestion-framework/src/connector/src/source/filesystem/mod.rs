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

pub use opendal_source::GcsProperties;
pub use s3::{S3FileReader, S3Properties, S3SplitEnumerator, S3_CONNECTOR};
pub mod file_common;
pub mod nd_streaming;
pub use file_common::{FsPage, FsPageItem, FsSplit, OpendalFsSplit};
pub mod opendal_source;
mod s3;
pub mod s3_v2;

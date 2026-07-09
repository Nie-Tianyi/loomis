//! # Tools — abstraction layer
//!
//! Defines the [`Tool`] trait, [`ToolRegistry`] container, [`WorkspaceFs`]
//! sandbox, and JSON Schema generation helpers.
//!
//! Concrete tool implementations live in downstream crates (e.g. `loomis`).

mod error;
mod fs;
mod registry;
mod schema;
mod tool;

pub use error::{FsError, ToolError};
pub use fs::{DirEntry, EntryType, GrepMatch, WorkspaceFs};
pub use registry::{tool_to_def, ToolRegistry};
pub use schema::generate_schema;
pub use tool::Tool;

use std::iter::{Extend, IntoIterator};
use std::sync::Arc;

use crate::statement::PgStatementMetadata;
use crate::PgColumn;

#[derive(Debug, Default)]
pub struct PgQueryResult {
    pub(super) rows_affected: u64,
    // Parliament patch: the statement's command tag and row description,
    // which psql prints and upstream sqlx discards.
    pub(super) command_tag: String,
    pub(super) row_description: Option<Arc<PgStatementMetadata>>,
}

impl PgQueryResult {
    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }

    /// The server's command tag for the statement — `SELECT 3`,
    /// `INSERT 0 1`, `CREATE TABLE`, `ROLLBACK` for a COMMIT of a failed
    /// transaction: what psql prints for a statement that returns no
    /// rows. (Parliament patch.)
    pub fn command_tag(&self) -> &str {
        &self.command_tag
    }

    /// The columns of the rows the statement returns — the server's row
    /// description, there even when no row follows it. Empty for a
    /// statement that doesn't return rows. (Parliament patch.)
    pub fn columns(&self) -> &[PgColumn] {
        self.row_description.as_deref().map_or(&[], |metadata| &metadata.columns)
    }
}

impl Extend<PgQueryResult> for PgQueryResult {
    /// Rows affected add up; the tag and columns are the last statement's.
    fn extend<T: IntoIterator<Item = PgQueryResult>>(&mut self, iter: T) {
        for elem in iter {
            self.rows_affected += elem.rows_affected;
            self.command_tag = elem.command_tag;
            self.row_description = elem.row_description;
        }
    }
}

#[cfg(feature = "any")]
impl From<PgQueryResult> for sqlx_core::any::AnyQueryResult {
    fn from(done: PgQueryResult) -> Self {
        sqlx_core::any::AnyQueryResult {
            rows_affected: done.rows_affected,
            last_insert_id: None,
        }
    }
}

use std::iter::{Extend, IntoIterator};

#[derive(Debug, Default)]
pub struct MySqlQueryResult {
    pub(super) rows_affected: u64,
    pub(super) last_insert_id: u64,
    // Parliament patch: what the mysql CLI prints with the row count,
    // which upstream sqlx discards.
    pub(super) warnings: u16,
    pub(super) info: String,
}

impl MySqlQueryResult {
    pub fn last_insert_id(&self) -> u64 {
        self.last_insert_id
    }

    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }

    /// The statement's warning count — `Query OK, 1 row affected,
    /// 1 warning` in the mysql CLI. (Parliament patch.)
    pub fn warnings(&self) -> u16 {
        self.warnings
    }

    /// The server's info line for the statement — `Rows matched: 1
    /// Changed: 0  Warnings: 0`, `Records: 3  Duplicates: 0  Warnings: 0`
    /// — which the mysql CLI prints under the row count. Empty when the
    /// server sends none. (Parliament patch.)
    pub fn info(&self) -> &str {
        &self.info
    }
}

impl Extend<MySqlQueryResult> for MySqlQueryResult {
    /// Rows affected add up; the rest is the last statement's.
    fn extend<T: IntoIterator<Item = MySqlQueryResult>>(&mut self, iter: T) {
        for elem in iter {
            self.rows_affected += elem.rows_affected;
            self.last_insert_id = elem.last_insert_id;
            self.warnings = elem.warnings;
            self.info = elem.info;
        }
    }
}
#[cfg(feature = "any")]
/// This conversion attempts to save last_insert_id by converting to i64.
impl From<MySqlQueryResult> for sqlx_core::any::AnyQueryResult {
    fn from(done: MySqlQueryResult) -> Self {
        sqlx_core::any::AnyQueryResult {
            rows_affected: done.rows_affected(),
            last_insert_id: done.last_insert_id().try_into().ok(),
        }
    }
}

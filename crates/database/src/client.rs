//! An object-safe query surface shared by a pooled client and a transaction.
//!
//! `postgres::GenericClient` is the natural abstraction here, but its methods
//! are generic over `ToStatement`, so it is not object-safe and every one of
//! the ~200 repository functions would have had to become generic to accept
//! either a `Client` or a `Transaction`. [`Db`] is the same idea with the
//! generics erased: `&mut dyn Db` is a thing you can pass around, which is what
//! lets `repo::*` keep taking a plain connection argument the way it did under
//! rusqlite.
//!
//! The method names deliberately echo the rusqlite ones they replaced
//! (`row_opt` for `query_row(..).optional()`, `rows` for `query_map`), so the
//! shape of a ported query is recognisable next to its git history.

use postgres::types::ToSql;
use postgres::Row;

use crate::Result;

/// The parameter list every method here takes. Build one with [`crate::params`].
pub type Params<'a> = &'a [&'a (dyn ToSql + Sync)];

/// Anything that can run a statement: a pooled connection, or a transaction
/// borrowed from one.
pub trait Db {
    /// Runs a statement and returns the number of rows it touched.
    fn exec(&mut self, sql: &str, params: Params<'_>) -> Result<u64>;

    /// Runs a query and collects every row.
    fn rows(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>>;

    /// Runs a query expected to match at most one row.
    fn row_opt(&mut self, sql: &str, params: Params<'_>) -> Result<Option<Row>>;

    /// Runs a script of semicolon-separated statements with no parameters.
    fn batch(&mut self, sql: &str) -> Result<()>;

    /// Runs a query that must match exactly one row.
    fn row_one(&mut self, sql: &str, params: Params<'_>) -> Result<Row> {
        self.row_opt(sql, params)?
            .ok_or_else(|| crate::DbError::NotFound(sql.trim().chars().take(60).collect()))
    }
}

macro_rules! impl_db {
    ($ty:ty) => {
        impl Db for $ty {
            fn exec(&mut self, sql: &str, params: Params<'_>) -> Result<u64> {
                Ok(postgres::Client::execute(self, sql, params)?)
            }

            fn rows(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>> {
                Ok(postgres::Client::query(self, sql, params)?)
            }

            fn row_opt(&mut self, sql: &str, params: Params<'_>) -> Result<Option<Row>> {
                Ok(postgres::Client::query_opt(self, sql, params)?)
            }

            fn batch(&mut self, sql: &str) -> Result<()> {
                postgres::Client::batch_execute(self, sql)?;
                Ok(())
            }
        }
    };
}

impl_db!(postgres::Client);
// The pooled connection derefs to a `Client`, but a blanket impl over `DerefMut`
// would collide with the `Transaction` impl below, so it is spelled out.
impl_db!(crate::DbConn);

impl Db for postgres::Transaction<'_> {
    fn exec(&mut self, sql: &str, params: Params<'_>) -> Result<u64> {
        Ok(postgres::Transaction::execute(self, sql, params)?)
    }

    fn rows(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>> {
        Ok(postgres::Transaction::query(self, sql, params)?)
    }

    fn row_opt(&mut self, sql: &str, params: Params<'_>) -> Result<Option<Row>> {
        Ok(postgres::Transaction::query_opt(self, sql, params)?)
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        postgres::Transaction::batch_execute(self, sql)?;
        Ok(())
    }
}

/// Forwarding impl so `&mut dyn Db` can be passed on to a function that wants
/// its own `&mut impl Db`, and so `&mut *conn` reborrows work.
impl<T: Db + ?Sized> Db for &mut T {
    fn exec(&mut self, sql: &str, params: Params<'_>) -> Result<u64> {
        (**self).exec(sql, params)
    }

    fn rows(&mut self, sql: &str, params: Params<'_>) -> Result<Vec<Row>> {
        (**self).rows(sql, params)
    }

    fn row_opt(&mut self, sql: &str, params: Params<'_>) -> Result<Option<Row>> {
        (**self).row_opt(sql, params)
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        (**self).batch(sql)
    }

    fn row_one(&mut self, sql: &str, params: Params<'_>) -> Result<Row> {
        (**self).row_one(sql, params)
    }
}

/// Builds the parameter slice the [`Db`] methods take.
///
/// The rusqlite macro of the same name is what the repository layer used, so
/// keeping the name means a ported query differs only in its placeholders
/// (`?1` became `$1`).
///
/// ```ignore
/// conn.exec("UPDATE shoots SET name = $1 WHERE id = $2", params![name, id])?;
/// ```
/// Every path is `$crate`-qualified so the macro works in crates that do not
/// depend on `postgres` themselves — `skwad-catalogue` reads the index through
/// `Db` without ever naming the driver.
#[macro_export]
macro_rules! params {
    () => {
        &[] as $crate::client::Params<'_>
    };
    ($($value:expr),+ $(,)?) => {
        &[$(&$value as &(dyn $crate::postgres::types::ToSql + Sync)),+] as $crate::client::Params<'_>
    };
}

//! The store's deliberately small SQL boundary. SQLite remains available for
//! local/offline tools; Cells uses real PostgreSQL rows in a private schema.
//! A dedicated worker owns the PostgreSQL connection, so callers may be inside
//! a Tokio runtime. There is no cache, shadow SQLite database, or async writeback.
use rusqlite::{
    Result, ToSql,
    types::{FromSql, ToSqlOutput, Value, ValueRef},
};
use std::{cell::Cell, ops::Deref, path::Path, sync::mpsc};

pub struct Connection {
    backend: Backend,
    transaction: Cell<bool>,
}
enum Backend {
    Sqlite(rusqlite::Connection),
    Postgres(Worker),
}
struct Worker {
    sender: Option<mpsc::Sender<Request>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
struct Request {
    sql: String,
    values: Vec<Value>,
    batch: bool,
    reply: mpsc::Sender<Result<Reply>>,
}
struct Reply {
    rows: Vec<Row>,
    changed: usize,
}
pub struct Row {
    pub(crate) values: Vec<Value>,
}
pub trait RowIndex {
    fn index(self) -> usize;
}
impl RowIndex for usize {
    fn index(self) -> usize {
        self
    }
}
impl Row {
    pub fn get<I: RowIndex, T: FromSql>(&self, index: I) -> Result<T> {
        let i = index.index();
        let value = self
            .values
            .get(i)
            .ok_or(rusqlite::Error::InvalidColumnIndex(i))?;
        T::column_result(ValueRef::from(value)).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, value.data_type(), Box::new(e))
        })
    }
}
pub trait Parameters {
    fn values(self) -> Result<Vec<Value>>;
}
fn owned(v: &dyn ToSql) -> Result<Value> {
    match v.to_sql()? {
        ToSqlOutput::Borrowed(v) => Ok(v.into()),
        ToSqlOutput::Owned(v) => Ok(v),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
impl Parameters for &[&dyn ToSql] {
    fn values(self) -> Result<Vec<Value>> {
        self.iter().map(|v| owned(*v)).collect()
    }
}
impl Parameters for [(); 0] {
    fn values(self) -> Result<Vec<Value>> {
        Ok(vec![])
    }
}
impl Parameters for Vec<Value> {
    fn values(self) -> Result<Vec<Value>> {
        Ok(self)
    }
}
macro_rules! parameter_array { ($($n:literal),*) => { $(impl<T: ToSql> Parameters for [T; $n] { fn values(self) -> Result<Vec<Value>> { self.iter().map(|v| owned(v)).collect() } })* }; }
parameter_array!(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);

impl Connection {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            backend: Backend::Sqlite(rusqlite::Connection::open(path)?),
            transaction: Cell::new(false),
        })
    }
    pub fn postgres(url: &str) -> Result<Self> {
        let mut config: postgres::Config =
            url.parse().map_err(|_| rusqlite::Error::InvalidQuery)?;
        config.connect_timeout(std::time::Duration::from_secs(10));
        // Only the Cells per-instance loopback gate may use plaintext. Remote
        // SQL gateways always require TLS with certificate/hostname verification,
        // even when a caller's URL asks for prefer or disable.
        if config.get_hosts().is_empty()
            || config
                .get_hosts()
                .iter()
                .any(|h| !matches!(h, postgres::config::Host::Tcp(_)))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let loopback = config.get_hosts().iter().all(|h| match h {
            postgres::config::Host::Tcp(h) => {
                h.parse::<std::net::IpAddr>().is_ok_and(|a| a.is_loopback())
            }
            _ => false,
        });
        let (sender, receiver) = mpsc::channel::<Request>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new().name("mcport-postgres".into()).spawn(move || {
            let connected = if loopback {
                config.connect(postgres::NoTls)
            } else {
                let mut config = config;
                config.ssl_mode(postgres::config::SslMode::Require);
                let connector = match native_tls::TlsConnector::new() {
                    Ok(c) => postgres_native_tls::MakeTlsConnector::new(c),
                    Err(_) => { let _ = ready_tx.send(Err(rusqlite::Error::InvalidQuery)); return; }
                };
                config.connect(connector)
            };
            let mut client = match connected {
                Ok(c) => c,
                Err(_) => { let _ = ready_tx.send(Err(rusqlite::Error::InvalidQuery)); return; }
            };
            let ready = (|| {
                // MCPort coordinates dispatch and families in one process. A
                // second writer must fail before loading any application data.
                let locked: bool = client.query_one("SELECT pg_try_advisory_lock(1929374771, 1296257091)", &[]).map_err(pg_error)?.get(0);
                if !locked { return Err(rusqlite::Error::InvalidQuery); }
                client.batch_execute("SET statement_timeout='30s'; SET idle_in_transaction_session_timeout='120s'; CREATE SCHEMA IF NOT EXISTS mcport; REVOKE ALL ON SCHEMA mcport FROM PUBLIC; SET search_path=mcport,pg_catalog;").map_err(pg_error)?;
                Ok(())
            })();
            if ready_tx.send(ready).is_err() { return; }
            while let Ok(request) = receiver.recv() {
                let result = if request.batch {
                    client.batch_execute(&request.sql).map(|_| Reply { rows: vec![], changed: 0 }).map_err(pg_error)
                } else { pg_query(&mut client, &request.sql, request.values) };
                let _ = request.reply.send(result);
            }
        }).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if let Err(error) = ready_rx
            .recv()
            .unwrap_or(Err(rusqlite::Error::InvalidQuery))
        {
            drop(sender);
            let _ = thread.join();
            return Err(error);
        }
        Ok(Self {
            backend: Backend::Postgres(Worker {
                sender: Some(sender),
                thread: Some(thread),
            }),
            transaction: Cell::new(false),
        })
    }
    pub fn is_postgres(&self) -> bool {
        matches!(self.backend, Backend::Postgres(_))
    }
    pub fn busy_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        if let Backend::Sqlite(db) = &self.backend {
            db.busy_timeout(timeout)?;
        }
        Ok(())
    }
    fn send(&self, sql: String, values: Vec<Value>, batch: bool) -> Result<Reply> {
        let Backend::Postgres(sender) = &self.backend else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        let (reply, result) = mpsc::channel();
        sender
            .sender
            .as_ref()
            .ok_or(rusqlite::Error::InvalidQuery)?
            .send(Request {
                sql,
                values,
                batch,
                reply,
            })
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        result.recv().map_err(|_| rusqlite::Error::InvalidQuery)?
    }
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        match &self.backend {
            Backend::Sqlite(db) => db.execute_batch(sql),
            Backend::Postgres(_) => {
                self.send(sql.into(), vec![], true)?;
                Ok(())
            }
        }
    }
    fn query(&self, sql: &str, values: Vec<Value>) -> Result<Reply> {
        match &self.backend {
            Backend::Sqlite(db) => {
                let mut statement = db.prepare(sql)?;
                let width = statement.column_count();
                if width == 0 {
                    return Ok(Reply {
                        rows: vec![],
                        changed: statement.execute(rusqlite::params_from_iter(values))?,
                    });
                }
                let rows = statement
                    .query_map(rusqlite::params_from_iter(values), |r| {
                        Ok(Row {
                            values: (0..width).map(|i| r.get(i)).collect::<Result<_>>()?,
                        })
                    })?
                    .collect::<Result<_>>()?;
                Ok(Reply { rows, changed: 0 })
            }
            Backend::Postgres(_) => self.send(translate(sql), values, false),
        }
    }
    pub fn execute<P: Parameters>(&self, sql: &str, params: P) -> Result<usize> {
        // SQLite permits handling a constraint violation and continuing a
        // transaction. Match that exact behavior with a Postgres savepoint.
        let savepoint = self.is_postgres() && self.transaction.get();
        if savepoint {
            self.execute_batch("SAVEPOINT mcport_statement")?;
        }
        let result = self.query(sql, params.values()?).map(|r| r.changed);
        if savepoint {
            if result.is_err() {
                self.execute_batch("ROLLBACK TO SAVEPOINT mcport_statement")?;
            }
            self.execute_batch("RELEASE SAVEPOINT mcport_statement")?;
        }
        result
    }
    pub fn query_row<P: Parameters, T>(
        &self,
        sql: &str,
        params: P,
        f: impl FnOnce(&Row) -> Result<T>,
    ) -> Result<T> {
        let rows = self.query(sql, params.values()?)?.rows;
        f(rows.first().ok_or(rusqlite::Error::QueryReturnedNoRows)?)
    }
    pub fn prepare<'a>(&'a self, sql: &str) -> Result<Statement<'a>> {
        Ok(Statement {
            db: self,
            sql: sql.into(),
        })
    }
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
    }
    pub fn transaction_with_behavior(
        &mut self,
        behavior: rusqlite::TransactionBehavior,
    ) -> Result<Transaction<'_>> {
        if self.transaction.get() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let begin = if self.is_postgres() {
            "BEGIN"
        } else {
            match behavior {
                rusqlite::TransactionBehavior::Deferred => "BEGIN DEFERRED",
                rusqlite::TransactionBehavior::Exclusive => "BEGIN EXCLUSIVE",
                _ => "BEGIN IMMEDIATE",
            }
        };
        self.execute_batch(begin)?;
        self.transaction.set(true);
        Ok(Transaction {
            db: self,
            committed: false,
        })
    }
}
pub struct Statement<'a> {
    db: &'a Connection,
    sql: String,
}
impl Statement<'_> {
    pub fn query_map<P: Parameters, T>(
        &mut self,
        params: P,
        mut f: impl FnMut(&Row) -> Result<T>,
    ) -> Result<std::vec::IntoIter<Result<T>>> {
        Ok(self
            .db
            .query(&self.sql, params.values()?)?
            .rows
            .iter()
            .map(&mut f)
            .collect::<Vec<_>>()
            .into_iter())
    }
}
pub struct Transaction<'a> {
    db: &'a Connection,
    committed: bool,
}
impl Deref for Transaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.db
    }
}
impl Transaction<'_> {
    pub fn commit(mut self) -> Result<()> {
        self.db.execute_batch("COMMIT")?;
        self.committed = true;
        self.db.transaction.set(false);
        Ok(())
    }
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.db.execute_batch("ROLLBACK");
            self.db.transaction.set(false);
        }
    }
}

fn pg_error(error: postgres::Error) -> rusqlite::Error {
    pg_error_at(error, "connection-or-schema", None)
}
fn pg_error_at(error: postgres::Error, phase: &str, sql: Option<&str>) -> rusqlite::Error {
    use sha2::Digest;
    let fingerprint = sql.map(|s| hex::encode(sha2::Sha256::digest(s.as_bytes())));
    tracing::error!(
        phase,
        sqlstate = error.code().map(|c| c.code()).unwrap_or("none"),
        connection_closed = error.is_closed(),
        sql_sha256 = fingerprint.as_deref().unwrap_or("none"),
        "PostgreSQL operation failed"
    );
    // PostgreSQL detail may contain a conflicting record or private query value.
    // Do not propagate its text into the application's storage error log.
    if error.code().is_some_and(|c| c.code().starts_with("23")) {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            None,
        )
    } else {
        rusqlite::Error::InvalidQuery
    }
}
#[derive(Debug)]
struct PgValue(Value);
impl postgres::types::ToSql for PgValue {
    fn to_sql(
        &self,
        ty: &postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>>
    {
        use postgres::types::{IsNull, Type};
        match &self.0 {
            Value::Null => Ok(IsNull::Yes),
            Value::Text(v) => postgres::types::ToSql::to_sql(v, ty, out),
            Value::Blob(v) => postgres::types::ToSql::to_sql(v, ty, out),
            Value::Real(v) => postgres::types::ToSql::to_sql(v, ty, out),
            Value::Integer(v) => match *ty {
                Type::INT4 => postgres::types::ToSql::to_sql(&i32::try_from(*v)?, ty, out),
                Type::INT2 => postgres::types::ToSql::to_sql(&i16::try_from(*v)?, ty, out),
                Type::BOOL => postgres::types::ToSql::to_sql(&(*v != 0), ty, out),
                _ => postgres::types::ToSql::to_sql(v, ty, out),
            },
        }
    }
    fn accepts(_: &postgres::types::Type) -> bool {
        true
    }
    postgres::types::to_sql_checked!();
}
fn pg_query(client: &mut postgres::Client, sql: &str, values: Vec<Value>) -> Result<Reply> {
    let statement = client
        .prepare(sql)
        .map_err(|e| pg_error_at(e, "prepare", Some(sql)))?;
    let values: Vec<_> = values.into_iter().map(PgValue).collect();
    let params: Vec<&(dyn postgres::types::ToSql + Sync)> = values.iter().map(|v| v as _).collect();
    if statement.columns().is_empty() {
        return Ok(Reply {
            rows: vec![],
            changed: client
                .execute(&statement, &params)
                .map_err(|e| pg_error_at(e, "execute", Some(sql)))? as usize,
        });
    }
    let rows = client
        .query(&statement, &params)
        .map_err(|e| pg_error_at(e, "query", Some(sql)))?
        .into_iter()
        .map(|row| {
            let values = row
                .columns()
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    use postgres::types::Type;
                    macro_rules! read {
                        ($t:ty, $map:expr) => {
                            row.try_get::<_, Option<$t>>(i)
                                .map(|v| v.map($map).unwrap_or(Value::Null))
                                .map_err(|e| pg_error_at(e, "decode", Some(sql)))
                        };
                    }
                    match *c.type_() {
                        Type::INT8 => read!(i64, Value::Integer),
                        Type::INT4 => read!(i32, |v| Value::Integer(v.into())),
                        Type::INT2 => read!(i16, |v| Value::Integer(v.into())),
                        Type::BOOL => read!(bool, |v| Value::Integer(i64::from(v))),
                        Type::BYTEA => read!(Vec<u8>, Value::Blob),
                        Type::FLOAT8 => read!(f64, Value::Real),
                        _ => read!(String, Value::Text),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Row { values })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Reply { rows, changed: 0 })
}

/// Translate only the store's small SQL dialect, preserving quoted strings and
/// repeated numbered bindings. No user SQL reaches this adapter.
fn translate(sql: &str) -> String {
    let ignore = sql.starts_with("INSERT OR IGNORE INTO ");
    let sql = sql.replacen("INSERT OR IGNORE INTO ", "INSERT INTO ", 1);
    let mut result = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote = false;
    let mut next = 1;
    while let Some(c) = chars.next() {
        if c == '\'' {
            quote = !quote;
            result.push(c);
        } else if c == '?' && !quote {
            let mut number = String::new();
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                number.push(chars.next().unwrap());
            }
            let index = number.parse::<usize>().unwrap_or(next);
            next = next.max(index + 1);
            result.push_str(&format!("${index}"));
        } else {
            result.push(c);
        }
    }
    if ignore {
        result.push_str(" ON CONFLICT DO NOTHING");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bindings_and_quoted_literals_are_preserved() {
        assert_eq!(
            translate("SELECT '?' FROM records WHERE id=?1 OR name=?1 OR kind=?2"),
            "SELECT '?' FROM records WHERE id=$1 OR name=$1 OR kind=$2"
        );
        assert_eq!(
            translate("INSERT OR IGNORE INTO public_ids(id) VALUES(?)"),
            "INSERT INTO public_ids(id) VALUES($1) ON CONFLICT DO NOTHING"
        );
    }
}

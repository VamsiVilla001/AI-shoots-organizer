//! `skwad-db-migrate` — copies an existing SQLite library into PostgreSQL.
//!
//! ```text
//! skwad-db-migrate --sqlite <media.db> --postgres <url> [--truncate] [--dry-run]
//! ```
//!
//! What it guarantees, and why each part is the way it is:
//!
//!   * **The source is opened read-only.** The old `media.db` is left exactly
//!     as it was, so a failed run costs nothing and the app can be pointed back
//!     at SQLite while the problem is worked out.
//!   * **Primary keys are preserved.** Every id is inserted explicitly rather
//!     than letting the identity column assign a new one, because ids leak out
//!     of the database: `thumbnail_path`, the sharded `face_cache/<id>.jpg`
//!     crops and the `skwadmedia://` URLs the webview holds all embed a media
//!     or face id. Renumbering would orphan every cached file on disk. The
//!     sequences are resynced at the end so the next insert continues cleanly.
//!   * **Tables are copied parent-first** and the whole run is one transaction,
//!     so foreign keys hold at every point and a failure rolls back to an empty
//!     database rather than a half-migrated one.
//!   * **Row counts are verified** per table before the transaction commits.
//!
//! It is deliberately a separate binary behind a feature flag: the app itself
//! never links SQLite, and this runs once per machine.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use postgres::types::{ToSql, Type};
use postgres::NoTls;
use rusqlite::OpenFlags;

/// Every table, in an order that satisfies the foreign keys, paired with the
/// column that carries its identity sequence (if it has one).
///
/// Order matters and is not alphabetical: `shoots` before `media` before
/// `faces`, `projects` before `project_collections` before its self-reference.
const TABLES: &[(&str, Option<&str>)] = &[
    ("shoots", Some("id")),
    ("people", Some("id")),
    ("media", Some("id")),
    ("clusters", Some("id")),
    ("faces", Some("id")),
    ("video_detections", Some("id")),
    ("video_sample_frames", None),
    ("albums", Some("id")),
    ("album_media", None),
    ("media_groups", Some("id")),
    ("media_group_items", None),
    ("jobs", Some("id")),
    ("exports", Some("id")),
    ("settings", None),
    ("app_log", Some("id")),
    ("library_mappings", None),
    ("sync_outbox", Some("id")),
    ("sync_conflicts", Some("id")),
    ("catalogue_revisions", None),
    ("imported_catalogues", None),
    ("processing_runs", Some("id")),
    ("processing_stage_runs", Some("id")),
    ("processing_resource_samples", Some("id")),
    ("projects", None),
    ("project_members", None),
    ("project_collections", None),
    ("project_collection_sources", None),
    ("roster_entries", Some("id")),
];

/// Rows per `INSERT`. Large enough that the round trips stop dominating, small
/// enough that the parameter count stays well under Postgres' 65535 per
/// statement even for `faces`, the widest table at 19 columns.
const BATCH: usize = 500;

struct Args {
    sqlite: PathBuf,
    postgres: String,
    truncate: bool,
    dry_run: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("\nmigration failed: {error}");
        eprintln!("The SQLite library was not modified, and PostgreSQL was rolled back.");
        std::process::exit(1);
    }
}

fn parse_args() -> Result<Args, String> {
    let mut sqlite = None;
    let mut postgres = std::env::var("SKWAD_DATABASE_URL").ok();
    let mut truncate = false;
    let mut dry_run = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sqlite" => sqlite = args.next().map(PathBuf::from),
            "--postgres" => postgres = args.next(),
            "--truncate" => truncate = true,
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!(
                    "skwad-db-migrate --sqlite <media.db> --postgres <url> [--truncate] [--dry-run]\n\n\
                     --truncate  empty the target tables first (required if it already holds rows)\n\
                     --dry-run   read and count everything, then roll back instead of committing"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }

    Ok(Args {
        sqlite: sqlite.ok_or("--sqlite <path to media.db> is required")?,
        postgres: postgres.ok_or("--postgres <url> is required (or set SKWAD_DATABASE_URL)")?,
        truncate,
        dry_run,
    })
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    let started = Instant::now();

    if !args.sqlite.is_file() {
        return Err(format!("no SQLite database at {}", args.sqlite.display()).into());
    }

    // Read-only, and without the "create if missing" flag, so a typo in the
    // path is an error rather than a new empty database.
    let source = rusqlite::Connection::open_with_flags(
        &args.sqlite,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let source_version: i32 = source.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    println!("source: {} (schema version {source_version})", args.sqlite.display());
    if source_version != 13 {
        eprintln!(
            "warning: expected SQLite schema version 13, found {source_version}. \
             Open the library in the previous app build once to upgrade it first."
        );
    }

    let mut target = postgres::Client::connect(&args.postgres, NoTls)?;
    println!("target: {}", redact(&args.postgres));

    // The schema has to exist already — `Database::connect` creates it on first
    // launch, and running the app once before migrating is the documented order.
    let schema_ready: bool = target
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables
                             WHERE table_schema = current_schema() AND table_name = 'shoots')",
            &[],
        )?
        .get(0);
    if !schema_ready {
        return Err("the target has no SKWAD schema — start the app once against it first".into());
    }

    let mut tx = target.transaction()?;

    if args.truncate {
        // One statement so the cascade cannot leave a gap between tables, and
        // `RESTART IDENTITY` so a re-run starts from the same clean state.
        let all = TABLES.iter().map(|(t, _)| *t).collect::<Vec<_>>().join(", ");
        tx.batch_execute(&format!("TRUNCATE {all} RESTART IDENTITY CASCADE"))?;
        println!("truncated {} tables", TABLES.len());
    } else {
        let occupied: i64 = tx.query_one("SELECT COUNT(*) FROM shoots", &[])?.get(0);
        if occupied > 0 {
            return Err(format!("the target already holds {occupied} shoots. Pass --truncate to replace them.").into());
        }
    }

    // Deferring lets the copy run parent-first without the self-referencing
    // tables (project_collections.parent_id) needing a topological sort of rows.
    tx.batch_execute("SET CONSTRAINTS ALL DEFERRED")?;

    let mut totals: Vec<(&str, i64, i64)> = Vec::new();
    for (table, _) in TABLES {
        let columns = target_columns(&mut tx, table)?;
        let copied = copy_table(&source, &mut tx, table, &columns)?;
        let landed: i64 = tx.query_one(&format!("SELECT COUNT(*) FROM {table}"), &[])?.get(0);
        println!("  {table:<30} {copied:>9} rows");
        totals.push((table, copied, landed));
    }

    // Verify before committing, so a mismatch rolls the whole thing back.
    for (table, copied, landed) in &totals {
        if copied != landed {
            return Err(format!("{table}: read {copied} rows from SQLite but {landed} landed in PostgreSQL").into());
        }
    }

    for (table, identity) in TABLES {
        if let Some(column) = identity {
            resync_sequence(&mut tx, table, column)?;
        }
    }

    let rows: i64 = totals.iter().map(|(_, copied, _)| copied).sum();
    if args.dry_run {
        tx.rollback()?;
        println!("\ndry run: {rows} rows verified, nothing committed.");
    } else {
        tx.commit()?;
        println!("\nmigrated {rows} rows in {:.1}s.", started.elapsed().as_secs_f64());
        println!("The SQLite library at {} was not modified.", args.sqlite.display());
    }

    Ok(())
}

/// The columns the Postgres table actually has, with their types, in ordinal
/// order. Driving the copy from the *target* rather than the source means a
/// column SQLite has and Postgres dropped is skipped rather than fatal.
fn target_columns(tx: &mut postgres::Transaction<'_>, table: &str) -> Result<Vec<Column>, Box<dyn std::error::Error>> {
    let rows = tx.query(
        "SELECT column_name, udt_name FROM information_schema.columns
          WHERE table_schema = current_schema() AND table_name = $1
          ORDER BY ordinal_position",
        &[&table],
    )?;
    Ok(rows
        .iter()
        .map(|row| Column {
            name: row.get::<_, String>(0),
            kind: Kind::of(&row.get::<_, String>(1)),
        })
        .collect())
}

struct Column {
    name: String,
    kind: Kind,
}

/// The five shapes the schema actually stores, which is all the conversion
/// needs to distinguish. SQLite is dynamically typed and rusqlite hands back
/// whatever was written, so each value is coerced to what the Postgres column
/// declares rather than to what the SQLite value happens to be.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Float,
    Text,
    Bytes,
}

impl Kind {
    fn of(udt: &str) -> Self {
        match udt {
            "int2" | "int4" | "int8" => Kind::Int,
            "float4" | "float8" | "numeric" => Kind::Float,
            "bytea" => Kind::Bytes,
            _ => Kind::Text,
        }
    }
}

/// A value on its way from SQLite to Postgres, already coerced to the target
/// column's shape.
#[derive(Debug)]
enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl ToSql for Value {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut postgres::types::private::BytesMut,
    ) -> Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            Value::Null => Ok(postgres::types::IsNull::Yes),
            Value::Int(v) => v.to_sql(ty, out),
            Value::Float(v) => v.to_sql(ty, out),
            Value::Text(v) => v.to_sql(ty, out),
            Value::Bytes(v) => v.to_sql(ty, out),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    postgres::types::to_sql_checked!();
}

fn copy_table(
    source: &rusqlite::Connection,
    tx: &mut postgres::Transaction<'_>,
    table: &str,
    columns: &[Column],
) -> Result<i64, Box<dyn std::error::Error>> {
    // Only copy columns both sides have; the Postgres baseline is the SQLite
    // schema at version 13, so in practice this is all of them.
    let source_columns = sqlite_columns(source, table)?;
    let shared: Vec<&Column> = columns
        .iter()
        .filter(|c| source_columns.contains_key(&c.name))
        .collect();
    if shared.is_empty() {
        return Ok(0);
    }

    let names = shared
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = source.prepare(&format!("SELECT {names} FROM \"{table}\""))?;
    let mut sqlite_rows = statement.query([])?;

    let mut pending: Vec<Value> = Vec::new();
    let mut in_batch = 0usize;
    let mut total = 0i64;

    while let Some(row) = sqlite_rows.next()? {
        for (index, column) in shared.iter().enumerate() {
            pending.push(convert(row.get_ref(index)?, column.kind));
        }
        in_batch += 1;
        total += 1;

        if in_batch == BATCH {
            flush(tx, table, &names, shared.len(), in_batch, &pending)?;
            pending.clear();
            in_batch = 0;
        }
    }

    if in_batch > 0 {
        flush(tx, table, &names, shared.len(), in_batch, &pending)?;
    }

    Ok(total)
}

/// Coerces one SQLite value to the target column's shape.
///
/// The interesting cases are the lossy ones SQLite allowed and Postgres will
/// not: an `INTEGER` sitting in a column Postgres declares `DOUBLE PRECISION`
/// (widths written as ints, durations written as whole seconds), and a `REAL`
/// in a `BIGINT` column. Both are real occurrences in shipped libraries.
fn convert(value: rusqlite::types::ValueRef<'_>, kind: Kind) -> Value {
    use rusqlite::types::ValueRef;
    match (value, kind) {
        (ValueRef::Null, _) => Value::Null,

        (ValueRef::Integer(v), Kind::Int) => Value::Int(v),
        (ValueRef::Integer(v), Kind::Float) => Value::Float(v as f64),
        (ValueRef::Integer(v), Kind::Text) => Value::Text(v.to_string()),
        (ValueRef::Integer(v), Kind::Bytes) => Value::Bytes(v.to_le_bytes().to_vec()),

        (ValueRef::Real(v), Kind::Float) => Value::Float(v),
        (ValueRef::Real(v), Kind::Int) => Value::Int(v as i64),
        (ValueRef::Real(v), Kind::Text) => Value::Text(v.to_string()),
        (ValueRef::Real(v), Kind::Bytes) => Value::Bytes(v.to_le_bytes().to_vec()),

        (ValueRef::Text(bytes), Kind::Text) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        (ValueRef::Text(bytes), Kind::Bytes) => Value::Bytes(bytes.to_vec()),
        (ValueRef::Text(bytes), Kind::Int) => String::from_utf8_lossy(bytes)
            .trim()
            .parse()
            .map(Value::Int)
            .unwrap_or(Value::Null),
        (ValueRef::Text(bytes), Kind::Float) => String::from_utf8_lossy(bytes)
            .trim()
            .parse()
            .map(Value::Float)
            .unwrap_or(Value::Null),

        // Embeddings and landmarks: raw little-endian f32, copied byte for byte.
        (ValueRef::Blob(bytes), Kind::Bytes) => Value::Bytes(bytes.to_vec()),
        (ValueRef::Blob(bytes), Kind::Text) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        (ValueRef::Blob(_), _) => Value::Null,
    }
}

fn flush(
    tx: &mut postgres::Transaction<'_>,
    table: &str,
    names: &str,
    width: usize,
    rows: usize,
    values: &[Value],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut placeholders = String::new();
    for row in 0..rows {
        if row > 0 {
            placeholders.push(',');
        }
        placeholders.push('(');
        for column in 0..width {
            if column > 0 {
                placeholders.push(',');
            }
            placeholders.push_str(&format!("${}", row * width + column + 1));
        }
        placeholders.push(')');
    }

    let params: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v as &(dyn ToSql + Sync)).collect();
    tx.execute(
        &format!("INSERT INTO \"{table}\" ({names}) VALUES {placeholders}"),
        &params,
    )?;
    Ok(())
}

fn sqlite_columns(
    source: &rusqlite::Connection,
    table: &str,
) -> Result<HashMap<String, ()>, Box<dyn std::error::Error>> {
    let mut statement = source.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    let mut out = HashMap::new();
    for name in names {
        out.insert(name?, ());
    }
    Ok(out)
}

/// Points the identity sequence past the highest id copied in, so the first
/// row the app inserts afterwards does not collide with a migrated one.
fn resync_sequence(
    tx: &mut postgres::Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tx.execute(
        &format!(
            "SELECT setval(
                 pg_get_serial_sequence('{table}', '{column}'),
                 COALESCE((SELECT MAX({column}) FROM {table}), 0) + 1,
                 false
             )"
        ),
        &[],
    )?;
    Ok(())
}

/// Keeps the password out of the console and any log the operator pastes into
/// a bug report.
fn redact(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            let credentials = &url[scheme + 3..at];
            let user = credentials.split(':').next().unwrap_or("");
            format!("{}{}:***{}", &url[..scheme + 3], user, &url[at..])
        }
        _ => url.to_string(),
    }
}

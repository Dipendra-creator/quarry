//! Microsoft SQL Server and Azure SQL over TDS (`tiberius`), on one connection
//! behind a mutex like the MySQL driver.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use futures_util::TryStreamExt as _;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, QueryItem, ToSql};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};

use super::Driver;
use crate::model::{ConnectionConfig, DriverKind, SslMode};
use crate::query::{QueryResult, ResultColumn};
use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
use crate::sql::{self, Dialect};
use crate::value::Value;

type Conn = Client<Compat<TcpStream>>;

pub struct MsDriver {
    client: Mutex<Conn>,
}

const D: Dialect = Dialect(DriverKind::SqlServer);

/// Column type as SSMS spells it, from `sys.columns c` and `sys.types ty`.
const TYPE_EXPR: &str = "CASE \
    WHEN ty.name IN ('varchar', 'char', 'varbinary', 'binary') \
        THEN ty.name + '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar(10)) END + ')' \
    WHEN ty.name IN ('nvarchar', 'nchar') \
        THEN ty.name + '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar(10)) END + ')' \
    WHEN ty.name IN ('decimal', 'numeric') \
        THEN ty.name + '(' + CAST(c.precision AS varchar(10)) + ',' + CAST(c.scale AS varchar(10)) + ')' \
    WHEN ty.name IN ('datetime2', 'time', 'datetimeoffset') \
        THEN ty.name + '(' + CAST(c.scale AS varchar(10)) + ')' \
    ELSE ty.name END";

/// Schemas owned by the system: dbo's siblings guest, INFORMATION_SCHEMA and
/// sys, and one per fixed database role (db_owner, db_datareader, …).
const SYSTEM_SCHEMA_IDS: &str = "s.schema_id IN (2, 3, 4) OR s.schema_id BETWEEN 16384 AND 16399";

impl MsDriver {
    pub async fn connect(cfg: &ConnectionConfig, database: Option<&str>) -> Result<Self> {
        let mut config = Config::new();
        // "host\INSTANCE" names a named instance; its TCP port goes in the port field.
        let host = cfg.host.split('\\').next().unwrap_or(&cfg.host).trim();
        config.host(if host.is_empty() { "127.0.0.1" } else { host });
        config.port(cfg.port_or_default());
        let db = database.unwrap_or(&cfg.database);
        if !db.is_empty() {
            config.database(db);
        }
        config.application_name("Quarry");
        config.authentication(AuthMethod::sql_server(&cfg.user, cfg.password.as_deref().unwrap_or_default()));
        match cfg.ssl_mode {
            // SQL Server always encrypts the login; "Disable" leaves the rest in plain text.
            SslMode::Disable => config.encryption(EncryptionLevel::Off),
            SslMode::Prefer => config.encryption(EncryptionLevel::On),
            _ => config.encryption(EncryptionLevel::Required),
        }
        // Servers present self-signed certificates unless one was installed.
        if !matches!(cfg.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull) {
            config.trust_cert();
        }
        let client = match Self::open(config.clone()).await {
            // Azure SQL gateways redirect to the node that hosts the database.
            Err(tiberius::error::Error::Routing { host, port }) => {
                config.host(&host);
                config.port(port);
                Self::open(config).await
            }
            other => other,
        }
        .context("could not connect to SQL Server")?;
        Ok(Self { client: Mutex::new(client) })
    }

    async fn open(config: Config) -> tiberius::Result<Conn> {
        let tcp = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(config.get_addr()))
            .await
            .map_err(|_| tiberius::error::Error::Io { kind: std::io::ErrorKind::TimedOut, message: "connection timed out".into() })??;
        tcp.set_nodelay(true)?;
        Client::connect(config, tcp.compat_write()).await
    }

    /// Rows of the first result set of a parameterized query (`@P1`, `@P2`, …).
    async fn rows(&self, query: &str, params: &[&dyn ToSql]) -> Result<Vec<Vec<Value>>> {
        let mut client = self.client.lock().await;
        let rows = client.query(query, params).await?.into_first_result().await?;
        Ok(rows.into_iter().map(|r| r.into_iter().map(cell).collect()).collect())
    }

    async fn one(&self, query: &str, params: &[&dyn ToSql]) -> Result<Value> {
        Ok(self.rows(query, params).await?.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or(Value::Null))
    }

    async fn database_or_current(&self, schema: Option<&str>) -> String {
        match schema {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => self.default_schema().await.ok().flatten().unwrap_or_else(|| "dbo".into()),
        }
    }

    /// `[schema].[name]`, the form OBJECT_ID takes.
    async fn object_name(&self, t: &TableRef) -> String {
        let schema = self.database_or_current(t.schema.as_deref()).await;
        D.table(&TableRef::new(Some(&schema), &t.name))
    }

    async fn table_ddl(&self, t: &TableRef) -> Result<String> {
        let obj = self.object_name(t).await;
        let schema = self.database_or_current(t.schema.as_deref()).await;
        let full = TableRef::new(Some(&schema), &t.name);
        let s = self.structure(&full).await?;
        let identity = self
            .rows("SELECT CAST(IDENT_SEED(@P1) AS bigint), CAST(IDENT_INCR(@P1) AS bigint)", &[&obj])
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let computed: Vec<Vec<Value>> = self
            .rows("SELECT name, definition FROM sys.computed_columns WHERE object_id = OBJECT_ID(@P1)", &[&obj])
            .await?;
        let mut lines: Vec<String> = s
            .columns
            .iter()
            .map(|c| {
                if let Some(def) = computed.iter().find(|r| text(&r[0]) == c.name) {
                    return format!("    {} AS {}", D.quote_ident(&c.name), text(&def[1]));
                }
                let mut l = format!("    {} {}", D.quote_ident(&c.name), c.data_type);
                if c.extra.as_deref() == Some("identity") {
                    let n = |i: usize| identity.get(i).map(text).filter(|s| !s.is_empty()).unwrap_or_else(|| "1".into());
                    l.push_str(&format!(" IDENTITY({},{})", n(0), n(1)));
                }
                l.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
                if let Some(def) = &c.default {
                    l.push_str(&format!(" DEFAULT {def}"));
                }
                l
            })
            .collect();
        let cols = |names: &[String]| names.iter().map(|c| D.quote_ident(c)).collect::<Vec<_>>().join(", ");
        for ix in s.indexes.iter().filter(|i| i.primary) {
            lines.push(format!(
                "    CONSTRAINT {} PRIMARY KEY {} ({})",
                D.quote_ident(&ix.name),
                ix.method.as_deref().unwrap_or("clustered").to_uppercase(),
                cols(&ix.columns)
            ));
        }
        for fk in &s.foreign_keys {
            let mut l = format!(
                "    CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
                D.quote_ident(&fk.name),
                cols(&fk.columns),
                D.table(&TableRef::new(fk.ref_schema.as_deref(), &fk.ref_table)),
                cols(&fk.ref_columns)
            );
            for (verb, action) in [("UPDATE", &fk.on_update), ("DELETE", &fk.on_delete)] {
                if action != "NO ACTION" {
                    l.push_str(&format!(" ON {verb} {action}"));
                }
            }
            lines.push(l);
        }
        let mut ddl = format!("CREATE TABLE {} (\n{}\n);", D.table(&full), lines.join(",\n"));
        for ix in s.indexes.iter().filter(|i| !i.primary) {
            ddl.push_str(&format!(
                "\nCREATE {}{} INDEX {} ON {} ({});",
                if ix.unique { "UNIQUE " } else { "" },
                ix.method.as_deref().unwrap_or("nonclustered").to_uppercase(),
                D.quote_ident(&ix.name),
                D.table(&full),
                cols(&ix.columns)
            ));
        }
        Ok(ddl)
    }
}

/// Any SQL Server value as a cell.
fn cell(d: ColumnData<'static>) -> Value {
    fn fmt<T: for<'a> FromSql<'a>>(d: &ColumnData<'static>, f: impl Fn(T) -> String) -> Value {
        match T::from_sql(d) {
            Ok(Some(v)) => Value::Text(f(v)),
            _ => Value::Null,
        }
    }
    match d {
        ColumnData::U8(Some(v)) => Value::Int(v.into()),
        ColumnData::I16(Some(v)) => Value::Int(v.into()),
        ColumnData::I32(Some(v)) => Value::Int(v.into()),
        ColumnData::I64(Some(v)) => Value::Int(v),
        ColumnData::F32(Some(v)) => Value::Float(f64::from(v)),
        ColumnData::F64(Some(v)) => Value::Float(v),
        ColumnData::Bit(Some(b)) => Value::Bool(b),
        ColumnData::String(Some(s)) => Value::Text(s.into_owned()),
        // SSMS spells GUIDs in upper case.
        ColumnData::Guid(Some(g)) => Value::Text(g.to_string().to_uppercase()),
        ColumnData::Binary(Some(b)) => Value::Bytes(b.into_owned()),
        ColumnData::Numeric(Some(n)) => Value::Text(n.to_string()),
        ColumnData::Xml(Some(x)) => Value::Text(x.to_string()),
        // As SSMS shows them: datetime to the millisecond, smalldatetime to the
        // second, and the 100 ns types without trailing zeros.
        ColumnData::DateTime(Some(_)) => fmt(&d, |v: NaiveDateTime| v.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
        ColumnData::SmallDateTime(Some(_)) => fmt(&d, |v: NaiveDateTime| v.format("%Y-%m-%d %H:%M:%S").to_string()),
        ColumnData::DateTime2(Some(_)) => fmt(&d, |v: NaiveDateTime| fraction(v.format("%Y-%m-%d %H:%M:%S%.9f").to_string())),
        ColumnData::Date(Some(_)) => fmt(&d, |v: NaiveDate| v.format("%Y-%m-%d").to_string()),
        ColumnData::Time(Some(_)) => fmt(&d, |v: NaiveTime| fraction(v.format("%H:%M:%S%.9f").to_string())),
        ColumnData::DateTimeOffset(Some(_)) => fmt(&d, |v: DateTime<FixedOffset>| {
            format!("{} {}", fraction(v.format("%Y-%m-%d %H:%M:%S%.9f").to_string()), v.format("%:z"))
        }),
        _ => Value::Null,
    }
}

/// `…:36.581391300` → `…:36.5813913`, `…:36.000000000` → `…:36`.
fn fraction(s: String) -> String {
    match s.rsplit_once('.') {
        Some((head, frac)) => {
            let frac = frac.trim_end_matches('0');
            if frac.is_empty() { head.to_string() } else { format!("{head}.{frac}") }
        }
        None => s,
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        v => v.display(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        _ => false,
    }
}

fn opt(v: &Value) -> Option<String> {
    Some(text(v)).filter(|s| !s.is_empty())
}

/// `((0))` → `(0)`, `(getdate())` → `getdate()`: SQL Server wraps stored
/// default expressions in one extra pair of parentheses.
fn unwrap_default(def: String) -> String {
    let inner = def.strip_prefix('(').and_then(|d| d.strip_suffix(')'));
    match inner {
        Some(inner) if balanced(inner) => inner.to_string(),
        _ => def,
    }
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn type_name(t: ColumnType) -> &'static str {
    use ColumnType::*;
    match t {
        Bit | Bitn => "bit",
        Int1 => "tinyint",
        Int2 => "smallint",
        Int4 | Intn => "int",
        Int8 => "bigint",
        Float4 => "real",
        Float8 | Floatn => "float",
        Money | Money4 => "money",
        Datetime | Datetimen => "datetime",
        Datetime4 => "smalldatetime",
        Daten => "date",
        Timen => "time",
        Datetime2 => "datetime2",
        DatetimeOffsetn => "datetimeoffset",
        Decimaln | Numericn => "decimal",
        Guid => "uniqueidentifier",
        BigVarBin => "varbinary",
        BigBinary => "binary",
        BigVarChar => "varchar",
        BigChar => "char",
        NVarchar => "nvarchar",
        NChar => "nchar",
        Xml => "xml",
        Text => "text",
        NText => "ntext",
        Image => "image",
        SSVariant => "sql_variant",
        _ => "",
    }
}

/// Drains a batch and returns its first (or last) result set.
async fn collect(client: &mut Conn, batch: &str, last: bool) -> Result<QueryResult> {
    let mut stream = client.simple_query(batch).await?;
    let mut sets: Vec<QueryResult> = Vec::new();
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(meta) => sets.push(QueryResult {
                statement: batch.to_string(),
                columns: meta
                    .columns()
                    .iter()
                    .map(|c| ResultColumn { name: c.name().to_string(), type_name: type_name(c.column_type()).into() })
                    .collect(),
                ..Default::default()
            }),
            QueryItem::Row(row) => {
                if let Some(set) = sets.last_mut() {
                    set.rows.push(row.into_iter().map(cell).collect());
                }
            }
        }
    }
    let set = if last { sets.pop() } else { sets.into_iter().next() };
    Ok(set.unwrap_or_else(|| QueryResult { statement: batch.to_string(), ..Default::default() }))
}

async fn run(client: &mut Conn, statement: &str) -> Result<()> {
    client.simple_query(statement).await?.into_results().await?;
    Ok(())
}

#[async_trait]
impl Driver for MsDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::SqlServer
    }

    async fn server_version(&self) -> Result<String> {
        let r = self
            .rows(
                "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), CAST(SERVERPROPERTY('EngineEdition') AS int)",
                &[],
            )
            .await?;
        let row = r.into_iter().next().unwrap_or_default();
        let version = row.first().map(text).unwrap_or_default();
        if row.get(1) == Some(&Value::Int(5)) {
            return Ok(format!("(Azure SQL) {version}"));
        }
        let year = match version.split('.').next().unwrap_or_default() {
            "11" => "2012",
            "12" => "2014",
            "13" => "2016",
            "14" => "2017",
            "15" => "2019",
            "16" => "2022",
            "17" => "2025",
            _ => return Ok(version),
        };
        Ok(year.to_string())
    }

    async fn current_database(&self) -> Result<String> {
        Ok(text(&self.one("SELECT DB_NAME()", &[]).await?))
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        Ok(self
            .rows("SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 ORDER BY name", &[])
            .await?
            .iter()
            .map(|r| text(&r[0]))
            .collect())
    }

    async fn use_database(&self, name: &str) -> Result<()> {
        let mut client = self.client.lock().await;
        run(&mut client, &format!("USE {}", D.quote_ident(name))).await
    }

    async fn default_schema(&self) -> Result<Option<String>> {
        Ok(opt(&self.one("SELECT SCHEMA_NAME()", &[]).await?))
    }

    async fn list_schemas(&self, include_system: bool) -> Result<Vec<String>> {
        let filter = if include_system { String::new() } else { format!(" WHERE NOT ({SYSTEM_SCHEMA_IDS})") };
        Ok(self
            .rows(&format!("SELECT s.name FROM sys.schemas s{filter} ORDER BY s.name"), &[])
            .await?
            .iter()
            .map(|r| text(&r[0]))
            .collect())
    }

    async fn list_items(&self, schema: Option<&str>) -> Result<Vec<Item>> {
        let schema = self.database_or_current(schema).await;
        let rows = self
            .rows(
                "SELECT o.name, RTRIM(o.type), CAST(ep.value AS nvarchar(max)) FROM sys.objects o \
                 LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id \
                   AND ep.minor_id = 0 AND ep.name = N'MS_Description' \
                 WHERE o.schema_id = SCHEMA_ID(@P1) AND o.is_ms_shipped = 0 \
                   AND o.type IN ('U', 'V', 'P', 'PC', 'FN', 'IF', 'TF', 'FS', 'FT', 'SO') \
                 ORDER BY o.name",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| Item {
                table: TableRef::new(Some(&schema), &text(&r[0])),
                kind: match text(&r[1]).as_str() {
                    "U" => ItemKind::Table,
                    "V" => ItemKind::View,
                    "P" | "PC" => ItemKind::Procedure,
                    "SO" => ItemKind::Sequence,
                    _ => ItemKind::Function,
                },
                comment: opt(&r[2]),
            })
            .collect())
    }

    async fn structure(&self, t: &TableRef) -> Result<TableStructure> {
        let obj = self.object_name(t).await;
        let columns = self
            .rows(
                &format!(
                    "SELECT c.name, {TYPE_EXPR}, c.is_nullable, OBJECT_DEFINITION(c.default_object_id), \
                     CAST(ep.value AS nvarchar(max)), \
                     CASE WHEN EXISTS (SELECT 1 FROM sys.index_columns ic JOIN sys.indexes i \
                       ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
                       WHERE i.is_primary_key = 1 AND ic.object_id = c.object_id AND ic.column_id = c.column_id) \
                       THEN 1 ELSE 0 END, \
                     CASE WHEN c.is_identity = 1 THEN 'identity' WHEN c.is_computed = 1 THEN 'computed' END \
                     FROM sys.columns c JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
                     LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = c.object_id \
                       AND ep.minor_id = c.column_id AND ep.name = N'MS_Description' \
                     WHERE c.object_id = OBJECT_ID(@P1) ORDER BY c.column_id"
                ),
                &[&obj],
            )
            .await
            .with_context(|| format!("could not read columns of {obj}"))?
            .iter()
            .map(|r| ColumnInfo {
                name: text(&r[0]),
                data_type: text(&r[1]),
                nullable: truthy(&r[2]),
                default: opt(&r[3]).map(unwrap_default),
                comment: opt(&r[4]),
                primary_key: truthy(&r[5]),
                extra: opt(&r[6]),
                references: None,
            })
            .collect();
        let mut indexes: Vec<IndexInfo> = Vec::new();
        for r in self
            .rows(
                "SELECT i.name, i.is_unique, i.is_primary_key, LOWER(i.type_desc), c.name \
                 FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
                 JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                 WHERE i.object_id = OBJECT_ID(@P1) AND i.name IS NOT NULL AND ic.is_included_column = 0 \
                 ORDER BY i.name, ic.key_ordinal",
                &[&obj],
            )
            .await?
        {
            let name = text(&r[0]);
            let column = text(&r[4]);
            match indexes.iter_mut().find(|i| i.name == name) {
                Some(ix) => ix.columns.push(column),
                None => indexes.push(IndexInfo {
                    unique: truthy(&r[1]),
                    primary: truthy(&r[2]),
                    method: opt(&r[3]),
                    columns: vec![column],
                    definition: None,
                    name,
                }),
            }
        }
        let mut foreign_keys: Vec<ForeignKeyInfo> = Vec::new();
        for r in self
            .rows(
                "SELECT fk.name, pc.name, SCHEMA_NAME(rt.schema_id), rt.name, rc.name, \
                 fk.update_referential_action_desc, fk.delete_referential_action_desc \
                 FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
                 JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
                 JOIN sys.objects rt ON rt.object_id = fkc.referenced_object_id \
                 JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
                 WHERE fk.parent_object_id = OBJECT_ID(@P1) ORDER BY fk.name, fkc.constraint_column_id",
                &[&obj],
            )
            .await?
        {
            let name = text(&r[0]);
            let (col, ref_col) = (text(&r[1]), text(&r[4]));
            match foreign_keys.iter_mut().find(|f| f.name == name) {
                Some(fk) => {
                    fk.columns.push(col);
                    fk.ref_columns.push(ref_col);
                }
                None => foreign_keys.push(ForeignKeyInfo {
                    name,
                    columns: vec![col],
                    ref_schema: opt(&r[2]),
                    ref_table: text(&r[3]),
                    ref_columns: vec![ref_col],
                    on_update: text(&r[5]).replace('_', " "),
                    on_delete: text(&r[6]).replace('_', " "),
                }),
            }
        }
        let mut s = TableStructure { table: t.clone(), columns, indexes, foreign_keys };
        s.link_foreign_keys();
        Ok(s)
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let obj = self.object_name(&item.table).await;
        match item.kind {
            ItemKind::Table => self.table_ddl(&item.table).await,
            ItemKind::Sequence => {
                let r = self
                    .rows(
                        "SELECT TYPE_NAME(user_type_id), CAST(start_value AS nvarchar(64)), CAST(increment AS nvarchar(64)), \
                         CAST(minimum_value AS nvarchar(64)), CAST(maximum_value AS nvarchar(64)), is_cycling \
                         FROM sys.sequences WHERE object_id = OBJECT_ID(@P1)",
                        &[&obj],
                    )
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("sequence {obj} not found"))?;
                Ok(format!(
                    "CREATE SEQUENCE {obj} AS {} START WITH {} INCREMENT BY {} MINVALUE {} MAXVALUE {}{};",
                    text(&r[0]),
                    text(&r[1]),
                    text(&r[2]),
                    text(&r[3]),
                    text(&r[4]),
                    if truthy(&r[5]) { " CYCLE" } else { " NO CYCLE" }
                ))
            }
            _ => {
                let def = self.one("SELECT OBJECT_DEFINITION(OBJECT_ID(@P1))", &[&obj]).await?;
                opt(&def).map(|d| d.trim().to_string()).ok_or_else(|| anyhow!("the definition of {obj} is not available (encrypted, or no permission)"))
            }
        }
    }

    async fn all_columns(&self, schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        let schema = self.database_or_current(schema).await;
        Ok(self
            .rows(
                &format!(
                    "SELECT o.name, c.name, {TYPE_EXPR} FROM sys.columns c \
                     JOIN sys.objects o ON o.object_id = c.object_id \
                     JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
                     WHERE o.schema_id = SCHEMA_ID(@P1) AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
                     ORDER BY o.name, c.column_id"
                ),
                &[&schema],
            )
            .await?
            .iter()
            .map(|r| (text(&r[0]), text(&r[1]), text(&r[2])))
            .collect())
    }

    async fn execute(&self, statement: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let mut client = self.client.lock().await;
        let kw = sql::first_keyword(statement);
        let returns_rows = sql::top_level_words(statement).iter().any(|w| w == "OUTPUT");
        let mut result = if matches!(kw.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE") && !returns_rows {
            // The row count only comes back through an execute.
            let done = client.execute(statement, &[]).await?;
            QueryResult { statement: statement.to_string(), rows_affected: Some(done.total()), ..Default::default() }
        } else {
            collect(&mut client, statement, false).await?
        };
        result.duration_ms = start.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        let mut client = self.client.lock().await;
        run(&mut client, "SET XACT_ABORT ON; BEGIN TRANSACTION").await?;
        for s in statements {
            if let Err(e) = run(&mut client, s).await {
                let _ = run(&mut client, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION; SET XACT_ABORT OFF").await;
                return Err(e.context(s.clone()));
            }
        }
        run(&mut client, "COMMIT TRANSACTION; SET XACT_ABORT OFF").await
    }

    async fn explain(&self, statement: &str, analyze: bool) -> Result<Option<QueryResult>> {
        // SHOWPLAN and STATISTICS PROFILE are session settings and must each be
        // the only statement of their batch, so the lock is held across all three.
        let (on, off) = if analyze {
            ("SET STATISTICS PROFILE ON", "SET STATISTICS PROFILE OFF")
        } else {
            ("SET SHOWPLAN_TEXT ON", "SET SHOWPLAN_TEXT OFF")
        };
        let mut client = self.client.lock().await;
        run(&mut client, on).await?;
        let result = collect(&mut client, statement, true).await;
        run(&mut client, off).await?;
        let mut result = result?;
        result.statement = statement.to_string();
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_lose_trailing_zeros() {
        assert_eq!(fraction("2026-11-27 10:46:36.581391300".into()), "2026-11-27 10:46:36.5813913");
        assert_eq!(fraction("10:46:36.000000000".into()), "10:46:36");
    }

    #[test]
    fn defaults_lose_their_outer_parentheses() {
        assert_eq!(unwrap_default("((0))".into()), "(0)");
        assert_eq!(unwrap_default("(getdate())".into()), "getdate()");
        assert_eq!(unwrap_default("(N'x')".into()), "N'x'");
        assert_eq!(unwrap_default("(1)+(2)".into()), "(1)+(2)");
    }
}

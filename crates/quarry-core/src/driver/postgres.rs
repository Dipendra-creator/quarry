use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, SimpleQueryMessage};

use super::Driver;
use crate::model::{ConnectionConfig, DriverKind, SslMode};
use crate::query::{QueryResult, ResultColumn};
use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
use crate::sql::Dialect;
use crate::value::Value;

pub struct PgDriver {
    client: Client,
}

const SYSTEM_SCHEMAS: [&str; 2] = ["pg_catalog", "information_schema"];

/// `regclass` expression for `$1`.`$2`.
const REL: &str = "(quote_ident($1::text) || '.' || quote_ident($2::text))::regclass";

impl PgDriver {
    pub async fn connect(cfg: &ConnectionConfig, database: Option<&str>) -> Result<Self> {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&cfg.host)
            .port(cfg.port_or_default())
            .user(&cfg.user)
            .application_name("Quarry")
            .connect_timeout(Duration::from_secs(15));
        let db = database.unwrap_or(&cfg.database);
        if !db.is_empty() {
            pg.dbname(db);
        }
        if let Some(pw) = cfg.password.as_deref().filter(|p| !p.is_empty()) {
            pg.password(pw);
        }
        pg.ssl_mode(match cfg.ssl_mode {
            SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            _ => tokio_postgres::config::SslMode::Require,
        });
        let verify = matches!(cfg.ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull);
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(!verify)
            .danger_accept_invalid_hostnames(cfg.ssl_mode != SslMode::VerifyFull)
            .build()?;
        let (client, connection) = pg
            .connect(postgres_native_tls::MakeTlsConnector::new(tls))
            .await
            .context("could not connect to PostgreSQL")?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("postgres connection closed: {e}");
            }
        });
        Ok(Self { client })
    }

    async fn strings(&self, sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Vec<String>> {
        Ok(self
            .client
            .query(sql, params)
            .await?
            .iter()
            .map(|r| r.get::<_, String>(0))
            .collect())
    }

    async fn functions(&self, schema: &str) -> Result<Vec<Item>> {
        let rows = match self
            .client
            .query(
                "SELECT DISTINCT p.proname::text, (p.prokind = 'p') FROM pg_proc p \
                 JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = $1 AND p.prokind IN ('f', 'p') ORDER BY 1",
                &[&schema],
            )
            .await
        {
            Ok(rows) => rows,
            // Before PostgreSQL 11 there is no prokind.
            Err(_) => {
                self.client
                    .query(
                        "SELECT DISTINCT p.proname::text, false FROM pg_proc p \
                         JOIN pg_namespace n ON n.oid = p.pronamespace \
                         WHERE n.nspname = $1 AND NOT p.proisagg ORDER BY 1",
                        &[&schema],
                    )
                    .await?
            }
        };
        Ok(rows
            .iter()
            .map(|r| Item {
                table: TableRef::new(Some(schema), r.get::<_, &str>(0)),
                kind: if r.get::<_, bool>(1) { ItemKind::Procedure } else { ItemKind::Function },
                comment: None,
            })
            .collect())
    }

    async fn table_ddl(&self, t: &TableRef) -> Result<String> {
        let d = Dialect(DriverKind::Postgres);
        let schema = t.schema.as_deref().unwrap_or("public");
        let s = self.structure(t).await?;
        let mut lines: Vec<String> = s
            .columns
            .iter()
            .map(|c| {
                let mut l = format!("    {} {}", d.quote_ident(&c.name), c.data_type);
                if !c.nullable {
                    l.push_str(" NOT NULL");
                }
                if let Some(def) = &c.default {
                    l.push_str(&format!(" DEFAULT {def}"));
                }
                l
            })
            .collect();
        let constraints = self
            .client
            .query(
                &format!(
                    "SELECT conname::text, pg_get_constraintdef(oid, true) FROM pg_constraint \
                     WHERE conrelid = {REL} ORDER BY contype DESC, conname"
                ),
                &[&schema, &t.name],
            )
            .await?;
        for r in &constraints {
            lines.push(format!(
                "    CONSTRAINT {} {}",
                d.quote_ident(r.get::<_, &str>(0)),
                r.get::<_, &str>(1)
            ));
        }
        let mut ddl = format!("CREATE TABLE {} (\n{}\n);", d.table(t), lines.join(",\n"));
        let indexes = self
            .client
            .query(
                &format!(
                    "SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i \
                     WHERE i.indrelid = {REL} AND NOT EXISTS \
                     (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid) ORDER BY 1"
                ),
                &[&schema, &t.name],
            )
            .await?;
        for r in &indexes {
            ddl.push_str(&format!("\n{};", r.get::<_, &str>(0)));
        }
        if let Some(comment) = self
            .client
            .query_one(&format!("SELECT obj_description({REL}, 'pg_class')"), &[&schema, &t.name])
            .await?
            .get::<_, Option<String>>(0)
        {
            ddl.push_str(&format!(
                "\nCOMMENT ON TABLE {} IS {};",
                d.table(t),
                d.string_literal(&comment)
            ));
        }
        Ok(ddl)
    }
}

/// Typed cell from the text protocol.
fn cell(text: Option<&str>, ty: Option<&Type>) -> Value {
    let Some(t) = text else {
        return Value::Null;
    };
    match ty {
        Some(&Type::INT2 | &Type::INT4 | &Type::INT8 | &Type::OID) => {
            t.parse().map(Value::Int).unwrap_or_else(|_| Value::Text(t.into()))
        }
        Some(&Type::FLOAT4 | &Type::FLOAT8) => t.parse().map(Value::Float).unwrap_or_else(|_| Value::Text(t.into())),
        Some(&Type::BOOL) => Value::Bool(t == "t"),
        _ => Value::Text(t.into()),
    }
}

#[async_trait]
impl Driver for PgDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Postgres
    }

    async fn server_version(&self) -> Result<String> {
        let v: String = self.client.query_one("SHOW server_version", &[]).await?.get(0);
        Ok(v.split(" (").next().unwrap_or_default().to_string())
    }

    async fn current_database(&self) -> Result<String> {
        Ok(self.client.query_one("SELECT current_database()::text", &[]).await?.get(0))
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        self.strings(
            "SELECT datname::text FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1",
            &[],
        )
        .await
    }

    async fn use_database(&self, _name: &str) -> Result<()> {
        bail!("PostgreSQL needs a new connection to switch database")
    }

    async fn list_schemas(&self, include_system: bool) -> Result<Vec<String>> {
        let all = self
            .strings(
                "SELECT nspname::text FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_toast%' \
                 AND nspname NOT LIKE 'pg\\_temp\\_%' ORDER BY 1",
                &[],
            )
            .await?;
        Ok(all
            .into_iter()
            .filter(|s| include_system || !SYSTEM_SCHEMAS.contains(&s.as_str()))
            .collect())
    }

    async fn list_items(&self, schema: Option<&str>) -> Result<Vec<Item>> {
        let schema = schema.unwrap_or("public");
        let rows = self
            .client
            .query(
                "SELECT c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class') \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S') ORDER BY 1",
                &[&schema],
            )
            .await?;
        let mut items: Vec<Item> = rows
            .iter()
            .map(|r| Item {
                table: TableRef::new(Some(schema), r.get::<_, &str>(0)),
                kind: match r.get::<_, &str>(1) {
                    "v" => ItemKind::View,
                    "m" => ItemKind::MaterializedView,
                    "S" => ItemKind::Sequence,
                    _ => ItemKind::Table,
                },
                comment: r.get(2),
            })
            .collect();
        items.extend(self.functions(schema).await?);
        Ok(items)
    }

    async fn structure(&self, t: &TableRef) -> Result<TableStructure> {
        let schema = t.schema.as_deref().unwrap_or("public");
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &t.name];
        let columns = self
            .client
            .query(
                &format!(
                    "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                     pg_get_expr(d.adbin, d.adrelid), col_description(a.attrelid, a.attnum), \
                     COALESCE((SELECT true FROM pg_index i WHERE i.indrelid = a.attrelid \
                       AND i.indisprimary AND a.attnum = ANY(i.indkey)), false), \
                     CASE WHEN a.attidentity IN ('a', 'd') THEN 'identity' \
                          WHEN a.attgenerated = 's' THEN 'generated' END \
                     FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                     WHERE a.attrelid = {REL} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
                ),
                &params,
            )
            .await
            .with_context(|| format!("could not read columns of {}", t.qualified()))?;
        let columns = columns
            .iter()
            .map(|r| ColumnInfo {
                name: r.get(0),
                data_type: r.get(1),
                nullable: r.get(2),
                default: r.get(3),
                comment: r.get(4),
                primary_key: r.get(5),
                extra: r.get(6),
                references: None,
            })
            .collect();
        let indexes = self
            .client
            .query(
                &format!(
                    "SELECT ic.relname::text, ix.indisunique, ix.indisprimary, am.amname::text, \
                     pg_get_indexdef(ix.indexrelid), \
                     ARRAY(SELECT pg_get_indexdef(ix.indexrelid, k + 1, true) \
                       FROM generate_subscripts(ix.indkey, 1) AS k ORDER BY k) \
                     FROM pg_index ix JOIN pg_class ic ON ic.oid = ix.indexrelid \
                     JOIN pg_am am ON am.oid = ic.relam WHERE ix.indrelid = {REL} ORDER BY 1"
                ),
                &params,
            )
            .await?
            .iter()
            .map(|r| IndexInfo {
                name: r.get(0),
                unique: r.get(1),
                primary: r.get(2),
                method: r.get(3),
                definition: r.get(4),
                columns: r.get(5),
            })
            .collect();
        let foreign_keys = self
            .client
            .query(
                &format!(
                    "SELECT con.conname::text, \
                     ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, o) \
                       JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.o), \
                     nf.nspname::text, cf.relname::text, \
                     ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, o) \
                       JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.o), \
                     con.confupdtype::text, con.confdeltype::text \
                     FROM pg_constraint con JOIN pg_class cf ON cf.oid = con.confrelid \
                     JOIN pg_namespace nf ON nf.oid = cf.relnamespace \
                     WHERE con.contype = 'f' AND con.conrelid = {REL} ORDER BY 1"
                ),
                &params,
            )
            .await?
            .iter()
            .map(|r| ForeignKeyInfo {
                name: r.get(0),
                columns: r.get(1),
                ref_schema: r.get(2),
                ref_table: r.get(3),
                ref_columns: r.get(4),
                on_update: fk_action(r.get(5)),
                on_delete: fk_action(r.get(6)),
            })
            .collect();
        let mut s = TableStructure { table: t.clone(), columns, indexes, foreign_keys };
        s.link_foreign_keys();
        Ok(s)
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let t = &item.table;
        let schema = t.schema.as_deref().unwrap_or("public");
        let d = Dialect(DriverKind::Postgres);
        match item.kind {
            ItemKind::Table => self.table_ddl(t).await,
            ItemKind::View | ItemKind::MaterializedView => {
                let def: String = self
                    .client
                    .query_one(&format!("SELECT pg_get_viewdef({REL}, true)"), &[&schema, &t.name])
                    .await?
                    .get(0);
                let kw = if item.kind == ItemKind::View { "OR REPLACE VIEW" } else { "MATERIALIZED VIEW" };
                Ok(format!("CREATE {kw} {} AS\n{}", d.table(t), def.trim_end()))
            }
            ItemKind::Function | ItemKind::Procedure => {
                let defs = self
                    .strings(
                        "SELECT pg_get_functiondef(p.oid) FROM pg_proc p \
                         JOIN pg_namespace n ON n.oid = p.pronamespace \
                         WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind IN ('f', 'p')",
                        &[&schema, &t.name],
                    )
                    .await?;
                Ok(defs.join(";\n\n"))
            }
            ItemKind::Sequence => {
                let r = self
                    .client
                    .query_one(
                        "SELECT start_value, increment_by, min_value, max_value, cycle \
                         FROM pg_sequences WHERE schemaname = $1 AND sequencename = $2",
                        &[&schema, &t.name],
                    )
                    .await?;
                Ok(format!(
                    "CREATE SEQUENCE {} START {} INCREMENT {} MINVALUE {} MAXVALUE {}{};",
                    d.table(t),
                    r.get::<_, i64>(0),
                    r.get::<_, i64>(1),
                    r.get::<_, i64>(2),
                    r.get::<_, i64>(3),
                    if r.get::<_, bool>(4) { " CYCLE" } else { "" }
                ))
            }
        }
    }

    async fn all_columns(&self, schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        let schema = schema.unwrap_or("public");
        Ok(self
            .client
            .query(
                "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod)                  FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace                  WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND a.attnum > 0 AND NOT a.attisdropped                  ORDER BY c.relname, a.attnum",
                &[&schema],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
            .collect())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let start = Instant::now();
        // The text protocol has no column types; a prepare provides them when
        // the statement allows it (single statement without parameters).
        let types: Option<Vec<Type>> = self
            .client
            .prepare(sql)
            .await
            .ok()
            .map(|st| st.columns().iter().map(|c| c.type_().clone()).collect());
        let messages = self.client.simple_query(sql).await?;
        let mut result = QueryResult { statement: sql.to_string(), ..Default::default() };
        for m in messages {
            match m {
                SimpleQueryMessage::RowDescription(cols) => {
                    result.columns = cols
                        .iter()
                        .enumerate()
                        .map(|(i, c)| ResultColumn {
                            name: c.name().to_string(),
                            type_name: types.as_ref().and_then(|t| t.get(i)).map(type_label).unwrap_or_default(),
                        })
                        .collect();
                }
                SimpleQueryMessage::Row(row) => {
                    let values = (0..row.len())
                        .map(|i| cell(row.get(i), types.as_ref().and_then(|t| t.get(i))))
                        .collect();
                    result.rows.push(values);
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    if result.columns.is_empty() {
                        result.rows_affected = Some(n);
                    }
                }
                _ => {}
            }
        }
        result.duration_ms = start.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        // A multi-statement simple query runs as one implicit transaction.
        let script = statements
            .iter()
            .map(|s| s.trim_end().trim_end_matches(';').to_string())
            .collect::<Vec<_>>()
            .join(";\n");
        self.client.batch_execute(&script).await?;
        Ok(())
    }
}

/// `_text` → `text[]`, like the structure view spells it.
fn type_label(t: &Type) -> String {
    match t.name().strip_prefix('_') {
        Some(elem) => format!("{elem}[]"),
        None => t.name().to_string(),
    }
}

fn fk_action(code: &str) -> String {
    match code {
        "a" => "NO ACTION",
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        other => other,
    }
    .to_string()
}

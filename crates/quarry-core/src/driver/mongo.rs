//! MongoDB (and Atlas, Amazon DocumentDB, Azure Cosmos DB for MongoDB).
//!
//! Collections are the sidebar's items. The grid shows documents as rows:
//! top-level fields are the columns, sampled from the collection, and nested
//! documents and arrays show as shell literals that can be edited in place.
//! The filter bar becomes a query, grid edits become `insertOne`,
//! `updateOne` and `deleteOne`, and the editor runs mongosh-style calls
//! (see [`crate::mongo_shell`]).

use std::collections::HashMap;
use std::str::FromStr as _;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::TryStreamExt as _;
use mongodb::bson::{self, Bson, Document, doc, oid::ObjectId};
use mongodb::options::ClientOptions;
use mongodb::{Client, IndexModel};

use super::Driver;
use crate::changes::{ColumnValues, KeyKind, RowChange};
use crate::model::{ConnectionConfig, DriverKind, SslMode};
use crate::mongo_shell::{self, Command, Target, parse_value, to_shell, to_shell_pretty};
use crate::query::{ANY_COLUMN, BrowseRequest, DocumentEntry, DocumentPage, DocumentRequest, Filter, FilterOp, QueryResult, RAW_SQL, ResultColumn};
use crate::schema::{ColumnInfo, IndexInfo, Item, ItemKind, TableRef, TableStructure};
use crate::value::Value;

/// Documents read to learn a collection's fields.
const SAMPLE: i64 = 200;
const SYSTEM_DATABASES: [&str; 3] = ["admin", "config", "local"];

pub struct MongoDriver {
    client: Client,
    db: Mutex<String>,
    /// Field names and types per collection, from the last sample, to type
    /// edited cells and keep columns in a stable order.
    fields: Mutex<HashMap<String, Vec<(String, String)>>>,
    show_system: bool,
}

/// `%`-encodes a user name or password for a connection string.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The connection string: the Host field itself when it is one (Atlas gives
/// `mongodb+srv://…`), else one built from the form.
pub fn connection_string(cfg: &ConnectionConfig) -> String {
    let host = cfg.host.trim();
    if host.starts_with("mongodb://") || host.starts_with("mongodb+srv://") {
        // Atlas strings carry a placeholder for the password.
        let pw = cfg.password.as_deref().unwrap_or_default();
        return host.replace("<db_password>", &encode(pw)).replace("<password>", &encode(pw));
    }
    let auth = match (cfg.user.trim(), cfg.password.as_deref().filter(|p| !p.is_empty())) {
        ("", _) => String::new(),
        (u, Some(p)) => format!("{}:{}@", encode(u), encode(p)),
        (u, None) => format!("{}@", encode(u)),
    };
    let mut params = vec!["appName=Quarry".to_string(), "serverSelectionTimeoutMS=15000".into(), "connectTimeoutMS=15000".into()];
    // One server reached directly, even when it is a replica set member
    // whose advertised host names do not resolve from here (Docker).
    if !host.contains(',') {
        params.push("directConnection=true".into());
    }
    if !cfg.user.trim().is_empty() {
        params.push("authSource=admin".into());
    }
    match cfg.ssl_mode {
        // A plain server takes the whole selection timeout to refuse TLS, so
        // "Preferred" connects without it.
        SslMode::Disable | SslMode::Prefer => {}
        SslMode::Require => params.push("tls=true&tlsAllowInvalidCertificates=true".into()),
        SslMode::VerifyCa => params.push("tls=true&tlsAllowInvalidHostnames=true".into()),
        SslMode::VerifyFull => params.push("tls=true".into()),
    }
    format!("mongodb://{auth}{host}:{}/?{}", cfg.port_or_default(), params.join("&"))
}

/// BSON type names as MongoDB's `$type` spells them.
fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null | Bson::Undefined => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Decimal128(_) => "decimal",
        _ => "other",
    }
}

/// A BSON value as a grid cell. Nested values show as shell literals, which
/// [`parse_value`] reads back after an edit.
fn cell(b: &Bson) -> Value {
    match b {
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Boolean(v) => Value::Bool(*v),
        Bson::Int32(n) => Value::Int(i64::from(*n)),
        Bson::Int64(n) => Value::Int(*n),
        Bson::Double(f) => Value::Float(*f),
        Bson::String(s) => Value::Text(s.clone()),
        Bson::ObjectId(o) => Value::Text(o.to_hex()),
        Bson::DateTime(d) => Value::Text(d.try_to_rfc3339_string().unwrap_or_else(|_| d.timestamp_millis().to_string())),
        Bson::Decimal128(d) => Value::Text(d.to_string()),
        other => Value::Text(to_shell(other)),
    }
}

/// An edited cell as BSON of the field's type: "65a1…" in an objectId field
/// is an ObjectId, "2024-05-06" in a date field a date, `{a: 1}` in an
/// object field a document.
fn typed(v: &Value, ty: &str, field: &str) -> Result<Bson> {
    let bad = |t: &str| anyhow!("{field}: {t:?} is not a valid {ty}; change the type in the SQL editor with updateOne");
    Ok(match v {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int(i) if ty == "long" => Bson::Int64(*i),
        Value::Int(i) if ty == "double" => Bson::Double(*i as f64),
        Value::Int(i) => i32::try_from(*i).map(Bson::Int32).unwrap_or(Bson::Int64(*i)),
        Value::Float(f) => Bson::Double(*f),
        Value::Bytes(b) => Bson::Binary(bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: b.clone() }),
        Value::Text(t) => {
            let s = t.trim();
            match ty {
                "objectId" => ObjectId::parse_str(s).map(Bson::ObjectId).or_else(|_| parse_value(s)).map_err(|_| bad(t))?,
                "int" => s.parse::<i32>().map(Bson::Int32).or_else(|_| s.parse::<i64>().map(Bson::Int64)).map_err(|_| bad(t))?,
                "long" => s.parse::<i64>().map(Bson::Int64).map_err(|_| bad(t))?,
                "double" => s.parse::<f64>().map(Bson::Double).map_err(|_| bad(t))?,
                "decimal" => bson::Decimal128::from_str(s).map(Bson::Decimal128).map_err(|_| bad(t))?,
                "bool" => match s {
                    "true" | "1" => Bson::Boolean(true),
                    "false" | "0" => Bson::Boolean(false),
                    _ => return Err(bad(t)),
                },
                "date" => mongo_shell::parse_date(s).map(Bson::DateTime).or_else(|_| parse_value(s)).map_err(|_| bad(t))?,
                "string" => Bson::String(t.clone()),
                // Objects, arrays and the rest are shell literals.
                _ if s.starts_with(['{', '[']) || s.contains('(') => parse_value(s).map_err(|e| anyhow!("{field}: {e}"))?,
                _ => Bson::String(t.clone()),
            }
        }
    })
}

/// Field names and types of documents, `_id` first, in the order seen.
fn infer_fields(docs: &[Document]) -> Vec<(String, String)> {
    let mut order: Vec<String> = vec![];
    let mut types: HashMap<String, HashMap<&'static str, usize>> = HashMap::new();
    for d in docs {
        for (k, v) in d {
            if !order.contains(k) {
                order.push(k.clone());
            }
            *types.entry(k.clone()).or_default().entry(type_name(v)).or_default() += 1;
        }
    }
    if let Some(i) = order.iter().position(|k| k == "_id") {
        let id = order.remove(i);
        order.insert(0, id);
    }
    order
        .into_iter()
        .map(|k| {
            let counts = &types[&k];
            // The most common type that is not null; ties go to the name, for stability.
            let mut best: Vec<(&&str, &usize)> = counts.iter().filter(|(t, _)| **t != "null").collect();
            best.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let ty = match best.as_slice() {
                [] => "null".to_string(),
                [(t, _)] => t.to_string(),
                [(t, _), ..] => format!("{t} (mixed)"),
            };
            (k, ty)
        })
        .collect()
}

/// Documents as a grid: the known fields first, then any new ones.
fn docs_result(statement: String, docs: &[Document], known: &[(String, String)]) -> QueryResult {
    let mut columns: Vec<String> = known.iter().map(|(n, _)| n.clone()).collect();
    for d in docs {
        for k in d.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    let rows = docs.iter().map(|d| columns.iter().map(|c| d.get(c).map(cell).unwrap_or(Value::Null)).collect()).collect();
    let types: HashMap<&str, &str> = known.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    QueryResult {
        statement,
        columns: columns.iter().map(|c| ResultColumn { name: c.clone(), type_name: types.get(c.as_str()).unwrap_or(&"").to_string() }).collect(),
        rows,
        rows_affected: None,
        duration_ms: 0,
    }
}

/// One document as field / value rows (server status, write results).
fn fields_result(statement: String, d: &Document) -> QueryResult {
    QueryResult {
        statement,
        columns: ["field", "value"].iter().map(|n| ResultColumn { name: n.to_string(), type_name: String::new() }).collect(),
        rows: d.iter().map(|(k, v)| vec![Value::Text(k.clone()), cell(v)]).collect(),
        rows_affected: None,
        duration_ms: 0,
    }
}

/// `%`/`_` LIKE patterns as an anchored regular expression.
fn like_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for c in pattern.chars() {
        match c {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            c => out.push_str(&regex_escape(&c.to_string())),
        }
    }
    out.push('$');
    out
}

fn regex_escape(s: &str) -> String {
    s.chars().flat_map(|c| if "\\^$.|?*+()[]{}/".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// The filter bar as a MongoDB query.
fn query(filters: &[Filter], fields: &[(String, String)]) -> Result<Document> {
    let ty = |c: &str| fields.iter().find(|(n, _)| n == c).map(|(_, t)| t.split(' ').next().unwrap_or("").to_string()).unwrap_or_default();
    let mut and: Vec<Bson> = vec![];
    for f in filters.iter().filter(|f| f.enabled) {
        if f.column == RAW_SQL {
            if f.value.trim().is_empty() {
                continue;
            }
            match parse_value(&f.value) {
                Ok(Bson::Document(d)) => and.push(Bson::Document(d)),
                _ => bail!("a raw filter is a query document, such as {{age: {{$gt: 30}}}}"),
            }
            continue;
        }
        let condition = |column: &str| -> Result<Option<Document>> {
            let t = ty(column);
            let v = |s: &str| typed(&Value::Text(s.trim().to_string()), &t, column);
            let rx = |pattern: String, ci: bool| Bson::RegularExpression(bson::Regex { pattern, options: if ci { "i".into() } else { String::new() } });
            let one = |op: &str, b: Bson| doc! { column: { op: b } };
            Ok(Some(match f.op {
                FilterOp::Eq => doc! { column: v(&f.value)? },
                FilterOp::NotEq => one("$ne", v(&f.value)?),
                FilterOp::Lt => one("$lt", v(&f.value)?),
                FilterOp::Gt => one("$gt", v(&f.value)?),
                FilterOp::LtEq => one("$lte", v(&f.value)?),
                FilterOp::GtEq => one("$gte", v(&f.value)?),
                FilterOp::In | FilterOp::NotIn => {
                    let items = f.value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(v).collect::<Result<Vec<_>>>()?;
                    one(if f.op == FilterOp::In { "$in" } else { "$nin" }, Bson::Array(items))
                }
                FilterOp::IsNull => doc! { column: Bson::Null },
                FilterOp::IsNotNull => one("$ne", Bson::Null),
                FilterOp::Between | FilterOp::NotBetween => {
                    let upper = f.value.to_ascii_uppercase();
                    let Some(at) = upper.find(" AND ") else { return Ok(None) };
                    let range = doc! { "$gte": v(&f.value[..at])?, "$lte": v(&f.value[at + 5..])? };
                    if f.op == FilterOp::Between { doc! { column: range } } else { doc! { column: { "$not": range } } }
                }
                FilterOp::Like => doc! { column: rx(like_regex(&f.value), false) },
                FilterOp::ILike => doc! { column: rx(like_regex(&f.value), true) },
                FilterOp::Contains | FilterOp::ContainsCi => doc! { column: rx(regex_escape(&f.value), f.op == FilterOp::ContainsCi) },
                FilterOp::NotContains | FilterOp::NotContainsCi => {
                    doc! { column: { "$not": rx(regex_escape(&f.value), f.op == FilterOp::NotContainsCi) } }
                }
                FilterOp::HasPrefix | FilterOp::HasPrefixCi => doc! { column: rx(format!("^{}", regex_escape(&f.value)), f.op == FilterOp::HasPrefixCi) },
                FilterOp::HasSuffix | FilterOp::HasSuffixCi => doc! { column: rx(format!("{}$", regex_escape(&f.value)), f.op == FilterOp::HasSuffixCi) },
            }))
        };
        if f.column == ANY_COLUMN {
            // Text matches on any field; other operators on fields where the
            // value makes sense for the field's type.
            let mut any = vec![];
            for (name, _) in fields {
                if let Ok(Some(c)) = condition(name) {
                    any.push(Bson::Document(c));
                }
            }
            if !any.is_empty() {
                and.push(Bson::Document(doc! { "$or": any }));
            }
        } else if !f.column.is_empty() {
            if let Some(c) = condition(&f.column)? {
                and.push(Bson::Document(c));
            }
        }
    }
    Ok(match and.len() {
        0 => Document::new(),
        1 => match and.pop() {
            Some(Bson::Document(d)) => d,
            _ => Document::new(),
        },
        _ => doc! { "$and": and },
    })
}

/// A filter or sort typed in the document view; empty is `{}`.
fn shell_doc(text: &str, what: &str) -> Result<Document> {
    if text.trim().is_empty() {
        return Ok(Document::new());
    }
    match parse_value(text) {
        Ok(Bson::Document(d)) => Ok(d),
        Ok(_) => bail!("the {what} is a document, such as {{age: {{$gt: 30}}}}"),
        Err(e) => bail!("{what}: {e}"),
    }
}

fn arg_doc(args: &[Bson], i: usize) -> Result<Document> {
    match args.get(i) {
        None | Some(Bson::Null) => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(other) => bail!("expected a document, got {}", to_shell(other)),
    }
}

fn arg_i64(b: Option<&Bson>) -> Option<i64> {
    match b? {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(f) => Some(*f as i64),
        _ => None,
    }
}

impl MongoDriver {
    pub async fn connect(cfg: &ConnectionConfig, database: Option<&str>) -> Result<Self> {
        let mut options = ClientOptions::parse(connection_string(cfg)).await.context("invalid MongoDB connection settings")?;
        options.app_name = Some("Quarry".into());
        options.server_selection_timeout.get_or_insert(Duration::from_secs(15));
        let client = Client::with_options(options)?;
        // Selecting a server proves the address and credentials.
        client.database("admin").run_command(doc! { "ping": 1 }).await.context("could not connect to MongoDB")?;
        let mut db = database.unwrap_or(&cfg.database).trim().to_string();
        if db.is_empty() {
            db = client.default_database().map(|d| d.name().to_string()).unwrap_or_default();
        }
        if db.is_empty() {
            // The first database that is not MongoDB's own, as mongosh users expect "test".
            let names = client.list_database_names().await.unwrap_or_default();
            db = names.into_iter().find(|n| !SYSTEM_DATABASES.contains(&n.as_str())).unwrap_or_else(|| "test".into());
        }
        Ok(Self { client, db: Mutex::new(db), fields: Mutex::new(HashMap::new()), show_system: cfg.show_system_schemas })
    }

    fn database(&self) -> mongodb::Database {
        self.client.database(&self.db.lock().unwrap())
    }

    fn collection(&self, name: &str) -> mongodb::Collection<Document> {
        self.database().collection(name)
    }

    /// Fields of a collection, sampling it when not known yet.
    async fn fields_of(&self, name: &str) -> Result<Vec<(String, String)>> {
        if let Some(f) = self.fields.lock().unwrap().get(name) {
            return Ok(f.clone());
        }
        let docs: Vec<Document> = self.collection(name).find(doc! {}).limit(SAMPLE).await?.try_collect().await?;
        let fields = infer_fields(&docs);
        self.fields.lock().unwrap().insert(name.to_string(), fields.clone());
        Ok(fields)
    }

    fn cached_fields(&self, name: &str) -> Vec<(String, String)> {
        self.fields.lock().unwrap().get(name).cloned().unwrap_or_default()
    }

    /// The `listCollections` entry of one collection.
    async fn collection_info(&self, name: &str) -> Result<Document> {
        let r = self.database().run_command(doc! { "listCollections": 1, "filter": { "name": name } }).await?;
        let batch = r.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).cloned().unwrap_or_default();
        match batch.into_iter().next() {
            Some(Bson::Document(d)) => Ok(d),
            _ => bail!("the collection {name:?} does not exist (any more)"),
        }
    }

    async fn run_calls(&self, statement: &str, target: Target, calls: Vec<mongo_shell::Call>) -> Result<QueryResult> {
        let first = &calls[0];
        let args = &first.args;
        let explain = calls.iter().skip(1).any(|c| c.name == "explain");
        let chained = |name: &str| calls.iter().skip(1).find(|c| c.name == name).map(|c| c.args.clone());
        let st = statement.to_string();
        let Target::Collection(name) = target else { return self.database_call(st, &first.name, args).await };
        let coll = self.collection(&name);
        let count = |n: u64, label: &str| QueryResult {
            statement: st.clone(),
            columns: vec![ResultColumn { name: label.into(), type_name: String::new() }],
            rows: vec![vec![Value::Int(n as i64)]],
            rows_affected: None,
            duration_ms: 0,
        };
        Ok(match first.name.as_str() {
            "find" | "findOne" => {
                let filter = arg_doc(args, 0)?;
                let mut projection = arg_doc(args, 1)?;
                if let Some(p) = chained("projection") {
                    projection = arg_doc(&p, 0)?;
                }
                let sort = chained("sort").map(|a| arg_doc(&a, 0)).transpose()?;
                let skip = chained("skip").and_then(|a| arg_i64(a.first()));
                let limit = if first.name == "findOne" { Some(1) } else { chained("limit").and_then(|a| arg_i64(a.first())) };
                if explain {
                    let mut find = doc! { "find": &name, "filter": filter };
                    if !projection.is_empty() {
                        find.insert("projection", projection);
                    }
                    if let Some(s) = sort {
                        find.insert("sort", s);
                    }
                    let plan = self.database().run_command(doc! { "explain": find, "verbosity": "queryPlanner" }).await?;
                    return Ok(fields_result(st, &plan));
                }
                // Projected results show only what was asked for.
                let known = if projection.is_empty() { self.cached_fields(&name) } else { vec![] };
                let mut action = coll.find(filter);
                if !projection.is_empty() {
                    action = action.projection(projection);
                }
                if let Some(s) = sort {
                    action = action.sort(s);
                }
                if let Some(n) = skip {
                    action = action.skip(n.max(0) as u64);
                }
                if let Some(n) = limit {
                    action = action.limit(n);
                }
                let docs: Vec<Document> = action.await?.try_collect().await?;
                docs_result(st, &docs, &known)
            }
            "aggregate" => {
                let pipeline: Vec<Document> = match args.first() {
                    Some(Bson::Array(stages)) => stages.iter().map(|s| s.as_document().cloned().ok_or_else(|| anyhow!("each stage is a document"))).collect::<Result<_>>()?,
                    None => vec![],
                    _ => bail!("aggregate takes an array of stages"),
                };
                if explain {
                    let cmd = doc! { "explain": { "aggregate": &name, "pipeline": pipeline, "cursor": {} }, "verbosity": "queryPlanner" };
                    return Ok(fields_result(st, &self.database().run_command(cmd).await?));
                }
                let docs: Vec<Document> = coll.aggregate(pipeline).await?.try_collect().await?;
                docs_result(st, &docs, &[])
            }
            "countDocuments" | "count" => count(coll.count_documents(arg_doc(args, 0)?).await?, "count"),
            "estimatedDocumentCount" => count(coll.estimated_document_count().await?, "count"),
            "distinct" => {
                let Some(Bson::String(field)) = args.first() else { bail!("distinct takes a field name") };
                let values = coll.distinct(field.as_str(), arg_doc(args, 1)?).await?;
                QueryResult {
                    statement: st,
                    columns: vec![ResultColumn { name: field.clone(), type_name: String::new() }],
                    rows: values.iter().map(|v| vec![cell(v)]).collect(),
                    rows_affected: None,
                    duration_ms: 0,
                }
            }
            "insertOne" => {
                let r = coll.insert_one(arg_doc(args, 0)?).await?;
                QueryResult { rows_affected: Some(1), ..fields_result(st, &doc! { "insertedId": r.inserted_id }) }
            }
            "insertMany" => {
                let Some(Bson::Array(items)) = args.first() else { bail!("insertMany takes an array of documents") };
                let docs = items.iter().map(|d| d.as_document().cloned().ok_or_else(|| anyhow!("insertMany takes documents"))).collect::<Result<Vec<_>>>()?;
                let r = coll.insert_many(docs).await?;
                let mut ids: Vec<(usize, Bson)> = r.inserted_ids.into_iter().collect();
                ids.sort_by_key(|(i, _)| *i);
                QueryResult {
                    statement: st,
                    columns: vec![ResultColumn { name: "insertedId".into(), type_name: String::new() }],
                    rows_affected: Some(ids.len() as u64),
                    rows: ids.iter().map(|(_, id)| vec![cell(id)]).collect(),
                    duration_ms: 0,
                }
            }
            "updateOne" | "updateMany" | "replaceOne" => {
                let filter = arg_doc(args, 0)?;
                let upsert = arg_doc(args, 2)?.get_bool("upsert").unwrap_or(false);
                let r = match (first.name.as_str(), args.get(1)) {
                    ("replaceOne", _) => coll.replace_one(filter, arg_doc(args, 1)?).upsert(upsert).await?,
                    (method, Some(Bson::Array(stages))) => {
                        let pipeline = stages.iter().filter_map(|s| s.as_document().cloned()).collect::<Vec<_>>();
                        if method == "updateOne" { coll.update_one(filter, pipeline).upsert(upsert).await? } else { coll.update_many(filter, pipeline).upsert(upsert).await? }
                    }
                    (method, _) => {
                        let update = arg_doc(args, 1)?;
                        if update.keys().next().is_some_and(|k| !k.starts_with('$')) {
                            bail!("an update needs operators such as {{$set: {{…}}}}; use replaceOne to replace a whole document");
                        }
                        if method == "updateOne" { coll.update_one(filter, update).upsert(upsert).await? } else { coll.update_many(filter, update).upsert(upsert).await? }
                    }
                };
                let mut d = doc! { "matchedCount": r.matched_count as i64, "modifiedCount": r.modified_count as i64 };
                if let Some(id) = r.upserted_id {
                    d.insert("upsertedId", id);
                }
                QueryResult { rows_affected: Some(r.modified_count), ..fields_result(st, &d) }
            }
            "deleteOne" | "deleteMany" => {
                let filter = arg_doc(args, 0)?;
                let r = if first.name == "deleteOne" { coll.delete_one(filter).await? } else { coll.delete_many(filter).await? };
                QueryResult { rows_affected: Some(r.deleted_count), ..fields_result(st, &doc! { "deletedCount": r.deleted_count as i64 }) }
            }
            "drop" => {
                coll.drop().await?;
                self.fields.lock().unwrap().remove(&name);
                fields_result(st, &doc! { "dropped": &name })
            }
            "createIndex" => {
                let keys = arg_doc(args, 0)?;
                let opts = arg_doc(args, 1)?;
                let index_options: Option<mongodb::options::IndexOptions> = (!opts.is_empty()).then(|| bson::from_document(opts)).transpose().context("index options")?;
                let r = coll.create_index(IndexModel::builder().keys(keys).options(index_options).build()).await?;
                fields_result(st, &doc! { "createdIndex": r.index_name })
            }
            "dropIndex" => {
                let Some(Bson::String(index)) = args.first() else { bail!("dropIndex takes the index name") };
                coll.drop_index(index.as_str()).await?;
                fields_result(st, &doc! { "droppedIndex": index })
            }
            "getIndexes" | "listIndexes" => {
                let indexes: Vec<IndexModel> = coll.list_indexes().await?.try_collect().await?;
                let docs: Vec<Document> = indexes
                    .iter()
                    .map(|i| {
                        let mut d = doc! { "keys": i.keys.clone() };
                        if let Some(o) = &i.options {
                            if let Ok(od) = bson::to_document(o) {
                                d.extend(od);
                            }
                        }
                        d
                    })
                    .collect();
                docs_result(st, &docs, &[])
            }
            "renameCollection" => {
                let Some(Bson::String(to)) = args.first() else { bail!("renameCollection takes the new name") };
                let db = self.db.lock().unwrap().clone();
                self.client.database("admin").run_command(doc! { "renameCollection": format!("{db}.{name}"), "to": format!("{db}.{to}") }).await?;
                fields_result(st, &doc! { "renamed": &name, "to": to })
            }
            "stats" => fields_result(st, &self.database().run_command(doc! { "collStats": &name }).await?),
            other => bail!("Quarry's editor does not run {other}() yet; db.runCommand({{…}}) runs any database command"),
        })
    }

    async fn database_call(&self, st: String, method: &str, args: &[Bson]) -> Result<QueryResult> {
        let db = self.database();
        Ok(match method {
            "runCommand" | "adminCommand" => {
                let cmd = match args.first() {
                    Some(Bson::Document(d)) => d.clone(),
                    Some(Bson::String(s)) => doc! { s: 1 },
                    _ => bail!("{method} takes a command document"),
                };
                let target = if method == "adminCommand" { self.client.database("admin") } else { db };
                let r = target.run_command(cmd).await?;
                command_result(st, &r)
            }
            "getCollectionNames" => {
                let mut names = db.list_collection_names().await?;
                names.sort();
                QueryResult {
                    statement: st,
                    columns: vec![ResultColumn { name: "collection".into(), type_name: String::new() }],
                    rows: names.into_iter().map(|n| vec![Value::Text(n)]).collect(),
                    rows_affected: None,
                    duration_ms: 0,
                }
            }
            "createCollection" => {
                let Some(Bson::String(name)) = args.first() else { bail!("createCollection takes a name") };
                let mut cmd = doc! { "create": name };
                cmd.extend(arg_doc(args, 1)?);
                fields_result(st, &db.run_command(cmd).await?)
            }
            "createView" => {
                let (Some(Bson::String(name)), Some(Bson::String(source))) = (args.first(), args.get(1)) else { bail!("createView takes a name, a source collection and a pipeline") };
                let pipeline = args.get(2).cloned().unwrap_or(Bson::Array(vec![]));
                fields_result(st, &db.run_command(doc! { "create": name, "viewOn": source, "pipeline": pipeline }).await?)
            }
            "dropDatabase" => {
                db.drop().await?;
                fields_result(st, &doc! { "dropped": db.name() })
            }
            "stats" => fields_result(st, &db.run_command(doc! { "dbStats": 1 }).await?),
            "currentOp" => command_result(st, &self.client.database("admin").run_command(doc! { "currentOp": 1, "active": true }).await?),
            "getName" => fields_result(st, &doc! { "name": db.name() }),
            "version" | "serverBuildInfo" => fields_result(st, &db.run_command(doc! { "buildInfo": 1 }).await?),
            other => bail!("Quarry's editor does not run db.{other}() yet; db.runCommand({{…}}) runs any database command"),
        })
    }
}

/// A command's reply: its documents when it returns some (a cursor's first
/// batch, currentOp's operations), else field / value rows.
fn command_result(st: String, r: &Document) -> QueryResult {
    let docs = r
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_array("firstBatch").ok())
        .or_else(|| r.get_array("inprog").ok())
        .or_else(|| r.get_array("databases").ok());
    match docs {
        Some(items) => {
            let docs: Vec<Document> = items.iter().filter_map(|d| d.as_document().cloned()).collect();
            docs_result(st, &docs, &[])
        }
        None => fields_result(st, r),
    }
}

fn get<'a>(values: &'a ColumnValues, column: &str) -> Option<&'a Value> {
    values.iter().find(|(c, _)| c == column).map(|(_, v)| v)
}

/// Staged grid edits as shell calls.
fn edit_commands(collection: &str, fields: &[(String, String)], changes: &[RowChange], key_kind: KeyKind) -> Result<Vec<String>> {
    let ty = |c: &str| fields.iter().find(|(n, _)| n == c).map(|(_, t)| t.split(' ').next().unwrap_or("").to_string()).unwrap_or_default();
    let coll = format!("db.getCollection({})", serde_json::to_string(collection)?);
    let to_doc = |values: &ColumnValues, skip_empty_id: bool| -> Result<Document> {
        let mut d = Document::new();
        for (c, v) in values {
            if skip_empty_id && c == "_id" && (matches!(v, Value::Null) || v.display().trim().is_empty()) {
                continue;
            }
            d.insert(c.clone(), typed(v, &ty(c), c)?);
        }
        Ok(d)
    };
    let filter = |key: &ColumnValues| -> Result<Document> {
        match (key_kind, get(key, "_id")) {
            (KeyKind::PrimaryKey, Some(id)) => Ok(doc! { "_id": typed(id, &ty("_id"), "_id")? }),
            _ => to_doc(key, false),
        }
    };
    changes
        .iter()
        .map(|change| {
            Ok(match change {
                RowChange::Insert { values } => format!("{coll}.insertOne({})", to_shell(&Bson::Document(to_doc(values, true)?))),
                RowChange::Update { key, values } => {
                    if values.iter().any(|(c, _)| c == "_id") {
                        bail!("_id cannot be changed; insert a new document and delete this one");
                    }
                    let set = to_doc(values, false)?;
                    format!("{coll}.updateOne({}, {})", to_shell(&Bson::Document(filter(key)?)), to_shell(&Bson::Document(doc! { "$set": set })))
                }
                RowChange::Delete { key } => format!("{coll}.deleteOne({})", to_shell(&Bson::Document(filter(key)?))),
            })
        })
        .collect()
}

#[async_trait]
impl Driver for MongoDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::MongoDb
    }

    async fn server_version(&self) -> Result<String> {
        let info = self.client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
        Ok(info.get_str("version").unwrap_or_default().to_string())
    }

    async fn current_database(&self) -> Result<String> {
        Ok(self.db.lock().unwrap().clone())
    }

    async fn list_databases(&self) -> Result<Vec<String>> {
        let mut names = self.client.list_database_names().await?;
        if !self.show_system {
            names.retain(|n| !SYSTEM_DATABASES.contains(&n.as_str()));
        }
        let current = self.db.lock().unwrap().clone();
        // A database exists only once it holds data; keep the current one listed.
        if !names.contains(&current) {
            names.push(current);
        }
        names.sort();
        Ok(names)
    }

    async fn use_database(&self, name: &str) -> Result<()> {
        *self.db.lock().unwrap() = name.to_string();
        self.fields.lock().unwrap().clear();
        Ok(())
    }

    async fn list_schemas(&self, _include_system: bool) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn list_items(&self, _schema: Option<&str>) -> Result<Vec<Item>> {
        let specs: Vec<mongodb::results::CollectionSpecification> = self.database().list_collections().await?.try_collect().await?;
        let mut items: Vec<Item> = specs
            .into_iter()
            .filter(|s| self.show_system || !s.name.starts_with("system."))
            .map(|s| Item {
                table: TableRef::new(None, &s.name),
                kind: if s.collection_type == mongodb::results::CollectionType::View { ItemKind::View } else { ItemKind::Collection },
                comment: None,
            })
            .collect();
        items.sort_by(|a, b| a.table.name.cmp(&b.table.name));
        Ok(items)
    }

    async fn structure(&self, table: &TableRef) -> Result<TableStructure> {
        let name = &table.name;
        self.fields.lock().unwrap().remove(name);
        let fields = self.fields_of(name).await?;
        let indexes: Vec<IndexModel> = match self.collection(name).list_indexes().await {
            Ok(cursor) => cursor.try_collect().await.unwrap_or_default(),
            Err(_) => vec![], // views have no indexes
        };
        let columns = fields
            .iter()
            .map(|(n, t)| ColumnInfo {
                name: n.clone(),
                data_type: t.clone(),
                nullable: n != "_id",
                default: None,
                primary_key: n == "_id",
                extra: None,
                comment: None,
                references: None,
            })
            .collect();
        let indexes = indexes
            .iter()
            .map(|i| {
                let opts = i.options.as_ref();
                let index_name = opts.and_then(|o| o.name.clone()).unwrap_or_default();
                let method = i.keys.values().find_map(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "btree".into());
                IndexInfo {
                    primary: index_name == "_id_",
                    unique: index_name == "_id_" || opts.and_then(|o| o.unique).unwrap_or(false),
                    method: Some(method),
                    definition: Some(to_shell(&Bson::Document(i.keys.clone()))),
                    columns: i.keys.keys().cloned().collect(),
                    name: index_name,
                }
            })
            .collect();
        Ok(TableStructure { table: table.clone(), columns, indexes, foreign_keys: vec![] })
    }

    async fn ddl(&self, item: &Item) -> Result<String> {
        let name = &item.table.name;
        let info = self.collection_info(name).await?;
        let mut options = info.get_document("options").cloned().unwrap_or_default();
        let quoted = serde_json::to_string(name)?;
        if info.get_str("type") == Ok("view") {
            let source = options.get_str("viewOn").unwrap_or_default().to_string();
            let pipeline = options.get("pipeline").cloned().unwrap_or(Bson::Array(vec![]));
            return Ok(format!("db.createView({quoted}, {}, {})", serde_json::to_string(&source)?, to_shell(&pipeline)));
        }
        options.remove("uuid");
        let mut out = vec![if options.is_empty() {
            format!("db.createCollection({quoted})")
        } else {
            format!("db.createCollection({quoted}, {})", to_shell(&Bson::Document(options)))
        }];
        let indexes: Vec<IndexModel> = self.collection(name).list_indexes().await?.try_collect().await?;
        for i in indexes {
            let opts = i.options.as_ref().and_then(|o| bson::to_document(o).ok()).unwrap_or_default();
            if opts.get_str("name") == Ok("_id_") {
                continue;
            }
            let mut shown = Document::new();
            for (k, v) in opts {
                // Server-set details, not choices.
                if !matches!(k.as_str(), "v" | "ns" | "key") {
                    shown.insert(k, v);
                }
            }
            out.push(format!("db.getCollection({quoted}).createIndex({}, {})", to_shell(&Bson::Document(i.keys)), to_shell(&Bson::Document(shown))));
        }
        Ok(out.join(";\n") + ";")
    }

    async fn all_columns(&self, _schema: Option<&str>) -> Result<Vec<(String, String, String)>> {
        // Sampling every collection would be slow; known ones are enough for autocomplete.
        let fields = self.fields.lock().unwrap().clone();
        Ok(fields.into_iter().flat_map(|(c, f)| f.into_iter().map(move |(n, t)| (c.clone(), n, t))).collect())
    }

    async fn execute(&self, statement: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let cmd = mongo_shell::parse(statement).map_err(|e| anyhow!(e))?;
        let mut r = match cmd {
            Command::Use(_) => bail!("switch databases with the database list (the database name in the title bar, or Ctrl+K)"),
            Command::Show(what) => match what.as_str() {
                "dbs" | "databases" => command_result(statement.into(), &self.client.database("admin").run_command(doc! { "listDatabases": 1 }).await?),
                "collections" | "tables" => self.database_call(statement.into(), "getCollectionNames", &[]).await?,
                other => bail!("show {other} is not supported; try show dbs or show collections"),
            },
            Command::Calls { target, calls } => self.run_calls(statement, target, calls).await?,
        };
        r.duration_ms = start.elapsed().as_millis() as u64;
        Ok(r)
    }

    async fn execute_batch(&self, statements: &[String]) -> Result<()> {
        // A standalone server has no multi-document transactions, so changes
        // run one by one and an error says which ones were made.
        for (i, s) in statements.iter().enumerate() {
            if let Err(e) = self.execute(s).await {
                if i == 0 {
                    return Err(e);
                }
                return Err(e.context(format!("{i} of {} changes were made before this error, and they stay", statements.len())));
            }
        }
        Ok(())
    }

    async fn browse(&self, req: &BrowseRequest) -> Option<Result<QueryResult>> {
        Some(
            async {
                let start = Instant::now();
                let name = &req.table.name;
                let fields = self.fields_of(name).await?;
                let filter = query(&req.filters, &fields)?;
                let mut sort = Document::new();
                for s in &req.sort {
                    sort.insert(s.column.clone(), if s.descending { -1 } else { 1 });
                }
                let statement = format!(
                    "db.getCollection({}).find({}){}.skip({}).limit({})",
                    serde_json::to_string(name)?,
                    to_shell(&Bson::Document(filter.clone())),
                    if sort.is_empty() { String::new() } else { format!(".sort({})", to_shell(&Bson::Document(sort.clone()))) },
                    req.offset,
                    req.limit
                );
                let coll = self.collection(name);
                let mut action = coll.find(filter).skip(u64::from(req.offset)).limit(i64::from(req.limit));
                if !sort.is_empty() {
                    action = action.sort(sort);
                }
                let docs: Vec<Document> = action.await?.try_collect().await?;
                let mut r = docs_result(statement, &docs, &fields);
                r.duration_ms = start.elapsed().as_millis() as u64;
                Ok(r)
            }
            .await,
        )
    }

    async fn count(&self, table: &TableRef, filters: &[Filter]) -> Option<Result<u64>> {
        Some(
            async {
                let fields = self.fields_of(&table.name).await?;
                Ok(self.collection(&table.name).count_documents(query(filters, &fields)?).await?)
            }
            .await,
        )
    }

    async fn documents(&self, req: &DocumentRequest) -> Option<Result<DocumentPage>> {
        Some(
            async {
                let name = &req.table.name;
                let filter = shell_doc(&req.filter, "filter")?;
                let sort = shell_doc(&req.sort, "sort")?;
                let coll = self.collection(name);
                let mut action = coll.find(filter.clone()).skip(req.skip).limit(req.limit as i64);
                if !sort.is_empty() {
                    action = action.sort(sort.clone());
                }
                let docs: Vec<Document> = action.await?.try_collect().await?;
                let total = coll.count_documents(filter.clone()).await?;
                let statement = format!(
                    "db.getCollection({}).find({}){}",
                    serde_json::to_string(name)?,
                    to_shell(&Bson::Document(filter)),
                    if sort.is_empty() { String::new() } else { format!(".sort({})", to_shell(&Bson::Document(sort))) }
                );
                let documents = docs
                    .into_iter()
                    .map(|d| DocumentEntry { id: d.get("_id").map(to_shell), text: to_shell_pretty(&Bson::Document(d)) })
                    .collect();
                Ok(DocumentPage { documents, total, statement, info: vec![] })
            }
            .await,
        )
    }

    async fn document_save(&self, table: &TableRef, id: Option<&str>, text: &str) -> Option<Result<Vec<String>>> {
        Some((|| {
            let Bson::Document(doc) = parse_value(text).map_err(|e| anyhow!(e))? else { bail!("a document is an object: {{ … }}") };
            let coll = format!("db.getCollection({})", serde_json::to_string(&table.name)?);
            Ok(vec![match id {
                Some(id) => {
                    let id = parse_value(id).map_err(|e| anyhow!(e))?;
                    if doc.get("_id").is_some_and(|new| *new != id) {
                        bail!("_id cannot be changed; add a new document and delete this one");
                    }
                    let mut replacement = doc;
                    replacement.remove("_id");
                    format!("{coll}.replaceOne({{\"_id\": {}}}, {})", to_shell(&id), to_shell(&Bson::Document(replacement)))
                }
                None => format!("{coll}.insertOne({})", to_shell(&Bson::Document(doc))),
            }])
        })())
    }

    fn document_delete(&self, table: &TableRef, id: &str) -> Option<Result<Vec<String>>> {
        Some((|| {
            let id = parse_value(id).map_err(|e| anyhow!(e))?;
            Ok(vec![format!("db.getCollection({}).deleteOne({{\"_id\": {}}})", serde_json::to_string(&table.name)?, to_shell(&id))])
        })())
    }

    fn browse_text(&self, req: &BrowseRequest) -> Option<Result<String>> {
        Some((|| {
            let filter = query(&req.filters, &self.cached_fields(&req.table.name))?;
            let mut sort = Document::new();
            for s in &req.sort {
                sort.insert(s.column.clone(), if s.descending { -1 } else { 1 });
            }
            let sort = if sort.is_empty() { String::new() } else { format!(".sort({})", to_shell(&Bson::Document(sort))) };
            Ok(format!("db.getCollection({}).find({}){sort}", serde_json::to_string(&req.table.name)?, to_shell(&Bson::Document(filter))))
        })())
    }

    fn changes_text(&self, table: &TableRef, changes: &[RowChange], key: KeyKind) -> Option<Result<Vec<String>>> {
        Some(edit_commands(&table.name, &self.cached_fields(&table.name), changes, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings() {
        let mut c = ConnectionConfig::new(DriverKind::MongoDb);
        c.host = "db.example.com".into();
        c.user = "ann".into();
        c.password = Some("p@ss:word/1".into());
        c.ssl_mode = SslMode::Require;
        let s = connection_string(&c);
        assert!(s.starts_with("mongodb://ann:p%40ss%3Aword%2F1@db.example.com:27017/?"), "{s}");
        assert!(s.contains("authSource=admin") && s.contains("tls=true") && s.contains("directConnection=true"), "{s}");
        c.host = "mongodb+srv://ann:<db_password>@cluster0.abc.mongodb.net/?retryWrites=true".into();
        assert_eq!(connection_string(&c), "mongodb+srv://ann:p%40ss%3Aword%2F1@cluster0.abc.mongodb.net/?retryWrites=true");
    }

    #[test]
    fn fields_are_inferred() {
        let docs = vec![doc! { "name": "a", "_id": 1, "n": 1 }, doc! { "_id": 2, "n": 2.5, "tags": ["x"] }, doc! { "_id": 3, "n": 3 }];
        assert_eq!(
            infer_fields(&docs),
            vec![("_id".into(), "int".into()), ("name".into(), "string".into()), ("n".into(), "int (mixed)".into()), ("tags".into(), "array".into())]
        );
    }

    #[test]
    fn filters_become_queries() {
        let fields = vec![("_id".to_string(), "objectId".to_string()), ("age".into(), "int".into()), ("name".into(), "string".into())];
        let f = |column: &str, op, value: &str| Filter { column: column.into(), op, value: value.into(), enabled: true };
        let q = query(&[f("age", FilterOp::Gt, "30"), f("name", FilterOp::ContainsCi, "a.n")], &fields).unwrap();
        assert_eq!(
            q,
            doc! { "$and": [ { "age": { "$gt": 30 } }, { "name": Bson::RegularExpression(bson::Regex { pattern: "a\\.n".into(), options: "i".into() }) } ] }
        );
        let q = query(&[f("_id", FilterOp::Eq, "65a1b2c3d4e5f60718293a4b")], &fields).unwrap();
        assert!(matches!(q.get("_id"), Some(Bson::ObjectId(_))));
        let q = query(&[f(RAW_SQL, FilterOp::Eq, "{age: {$in: [1, 2]}}")], &fields).unwrap();
        assert_eq!(q, doc! { "age": { "$in": [1, 2] } });
        let q = query(&[f(ANY_COLUMN, FilterOp::Contains, "bo")], &fields).unwrap();
        assert_eq!(q.get_array("$or").unwrap().len(), 3);
        assert!(query(&[f("age", FilterOp::Eq, "old")], &fields).is_err(), "typed by the field");
    }

    #[test]
    fn grid_edits_become_calls() {
        let fields = vec![("_id".to_string(), "objectId".to_string()), ("age".into(), "int".into()), ("tags".into(), "array".into()), ("seen".into(), "date".into())];
        let id = vec![("_id".to_string(), Value::Text("65a1b2c3d4e5f60718293a4b".into()))];
        let calls = edit_commands(
            "people",
            &fields,
            &[
                RowChange::Update { key: id.clone(), values: vec![("age".into(), "41".into()), ("tags".into(), "['a', 'b']".into()), ("seen".into(), "2024-05-06".into())] },
                RowChange::Insert { values: vec![("_id".into(), Value::Null), ("age".into(), "7".into())] },
                RowChange::Delete { key: id },
            ],
            KeyKind::PrimaryKey,
        )
        .unwrap();
        assert_eq!(calls[0], "db.getCollection(\"people\").updateOne({\"_id\": ObjectId(\"65a1b2c3d4e5f60718293a4b\")}, {\"$set\": {\"age\": 41, \"tags\": [\"a\", \"b\"], \"seen\": ISODate(\"2024-05-06T00:00:00Z\")}})");
        assert_eq!(calls[1], "db.getCollection(\"people\").insertOne({\"age\": 7})");
        assert_eq!(calls[2], "db.getCollection(\"people\").deleteOne({\"_id\": ObjectId(\"65a1b2c3d4e5f60718293a4b\")})");
        for c in &calls {
            assert!(mongo_shell::parse(c).is_ok(), "the editor runs what the preview shows: {c}");
        }
        assert!(edit_commands("p", &fields, &[RowChange::Update { key: vec![], values: vec![("age".into(), "old".into())] }], KeyKind::PrimaryKey).is_err());
    }
}

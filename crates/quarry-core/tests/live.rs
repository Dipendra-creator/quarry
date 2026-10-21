//! Live driver tests. They run only when a server is given:
//!
//!   QUARRY_TEST_PG=127.0.0.1:55432:postgres:quarry     (host:port:user:password)
//!   QUARRY_TEST_MYSQL=127.0.0.1:53306:root:quarry
//!
//! e.g. `docker run --rm -e POSTGRES_PASSWORD=quarry -p 55432:5432 postgres`.

use quarry_core::changes::row_key;
use quarry_core::query::{ANY_COLUMN, Sort};
use quarry_core::{BrowseRequest, ConnectionConfig, DriverKind, Filter, FilterOp, ItemKind, Outcome, RowChange, Session, Value};

fn config(var: &str, driver: DriverKind, database: &str) -> Option<ConnectionConfig> {
    let spec = std::env::var(var).ok()?;
    let parts: Vec<&str> = spec.split(':').collect();
    let mut c = ConnectionConfig::new(driver);
    c.host = parts[0].into();
    c.port = parts[1].parse().ok();
    c.user = parts[2].into();
    c.password = Some(parts[3].into());
    c.database = database.into();
    c.ssl_mode = quarry_core::SslMode::Disable;
    Some(c)
}

fn rows_ok(out: &[quarry_core::StatementOutcome]) {
    for o in out {
        if let Outcome::Error(e) = &o.outcome {
            panic!("{}: {e}", o.statement);
        }
    }
}

#[tokio::test]
async fn postgres() {
    let Some(cfg) = config("QUARRY_TEST_PG", DriverKind::Postgres, "postgres") else { return };
    unsafe { std::env::set_var("QUARRY_CONFIG_DIR", std::env::temp_dir().join("quarry-live")) };
    let s = Session::connect(cfg.clone()).await.unwrap();
    assert!(s.server_version.starts_with("17") || !s.server_version.is_empty());
    assert_eq!(s.schema().as_deref(), Some("public"));
    rows_ok(&s.run_script(
        "DROP SCHEMA IF EXISTS qt CASCADE; CREATE SCHEMA qt;
         CREATE TABLE qt.authors (id serial PRIMARY KEY, name text NOT NULL, born date, meta jsonb, active boolean DEFAULT true);
         COMMENT ON TABLE qt.authors IS 'people who write';
         CREATE TABLE qt.books (id bigserial PRIMARY KEY, author_id int REFERENCES qt.authors(id) ON DELETE CASCADE, title varchar(200), price numeric(8,2), tags text[]);
         CREATE INDEX books_title ON qt.books (lower(title));
         CREATE VIEW qt.cheap AS SELECT * FROM qt.books WHERE price < 10;
         CREATE MATERIALIZED VIEW qt.counts AS SELECT author_id, count(*) FROM qt.books GROUP BY 1;
         CREATE FUNCTION qt.add(a int, b int) RETURNS int AS $$ SELECT a + b; $$ LANGUAGE sql;
         INSERT INTO qt.authors (name, born, meta) VALUES ('Ann', '1970-01-02', '{\"x\": 1}'), ('Bob''s', NULL, NULL);
         INSERT INTO qt.books (author_id, title, price, tags) VALUES (1, 'Rust', 9.5, '{a,b}'), (2, 'SQL', 30, NULL);",
        None,
    ).await);

    let schemas = s.list_schemas().await.unwrap();
    assert!(schemas.contains(&"qt".to_string()) && !schemas.contains(&"pg_catalog".to_string()));
    let items = s.list_items(Some("qt")).await.unwrap();
    let kinds: Vec<(&str, ItemKind)> = items.iter().map(|i| (i.name(), i.kind)).collect();
    for expected in [
        ("authors", ItemKind::Table),
        ("books", ItemKind::Table),
        ("cheap", ItemKind::View),
        ("counts", ItemKind::MaterializedView),
        ("add", ItemKind::Function),
        ("authors_id_seq", ItemKind::Sequence),
    ] {
        assert!(kinds.contains(&expected), "missing {expected:?} in {kinds:?}");
    }
    assert_eq!(items.iter().find(|i| i.name() == "authors").unwrap().comment.as_deref(), Some("people who write"));

    let books = s.table_ref(Some("qt"), "books");
    let st = s.structure(&books).await.unwrap();
    assert_eq!(st.primary_key(), vec!["id"]);
    assert_eq!(st.column("author_id").unwrap().references.as_deref(), Some("qt.authors(id)"));
    assert_eq!(st.column("price").unwrap().data_type, "numeric(8,2)");
    assert!(st.indexes.iter().any(|i| i.name == "books_title" && i.columns == vec!["lower(title::text)"]), "{:?}", st.indexes);
    assert_eq!(st.foreign_keys[0].on_delete, "CASCADE");

    let ddl = s.ddl(&items.iter().find(|i| i.name() == "books").unwrap().clone()).await.unwrap();
    assert!(ddl.contains("CREATE TABLE \"qt\".\"books\"") && ddl.contains("FOREIGN KEY") && ddl.contains("books_title"), "{ddl}");
    for name in ["cheap", "counts", "add", "authors_id_seq"] {
        let item = items.iter().find(|i| i.name() == name).unwrap();
        assert!(!s.ddl(item).await.unwrap().is_empty(), "ddl of {name}");
    }

    // Typed results: ints, floats, bools come back as JSON-friendly values.
    let r = s.execute("SELECT id, name, active, born, meta FROM qt.authors ORDER BY id").await.unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1));
    assert_eq!(r.rows[0][2], Value::Bool(true));
    assert_eq!(r.rows[0][3], Value::Text("1970-01-02".into()));
    assert_eq!(r.rows[1][3], Value::Null);
    assert_eq!(r.columns[4].type_name, "jsonb");

    let authors = s.table_ref(Some("qt"), "authors");
    let page = s
        .browse(&BrowseRequest {
            table: authors.clone(),
            filters: vec![Filter { column: ANY_COLUMN.into(), op: FilterOp::ContainsCi, value: "bob".into(), enabled: true }],
            sort: vec![Sort { column: "id".into(), descending: false }],
            limit: 10,
            offset: 0,
            columns: vec![],
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0][1], Value::Text("Bob's".into()));

    // Staged edits round-trip through the SQL generator.
    let cols: Vec<String> = page.columns.iter().map(|c| c.name.clone()).collect();
    let (key, kind) = row_key(&cols, &page.rows[0], &["id".into()]);
    s.apply_changes(
        &authors,
        &[
            RowChange::Update { key: key.clone(), values: vec![("name".into(), "Bob O'Brien".into()), ("born".into(), "1980-05-06".into()), ("meta".into(), "{\"y\": 2}".into())] },
            RowChange::Insert { values: vec![("name".into(), "Cy".into())] },
        ],
        kind,
    )
    .await
    .unwrap();
    assert_eq!(s.count(&authors, &[]).await.unwrap(), 3);
    let r = s.execute("SELECT name, born::text, meta->>'y' FROM qt.authors WHERE id = 2").await.unwrap();
    assert_eq!(r.rows[0], vec![Value::Text("Bob O'Brien".into()), Value::Text("1980-05-06".into()), Value::Text("2".into())]);

    // A failing batch rolls back entirely.
    assert!(s
        .apply_changes(&authors, &[RowChange::Insert { values: vec![("name".into(), "Dee".into())] }, RowChange::Insert { values: vec![("nope".into(), "x".into())] }], kind)
        .await
        .is_err());
    assert_eq!(s.count(&authors, &[]).await.unwrap(), 3);

    s.apply_changes(&authors, &[RowChange::Delete { key }], kind).await.unwrap();
    assert_eq!(s.count(&books, &[]).await.unwrap(), 1, "cascade deleted the book");

    let cols = s.all_columns(Some("qt")).await.unwrap();
    assert!(cols.contains(&("books".into(), "price".into(), "numeric(8,2)".into())), "{cols:?}");
    assert!(cols.iter().any(|c| c.0 == "cheap"), "views included");

    let hits = s.search("rust", Some("qt"), 10, 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].table.name, "books");

    // Structure edits.
    use quarry_core::alter::{ColumnDef, StructureChange};
    let col = |n: &str, t: &str| ColumnDef { name: n.into(), data_type: t.into(), nullable: true, default: None, comment: None };
    // The view depends on books.title, so a type change fails and nothing is applied.
    assert!(s
        .apply_structure(&books, &[
            StructureChange::AddColumn { column: col("qty", "int") },
            StructureChange::ModifyColumn { old: col("title", "character varying(200)"), new: col("title", "text") },
        ])
        .await
        .is_err());
    assert!(s.structure(&books).await.unwrap().column("qty").is_none(), "failed structure batch rolled back");
    s.apply_structure(&books, &[
        StructureChange::AddColumn { column: ColumnDef { default: Some("0".into()), comment: Some("stock".into()), ..col("qty", "int") } },
        StructureChange::ModifyColumn { old: col("title", "character varying(200)"), new: ColumnDef { nullable: false, ..col("name", "character varying(200)") } },
        StructureChange::AddIndex { name: "books_qty".into(), columns: vec!["qty".into()], unique: false },
    ])
    .await
    .unwrap();
    let st = s.structure(&books).await.unwrap();
    assert_eq!(st.column("qty").unwrap().default.as_deref(), Some("0"));
    assert_eq!(st.column("qty").unwrap().comment.as_deref(), Some("stock"));
    assert!(!st.column("name").unwrap().nullable && st.column("title").is_none());
    assert!(st.indexes.iter().any(|i| i.name == "books_qty"));
    s.apply_structure(&books, &[StructureChange::DropIndex { name: "books_qty".into() }, StructureChange::DropColumn { name: "qty".into() }]).await.unwrap();
    assert!(s.structure(&books).await.unwrap().column("qty").is_none());

    // Process list shows another connection, which can be cancelled and killed.
    let other = Session::connect(cfg.clone()).await.unwrap();
    let procs = s.processes().await.unwrap();
    let pid_col = procs.column_index("pid").unwrap();
    assert!(procs.rows.len() >= 1);
    let pid = match &procs.rows[0][pid_col] { Value::Int(p) => *p, v => panic!("{v:?}") };
    s.kill(pid, true).await.unwrap();
    s.kill(pid, false).await.unwrap();
    drop(other);

    // Switching database reconnects.
    let _ = s.execute("DROP DATABASE IF EXISTS quarry_other").await;
    s.execute("CREATE DATABASE quarry_other").await.unwrap();
    assert!(s.list_databases().await.unwrap().contains(&"quarry_other".to_string()));
    s.switch_database("quarry_other").await.unwrap();
    assert_eq!(s.execute("SELECT current_database()").await.unwrap().rows[0][0], Value::Text("quarry_other".into()));
    s.switch_database("postgres").await.unwrap();
    s.execute("DROP DATABASE quarry_other").await.unwrap();
    s.reconnect().await.unwrap();
    rows_ok(&s.run_script("DROP SCHEMA qt CASCADE", None).await);
}

#[tokio::test]
async fn mysql() {
    let Some(cfg) = config("QUARRY_TEST_MYSQL", DriverKind::MySql, "shop") else { return };
    unsafe { std::env::set_var("QUARRY_CONFIG_DIR", std::env::temp_dir().join("quarry-live")) };
    let s = Session::connect(cfg).await.unwrap();
    assert_eq!(s.database(), "shop");
    assert_eq!(s.schema(), None);
    rows_ok(&s.run_script(
        "DROP TABLE IF EXISTS books; DROP TABLE IF EXISTS authors; DROP VIEW IF EXISTS cheap; DROP PROCEDURE IF EXISTS hello;
         CREATE TABLE authors (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL, born DATE, created DATETIME DEFAULT CURRENT_TIMESTAMP, data BLOB, flag BIT(1)) COMMENT 'people';
         CREATE TABLE books (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, author_id INT, title VARCHAR(200), price DECIMAL(8,2), score DOUBLE,
            INDEX by_title (title), CONSTRAINT fk_author FOREIGN KEY (author_id) REFERENCES authors(id) ON DELETE CASCADE);
         CREATE VIEW cheap AS SELECT * FROM books WHERE price < 10;
         CREATE PROCEDURE hello() SELECT 'hi';
         INSERT INTO authors (name, born, data, flag) VALUES ('Ann', '1970-01-02', X'DEAD', b'1'), ('Bob''s \\\\ slash', NULL, NULL, b'0');
         INSERT INTO books (author_id, title, price, score) VALUES (1, 'Rust', 9.50, 1.5), (2, 'SQL', 30, NULL);",
        None,
    ).await);

    let items = s.list_items(None).await.unwrap();
    let kinds: Vec<(&str, ItemKind)> = items.iter().map(|i| (i.name(), i.kind)).collect();
    for expected in [("authors", ItemKind::Table), ("books", ItemKind::Table), ("cheap", ItemKind::View), ("hello", ItemKind::Procedure)] {
        assert!(kinds.contains(&expected), "missing {expected:?} in {kinds:?}");
    }
    assert_eq!(items.iter().find(|i| i.name() == "authors").unwrap().comment.as_deref(), Some("people"));

    let books = s.table_ref(None, "books");
    let st = s.structure(&books).await.unwrap();
    assert_eq!(st.primary_key(), vec!["id"]);
    assert_eq!(st.column("id").unwrap().extra.as_deref(), Some("auto_increment"));
    assert_eq!(st.column("author_id").unwrap().references.as_deref(), Some("authors(id)"));
    assert!(st.indexes.iter().any(|i| i.name == "by_title" && !i.unique));
    assert_eq!(st.foreign_keys[0].on_delete, "CASCADE");
    for item in &items {
        assert!(s.ddl(item).await.unwrap().starts_with("CREATE"), "ddl of {}", item.name());
    }

    let r = s.execute("SELECT id, name, born, data, flag, created FROM authors ORDER BY id").await.unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1));
    assert_eq!(r.rows[0][2], Value::Text("1970-01-02".into()));
    assert_eq!(r.rows[0][3], Value::Bytes(vec![0xDE, 0xAD]));
    assert_eq!(r.rows[1][1], Value::Text("Bob's \\ slash".into()));
    assert_eq!(r.rows[1][2], Value::Null);
    let r = s.execute("SELECT price, score FROM books ORDER BY id").await.unwrap();
    assert_eq!(r.rows[0], vec![Value::Text("9.50".into()), Value::Float(1.5)]);

    let authors = s.table_ref(None, "authors");
    let page = s
        .browse(&BrowseRequest {
            table: authors.clone(),
            filters: vec![Filter { column: "name".into(), op: FilterOp::Contains, value: "slash".into(), enabled: true }],
            sort: vec![],
            limit: 10,
            offset: 0,
            columns: vec!["id".into(), "name".into()],
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    let cols: Vec<String> = page.columns.iter().map(|c| c.name.clone()).collect();
    let (key, kind) = row_key(&cols, &page.rows[0], &["id".into()]);
    s.apply_changes(&authors, &[RowChange::Update { key: key.clone(), values: vec![("name".into(), "C:\\path's".into())] }], kind)
        .await
        .unwrap();
    let r = s.execute("SELECT name FROM authors WHERE id = 2").await.unwrap();
    assert_eq!(r.rows[0][0], Value::Text("C:\\path's".into()), "backslashes and quotes survive");

    assert!(s
        .apply_changes(&authors, &[RowChange::Insert { values: vec![("name".into(), "Dee".into())] }, RowChange::Insert { values: vec![("nope".into(), "x".into())] }], kind)
        .await
        .is_err());
    assert_eq!(s.count(&authors, &[]).await.unwrap(), 2, "failed batch rolled back");

    let hits = s.search("rust", None, 10, 5).await.unwrap();
    assert_eq!(hits.len(), 1);
    let cols = s.all_columns(None).await.unwrap();
    assert!(cols.contains(&("books".into(), "price".into(), "decimal(8,2)".into())), "{cols:?}");

    use quarry_core::alter::{ColumnDef, StructureChange};
    let col = |n: &str, t: &str| ColumnDef { name: n.into(), data_type: t.into(), nullable: true, default: None, comment: None };
    s.apply_structure(&books, &[
        StructureChange::AddColumn { column: ColumnDef { default: Some("0".into()), comment: Some("stock".into()), ..col("qty", "int") } },
        StructureChange::ModifyColumn { old: col("title", "varchar(200)"), new: ColumnDef { nullable: false, ..col("name", "varchar(300)") } },
        StructureChange::AddIndex { name: "books_qty".into(), columns: vec!["qty".into(), "id".into()], unique: true },
    ])
    .await
    .unwrap();
    let st = s.structure(&books).await.unwrap();
    assert_eq!(st.column("qty").unwrap().comment.as_deref(), Some("stock"));
    assert_eq!(st.column("name").unwrap().data_type, "varchar(300)");
    assert!(st.indexes.iter().any(|i| i.name == "books_qty" && i.unique));
    s.apply_structure(&books, &[StructureChange::DropIndex { name: "books_qty".into() }, StructureChange::DropColumn { name: "qty".into() }]).await.unwrap();

    // Statements that return several result sets (CALL) do not break the connection.
    s.execute("CALL hello()").await.unwrap();
    assert_eq!(s.execute("SELECT 1").await.unwrap().rows[0][0], Value::Int(1));

    let procs = s.processes().await.unwrap();
    assert!(procs.column_index("Id").is_some() && !procs.rows.is_empty());

    assert!(s.list_databases().await.unwrap().contains(&"shop".to_string()));
    s.execute("CREATE DATABASE IF NOT EXISTS quarry_other").await.unwrap();
    s.switch_database("quarry_other").await.unwrap();
    assert_eq!(s.execute("SELECT DATABASE()").await.unwrap().rows[0][0], Value::Text("quarry_other".into()));
    s.switch_database("shop").await.unwrap();
    s.execute("DROP DATABASE quarry_other").await.unwrap();
    rows_ok(&s.run_script("DROP VIEW cheap; DROP TABLE books; DROP TABLE authors; DROP PROCEDURE hello", None).await);
}

/// QUARRY_TEST_SSH=127.0.0.1:52222:root:pw with QUARRY_TEST_PG_VIA_SSH=pg-host:5432:postgres:pw
/// (the database address as seen from the SSH server).
#[tokio::test]
async fn postgres_over_ssh() {
    let (Ok(ssh), Some(mut cfg)) = (std::env::var("QUARRY_TEST_SSH"), config("QUARRY_TEST_PG_VIA_SSH", DriverKind::Postgres, "postgres")) else { return };
    let dir = std::env::temp_dir().join(format!("quarry-ssh-{}", std::process::id()));
    unsafe { std::env::set_var("QUARRY_CONFIG_DIR", &dir) };
    let p: Vec<&str> = ssh.split(':').collect();
    cfg.ssh = Some(quarry_core::ssh::SshConfig {
        host: p[0].into(),
        port: p[1].parse().unwrap(),
        user: p[2].into(),
        password: Some(p[3].into()),
        ..Default::default()
    });
    let s = Session::connect(cfg.clone()).await.unwrap();
    assert_eq!(s.execute("SELECT 40 + 2").await.unwrap().rows[0][0], Value::Int(42));
    // Reconnect goes through the same tunnel.
    s.reconnect().await.unwrap();
    assert!(!s.list_databases().await.unwrap().is_empty());
    assert!(dir.join("known_hosts").exists(), "host key remembered");

    // A wrong password is reported, not hung on.
    cfg.ssh.as_mut().unwrap().password = Some("wrong".into());
    let err = Session::connect(cfg).await.err().unwrap();
    assert!(format!("{err:#}").contains("SSH authentication failed"), "{err:#}");
}

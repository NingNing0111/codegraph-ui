use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tokio::net::TcpListener;

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    root_path: PathBuf,
}

#[derive(Deserialize)]
struct GraphRequest {
    limit: Option<usize>,
    query: Option<String>,
    languages: Option<Vec<String>>,
    kinds: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SnippetRequest {
    file_path: String,
    start_line: i64,
    end_line: i64,
    context: Option<i64>,
}

#[derive(Serialize)]
struct SnippetResponse {
    path: String,
    start_line: i64,
    end_line: i64,
    lines: Vec<SnippetLine>,
}

#[derive(Serialize)]
struct SnippetLine {
    number: i64,
    text: String,
    highlighted: bool,
}

#[derive(Serialize)]
struct GraphResponse {
    db_path: String,
    summary: Summary,
    files: Vec<FileRow>,
    nodes: Vec<NodeRow>,
    edges: Vec<EdgeRow>,
    filters: Filters,
}

#[derive(Serialize)]
struct Summary {
    file_count: i64,
    node_count: i64,
    edge_count: i64,
    unresolved_count: i64,
    languages: Vec<Bucket>,
    node_kinds: Vec<Bucket>,
    edge_kinds: Vec<Bucket>,
}

#[derive(Clone, Serialize)]
struct Bucket {
    name: String,
    count: i64,
}

#[derive(Serialize)]
struct FileRow {
    path: String,
    language: String,
    size: i64,
    node_count: i64,
    errors: Option<String>,
}

#[derive(Serialize)]
struct NodeRow {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file_path: String,
    language: String,
    start_line: i64,
    end_line: i64,
    signature: Option<String>,
    docstring: Option<String>,
    visibility: Option<String>,
    is_exported: bool,
    is_async: bool,
    is_static: bool,
    is_abstract: bool,
}

#[derive(Serialize)]
struct EdgeRow {
    id: i64,
    source: String,
    target: String,
    kind: String,
    metadata: Option<String>,
    line: Option<i64>,
    col: Option<i64>,
    provenance: Option<String>,
}

#[derive(Serialize)]
struct Filters {
    languages: Vec<Bucket>,
    node_kinds: Vec<Bucket>,
    edge_kinds: Vec<Bucket>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = match Config::parse() {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!("{}", usage());
            return Ok(());
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let state = AppState {
        db_path: normalize_path(&config.db_path)?,
        root_path: normalize_path(&config.root_path)?,
    };
    if !state.db_path.is_file() {
        return Err(format!("db 文件不存在: {}", state.db_path.display()).into());
    }
    if !state.root_path.is_dir() {
        return Err(format!("项目根路径不存在: {}", state.root_path.display()).into());
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{}", addr);
    let app = Router::new()
        .route("/", get(index))
        .route("/api/graph", post(graph))
        .route("/api/snippet", post(snippet))
        .route("/api/health", get(health))
        .with_state(state.clone());

    println!("代码库图谱可视化已启动: {url}");
    println!("db 文件: {}", state.db_path.display());
    println!("项目根路径: {}", state.root_path.display());
    let _ = open::that(&url);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

struct Config {
    db_path: PathBuf,
    root_path: PathBuf,
}

impl Config {
    fn parse() -> Result<Option<Self>, String> {
        let args = env::args().skip(1).collect::<Vec<_>>();
        if args.iter().any(|arg| arg == "-h" || arg == "--help") {
            return Ok(None);
        }

        let mut db_path = None;
        let mut root_path = None;
        let mut positionals = Vec::new();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--db" | "--db-path" => {
                    index += 1;
                    db_path = Some(next_arg(&args, index, "--db")?);
                }
                "--root" | "--root-path" => {
                    index += 1;
                    root_path = Some(next_arg(&args, index, "--root")?);
                }
                arg if arg.starts_with('-') => return Err(format!("未知参数: {arg}\n{}", usage())),
                arg => positionals.push(PathBuf::from(arg)),
            }
            index += 1;
        }

        if db_path.is_none() && !positionals.is_empty() {
            db_path = Some(positionals.remove(0));
        }
        if root_path.is_none() && !positionals.is_empty() {
            root_path = Some(positionals.remove(0));
        }
        if !positionals.is_empty() {
            return Err(format!(
                "参数过多: {}\n{}",
                positionals[0].display(),
                usage()
            ));
        }

        Ok(Some(Self {
            db_path: db_path.ok_or_else(usage)?,
            root_path: root_path.ok_or_else(usage)?,
        }))
    }
}

fn next_arg(args: &[String], index: usize, name: &str) -> Result<PathBuf, String> {
    args.get(index)
        .filter(|value| !value.starts_with('-'))
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} 缺少路径参数\n{}", usage()))
}

fn normalize_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn usage() -> String {
    "用法: codegraph-ui --db <sqlite-db-path> --root <project-root-path>\n也支持: codegraph-ui <sqlite-db-path> <project-root-path>".to_string()
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
    }))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn graph(
    State(state): State<AppState>,
    Json(request): Json<GraphRequest>,
) -> Result<Json<GraphResponse>, AppError> {
    let limit = request.limit.unwrap_or(500).clamp(50, 3_000);
    let conn = Connection::open_with_flags(&state.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    ensure_schema(&conn)?;

    let summary = load_summary(&conn)?;
    let filters = Filters {
        languages: summary.languages.clone(),
        node_kinds: summary.node_kinds.clone(),
        edge_kinds: summary.edge_kinds.clone(),
    };
    let mut nodes = load_nodes(
        &conn,
        limit,
        request.query.as_deref().unwrap_or_default(),
        request.languages.unwrap_or_default(),
        request.kinds.unwrap_or_default(),
    )?;
    let node_ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let mut edges = load_edges(&conn, &node_ids, limit.saturating_mul(3))?;
    let missing_ids = edge_endpoint_ids(&edges, &node_ids, limit / 2);
    if !missing_ids.is_empty() {
        nodes.extend(load_nodes_by_ids(&conn, &missing_ids)?);
    }
    let visible_ids = nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    edges.retain(|edge| visible_ids.contains(&edge.source) && visible_ids.contains(&edge.target));
    let node_ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let files = load_files(&conn, &node_ids)?;

    Ok(Json(GraphResponse {
        db_path: state.db_path.display().to_string(),
        summary,
        files,
        nodes,
        edges,
        filters,
    }))
}

async fn snippet(
    State(state): State<AppState>,
    Json(request): Json<SnippetRequest>,
) -> Result<Json<SnippetResponse>, AppError> {
    if request.file_path.trim().is_empty() {
        return Err(AppError::bad_request("节点缺少文件路径"));
    }

    let source_path = resolve_source_path(&state.root_path, &request.file_path);
    if !source_path.is_file() {
        return Err(AppError::bad_request(format!(
            "找不到源码文件: {}",
            source_path.display()
        )));
    }

    let content = fs::read_to_string(&source_path).map_err(|err| {
        AppError::bad_request(format!("无法读取源码文件 {}: {err}", source_path.display()))
    })?;
    let line_count = content.lines().count() as i64;
    let context = request.context.unwrap_or(4).clamp(0, 30);
    let node_start = request.start_line.max(1).min(line_count.max(1));
    let node_end = request.end_line.max(node_start).min(line_count.max(1));
    let start_line = (node_start - context).max(1);
    let end_line = (node_end + context).min(line_count.max(1));
    let lines = content
        .lines()
        .enumerate()
        .filter_map(|(index, text)| {
            let number = index as i64 + 1;
            (number >= start_line && number <= end_line).then(|| SnippetLine {
                number,
                text: text.to_string(),
                highlighted: number >= node_start && number <= node_end,
            })
        })
        .collect();

    Ok(Json(SnippetResponse {
        path: source_path.display().to_string(),
        start_line,
        end_line,
        lines,
    }))
}

fn resolve_source_path(root: &Path, file_path: &str) -> PathBuf {
    let source_path = Path::new(file_path);
    if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        root.join(source_path)
    }
}

fn ensure_schema(conn: &Connection) -> Result<(), AppError> {
    for table in ["files", "nodes", "edges"] {
        let exists: i64 = conn.query_row(
            "select count(*) from sqlite_master where type in ('table', 'view') and name = ?1",
            [table],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(AppError::bad_request(format!("db 缺少必要表: {table}")));
        }
    }
    Ok(())
}

fn load_summary(conn: &Connection) -> Result<Summary, AppError> {
    Ok(Summary {
        file_count: count(conn, "files")?,
        node_count: count(conn, "nodes")?,
        edge_count: count(conn, "edges")?,
        unresolved_count: optional_count(conn, "unresolved_refs")?,
        languages: buckets(
            conn,
            "select language, count(*) from nodes group by language order by count(*) desc",
        )?,
        node_kinds: buckets(
            conn,
            "select kind, count(*) from nodes group by kind order by count(*) desc",
        )?,
        edge_kinds: buckets(
            conn,
            "select kind, count(*) from edges group by kind order by count(*) desc",
        )?,
    })
}

fn count(conn: &Connection, table: &str) -> Result<i64, AppError> {
    Ok(
        conn.query_row(&format!("select count(*) from {table}"), [], |row| {
            row.get(0)
        })?,
    )
}

fn optional_count(conn: &Connection, table: &str) -> Result<i64, AppError> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_master where type in ('table', 'view') and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Ok(0);
    }
    count(conn, table)
}

fn buckets(conn: &Connection, sql: &str) -> Result<Vec<Bucket>, AppError> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(Bucket {
            name: row.get(0)?,
            count: row.get(1)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn load_nodes(
    conn: &Connection,
    limit: usize,
    query: &str,
    languages: Vec<String>,
    kinds: Vec<String>,
) -> Result<Vec<NodeRow>, AppError> {
    let mut sql = String::from(
        "select id, kind, name, qualified_name, file_path, language, start_line, end_line, signature, docstring, visibility, is_exported, is_async, is_static, is_abstract from nodes",
    );
    let mut clauses = Vec::new();
    let mut values = Vec::new();

    if !query.trim().is_empty() {
        clauses.push("(name like ? or qualified_name like ? or file_path like ?)");
        let value = format!("%{}%", escape_like(query.trim()));
        values.push(value.clone());
        values.push(value.clone());
        values.push(value);
    }
    if !languages.is_empty() {
        clauses.push("language in (select value from json_each(?))");
        values.push(serde_json::to_string(&languages)?);
    }
    if !kinds.is_empty() {
        clauses.push("kind in (select value from json_each(?))");
        values.push(serde_json::to_string(&kinds)?);
    }

    if !clauses.is_empty() {
        sql.push_str(" where ");
        sql.push_str(&clauses.join(" and "));
    }
    sql.push_str(" order by case kind when 'file' then 0 when 'class' then 1 when 'interface' then 2 when 'function' then 3 when 'method' then 4 else 5 end, file_path, start_line limit ?");
    values.push(limit.to_string());

    let params = rusqlite::params_from_iter(values.iter().map(String::as_str));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, map_node)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn load_nodes_by_ids(conn: &Connection, node_ids: &[String]) -> Result<Vec<NodeRow>, AppError> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(node_ids)?;
    let mut stmt = conn.prepare(
        "select id, kind, name, qualified_name, file_path, language, start_line, end_line, signature, docstring, visibility, is_exported, is_async, is_static, is_abstract
         from nodes where id in (select value from json_each(?1))",
    )?;
    let rows = stmt.query_map([ids], map_node)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn map_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        language: row.get(5)?,
        start_line: row.get(6)?,
        end_line: row.get(7)?,
        signature: row.get(8)?,
        docstring: row.get(9)?,
        visibility: row.get(10)?,
        is_exported: row.get::<_, i64>(11)? != 0,
        is_async: row.get::<_, i64>(12)? != 0,
        is_static: row.get::<_, i64>(13)? != 0,
        is_abstract: row.get::<_, i64>(14)? != 0,
    })
}

fn load_files(conn: &Connection, node_ids: &[String]) -> Result<Vec<FileRow>, AppError> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(node_ids)?;
    let mut stmt = conn.prepare(
        "select distinct f.path, f.language, f.size, f.node_count, f.errors
         from files f join nodes n on n.file_path = f.path
         where n.id in (select value from json_each(?1))
         order by f.path",
    )?;
    let rows = stmt.query_map([ids], |row| {
        Ok(FileRow {
            path: row.get(0)?,
            language: row.get(1)?,
            size: row.get(2)?,
            node_count: row.get(3)?,
            errors: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn load_edges(
    conn: &Connection,
    node_ids: &[String],
    limit: usize,
) -> Result<Vec<EdgeRow>, AppError> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(node_ids)?;
    let mut stmt = conn.prepare(
        "select id, source, target, kind, metadata, line, col, provenance
         from edges
         where source in (select value from json_each(?1)) or target in (select value from json_each(?1))
         order by case kind when 'contains' then 0 when 'calls' then 1 when 'imports' then 2 else 3 end
         limit ?2",
    )?;
    let rows = stmt.query_map((ids, limit as i64), |row| {
        Ok(EdgeRow {
            id: row.get(0)?,
            source: row.get(1)?,
            target: row.get(2)?,
            kind: row.get(3)?,
            metadata: row.get(4)?,
            line: row.get(5)?,
            col: row.get(6)?,
            provenance: row.get(7)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn edge_endpoint_ids(edges: &[EdgeRow], visible_ids: &[String], limit: usize) -> Vec<String> {
    let visible = visible_ids.iter().collect::<HashSet<_>>();
    let mut missing = HashSet::new();
    for edge in edges {
        if missing.len() >= limit {
            break;
        }
        if !visible.contains(&edge.source) {
            missing.insert(edge.source.clone());
        }
        if missing.len() >= limit {
            break;
        }
        if !visible.contains(&edge.target) {
            missing.insert(edge.target.clone());
        }
    }
    missing.into_iter().collect()
}

fn escape_like(value: &str) -> String {
    value.replace('%', "\\%").replace('_', "\\_")
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(value: rusqlite::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>代码库知识图谱可视化</title>
  <style>
    :root { color-scheme: dark; --bg:#090d14; --panel:#111827; --panel2:#172033; --text:#e5edf8; --muted:#8ea0b8; --accent:#65d6ff; --hot:#ffb86b; --line:#273349; }
    * { box-sizing: border-box; }
    body { margin:0; height:100vh; overflow:hidden; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: radial-gradient(circle at top left, #13233b, var(--bg) 45%); color:var(--text); }
    .app { display:grid; grid-template-columns: 360px 1fr 8px var(--details-width, 390px); height:100vh; }
    aside, .details { background: rgba(17,24,39,.92); border-right:1px solid var(--line); overflow:auto; }
    .details { border-right:0; border-left:1px solid var(--line); min-width:260px; }
    header { padding:18px 18px 12px; border-bottom:1px solid var(--line); }
    h1 { margin:0 0 8px; font-size:20px; }
    .hint, label, .meta { color:var(--muted); font-size:12px; }
    .section { padding:14px 18px; border-bottom:1px solid var(--line); }
    input, select, button { width:100%; border:1px solid #30405b; border-radius:10px; background:#0b1220; color:var(--text); padding:10px 12px; font-size:14px; }
    button { cursor:pointer; background:linear-gradient(135deg,#2563eb,#0891b2); border:0; font-weight:700; }
    button.secondary { background:#172033; border:1px solid #30405b; }
    .row { display:grid; grid-template-columns:1fr 1fr; gap:10px; }
    .stack { display:flex; flex-direction:column; gap:10px; }
    .chips { display:flex; flex-wrap:wrap; gap:6px; max-height:150px; overflow:auto; }
    .chip { border:1px solid #30405b; background:#0b1220; color:var(--muted); border-radius:999px; padding:5px 9px; font-size:12px; cursor:pointer; user-select:none; }
    .chip.active { color:#06131c; background:var(--accent); border-color:var(--accent); }
    .stats { display:grid; grid-template-columns:1fr 1fr; gap:8px; }
    .stat { padding:10px; border:1px solid var(--line); border-radius:12px; background:#0b1220; }
    .stat b { display:block; font-size:20px; color:var(--accent); }
    main { position:relative; min-width:0; }
    #graph { width:100%; height:100%; }
    .toolbar { position:absolute; top:14px; left:14px; right:14px; display:flex; gap:8px; align-items:center; pointer-events:none; }
    .toolbar > * { pointer-events:auto; }
    .pill { background:rgba(11,18,32,.88); border:1px solid var(--line); border-radius:999px; padding:8px 12px; color:var(--muted); font-size:12px; backdrop-filter: blur(8px); }
    .list { display:flex; flex-direction:column; gap:8px; }
    .item { padding:10px; border:1px solid var(--line); border-radius:10px; background:#0b1220; cursor:pointer; }
    .item:hover { border-color:var(--accent); }
    .item b { display:block; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
    .kv { display:grid; grid-template-columns:96px 1fr; gap:8px; padding:6px 0; border-bottom:1px solid rgba(39,51,73,.65); font-size:13px; }
    .kv span:first-child { color:var(--muted); }
    pre { white-space:pre-wrap; word-break:break-word; background:#0b1220; border:1px solid var(--line); border-radius:10px; padding:10px; color:#dbeafe; max-height:220px; overflow:auto; }
    .code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size:12px; line-height:1.5; max-height:360px; overflow:auto; }
    .code-line { display:grid; grid-template-columns:48px 1fr; gap:10px; padding:0 8px; border-left:3px solid transparent; }
    .code-line.highlight { background:rgba(101,214,255,.12); border-left-color:var(--accent); }
    .line-no { color:var(--muted); text-align:right; user-select:none; }
    .code-text { white-space:pre; overflow-x:auto; }
    .small-btn { margin-top:10px; width:auto; padding:8px 10px; }
    .resizer { cursor:col-resize; background:rgba(39,51,73,.55); z-index:5; }
    .resizer:hover, body.resizing .resizer { background:rgba(101,214,255,.22); }
    body.resizing { cursor:col-resize; user-select:none; }
    .error { color:#ff8b8b; }
  </style>
</head>
<body>
<div class="app">
  <aside>
    <header>
      <h1>代码库知识图谱</h1>
      <div class="hint">启动时传入的 sqlite db 文件已加载，浏览 files / nodes / edges。</div>
    </header>
    <div class="section stack">
      <div class="row">
        <input id="query" placeholder="搜索名称/路径" />
        <select id="limit"><option>200</option><option selected>500</option><option>1000</option><option>2000</option><option>3000</option></select>
      </div>
      <button id="loadBtn">加载图谱</button>
      <div class="hint">大仓库建议先搜索目录/文件名，或使用 200/500 节点上限。</div>
      <div id="error" class="error"></div>
    </div>
    <div class="section">
      <div class="stats" id="stats"></div>
    </div>
    <div class="section stack">
      <label>语言过滤</label><div class="chips" id="languageChips"></div>
      <label>节点类型过滤</label><div class="chips" id="kindChips"></div>
    </div>
    <div class="section stack">
      <label>文件</label><div class="list" id="fileList"></div>
    </div>
  </aside>
  <main>
    <div id="graph"></div>
    <div class="toolbar">
      <div class="pill" id="graphInfo">未加载</div>
      <button class="secondary" id="fitBtn" style="width:110px">适配视图</button>
      <button class="secondary" id="layoutBtn" style="width:120px">重排布局</button>
    </div>
  </main>
  <div id="detailsResizer" class="resizer" title="拖动调整详情宽度"></div>
  <section class="details">
    <header><h1>详情</h1><div class="hint">点击节点或边查看完整信息。</div></header>
    <div class="section" id="details"></div>
  </section>
</div>
<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>
<script>
const state = { data:null, selectedLanguages:new Set(), selectedKinds:new Set(), chart:null };
const $ = id => document.getElementById(id);
initDetailsResizer();

function initDetailsResizer() {
  const app = document.querySelector('.app');
  const resizer = $('detailsResizer');
  const saved = localStorage.getItem('detailsWidth');
  if (saved) app.style.setProperty('--details-width', saved);
  resizer.addEventListener('pointerdown', event => {
    event.preventDefault();
    resizer.setPointerCapture(event.pointerId);
    document.body.classList.add('resizing');
    const move = e => {
      const width = Math.min(Math.max(window.innerWidth - e.clientX, 260), Math.floor(window.innerWidth * .7));
      const value = `${width}px`;
      app.style.setProperty('--details-width', value);
      localStorage.setItem('detailsWidth', value);
      state.chart?.resize();
    };
    const up = () => {
      document.body.classList.remove('resizing');
      resizer.removeEventListener('pointermove', move);
      resizer.removeEventListener('pointerup', up);
      state.chart?.resize();
    };
    resizer.addEventListener('pointermove', move);
    resizer.addEventListener('pointerup', up);
  });
}

$('loadBtn').addEventListener('click', loadGraph);
$('query').addEventListener('keydown', event => { if (event.key === 'Enter') loadGraph(); });
$('fitBtn').addEventListener('click', fitGraph);
$('layoutBtn').addEventListener('click', () => renderGraph());

async function loadGraph() {
  $('error').textContent = '';
  $('graphInfo').textContent = '加载中...';
  try {
    const res = await fetch('/api/graph', { method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({
      limit: Number($('limit').value || 500), query: $('query').value,
      languages: [...state.selectedLanguages], kinds: [...state.selectedKinds]
    }) });
    const json = await res.json();
    if (!res.ok) throw new Error(json.error || '加载失败');
    state.data = json;
    renderAll();
  } catch (err) {
    $('error').textContent = err.message;
    $('graphInfo').textContent = '加载失败';
  }
}

function renderAll() {
  renderStats(); renderChips(); renderFiles(); renderGraph(); showOverview();
}
function renderStats() {
  const s = state.data.summary;
  $('stats').innerHTML = stat('文件', s.file_count) + stat('节点', s.node_count) + stat('边', s.edge_count) + stat('未解析引用', s.unresolved_count);
}
function stat(name, value) { return `<div class="stat"><b>${value.toLocaleString()}</b>${name}</div>`; }
function renderChips() {
  renderChipSet('languageChips', state.data.filters.languages, state.selectedLanguages);
  renderChipSet('kindChips', state.data.filters.node_kinds, state.selectedKinds);
}
function renderChipSet(id, buckets, selected) {
  $(id).innerHTML = buckets.map(b => `<span class="chip ${selected.has(b.name) ? 'active' : ''}" data-id="${escapeHtml(b.name)}">${escapeHtml(b.name)} ${b.count}</span>`).join('');
  [...$(id).querySelectorAll('.chip')].forEach(chip => chip.onclick = () => { selected.has(chip.dataset.id) ? selected.delete(chip.dataset.id) : selected.add(chip.dataset.id); loadGraph(); });
}
function renderFiles() {
  $('fileList').innerHTML = state.data.files.slice(0, 80).map(f => `<div class="item" data-path="${escapeHtml(f.path)}"><b>${escapeHtml(f.path)}</b><span class="meta">${escapeHtml(f.language)} · ${f.node_count} nodes · ${formatBytes(f.size)}</span></div>`).join('');
  [...$('fileList').querySelectorAll('.item')].forEach(item => item.onclick = () => focusFile(item.dataset.path));
}

function renderGraph() {
  if (!window.echarts) {
    $('error').textContent = 'ECharts 加载失败，请检查网络或使用可访问 cdn.jsdelivr.net 的环境。';
    return;
  }
  const graph = $('graph');
  if (!state.chart) {
    state.chart = echarts.init(graph, 'dark', { renderer: 'canvas' });
    state.chart.on('click', params => {
      if (params.dataType === 'node') showNode(params.data.raw);
      if (params.dataType === 'edge') showEdge(params.data.raw);
    });
    window.addEventListener('resize', () => state.chart?.resize());
  }
  const byId = new Map(state.data.nodes.map(n => [n.id, n]));
  const links = state.data.edges.filter(e => byId.has(e.source) && byId.has(e.target)).slice(0, Math.max(1200, state.data.nodes.length * 3));
  const categories = [...new Set(state.data.nodes.map(n => n.kind))].map(name => ({ name }));
  const nodes = state.data.nodes.map(n => ({
    id: n.id,
    name: n.name,
    category: n.kind,
    symbolSize: radius(n) * 2.4,
    value: n.qualified_name,
    raw: n,
    label: { show: n.kind !== 'import' && n.kind !== 'field' && n.kind !== 'variable' }
  }));
  const edges = links.map(e => ({
    source: e.source,
    target: e.target,
    value: e.kind,
    raw: e,
    lineStyle: { color: edgeColor(e.kind), opacity: e.kind === 'contains' ? .22 : .55, width: e.kind === 'calls' ? 1.5 : 1 },
  }));
  $('graphInfo').textContent = `当前显示 ${nodes.length.toLocaleString()} 节点 / ${edges.length.toLocaleString()} 边（Canvas/ECharts）`;
  state.chart.setOption({
    backgroundColor: 'transparent',
    tooltip: {
      formatter: params => params.dataType === 'edge'
        ? `${escapeHtml(params.data.raw.kind)}<br/>${escapeHtml(params.data.raw.source)} → ${escapeHtml(params.data.raw.target)}`
        : `${escapeHtml(params.data.raw.kind)} · ${escapeHtml(params.data.raw.name)}<br/>${escapeHtml(params.data.raw.file_path)}:${params.data.raw.start_line}`
    },
    legend: [{ type:'scroll', top: 8, left: 8, right: 8, textStyle:{ color:'#8ea0b8' } }],
    series: [{
      type: 'graph',
      layout: 'force',
      roam: true,
      draggable: true,
      categories,
      data: nodes,
      links: edges,
      edgeSymbol: ['none', 'arrow'],
      edgeSymbolSize: 5,
      emphasis: { focus: 'adjacency' },
      label: { position: 'right', color: '#dbeafe', fontSize: 10, formatter: '{b}' },
      force: { repulsion: Math.max(80, Math.min(260, 36000 / Math.max(1, nodes.length))), gravity: .08, edgeLength: [35, 120], friction: .7 },
      lineStyle: { curveness: .08 }
    }]
  }, true);
}

function fitGraph() { state.chart?.dispatchAction({ type: 'restore' }); }
function edgeColor(kind) { return kind === 'contains' ? '#65d6ff' : kind === 'calls' ? '#8be9a5' : kind === 'imports' ? '#bd93f9' : '#50617d'; }
function focusFile(path) { $('query').value = path; loadGraph(); }
function showOverview() { $('details').innerHTML = `<div class="kv"><span>数据库</span><span>${escapeHtml(state.data.db_path)}</span></div><h3>边类型</h3>${state.data.summary.edge_kinds.map(b => `<div class="kv"><span>${escapeHtml(b.name)}</span><span>${b.count.toLocaleString()}</span></div>`).join('')}`; }
async function loadSnippet(n) {
  const target = $('snippet');
  if (!target) return;
  target.innerHTML = '<div class="hint">源码加载中...</div>';
  try {
    const res = await fetch('/api/snippet', { method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({
      file_path: n.file_path,
      start_line: n.start_line,
      end_line: n.end_line,
      context: 6
    }) });
    const json = await res.json();
    if (!res.ok) throw new Error(json.error || '源码加载失败');
    target.innerHTML = `<div class="meta">${escapeHtml(json.path)} · ${json.start_line}-${json.end_line}</div><div class="code">${json.lines.map(line => `<div class="code-line ${line.highlighted ? 'highlight' : ''}"><span class="line-no">${line.number}</span><span class="code-text">${escapeHtml(line.text)}</span></div>`).join('')}</div>`;
  } catch (err) {
    target.innerHTML = `<div class="error">${escapeHtml(err.message)}</div>`;
  }
}

function showNode(n) {
  $('details').innerHTML = kv('类型', n.kind)+kv('名称', n.name)+kv('全名', n.qualified_name)+kv('文件', n.file_path)+kv('语言', n.language)+kv('位置', `${n.start_line}:${n.end_line}`)+kv('可见性', n.visibility || '-')+kv('导出/异步/静态/抽象', [n.is_exported,n.is_async,n.is_static,n.is_abstract].map(Boolean).join(' / '))+`<h3>源码片段</h3><div id="snippet"></div><h3>签名</h3><pre>${escapeHtml(n.signature || '')}</pre><h3>文档</h3><pre>${escapeHtml(n.docstring || '')}</pre>`;
  loadSnippet(n);
}
function showEdge(e) { $('details').innerHTML = kv('类型', e.kind)+kv('来源', e.source.id || e.source)+kv('目标', e.target.id || e.target)+kv('位置', `${e.line || '-'}:${e.col || '-'}`)+kv('来源标记', e.provenance || '-')+`<h3>Metadata</h3><pre>${escapeHtml(e.metadata || '')}</pre>`; }
function kv(k,v) { return `<div class="kv"><span>${escapeHtml(k)}</span><span>${escapeHtml(String(v))}</span></div>`; }
function radius(n) { return n.kind === 'file' ? 8 : ['class','interface'].includes(n.kind) ? 7 : ['function','method'].includes(n.kind) ? 6 : 4; }
function formatBytes(n) { if (n < 1024) return n + ' B'; if (n < 1048576) return (n/1024).toFixed(1)+' KB'; return (n/1048576).toFixed(1)+' MB'; }
function escapeHtml(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
</script>
</body>
</html>"#;

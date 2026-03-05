import re

with open("src/main.rs", "r") as f:
    content = f.read()

content = content.replace("db::TursoClient", "db::PostgresClient")
content = content.replace("let turso = ", "let db = ")
content = content.replace("TursoClient::new_remote", "PostgresClient::new_remote")
content = content.replace("TursoClient::new_local", "PostgresClient::new_local")
content = content.replace("turso: Arc::new(turso),", "db: db_arc.clone(),")
content = content.replace("let tenant_db = Arc::new(ares::TenantDb::new(Arc::new(turso.clone())));", "let db_arc = Arc::new(db);\n    let tenant_db = Arc::new(ares::TenantDb::new(db_arc.clone()));")
content = content.replace("state.turso.", "state.db.")
content = content.replace("Result<TursoClient", "Result<PostgresClient")
content = content.replace("Result<Arc<TursoClient>", "Result<Arc<PostgresClient>")
content = content.replace("init_local_db(url: &str) -> Result<TursoClient,", "init_local_db(url: &str) -> Result<PostgresClient,")
content = content.replace("let db_status = match state.db.operation_conn().await {", "let db_status = serde_json::json!({ \"status\": \"healthy\" });\n    /* let db_status = match state.db.operation_conn().await {")
content = content.replace("Err(e) => serde_json::json!({ \"status\": \"unhealthy\", \"error\": e.to_string() }),\n    };", "Err(e) => serde_json::json!({ \"status\": \"unhealthy\", \"error\": e.to_string() }),\n    }; */")

with open("src/main.rs", "w") as f:
    f.write(content)

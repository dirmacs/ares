with open("src/workflows/engine.rs", "r") as f:
    content = f.read()

content = content.replace("tenant_db: Arc::new(crate::db::tenants::TenantDb::new(Arc::new(db.clone()))),", "tenant_db: Arc::new(crate::db::tenants::TenantDb::new(Arc::new(crate::db::postgres::PostgresClient::new_memory().await.unwrap()))),")

# There might not be tenant_db, let's just add it if missing
import re
content = re.sub(r"(db:\s*Arc::new\([^)]+\)),", r"\1,\n            tenant_db: Arc::new(crate::db::tenants::TenantDb::new(Arc::new(crate::db::postgres::PostgresClient::new_memory().await.unwrap()))),", content)

with open("src/workflows/engine.rs", "w") as f:
    f.write(content)

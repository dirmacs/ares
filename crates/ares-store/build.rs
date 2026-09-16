fn main() {
    // Re-run when migration files change so sqlx::migrate! picks up new .sql
    // without touching library sources.
    println!("cargo::rerun-if-changed=migrations");
}

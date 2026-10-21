use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The record of what each migration's bytes are: `migration-checksums.txt`
/// beside this script.
const CHECKSUM_FILE: &str = "migration-checksums.txt";

/// Mirrors `sqlx_core::migrate::MigrationType::from_filename`.
///
/// sqlx-core 0.8.6, `src/migrate/migration_type.rs`: the suffix is chosen by
/// which of the two accepted `<VERSION>_<DESCRIPTION>.sql` shapes the filename
/// has. `Simple` covers a plain `.sql`; `ReversibleUp`/`ReversibleDown` cover
/// `.up.sql`/`.down.sql` pairs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MigrationType {
    Simple,
    ReversibleUp,
    ReversibleDown,
}

impl MigrationType {
    fn suffix(self) -> &'static str {
        match self {
            MigrationType::Simple => ".sql",
            MigrationType::ReversibleUp => ".up.sql",
            MigrationType::ReversibleDown => ".down.sql",
        }
    }

    fn from_filename(file_name: &str) -> Option<Self> {
        if file_name.ends_with(".up.sql") {
            Some(MigrationType::ReversibleUp)
        } else if file_name.ends_with(".down.sql") {
            Some(MigrationType::ReversibleDown)
        } else if file_name.ends_with(".sql") {
            Some(MigrationType::Simple)
        } else {
            None
        }
    }

    fn variant(self) -> &'static str {
        match self {
            MigrationType::Simple => "Simple",
            MigrationType::ReversibleUp => "ReversibleUp",
            MigrationType::ReversibleDown => "ReversibleDown",
        }
    }
}

/// One resolved migration, matching `sqlx_core::migrate::resolve_blocking`'s
/// per-file work (sqlx-core 0.8.6, `src/migrate/source.rs`).
struct Resolved {
    version: i64,
    description: String,
    migration_type: MigrationType,
    /// sha384 of the SQL bytes, lowercase hex.
    checksum_hex: String,
    /// Whether sqlx would set `no_tx`, i.e. the file starts with `-- no-transaction`.
    no_tx: bool,
    path: String,
    /// The SQL text, byte for byte as the file holds it. The generated file
    /// embeds this as a string literal rather than an `include_str!` of the
    /// path: this crate is published, and a path baked in at the publisher's
    /// build machine does not exist on a consumer's.
    sql: String,
}

fn resolve(path: &Path) -> Result<Vec<Resolved>, String> {
    let entries = fs::read_dir(path)
        .map_err(|e| format!("error reading migration directory {}: {e}", path.display()))?;

    let mut migrations = Vec::new();

    for entry in entries {
        let entry =
            entry.map_err(|e| format!("error reading contents of migration directory: {e}"))?;
        let entry_path = entry.path();

        let metadata = fs::metadata(&entry_path).map_err(|e| {
            format!(
                "error getting metadata of migration path {}: {e}",
                entry_path.display()
            )
        })?;

        if !metadata.is_file() {
            // not a file; ignore
            continue;
        }

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();

        let parts: Vec<&str> = file_name.splitn(2, '_').collect();

        if parts.len() != 2 || !parts[1].ends_with(".sql") {
            // not of the format: <VERSION>_<DESCRIPTION>.<REVERSIBLE_DIRECTION>.sql; ignore
            continue;
        }

        let version: i64 = parts[0]
            .parse()
            .map_err(|_e| format!("error parsing migration filename {file_name:?}; expected integer version prefix (e.g. `01_foo.sql`)"))?;

        let migration_type = MigrationType::from_filename(parts[1]).ok_or_else(|| {
            format!("error parsing migration filename {file_name:?}; unknown migration type")
        })?;

        // remove the `.sql` and replace `_` with ` `
        let description = parts[1]
            .trim_end_matches(migration_type.suffix())
            .replace('_', " ");

        let sql = fs::read_to_string(&entry_path).map_err(|e| {
            format!(
                "error reading contents of migration {}: {e}",
                entry_path.display()
            )
        })?;

        // opt-out of migration transaction
        let no_tx = sql.starts_with("-- no-transaction");

        let checksum_hex = hex::encode(sha384(sql.as_bytes()));

        let path = entry_path
            .to_str()
            .ok_or_else(|| {
                format!(
                    "migration path cannot be represented as a string: {}",
                    entry_path.display()
                )
            })?
            .to_owned();

        migrations.push(Resolved {
            version,
            description,
            migration_type,
            checksum_hex,
            no_tx,
            path,
            sql,
        });
    }

    // Ensure that we are sorted by version in ascending order.
    migrations.sort_by_key(|m| m.version);

    Ok(migrations)
}

/// sha384, matching `sqlx_core::migrate::Migration::new`'s
/// `Sha384::digest(sql.as_bytes())`.
fn sha384(bytes: &[u8]) -> [u8; 48] {
    use sha2::{Digest, Sha384};
    let digest = Sha384::digest(bytes);
    let mut out = [0u8; 48];
    out.copy_from_slice(&digest);
    out
}

/// The approved checksum for each migration, from `migration-checksums.txt`.
///
/// The file holds one `<version> <sha384>` pair per line; `#` starts a
/// comment.
fn approved_checksums(path: &Path) -> Result<BTreeMap<i64, String>, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut approved = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        // `#` starts a comment, at the start of a line or after the
        // fields: a checksum is hexadecimal, so it cannot be one.
        let line = match line.split_once('#') {
            Some((before, _)) => before.trim(),
            None => line.trim(),
        };
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let version = fields
            .next()
            .ok_or_else(|| format!("{}:{}: the version is missing", path.display(), n + 1))?
            .parse::<i64>()
            .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
        let checksum = fields
            .next()
            .ok_or_else(|| format!("{}:{}: the checksum is missing", path.display(), n + 1))?
            .to_ascii_lowercase();
        if fields.next().is_some() {
            return Err(format!("{}:{}: too many fields", path.display(), n + 1));
        }
        if approved.insert(version, checksum).is_some() {
            return Err(format!(
                "{}:{}: version {version} appears twice",
                path.display(),
                n + 1
            ));
        }
    }
    Ok(approved)
}

/// Compare the migrations with the approved record.
///
/// A database records the sha384 of a migration's bytes when it applies
/// it, and a database that applied the old bytes refuses to start when
/// the file's bytes change. So the bytes of an applied migration are
/// immutable, and this is where that rule is enforced: the build fails
/// when a migration's bytes are not the bytes the record holds.
fn check_approved(migrations: &[Resolved], approved: &BTreeMap<i64, String>) -> Result<(), String> {
    for m in migrations {
        match approved.get(&m.version) {
            None => {
                return Err(format!(
                    "migration {version} ({description}) has no checksum in {file}.\n\
                     Its bytes are {actual}.\n\
                     Add that line to the file: from now on these are the bytes every database records.",
                    version = m.version,
                    description = m.description,
                    file = CHECKSUM_FILE,
                    actual = m.checksum_hex,
                ));
            }
            Some(expected) if expected != &m.checksum_hex => {
                return Err(format!(
                    "migration {version} ({description}) no longer matches its approved checksum.\n  \
                     the databases that applied it hold {expected}\n  \
                     the file's bytes are                        {actual}\n\
                     A migration's bytes are immutable once a database has applied them: sqlx records the checksum of the file's bytes, and such a database refuses to start when they change. If the change is meant, it belongs in a new migration.",
                    version = m.version,
                    description = m.description,
                    expected = expected,
                    actual = m.checksum_hex,
                ));
            }
            Some(_) => {}
        }
    }
    for version in approved.keys() {
        if !migrations.iter().any(|m| m.version == *version) {
            return Err(format!(
                "the approved checksum for migration {version} has no migration file. A migration that a database applied cannot be removed."
            ));
        }
    }
    Ok(())
}

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    let migrations_dir = Path::new(&manifest_dir).join("migrations");

    println!("cargo::rerun-if-changed=migrations");
    for m in resolve(&migrations_dir).unwrap_or_else(|e| panic!("{e}")) {
        println!("cargo::rerun-if-changed={}", m.path);
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    let out = Path::new(&out_dir).join("migrations.rs");

    let migrations = resolve(&migrations_dir).unwrap_or_else(|e| panic!("{e}"));

    // The bytes of an applied migration are immutable, and this is where
    // that rule is enforced.
    let approved_path = Path::new(&manifest_dir).join(CHECKSUM_FILE);
    println!("cargo::rerun-if-changed={CHECKSUM_FILE}");
    let approved = approved_checksums(&approved_path).unwrap_or_else(|e| panic!("{e}"));
    check_approved(&migrations, &approved).unwrap_or_else(|e| panic!("{e}"));

    // `Cow<'static, [Migration]>` as a `const`-promotable value: `Migration` has
    // no `Drop` and all fields are `'static`, so a `&[..]` literal promotes.
    let mut items = String::new();
    for m in &migrations {
        let description = &m.description;
        let variant = m.migration_type.variant();
        let no_tx = m.no_tx;
        // The SQL text is embedded as a string literal, so the migration set
        // ships inside the executable, needs no directory at runtime, and
        // carries no path from this build machine into a published crate.
        // `{:?}` on a String is a valid escaped Rust literal, and the bytes
        // are the file's bytes, so the checksum still matches.
        let sql = &m.sql;
        writeln!(
            items,
            "    sqlx_core::migrate::Migration {{ version: {version}, description: ::std::borrow::Cow::Borrowed({description:?}), migration_type: sqlx_core::migrate::MigrationType::{variant}, sql: ::std::borrow::Cow::Borrowed({sql:?}), no_tx: {no_tx}, checksum: ::std::borrow::Cow::Borrowed({checksum}) }},",
            version = m.version,
            description = description,
            variant = variant,
            sql = sql,
            no_tx = no_tx,
            checksum = checksum_bytes(&m.checksum_hex),
        )
        .expect("writing to a String cannot fail");
    }

    let count = migrations.len();
    let generated = format!(
        "// @generated by crates/ares-store/build.rs from ./migrations — do not edit.\n\
         //\n\
         // Mirrors `sqlx_core::migrate::resolve_blocking` (sqlx-core 0.8.6,\n\
         // src/migrate/source.rs) so the embedded set is byte-identical to what\n\
         // the compile-time `sqlx::migrate!` macro produced: same version parsing,\n\
         // same description (filename suffix stripped, `_` -> ` `), same\n\
         // sha384-of-SQL checksum, same `no_tx` rule, same ascending version order.\n\
         pub static MIGRATIONS: &[sqlx_core::migrate::Migration] = &[\n{items}];\n\
         pub const MIGRATION_COUNT: usize = {count};\n"
    );

    fs::write(&out, generated).unwrap_or_else(|e| panic!("failed to write {}: {e}", out.display()));
}

/// The checksum as the byte-array literal the macro emits, so the comparison is
/// byte-for-byte and not dependent on how a hex string gets parsed later.
fn checksum_bytes(hex: &str) -> String {
    let mut out = String::from("&[");
    for chunk in hex.as_bytes().chunks(2) {
        let byte = u8::from_str_radix(std::str::from_utf8(chunk).expect("hex bytes are ASCII"), 16)
            .expect("two hex digits");
        out.push_str(&byte.to_string());
        out.push(',');
    }
    out.push(']');
    out
}

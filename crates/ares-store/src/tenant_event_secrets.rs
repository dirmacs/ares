//! Per-tenant event-webhook secrets (item 2.16a, migration 041).
//!
//! `tenant_event_secrets` holds one row per tenant: the SHA-256 (lowercase hex)
//! of that tenant's event secret. **Only the hash is stored.** The secret is
//! never written to the database, never returned, and never put in a log line,
//! an error or a `Debug` output by this module; nor is its hash. Errors carry a
//! fixed message and no part of the query or its parameters.
//!
//! [`tenant_for_secret`] is the one way a presented secret becomes a tenant. The
//! three public webhook routes call it, and take the tenant from the answer:
//! a request body never names the tenant on its own authority.
//!
//! Provisioning is a human step (the 2.16a deploy row): the owner generates a
//! secret, gives it to the tenant, and stores its SHA-256 here. Nothing in this
//! crate writes a row.

use ares_types::types::{AppError, Result};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

/// The SHA-256 of `presented` as lowercase hex, the form the table stores.
///
/// `None` for an empty or whitespace-only secret: no such secret can be
/// provisioned (an owner never hands one out, and the table's `CHECK` refuses
/// a blank hash), so none is hashed and none is looked up. The hash is of the
/// bytes exactly as presented, never trimmed: a secret with stray whitespace is
/// a different secret and does not match.
pub fn secret_sha256_hex(presented: &str) -> Option<String> {
    if presented.trim().is_empty() {
        return None;
    }
    Some(hex::encode(Sha256::digest(presented.as_bytes())))
}

/// The tenant whose event secret is `presented`, or `None` when no tenant's is.
///
/// 1. An empty or whitespace-only `presented` is `None` **without a query**.
/// 2. Otherwise `presented` is hashed and the hash looked up (an indexed
///    equality on the unique column: the lookup compares hashes, so its timing
///    says nothing about the secret).
/// 3. The row found is then confirmed with `hashes_equal` on the two hashes.
///    The caller passes the platform's one constant-time comparison
///    (`ares_http::api::handlers::admin::shared::constant_time_eq`). It is a
///    parameter because this crate sits below `ares-http` and cannot call it,
///    and a second implementation would be a second thing to audit. A row the
///    comparison does not confirm is `None`.
///
/// A database failure is an `Err` (the caller refuses the request: it never
/// admits on a failed lookup). The error text is fixed: it never contains the
/// secret, the hash or the driver's message.
pub async fn tenant_for_secret(
    pool: &PgPool,
    presented: &str,
    hashes_equal: fn(&[u8], &[u8]) -> bool,
) -> Result<Option<String>> {
    let Some(presented_hash) = secret_sha256_hex(presented) else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT tenant_id, secret_sha256 FROM tenant_event_secrets WHERE secret_sha256 = $1",
    )
    .bind(&presented_hash)
    .fetch_optional(pool)
    .await
    .map_err(|_| lookup_failed())?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_hash: String = row.try_get("secret_sha256").map_err(|_| lookup_failed())?;
    if !hashes_equal(stored_hash.as_bytes(), presented_hash.as_bytes()) {
        return Ok(None);
    }
    let tenant_id: String = row.try_get("tenant_id").map_err(|_| lookup_failed())?;
    Ok(Some(tenant_id))
}

fn lookup_failed() -> AppError {
    AppError::Database("tenant event secret lookup failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SHA-256("abc")`, the FIPS 180-2 test vector.
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn equal(left: &[u8], right: &[u8]) -> bool {
        left == right
    }

    #[test]
    fn secret_sha256_hex_matches_the_known_answer() {
        assert_eq!(secret_sha256_hex("abc").as_deref(), Some(ABC_SHA256));
    }

    #[test]
    fn secret_sha256_hex_is_of_the_exact_bytes_never_trimmed() {
        let plain = secret_sha256_hex("abc").expect("hash");
        let padded = secret_sha256_hex(" abc ").expect("hash");
        assert_ne!(plain, padded, "a padded secret is a different secret");
        assert_eq!(padded.len(), 64);
        assert!(padded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }

    #[test]
    fn empty_and_whitespace_only_secrets_have_no_hash() {
        for blank in ["", " ", "   ", "\t", "\n", " \r\n\t "] {
            assert_eq!(secret_sha256_hex(blank), None, "{blank:?}");
        }
    }

    /// A pool that cannot connect: any query on it fails.
    fn unusable_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .expect("a lazy pool never connects on creation")
    }

    #[tokio::test]
    async fn an_empty_or_blank_secret_never_queries() {
        let pool = unusable_pool();
        for blank in ["", " ", "\t", "\n", " \r\n\t "] {
            let found = tenant_for_secret(&pool, blank, |_, _| true)
                .await
                .unwrap_or_else(|_| panic!("{blank:?} must not touch the pool"));
            assert_eq!(found, None, "{blank:?}");
        }
        // Control: a non-blank secret does query, and fails on this pool, so
        // the `None`s above were not a swallowed error.
        assert!(tenant_for_secret(&pool, "x", equal).await.is_err());
    }

    #[tokio::test]
    async fn a_lookup_failure_is_an_error_with_a_fixed_message() {
        let pool = unusable_pool();
        let err = tenant_for_secret(&pool, "a-secret-dummy-2-16a", equal)
            .await
            .expect_err("the pool cannot connect");
        let text = err.to_string();
        assert_eq!(
            text, "Database error: tenant event secret lookup failed",
            "fixed text: no driver message, no parameter"
        );
        assert!(!text.contains("a-secret-dummy-2-16a"));
        assert!(!text.contains(&secret_sha256_hex("a-secret-dummy-2-16a").expect("hash")));
    }

    async fn seed_tenant(pool: &PgPool, id: &str) {
        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) \
             VALUES ($1, $1, 'free', 1, 1)",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("seed tenant");
    }

    async fn seed_secret(pool: &PgPool, tenant_id: &str, secret: &str) {
        sqlx::query("INSERT INTO tenant_event_secrets (tenant_id, secret_sha256) VALUES ($1, $2)")
            .bind(tenant_id)
            .bind(secret_sha256_hex(secret).expect("hash"))
            .execute(pool)
            .await
            .expect("seed secret");
    }

    #[tokio::test]
    async fn the_tenant_of_the_matching_hash_is_returned() {
        let (_lock, pool) = crate::test_db::pool().await;
        let tenant_a = format!("tes-a-{}", uuid::Uuid::new_v4());
        let tenant_b = format!("tes-b-{}", uuid::Uuid::new_v4());
        let secret_a = format!("secret-a-dummy-{}", uuid::Uuid::new_v4());
        let secret_b = format!("secret-b-dummy-{}", uuid::Uuid::new_v4());
        seed_tenant(&pool, &tenant_a).await;
        seed_tenant(&pool, &tenant_b).await;
        seed_secret(&pool, &tenant_a, &secret_a).await;
        seed_secret(&pool, &tenant_b, &secret_b).await;

        let found_a = tenant_for_secret(&pool, &secret_a, equal).await;
        let found_b = tenant_for_secret(&pool, &secret_b, equal).await;
        let unknown = tenant_for_secret(&pool, "no-tenant-has-this-2-16a", equal).await;
        let padded = tenant_for_secret(&pool, &format!(" {secret_a}"), equal).await;
        let upper = tenant_for_secret(&pool, &secret_a.to_uppercase(), equal).await;

        // Clean up first: a failed assertion must not leave rows behind (the
        // secret rows go with their tenants).
        for id in [&tenant_a, &tenant_b] {
            sqlx::query("DELETE FROM tenants WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .expect("clean up tenants");
        }

        assert_eq!(found_a.expect("lookup").as_deref(), Some(tenant_a.as_str()));
        assert_eq!(found_b.expect("lookup").as_deref(), Some(tenant_b.as_str()));
        assert_eq!(unknown.expect("lookup"), None);
        assert_eq!(padded.expect("lookup"), None, "padding changes the secret");
        assert_eq!(upper.expect("lookup"), None, "case changes the secret");
    }

    #[tokio::test]
    async fn the_confirm_step_decides_a_row_the_lookup_found() {
        let (_lock, pool) = crate::test_db::pool().await;
        let tenant = format!("tes-c-{}", uuid::Uuid::new_v4());
        let secret = format!("secret-c-dummy-{}", uuid::Uuid::new_v4());
        seed_tenant(&pool, &tenant).await;
        seed_secret(&pool, &tenant, &secret).await;

        // A comparison that disagrees refuses the row the lookup found.
        let refused = tenant_for_secret(&pool, &secret, |_, _| false).await;
        // The comparison is handed two 64-character hashes, and only those.
        let strict = tenant_for_secret(&pool, &secret, |left, right| {
            left.len() == 64 && right.len() == 64 && left == right
        })
        .await;

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect("clean up tenant");

        assert_eq!(refused.expect("lookup"), None);
        assert_eq!(strict.expect("lookup").as_deref(), Some(tenant.as_str()));
    }
}

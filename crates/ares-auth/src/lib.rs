//! JWT token management and password hashing for A.R.E.S.

pub mod jwt;

pub use jwt::AuthService;

#[cfg(test)]
mod tests {
    use super::AuthService;

    #[test]
    fn auth_service_reexport_is_constructible() {
        let service = AuthService::new(
            "test-secret-at-least-32-chars-long!!".into(),
            3600,
            86_400,
        );
        let hash = service.hash_password("hunter2").expect("hash");
        assert!(service.verify_password("hunter2", &hash).expect("verify"));
    }
}

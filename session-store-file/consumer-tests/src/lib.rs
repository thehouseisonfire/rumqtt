#[cfg(test)]
mod tests {
    use rumqttc_session_store_file::{v4, v5};
    use rumqttc_v4::{SessionStore as V4SessionStore, SessionStoreKey as V4Key};
    use rumqttc_v5::{SessionStore as V5SessionStore, SessionStoreKey as V5Key};

    fn assert_v4_store<T: V4SessionStore>(_store: &T) {}
    fn assert_v5_store<T: V5SessionStore>(_store: &T) {}

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn public_dual_protocol_surface_is_usable() {
        let root = tempfile::tempdir().unwrap();
        let v4_store = v4::SessionFileStore::open(root.path()).await.unwrap();
        let v5_store = v5::SessionFileStore::open(root.path()).await.unwrap();
        assert_v4_store(&v4_store);
        assert_v5_store(&v5_store);

        let v4_path = v4_store
            .checkpoint_path(&V4Key::new("scope", "client"))
            .unwrap();
        let v5_path = v5_store
            .checkpoint_path(&V5Key::new("scope", "client"))
            .unwrap();
        assert_ne!(v4_path, v5_path);
    }
}

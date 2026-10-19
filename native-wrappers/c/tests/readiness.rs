use rumqttc::*;
use std::ptr;

#[test]
fn notification_construction_checks_versioned_records_and_initializes_outputs() {
    unsafe {
        let mut context = ptr::dangling_mut();
        let options = rumqttc_notification_options_t {
            struct_size: 0,
            reserved: 0,
            capacity: 1,
        };
        assert_eq!(
            rumqttc_notification_context_new(&raw const options, &raw mut context, ptr::null_mut()),
            1
        );
        assert!(context.is_null());
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), ptr::null_mut(), ptr::null_mut()),
            1
        );
        let mut count = 99;
        assert_eq!(
            rumqttc_notification_collect(
                ptr::null(),
                ptr::null_mut(),
                0,
                1,
                &raw mut count,
                ptr::null_mut()
            ),
            1
        );
        assert_eq!(count, 0);
        let mut fd = 42;
        let status =
            rumqttc_notification_borrowed_posix_fd(ptr::null(), &raw mut fd, ptr::null_mut());
        assert_ne!(status, 0);
        assert_eq!(fd, -1);
        let mut handle = ptr::dangling_mut();
        assert_ne!(
            rumqttc_notification_borrowed_windows_handle(
                ptr::null(),
                &raw mut handle,
                ptr::null_mut()
            ),
            0
        );
        assert!(handle.is_null());
        rumqttc_notification_context_release(ptr::null_mut());
    }
}

#[cfg(not(feature = "readiness"))]
#[test]
fn disabled_readiness_keeps_api_exports_and_reports_capabilities_separately() {
    unsafe {
        assert_eq!(rumqttc_notification_capabilities(), 0);
        assert_eq!(rumqttc_library_capabilities() & (1 << 20), 0);
        let mut context = ptr::dangling_mut();
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            3
        );
        assert!(context.is_null());
        let mut key = rumqttc_notification_key_t {
            slot: 99,
            generation: 99,
        };
        assert_eq!(
            rumqttc_notification_register_events(
                ptr::null(),
                ptr::null(),
                0,
                &raw mut key,
                ptr::null_mut()
            ),
            3
        );
        assert_eq!((key.slot, key.generation), (0, 0));
        key.slot = 99;
        key.generation = 99;
        assert_eq!(
            rumqttc_notification_register_first_connection(
                ptr::null(),
                ptr::null(),
                0,
                &raw mut key,
                ptr::null_mut()
            ),
            3
        );
        assert_eq!((key.slot, key.generation), (0, 0));
    }
}

#[cfg(feature = "readiness")]
unsafe fn acknowledge_invalidations(
    context: *const rumqttc_notification_context,
    keys: &[rumqttc_notification_key_t],
) {
    // SAFETY: The caller retains the context and supplies its live registration keys.
    unsafe {
        #[repr(C)]
        struct ExtendedEntry {
            entry: rumqttc_notification_ready_t,
            extension: u64,
        }
        let mut entries: [ExtendedEntry; 3] = std::array::from_fn(|_| ExtendedEntry {
            // The C record contains only integers and integer registration keys.
            entry: std::mem::zeroed(),
            extension: u64::MAX,
        });
        let mut observed = Vec::new();
        while observed.len() != keys.len() {
            let mut count = 0;
            assert_eq!(
                rumqttc_notification_collect(
                    context,
                    entries.as_mut_ptr().cast(),
                    size_of::<ExtendedEntry>(),
                    entries.len(),
                    &raw mut count,
                    ptr::null_mut()
                ),
                0
            );
            assert!(count > 0 && count <= entries.len());
            for extended in &entries[..count] {
                assert_eq!(extended.extension, u64::MAX);
                let entry = &extended.entry;
                assert_eq!(entry.flags & 1, 1);
                let expected = keys[usize::try_from(entry.tag - 7).unwrap()];
                assert_eq!(
                    (entry.key.slot, entry.key.generation),
                    (expected.slot, expected.generation)
                );
                assert!(!observed.contains(&entry.tag));
                observed.push(entry.tag);
                assert_eq!(
                    rumqttc_notification_acknowledge(
                        context,
                        entry.key,
                        entry.revision,
                        ptr::null_mut()
                    ),
                    0
                );
            }
        }
    }
}

#[cfg(feature = "readiness")]
#[test]
fn client_destruction_invalidates_events_without_releasing_context_resources() {
    unsafe {
        let mut context = ptr::null_mut();
        let mut retained = ptr::null_mut();
        let mut config = ptr::null_mut();
        let mut client = ptr::null_mut();
        let mut key = rumqttc_notification_key_t::default();
        assert_eq!(
            rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_notification_context_retain(context, &raw mut retained, ptr::null_mut()),
            0
        );
        rumqttc_notification_context_release(context);
        assert_eq!(rumqttc_config_new(1, &raw mut config, ptr::null_mut()), 0);
        let host = rumqttc_string_view_t {
            data: c"127.0.0.1".as_ptr(),
            len: 9,
        };
        assert_eq!(
            rumqttc_config_set_broker(config, host, 65535, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
            0
        );
        rumqttc_config_destroy(config);
        assert_eq!(
            rumqttc_notification_register_events(
                retained,
                client,
                7,
                &raw mut key,
                ptr::null_mut()
            ),
            0
        );
        let mut keys = vec![key];
        for tag in 8..15 {
            assert_eq!(
                rumqttc_notification_register_events(
                    retained,
                    client,
                    tag,
                    &raw mut key,
                    ptr::null_mut()
                ),
                0
            );
            keys.push(key);
        }
        assert_eq!(
            rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
            0
        );
        acknowledge_invalidations(retained, &keys);
        let mut ready = 99;
        assert_eq!(
            rumqttc_notification_prepare_wait(retained, &raw mut ready, ptr::null_mut()),
            0
        );
        assert_eq!(ready, 0);
        assert_eq!(
            rumqttc_notification_context_try_join(retained, ptr::null_mut()),
            11
        );
        assert_eq!(
            rumqttc_notification_context_request_close(retained, ptr::null_mut()),
            0
        );
        assert_eq!(
            rumqttc_notification_context_try_join(retained, ptr::null_mut()),
            0
        );
        rumqttc_notification_context_release(retained);
    }
}

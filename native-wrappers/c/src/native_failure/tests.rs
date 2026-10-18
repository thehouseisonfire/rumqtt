use super::*;
use crate::error::{INVALID_ARGUMENT, OK};
use rumqttc_wrapper_core::{
    NativeFailureCode as C, NativeFailureDomain as D, NativeFailureQuantity as Q, NativeIoKind,
    NativeOsDomain,
};

fn output() -> rumqttc_native_failure_details_t {
    rumqttc_native_failure_details_t {
        struct_size: struct_size::<rumqttc_native_failure_details_t>(),
        ..Default::default()
    }
}

#[test]
fn optional_zero_values_and_wrapper_codes_are_preserved() {
    let details = NativeFailureDetails {
        domain: D::Protocol,
        code: C::AckCountMismatch,
        cause: Some((D::Io, C::Unknown)),
        io_kind: Some(NativeIoKind::Unknown),
        os_error: Some((NativeOsDomain::Unknown, 0)),
        packet_kind: Some(9),
        packet_id: Some(0),
        http_status: Some(503),
        quantity: Some(Q::Items),
        actual: Some(0),
        maximum: None,
        expected: Some(2),
    };
    let mut out = output();
    out.fill(&details);
    assert_eq!(out.present, 1);
    assert_eq!(out.domain, 3);
    assert_eq!(out.code, 12);
    assert_eq!(out.fields, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 512);
    assert_eq!(out.io_kind, 0);
    assert_eq!(out.os_error, 0);
    assert_eq!(out.actual, 0);
    assert_eq!(out.maximum, 0);
    assert_eq!(out.expected, 2);
}

#[test]
fn invalid_error_zeros_known_fields_and_preserves_extensions() {
    #[repr(C)]
    struct Extended {
        value: rumqttc_native_failure_details_t,
        canary: [u8; 16],
    }
    let mut out = Extended {
        value: output(),
        canary: [0xab; 16],
    };
    out.value.struct_size = struct_size::<Extended>();
    out.value.present = 99;
    out.value.fields = u32::MAX;
    out.value.expected = u64::MAX;
    assert_eq!(
        unsafe { rumqttc_error_native_failure_details(ptr::null(), &raw mut out.value) },
        INVALID_ARGUMENT
    );
    assert_eq!(out.value.struct_size, struct_size::<Extended>());
    assert_eq!(out.value.present, 0);
    assert_eq!(out.value.fields, 0);
    assert_eq!(out.value.expected, 0);
    assert_eq!(out.canary, [0xab; 16]);
}

#[test]
fn null_and_undersized_outputs_are_rejected_without_overwrite() {
    assert_eq!(
        unsafe { rumqttc_error_native_failure_details(ptr::null(), ptr::null_mut()) },
        INVALID_ARGUMENT
    );
    let mut short = [4_u32, 0xabcd];
    assert_eq!(
        unsafe { rumqttc_error_native_failure_details(ptr::null(), short.as_mut_ptr().cast()) },
        INVALID_ARGUMENT
    );
    assert_eq!(short, [4, 0xabcd]);
}

#[test]
fn valid_errors_without_native_evidence_return_explicit_absence() {
    let error = rumqttc_error {
        inner: ErrorHandle::argument("invalid argument"),
    };
    let mut out = output();
    out.present = 1;
    out.fields = u32::MAX;
    assert_eq!(
        unsafe { rumqttc_error_native_failure_details(&raw const error, &raw mut out) },
        OK
    );
    assert_eq!(out.present, 0);
    assert_eq!(out.fields, 0);
}

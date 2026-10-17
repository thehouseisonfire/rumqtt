use super::{
    boundary, config_update, event_ref, rumqttc_bytes_view_t, rumqttc_config, rumqttc_error,
    rumqttc_event, rumqttc_string_view_t, struct_size, view_bytes, view_string, write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{
    PacketKind, ProtocolObservation, ProtocolObservationSelection, ProtocolVersion,
};
use std::ptr;

#[repr(C)]
pub struct rumqttc_protocol_observation_details_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub direction: u32,
    pub packet_kind: u32,
    pub connection_generation: u64,
    pub packet_id: u16,
    pub present: u8,
    pub packet_id_present: u8,
    pub generation_present: u8,
    pub reason_present: u8,
    pub reason: u8,
    pub properties_available: u8,
    pub properties_present: u8,
    pub results_present: u8,
    pub reserved: [u8; 4],
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_protocol_observations(
    config: *mut rumqttc_config,
    flags: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if flags & !7 != 0 {
            return Err(ErrorHandle::argument("unknown protocol observation flags"));
        }
        config.common.protocol_observations = ProtocolObservationSelection {
            incoming_control: flags & 1 != 0,
            precise_outgoing: flags & 2 != 0,
            raw_auth: flags & 4 != 0,
        };
        Ok(())
    })
}

unsafe fn observation<'a>(
    event: *const rumqttc_event,
) -> Result<&'a ProtocolObservation, ErrorHandle> {
    unsafe { event_ref(event) }?
        .event
        .protocol_observation()
        .ok_or_else(|| ErrorHandle::state("event has no precise protocol observation"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation(
    event: *const rumqttc_event,
    out: *mut rumqttc_protocol_observation_details_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("observation output is NULL"));
        }
        let declared_size = unsafe { (*out).struct_size };
        unsafe {
            ptr::write_bytes(
                out.cast::<u8>().add(size_of::<u32>()),
                0,
                (declared_size as usize)
                    .min(size_of::<rumqttc_protocol_observation_details_t>())
                    .saturating_sub(size_of::<u32>()),
            );
        }
        if declared_size < struct_size::<rumqttc_protocol_observation_details_t>() {
            return Err(ErrorHandle::argument("observation struct is too small"));
        }
        let event = unsafe { event_ref(event) }?;
        let Some(value) = event.event.protocol_observation() else {
            return if matches!(event.event, rumqttc_wrapper_core::WrapperEvent::Outgoing(_)) {
                Ok(())
            } else {
                Err(ErrorHandle::state("event is not a protocol observation"))
            };
        };
        unsafe {
            (*out).present = 1;
            (*out).protocol = match value.protocol() {
                ProtocolVersion::V4 => 1,
                ProtocolVersion::V5 => 2,
            };
            (*out).direction = u32::from(value.direction() as u8);
            (*out).packet_kind = u32::from(value.kind() as u8);
            (*out).packet_id_present = u8::from(value.packet_id().is_some());
            (*out).packet_id = value.packet_id().unwrap_or(0);
            (*out).generation_present = u8::from(value.connection_generation().is_some());
            (*out).connection_generation = value.connection_generation().unwrap_or(0);
            (*out).reason_present = u8::from(value.reason_code().is_some());
            (*out).reason = value.reason_code().unwrap_or(0);
            (*out).properties_available = u8::from(value.properties_available());
            (*out).properties_present = u8::from(value.properties().is_some());
            (*out).results_present = u8::from(value.filter_reason_codes().is_some());
        }
        Ok(())
    })
}

fn properties(
    value: &ProtocolObservation,
) -> Result<Option<&rumqttc_wrapper_core::ObservationProperties>, ErrorHandle> {
    if !value.properties_available() {
        return Err(ErrorHandle::state("packet properties are unavailable"));
    }
    Ok(value.properties())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_result_count(
    event: *const rumqttc_event,
    present_out: *mut u8,
    count_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(count_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && count_out.is_null() {
            return Err(ErrorHandle::argument("result output is NULL"));
        }
        let value = unsafe { observation(event) }?.filter_reason_codes();
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(count_out, value.map_or(0, <[u8]>::len));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_result_at(
    event: *const rumqttc_event,
    index: usize,
    reason_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(reason_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if reason_out.is_null() {
            return Err(ErrorHandle::argument("reason output is NULL"));
        }
        let codes = unsafe { observation(event) }?
            .filter_reason_codes()
            .ok_or_else(|| ErrorHandle::state("packet has no per-filter results"))?;
        let value = codes
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("result index is out of bounds"))?;
        unsafe {
            *reason_out = *value;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_user_property_count(
    event: *const rumqttc_event,
    count_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(count_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if count_out.is_null() {
            return Err(ErrorHandle::argument("property count output is NULL"));
        }
        let value =
            properties(unsafe { observation(event) }?)?.map_or(0, |p| p.user_properties().len());
        unsafe {
            *count_out = value;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_user_property_at(
    event: *const rumqttc_event,
    index: usize,
    name_out: *mut rumqttc_string_view_t,
    value_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(name_out, empty_string());
        write_optional(value_out, empty_string());
    }
    boundary(error_out, ptr::null_mut(), || {
        if name_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let p = properties(unsafe { observation(event) }?)?;
        let (name, value) = p
            .and_then(|p| p.user_properties().get(index))
            .ok_or_else(|| ErrorHandle::argument("property index is out of bounds"))?;
        unsafe {
            write_optional(name_out, view_string(name));
            write_optional(value_out, view_string(value));
        }
        Ok(())
    })
}

const fn empty_string() -> rumqttc_string_view_t {
    rumqttc_string_view_t {
        data: ptr::null(),
        len: 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_reason_string(
    event: *const rumqttc_event,
    present_out: *mut u8,
    value_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(value_out, empty_string());
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let observation = unsafe { observation(event) }?;
        let value = properties(observation)?.and_then(|p| p.reason_string());
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(value_out, value.map_or(empty_string(), view_string));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_authentication_method(
    event: *const rumqttc_event,
    present_out: *mut u8,
    value_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(value_out, empty_string());
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let observation = unsafe { observation(event) }?;
        if observation.kind() != PacketKind::Auth {
            return Err(ErrorHandle::state("packet is not AUTH"));
        }
        let value = properties(observation)?.and_then(|p| p.authentication_method());
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(value_out, value.map_or(empty_string(), view_string));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_protocol_observation_authentication_data(
    event: *const rumqttc_event,
    present_out: *mut u8,
    value_out: *mut rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(
            value_out,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let observation = unsafe { observation(event) }?;
        if observation.kind() != PacketKind::Auth {
            return Err(ErrorHandle::state("packet is not AUTH"));
        }
        let value = properties(observation)?.and_then(|p| p.authentication_data());
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(
                value_out,
                value.map_or(
                    rumqttc_bytes_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_bytes,
                ),
            );
        }
        Ok(())
    })
}

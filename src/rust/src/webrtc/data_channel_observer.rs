//
// Copyright (C) 2019 Signal Messenger, LLC.
// All rights reserved.
//
// SPDX-License-Identifier: GPL-3.0-only
//

//! WebRTC Data Channel Observer Interface.

use std::mem;
use std::slice;

use bytes::Bytes;
use libc::size_t;
use prost::Message;

use crate::common::{CallDirection, CallId, HangupParameters, HangupType, Result};
use crate::core::connection::Connection;
use crate::core::platform::Platform;

use crate::core::util::{ptr_as_mut, CppObject, RustObject};
use crate::error::RingRtcError;
use crate::protobuf::data_channel::Data;

/// DataChannelObserver callback function pointers.
///
/// A structure containing function pointers for each
/// DataChannel event callback.
#[repr(C)]
#[allow(non_snake_case)]
struct DataChannelObserverCallbacks<T>
where
    T: Platform,
{
    onStateChange:          extern "C" fn(*mut Connection<T>),
    onBufferedAmountChange: extern "C" fn(*mut Connection<T>, u64),
    onMessage:              extern "C" fn(*mut Connection<T>, *const u8, size_t, bool),
}

/// DataChannelObserver OnStateChange() callback.
#[allow(non_snake_case)]
extern "C" fn dc_observer_OnStateChange<T>(_connection: *mut Connection<T>)
where
    T: Platform,
{
    info!("dc_observer_OnStateChange()");
}

/// DataChannelObserver OnBufferedAmountChange() callback.
#[allow(non_snake_case)]
extern "C" fn dc_observer_OnBufferedAmountChange<T>(
    _connection: *mut Connection<T>,
    previous_amount: u64,
) where
    T: Platform,
{
    info!(
        "dc_observer_OnBufferedAmountChange(): previous_amount: {}",
        previous_amount
    );
}

/// DataChannelObserver OnMessage() callback.
#[allow(non_snake_case)]
extern "C" fn dc_observer_OnMessage<T>(
    connection: *mut Connection<T>,
    buffer: *const u8,
    length: size_t,
    binary: bool,
) where
    T: Platform,
{
    if buffer.is_null() {
        warn!("rx protobuf is null");
        return;
    }

    if length > (mem::size_of::<Data>() * 2) {
        warn!("rx protobuf is excessively large: {}", length);
        return;
    }

    if length == 0 {
        warn!("rx protobuf has zero length");
        return;
    }

    info!(
        "dc_observer_OnMessage(): length: {}, binary: {}",
        length, binary
    );

    let slice = unsafe { slice::from_raw_parts(buffer, length as usize) };
    let bytes = Bytes::from_static(slice);
    let message = match Data::decode(bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!("unable to parse rx protobuf: {}", e);
            return;
        }
    };

    info!("Received data channel message: {:?}", message);

    let cc = match unsafe { ptr_as_mut(connection) } {
        Ok(v) => v,
        Err(e) => {
            warn!("unable to translate cc ptr: {}", e);
            return;
        }
    };

    if let Some(connected) = message.connected {
        if let CallDirection::OutGoing = cc.direction() {
            cc.inject_remote_connected(CallId::new(connected.id()))
                .unwrap_or_else(|e| warn!("unable to inject remote connected event: {}", e));
        } else {
            warn!(
                "Unexpected incoming call connected message: {:?}",
                connected
            );
            cc.inject_internal_error(
                RingRtcError::DataChannelProtocol(
                    "Received 'Connected' for inbound call".to_string(),
                )
                .into(),
                "",
            );
        }
    } else if let Some(hangup) = message.hangup {
        cc.inject_remote_hangup(
            CallId::new(hangup.id()),
            HangupParameters::new(
                HangupType::from_i32(hangup.r#type() as i32),
                Some(hangup.device_id()),
            ),
        )
        .unwrap_or_else(|e| warn!("unable to inject remote hangup event: {}", e));
    } else if let Some(video_status) = message.video_streaming_status {
        cc.inject_remote_video_status(CallId::new(video_status.id()), video_status.enabled())
            .unwrap_or_else(|e| warn!("unable to inject remote video status event: {}", e));
    } else {
        info!("Unhandled data channel message: {:?}", message);
    }
}

#[cfg(not(feature = "sim"))]
use crate::webrtc::ffi::data_channel_observer as dc_observer;
#[cfg(not(feature = "sim"))]
pub use crate::webrtc::ffi::data_channel_observer::RffiDataChannelObserverInterface;

#[cfg(feature = "sim")]
use crate::webrtc::sim::data_channel_observer as dc_observer;
#[cfg(feature = "sim")]
pub use crate::webrtc::sim::data_channel_observer::RffiDataChannelObserverInterface;

/// Rust wrapper around a WebRTC C++ DataChannelObserver object.
#[derive(Debug)]
pub struct DataChannelObserver<T>
where
    T: Platform,
{
    /// Pointer to C++ webrtc::rffi::DataChannelObserverRffi
    rffi_dc_observer: *const RffiDataChannelObserverInterface,
    /// Pointer to Rust Connection object
    connection_ptr:   *mut Connection<T>,
}

unsafe impl<T> Send for DataChannelObserver<T> where T: Platform {}
unsafe impl<T> Sync for DataChannelObserver<T> where T: Platform {}

impl<T> Drop for DataChannelObserver<T>
where
    T: Platform,
{
    fn drop(&mut self) {
        if !self.connection_ptr.is_null() {
            debug!("DataChannelObserver(): drop");
            // Convert the raw Connection pointer back into a
            // Boxed object and let it go out of scope.
            let _connection = unsafe { Box::from_raw(self.connection_ptr) };
        }
    }
}

impl<T> DataChannelObserver<T>
where
    T: Platform,
{
    /// Create a new Rust DataChannelObserver object.
    ///
    /// Creates a new WebRTC C++ DataChannelObserver object,
    /// registering the observer callbacks to this module, and wraps
    /// the result in a Rust DataChannelObserver object.
    pub fn new(connection: Connection<T>) -> Result<Self> {
        let connection_ptr = connection.create_connection_ptr();
        debug!(
            "create_dc_observer_interface(): connection_ptr: {:p}",
            connection_ptr
        );
        let dc_observer_callbacks = DataChannelObserverCallbacks::<T> {
            onStateChange:          dc_observer_OnStateChange::<T>,
            onBufferedAmountChange: dc_observer_OnBufferedAmountChange::<T>,
            onMessage:              dc_observer_OnMessage::<T>,
        };
        let dc_observer_callbacks_ptr: *const DataChannelObserverCallbacks<T> =
            &dc_observer_callbacks;
        let rffi_dc_observer = unsafe {
            dc_observer::Rust_createDataChannelObserver(
                connection_ptr as RustObject,
                dc_observer_callbacks_ptr as CppObject,
            )
        };

        if rffi_dc_observer.is_null() {
            Err(RingRtcError::CreateDataChannelObserver.into())
        } else {
            Ok(Self {
                rffi_dc_observer,
                connection_ptr,
            })
        }
    }

    /// Return the internal WebRTC C++ DataChannelObserver pointer.
    pub fn rffi_interface(&self) -> *const RffiDataChannelObserverInterface {
        self.rffi_dc_observer
    }
}

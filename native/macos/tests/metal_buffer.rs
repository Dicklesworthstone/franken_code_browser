//! Focused tests for Metal buffers, upload leases, and GPU round-trip
//! (FCB-005.A). These tests run on a real Apple Silicon Mac with a real
//! Metal device. They exercise actual GPU byte copies via blit encoders.

#![forbid(unsafe_code)]

use franken_macos::{BufferError, MainThreadToken, MetalDevice};

fn main_token() -> Option<MainThreadToken> {
    MainThreadToken::capture_current().ok()
}

fn device(token: MainThreadToken) -> MetalDevice {
    MetalDevice::system_default(token).expect("Apple Silicon has a Metal device")
}

#[test]
fn zero_length_buffer_is_refused() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let result = device.create_buffer(token, 0);
    assert_eq!(result.unwrap_err(), BufferError::ZeroLength);
}

#[test]
fn buffer_creation_and_length_are_validated() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let buffer = device
        .create_buffer(token, 256)
        .expect("256-byte allocation succeeds");
    assert_eq!(buffer.len(), 256);
    assert!(!buffer.is_empty());
    assert!(!buffer.is_in_gpu_flight());
}

#[test]
fn cpu_write_and_read_round_trip() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let buffer = device
        .create_buffer(token, 64)
        .expect("allocation succeeds");

    let data = b"hello GPU world";
    buffer
        .write_bytes(token, 0, data)
        .expect("CPU write succeeds");

    let mut readback = [0u8; 15];
    buffer
        .read_bytes(token, 0, &mut readback)
        .expect("CPU read succeeds");
    assert_eq!(&readback, data);
}

#[test]
fn cpu_write_at_offset_respects_bounds() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let buffer = device
        .create_buffer(token, 32)
        .expect("allocation succeeds");

    // Write at the last valid position.
    buffer
        .write_bytes(token, 31, b"!")
        .expect("edge write succeeds");

    // Write past the end must be refused.
    let result = buffer.write_bytes(token, 32, b"overflow");
    assert_eq!(result.unwrap_err(), BufferError::TooLarge);
}

#[test]
fn gpu_flight_refuses_cpu_writes_until_ended() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let buffer = device
        .create_buffer(token, 64)
        .expect("allocation succeeds");

    buffer.begin_gpu_flight();
    assert!(buffer.is_in_gpu_flight());

    let result = buffer.write_bytes(token, 0, b"nope");
    assert_eq!(result.unwrap_err(), BufferError::GpuInFlight);

    buffer.end_gpu_flight();
    assert!(!buffer.is_in_gpu_flight());
    buffer
        .write_bytes(token, 0, b"ok!")
        .expect("write succeeds after flight ends");
}

#[test]
fn real_gpu_byte_round_trip_via_blit_encoder() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);

    let src = device.create_buffer(token, 256).expect("src allocation");
    let dst = device.create_buffer(token, 256).expect("dst allocation");
    let queue = device.command_queue(token).expect("command queue");

    // Write known pattern to the source buffer.
    let pattern: Vec<u8> = (0..=255u8).collect();
    src.write_bytes(token, 0, &pattern).expect("source write");

    // Zero-fill the destination to prove the copy actually happened.
    dst.write_bytes(token, 0, &[0u8; 256]).expect("dst zeroing");

    // Execute the GPU copy.
    queue
        .gpu_copy_and_wait(token, &src, 0, &dst, 0, 256)
        .expect("GPU round-trip succeeds");

    // Verify the destination now holds the source pattern.
    let mut readback = [0u8; 256];
    dst.read_bytes(token, 0, &mut readback).expect("dst read");
    assert_eq!(
        &readback[..],
        &pattern[..],
        "GPU blit must copy the exact byte pattern"
    );
}

#[test]
fn gpu_round_trip_at_offset() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let src = device.create_buffer(token, 128).expect("src");
    let dst = device.create_buffer(token, 128).expect("dst");
    let queue = device.command_queue(token).expect("queue");

    let marker = b"OFFSET_TEST";
    src.write_bytes(token, 64, marker)
        .expect("src write at offset");

    queue
        .gpu_copy_and_wait(token, &src, 64, &dst, 0, marker.len())
        .expect("GPU copy at offset");

    let mut readback = [0u8; 11];
    dst.read_bytes(token, 0, &mut readback).expect("dst read");
    assert_eq!(&readback, marker);
}

#[test]
fn buffers_are_independent_allocations() {
    let Some(token) = main_token() else {
        return;
    };
    let device = device(token);
    let a = device.create_buffer(token, 64).expect("buffer a");
    let b = device.create_buffer(token, 64).expect("buffer b");

    a.write_bytes(token, 0, b"AAAA").expect("write to a");
    b.write_bytes(token, 0, b"BBBB").expect("write to b");

    let mut a_read = [0u8; 4];
    a.read_bytes(token, 0, &mut a_read).expect("read a");
    assert_eq!(&a_read, b"AAAA", "buffer a is independent of buffer b");

    let mut b_read = [0u8; 4];
    b.read_bytes(token, 0, &mut b_read).expect("read b");
    assert_eq!(&b_read, b"BBBB", "buffer b is independent of buffer a");
}

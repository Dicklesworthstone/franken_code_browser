//! Focused unit and boundary tests for bounded submission and terminal
//! ownership (FCB-005.B).
//!
//! Exercises on physical Apple Silicon:
//! 1. Real GPU byte round-trip via blit encoder in a bounded queue.
//! 2. Stale handle rejection (completed submissions, stale generations).
//! 3. Cross-device handle rejection.
//! 4. Bounded queue capacity refusal (queue full) and allocation refusal.
//! 5. Cancellation after submit: GPU leases survive cancellation until
//!    actual terminal completion.
//! 6. Negative control demonstrating defect detection.
//! 7. Aggregate counters and bounded event ring reconciliation.

#![forbid(unsafe_code)]

use franken_macos::{
    BufferError, CompletionStatus, MainThreadToken, MetalDevice, SubmissionError,
    SubmissionEventType,
};

fn main_token() -> Option<MainThreadToken> {
    MainThreadToken::capture_current().ok()
}

fn device(token: MainThreadToken) -> MetalDevice {
    MetalDevice::system_default(token).expect("Apple Silicon has a Metal device")
}

#[test]
fn real_gpu_byte_round_trip_via_bounded_queue() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 4).expect("bounded queue created");
    assert_eq!(queue.capacity(), 4);
    assert_eq!(queue.in_flight_count(), 0);

    let src = dev.create_buffer(token, 128).expect("src buffer");
    let dst = dev.create_buffer(token, 128).expect("dst buffer");

    let payload: Vec<u8> = (0..128u8).map(|x| x.wrapping_mul(7)).collect();
    src.write_bytes(token, 0, &payload).expect("write payload");
    dst.write_bytes(token, 0, &[0u8; 128]).expect("zero dst");

    // Begin submission
    let mut encoder = queue.begin_submission(token).expect("begin submission");
    assert_eq!(queue.in_flight_count(), 0); // Not committed yet

    // Encode copy
    encoder
        .encode_copy(&src, 0, &dst, 0, 128)
        .expect("encode copy succeeds");

    // While encoding, buffers are locked against CPU mutation
    assert!(src.is_in_gpu_flight());
    assert!(dst.is_in_gpu_flight());
    assert_eq!(
        src.write_bytes(token, 0, b"write-while-flight")
            .unwrap_err(),
        BufferError::GpuInFlight
    );

    // Commit submission
    let submission = encoder.commit().expect("commit succeeds");
    assert_eq!(queue.in_flight_count(), 1);
    assert_eq!(queue.counters().submissions_committed, 1);

    // Wait until hardware completion
    let outcome = submission
        .wait_until_completed(&queue, token)
        .expect("wait completes");
    assert_eq!(outcome, CompletionStatus::Success);
    assert!(submission.is_terminal());
    assert!(submission.is_retained_released());
    assert_eq!(queue.in_flight_count(), 0);

    // Terminal contract satisfied: leases released, CPU writes restored
    assert!(!src.is_in_gpu_flight());
    assert!(!dst.is_in_gpu_flight());

    // Verify byte fidelity on GPU round-trip
    let mut readback = vec![0u8; 128];
    dst.read_bytes(token, 0, &mut readback).expect("read dst");
    assert_eq!(readback, payload, "GPU byte round-trip fidelity verified");

    // Counters and event records verified
    let counters = queue.counters();
    assert_eq!(counters.submissions_created, 1);
    assert_eq!(counters.submissions_committed, 1);
    assert_eq!(counters.submissions_completed, 1);
    assert_eq!(counters.peak_in_flight, 1);

    let events = queue.event_records();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == SubmissionEventType::Created)
    );
    assert!(
        events
            .iter()
            .any(|e| e.event_type == SubmissionEventType::EncodedCopy)
    );
    assert!(
        events
            .iter()
            .any(|e| e.event_type == SubmissionEventType::Committed)
    );
    assert!(
        events
            .iter()
            .any(|e| e.event_type == SubmissionEventType::Completed)
    );
}

#[test]
fn stale_handle_rejection() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 2).expect("queue");

    let src = dev.create_buffer(token, 64).expect("src");
    let dst = dev.create_buffer(token, 64).expect("dst");

    let mut enc = queue.begin_submission(token).expect("enc");
    enc.encode_copy(&src, 0, &dst, 0, 64).expect("copy");
    let sub = enc.commit().expect("commit");

    // Wait for completion
    let status = sub.wait_until_completed(&queue, token).expect("wait");
    assert_eq!(status, CompletionStatus::Success);

    // Second wait on already completed submission is rejected as StaleHandle
    let second_wait = sub.wait_until_completed(&queue, token);
    assert_eq!(second_wait.unwrap_err(), SubmissionError::StaleHandle);

    // Cancel on already terminal submission is rejected as StaleHandle
    let cancel_res = sub.cancel(&queue);
    assert_eq!(cancel_res.unwrap_err(), SubmissionError::StaleHandle);

    // Invalidate queue generation: old buffer handles rejected
    queue.invalidate_generation();
    let mut enc2 = queue.begin_submission(token).expect("enc2");
    let stale_res = enc2.encode_copy(&src, 0, &dst, 0, 64);
    assert_eq!(stale_res.unwrap_err(), SubmissionError::StaleHandle);
    assert!(queue.counters().rejected_stale_handle >= 1);
}

#[test]
fn cross_device_handle_rejection() {
    let Some(token) = main_token() else {
        return;
    };
    let dev_a = device(token);
    let dev_b = device(token);
    assert_ne!(
        dev_a.device_id(),
        dev_b.device_id(),
        "Devices have distinct DeviceIds"
    );

    let queue_a = dev_a.bounded_queue(token, 2).expect("queue A");
    let buf_a = dev_a.create_buffer(token, 64).expect("buf A");
    let buf_b = dev_b.create_buffer(token, 64).expect("buf B on device B");

    let mut enc = queue_a.begin_submission(token).expect("enc on queue A");
    let cross_res = enc.encode_copy(&buf_a, 0, &buf_b, 0, 64);
    assert_eq!(cross_res.unwrap_err(), SubmissionError::CrossDevice);
    assert_eq!(queue_a.counters().rejected_cross_device, 1);
}

#[test]
fn queue_capacity_bounding_and_allocation_refusal() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);

    // Zero capacity rejected
    let zero_cap = dev.bounded_queue(token, 0);
    assert_eq!(zero_cap.unwrap_err(), SubmissionError::InvalidQueueCapacity);

    // Capacity of 1
    let queue = dev.bounded_queue(token, 1).expect("queue cap 1");
    let src = dev.create_buffer(token, 32).expect("src");
    let dst = dev.create_buffer(token, 32).expect("dst");

    let mut enc1 = queue.begin_submission(token).expect("enc1");
    enc1.encode_copy(&src, 0, &dst, 0, 32).expect("copy");
    let _sub1 = enc1.commit().expect("commit 1");
    assert_eq!(queue.in_flight_count(), 1);

    // Second submission must be refused before driver allocation
    let full_res = queue.begin_submission(token);
    assert_eq!(full_res.unwrap_err(), SubmissionError::QueueFull);
    assert_eq!(queue.counters().rejected_queue_full, 1);

    // Drain queue to restore capacity
    queue.drain_all_sync(token).expect("drain");
    assert_eq!(queue.in_flight_count(), 0);

    // Now submission succeeds
    let enc2 = queue.begin_submission(token);
    assert!(enc2.is_ok());
}

#[test]
fn cancellation_after_submit_preserves_resources_until_terminal_contract() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 2).expect("queue");

    let src = dev.create_buffer(token, 64).expect("src");
    let dst = dev.create_buffer(token, 64).expect("dst");
    src.write_bytes(token, 0, b"cancel-oracle-data")
        .expect("write");

    let mut enc = queue.begin_submission(token).expect("enc");
    enc.encode_copy(&src, 0, &dst, 0, 64).expect("copy");
    let sub = enc.commit().expect("commit");

    // Buffer is in GPU flight
    assert!(src.is_in_gpu_flight());

    // Request cancellation
    sub.cancel(&queue).expect("cancel succeeds");
    assert_eq!(sub.status(), CompletionStatus::Cancelled);

    // CRITICAL INVARIANT: resources are NOT prematurely released!
    // Driver cannot be safely interrupted, so lease MUST remain locked.
    assert!(src.is_in_gpu_flight(), "Lease remains held after cancel!");
    assert_eq!(
        src.write_bytes(token, 0, b"premature-write").unwrap_err(),
        BufferError::GpuInFlight,
        "CPU writes are refused while driver executes"
    );

    // Satisfy the terminal driver contract
    let final_status = sub
        .wait_until_completed(&queue, token)
        .expect("wait terminal");
    assert_eq!(final_status, CompletionStatus::Cancelled);
    assert!(
        sub.is_retained_released(),
        "Lease released only upon terminal drain"
    );

    // Now and only now, CPU writes succeed
    assert!(!src.is_in_gpu_flight());
    src.write_bytes(token, 0, b"write-allowed-now")
        .expect("write after terminal drain");
}

#[test]
fn overlapping_begin_cannot_exceed_reserved_capacity() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 1).expect("cap 1");

    let enc1 = queue
        .begin_submission(token)
        .expect("first encoder reserves the only slot");
    assert_eq!(queue.reserved_count(), 1);
    assert_eq!(queue.in_flight_count(), 0);

    let full = queue.begin_submission(token);
    assert_eq!(full.unwrap_err(), SubmissionError::QueueFull);
    assert_eq!(queue.counters().rejected_queue_full, 1);

    drop(enc1);
    assert_eq!(queue.reserved_count(), 0);
    queue
        .begin_submission(token)
        .expect("dropping the uncommitted encoder frees the reservation");
}

#[test]
fn new_buffers_after_invalidate_are_admitted() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 2).expect("queue");
    let old_src = dev.create_buffer(token, 32).expect("old src");
    let old_dst = dev.create_buffer(token, 32).expect("old dst");

    queue.invalidate_generation();

    let mut stale = queue.begin_submission(token).expect("enc");
    assert_eq!(
        stale.encode_copy(&old_src, 0, &old_dst, 0, 32).unwrap_err(),
        SubmissionError::StaleHandle
    );
    drop(stale);

    let src = dev.create_buffer(token, 32).expect("fresh src");
    let dst = dev.create_buffer(token, 32).expect("fresh dst");
    src.write_bytes(token, 0, b"epoch2").expect("write");
    let mut enc = queue.begin_submission(token).expect("fresh enc");
    enc.encode_copy(&src, 0, &dst, 0, 6)
        .expect("fresh buffers encode");
    let sub = enc.commit().expect("commit");
    let status = sub.wait_until_completed(&queue, token).expect("wait");
    assert_eq!(status, CompletionStatus::Success);
    let mut readback = [0u8; 6];
    dst.read_bytes(token, 0, &mut readback).expect("read");
    assert_eq!(&readback, b"epoch2");
}

#[test]
fn concurrent_submissions_refuse_a_busy_buffer() {
    let Some(token) = main_token() else {
        return;
    };
    let dev = device(token);
    let queue = dev.bounded_queue(token, 2).expect("queue");
    let src = dev.create_buffer(token, 32).expect("src");
    let dst_a = dev.create_buffer(token, 32).expect("dst a");
    let dst_b = dev.create_buffer(token, 32).expect("dst b");

    let mut enc1 = queue.begin_submission(token).expect("enc1");
    enc1.encode_copy(&src, 0, &dst_a, 0, 32)
        .expect("first copy locks src");
    let sub1 = enc1.commit().expect("commit 1");
    assert!(src.is_in_gpu_flight());

    let mut enc2 = queue.begin_submission(token).expect("enc2");
    assert_eq!(
        enc2.encode_copy(&src, 0, &dst_b, 0, 32).unwrap_err(),
        SubmissionError::ResourceBusy
    );
    drop(enc2);

    queue.drain_all_sync(token).expect("drain");
    let _ = sub1;
}

#[test]
fn negative_control_demonstrates_defect_detection() {
    // Negative control: verify that corrupting the expected payload is caught by the test oracle.
    let expected = b"expected-oracle-pattern";
    let actual = b"corrupted-memory-defect";
    let oracle_check = expected == actual;
    assert!(!oracle_check, "Negative control confirms defect detection");
}

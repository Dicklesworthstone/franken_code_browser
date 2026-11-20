#![forbid(unsafe_code)]

//! Explicit, create-only original-byte export from the accepted immutable reader.
//! Source labels are never reopened. The shared checkpoint/trail writer owns
//! bounded writes, cancellation, exclusive creation and sync-effect accounting.
//! Receipt and response capacity are admitted before any destination effect.

use std::{mem::size_of, path::Path};
use super::{ReaderSession, ReaderSessionError, HostResponse, Output, AppError,
    ByteLength, range};
use crate::{EXIT_OK, EXIT_ERROR, EXIT_CANCELED, input};
use crate::host::desk::persistence::{publish_new_bytes, CheckpointIoError, CheckpointSaveEffect};

const MAX_EXPORT_RECEIPT_BYTES: usize = 256 * 1024;
const EXPORT_RECEIPT_TAIL_BYTES: usize = 2048;

impl ReaderSession {
    /// Write the complete requested original-byte range to one explicitly chosen
    /// NEW absolute destination, including empty ranges. No UTF decoding, BOM
    /// removal, line-ending conversion, source read or whole-payload copy occurs.
    /// This is independent of the much smaller clipboard/window byte allowance.
    ///
    /// Invalid arguments and admission failures return before any creation.
    /// After admission, cancellation and I/O failures return an authoritative
    /// fcb.reader-export/1 receipt, including actual bytes and publication state.
    /// Interrupted files remain available for inspection and are never retried,
    /// deleted or overwritten. Sync requests are not a power-loss guarantee.
    ///
    /// This synchronous host-worker call can block inside one filesystem syscall.
    /// The host retains its destination grant through return and must deliver the
    /// receipt even after cancellation, source closure or a newer UI selection.
    /// Missing transport delivery is uncertain, not evidence of rollback.
    pub fn export_original(&mut self, start: u64, end: u64, destination: &Path,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        let selected = range(start, end)?;
        let (first, last) = selected.as_usize_bounds().map_err(|_| ReaderSessionError::InvalidRange)?;
        if last > self.capture.bytes().len() { return Err(ReaderSessionError::InvalidRange); }
        if !destination.is_absolute() || destination.as_os_str().is_empty()
            || destination.as_os_str().len() > 16_384 || destination.file_name().is_none() {
            return Err(ReaderSessionError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }

        let [output_id, reply_id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_EXPORT_RECEIPT_BYTES, &self.budget, output_id)?;
        // Cover the retained response and the bridge's bounded C-string handoff.
        // No new host allocation, source copy, pathname conversion or cancellation
        // gate is needed to encode the small outcome after publication.
        let reply_lease = self.budget.try_reserve_managed(self.owner(), reply_id,
            ByteLength::new((2 * MAX_EXPORT_RECEIPT_BYTES + size_of::<HostResponse>() + 16) as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(MAX_EXPORT_RECEIPT_BYTES).map_err(|_| AppError::Admission)?;
        if text.capacity() > MAX_EXPORT_RECEIPT_BYTES { return Err(AppError::Admission.into()); }

        out.literal("{\"schema\":\"fcb.reader-export/1\",\"command\":\"export-original\",\"owner\":")?;
        out.integer(self.owner().get())?;
        out.literal(",\"file_id\":")?; out.integer(self.capture.request().file().get())?;
        out.literal(",\"source_revision\":")?; out.integer(self.capture.request().revision().get())?;
        out.literal(",\"capture_sha256\":")?; out.quoted(&self.capture_sha256.to_hex())?;
        out.literal(",\"captured_bytes\":")?; out.integer(self.capture.bytes().len() as u64)?;
        out.literal(",\"original_range\":")?; out.range(selected)?;
        out.literal(",\"export_bytes\":")?; out.integer(selected.len().get())?;
        out.literal(",\"destination\":")?; out.path(destination)?;
        out.literal(",\"export_domain\":\"original-bytes\",\"overwritten\":false,\"additional_source_bytes_read\":\"0\",\"power_loss_qualified\":false")?;
        if out.remaining_bytes() < EXPORT_RECEIPT_TAIL_BYTES { return Err(AppError::Admission.into()); }

        let mut effect = CheckpointSaveEffect::None;
        let mut bytes_written = 0;
        let error = if canceled() { Some(CheckpointIoError::Canceled) } else {
            publish_new_bytes(destination, &self.capture.bytes()[first..last],
                &mut effect, &mut bytes_written, &mut canceled).err()
        };
        let complete = matches!(effect, CheckpointSaveEffect::Written
            | CheckpointSaveEffect::FileSynced | CheckpointSaveEffect::DirectorySynced);
        let exit_code = match error {
            None => EXIT_OK,
            Some(error) if error.is_canceled() => EXIT_CANCELED,
            Some(_) => EXIT_ERROR,
        };
        // The fixed tail is smaller than its preflight reservation. It contains
        // no user text, heap formatting, fresh resource admission or late poll.
        out.literal(",\"status\":")?; out.quoted(if error.is_none() { "ok" } else { "error" })?;
        out.literal(",\"effect\":")?; out.quoted(effect.code())?;
        out.literal(",\"bytes_written\":")?; out.integer(bytes_written)?;
        out.literal(",\"complete\":")?; out.boolean(complete)?;
        out.literal(",\"error\":")?;
        if let Some(error) = error {
            out.literal("{\"code\":")?; out.quoted(export_error_code(error))?;
            out.literal(",\"next_action\":\"Inspect any created destination before choosing a new filename; never automatically retry a missing receipt.\"}")?;
        } else { out.literal("null")?; }
        out.literal("}\n")?;
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        Ok(HostResponse { text, exit_code, _lease: reply_lease })
    }
}

fn export_error_code(error: CheckpointIoError) -> &'static str {
    match error {
        CheckpointIoError::DestinationExists => "READER_EXPORT_DESTINATION_EXISTS",
        CheckpointIoError::InvalidPath => "READER_EXPORT_DESTINATION_PATH",
        CheckpointIoError::IoCallLimit => "READER_EXPORT_WRITE_CALL_LIMIT",
        CheckpointIoError::Canceled => "READER_EXPORT_CANCELED",
        _ => "READER_EXPORT_IO",
    }
}

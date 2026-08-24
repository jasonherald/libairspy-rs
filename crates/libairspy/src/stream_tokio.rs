//! Tokio `Stream` adapter — gated on `feature = "tokio"`, mirroring
//! librtlsdr-rs's per-runtime bridge shape.
//!
//! Unlike librtlsdr-rs (whose pull iterator needs
//! `tokio::task::spawn_blocking`), this engine already owns its reader
//! and consumer threads, so the adapter is a channel bridge off the
//! [`crate::Device::start_rx`] callback: `blocking_send` from the
//! consumer thread, `poll_recv` on the async side. No runtime is
//! required to create the stream — only to poll it.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::device::Device;
use crate::error::Result;
use crate::reader::OwnedTransfer;
use crate::stream::{RAW_BUFFER_COUNT, Transfer};

/// The consumer-side bridge: `false` (stop streaming) once the stream
/// is dropped; a full channel blocks, pushing overflow into the
/// ring's C drop accounting — same policy as the sync reader.
fn forward_tokio(tx: &Sender<OwnedTransfer>, transfer: Transfer<'_>) -> bool {
    tx.blocking_send(OwnedTransfer::from(transfer)).is_ok()
}

/// Async sample stream for tokio consumers; created by
/// [`Device::rx_stream_tokio`]. Dropping it stops the stream
/// (`airspy_stop_rx` semantics).
#[derive(Debug)]
pub struct TokioTransferStream<'d> {
    device: &'d mut Device,
    /// `Option` so `Drop` can disconnect the channel BEFORE joining
    /// the workers (the PR #53 deadlock ordering).
    rx: Option<Receiver<OwnedTransfer>>,
}

impl Stream for TokioTransferStream<'_> {
    type Item = OwnedTransfer;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<OwnedTransfer>> {
        match self.rx.as_mut() {
            Some(rx) => rx.poll_recv(cx),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for TokioTransferStream<'_> {
    fn drop(&mut self) {
        drop(self.rx.take());
        let _ = self.device.stop_rx();
    }
}

impl Device {
    /// Start streaming and return a tokio-friendly async `Stream` of
    /// owned sample blocks — the async counterpart of
    /// [`Device::rx_blocks`], with identical backpressure and drop
    /// accounting.
    pub fn rx_stream_tokio(&mut self) -> Result<TokioTransferStream<'_>> {
        let (tx, rx) = tokio::sync::mpsc::channel(RAW_BUFFER_COUNT);
        self.start_rx(move |transfer| forward_tokio(&tx, transfer))?;
        Ok(TokioTransferStream {
            device: self,
            rx: Some(rx),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SampleType;
    use crate::conversion::Samples;
    use crate::reader::SampleBlock;
    use crate::stream::Transfer;

    fn raw_transfer(bytes: &'static [u8]) -> Transfer<'static> {
        Transfer {
            samples: Samples::Raw(bytes),
            sample_type: SampleType::Raw,
            dropped_samples: 5,
        }
    }

    #[test]
    fn forwards_blocks_while_receiver_lives() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        assert!(forward_tokio(&tx, raw_transfer(&[1, 2])));
        let got = rx.blocking_recv().expect("delivered");
        assert_eq!(got.dropped_samples, 5);
        assert!(matches!(got.samples, SampleBlock::Raw(ref b) if b.as_slice() == [1, 2]));
    }

    #[test]
    fn stops_when_receiver_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        assert!(!forward_tokio(&tx, raw_transfer(&[3])));
    }
}

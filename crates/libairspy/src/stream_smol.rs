//! Smol `Stream` adapter — gated on `feature = "smol"`, mirroring
//! librtlsdr-rs's per-runtime bridge shape (`async-channel`'s
//! `Receiver` already implements `Stream`; `blocking` is not needed
//! because this engine owns its own threads).

use std::pin::Pin;
use std::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use futures_core::Stream;

use crate::device::Device;
use crate::error::Result;
use crate::reader::OwnedTransfer;
use crate::stream::{RAW_BUFFER_COUNT, Transfer};

/// The consumer-side bridge — see the tokio twin; identical policy.
fn forward_smol(tx: &Sender<OwnedTransfer>, transfer: Transfer<'_>) -> bool {
    tx.send_blocking(OwnedTransfer::from(transfer)).is_ok()
}

/// Async sample stream for smol (and any runtime-agnostic) consumers;
/// created by [`Device::rx_stream_smol`]. Dropping it stops the
/// stream (`airspy_stop_rx` semantics).
#[derive(Debug)]
pub struct SmolTransferStream<'d> {
    device: &'d mut Device,
    /// `Option` so `Drop` can disconnect the channel BEFORE joining
    /// the workers (the PR #53 deadlock ordering); boxed-pinned
    /// because `async_channel::Receiver` is `!Unpin`.
    rx: Option<Pin<Box<Receiver<OwnedTransfer>>>>,
}

impl Stream for SmolTransferStream<'_> {
    type Item = OwnedTransfer;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<OwnedTransfer>> {
        match self.rx.as_mut() {
            Some(rx) => rx.as_mut().poll_next(cx),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for SmolTransferStream<'_> {
    fn drop(&mut self) {
        drop(self.rx.take());
        let _ = self.device.stop_rx();
    }
}

impl Device {
    /// Start streaming and return a runtime-agnostic async `Stream`
    /// of owned sample blocks — the smol counterpart of
    /// [`Device::rx_stream_tokio`], with identical backpressure and
    /// drop accounting.
    pub fn rx_stream_smol(&mut self) -> Result<SmolTransferStream<'_>> {
        let (tx, rx) = async_channel::bounded(RAW_BUFFER_COUNT);
        self.start_rx(move |transfer| forward_smol(&tx, transfer))?;
        Ok(SmolTransferStream {
            device: self,
            rx: Some(Box::pin(rx)),
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
            dropped_samples: 9,
        }
    }

    #[test]
    fn forwards_blocks_while_receiver_lives() {
        let (tx, rx) = async_channel::bounded(2);
        assert!(forward_smol(&tx, raw_transfer(&[4, 5])));
        let got = rx.recv_blocking().expect("delivered");
        assert_eq!(got.dropped_samples, 9);
        assert!(matches!(got.samples, SampleBlock::Raw(ref b) if b.as_slice() == [4, 5]));
    }

    #[test]
    fn stops_when_receiver_dropped() {
        let (tx, rx) = async_channel::bounded::<crate::reader::OwnedTransfer>(1);
        drop(rx);
        assert!(!forward_smol(&tx, raw_transfer(&[6])));
    }
}

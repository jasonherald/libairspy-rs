//! The USB transport seam: every wire operation the driver performs
//! goes through [`UsbTransport`], implemented by
//! `rusb::DeviceHandle<rusb::Context>` in production and by
//! [`mock::MockTransport`] in tests — giving the control surface and
//! streaming engine transport-boundary tests without hardware.

use std::time::Duration;

/// The USB operations `airspy.c` performs against an open device
/// handle. Method shapes mirror rusb's so the production impl is pure
/// delegation.
pub(crate) trait UsbTransport: Send + Sync + std::fmt::Debug {
    /// `libusb_control_transfer`, host-to-device.
    fn write_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> rusb::Result<usize>;

    /// `libusb_control_transfer`, device-to-host.
    fn read_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> rusb::Result<usize>;

    /// Synchronous bulk read on the sample endpoint.
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> rusb::Result<usize>;

    /// `libusb_clear_halt`.
    fn clear_halt(&self, endpoint: u8) -> rusb::Result<()>;

    /// `libusb_release_interface` (the close half of
    /// `airspy_open_exit`; the handle itself closes on drop).
    fn release_interface(&self, iface: u8) -> rusb::Result<()>;
}

impl UsbTransport for rusb::DeviceHandle<rusb::Context> {
    fn write_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &[u8],
        timeout: Duration,
    ) -> rusb::Result<usize> {
        rusb::DeviceHandle::write_control(self, request_type, request, value, index, buf, timeout)
    }

    fn read_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> rusb::Result<usize> {
        rusb::DeviceHandle::read_control(self, request_type, request, value, index, buf, timeout)
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> rusb::Result<usize> {
        rusb::DeviceHandle::read_bulk(self, endpoint, buf, timeout)
    }

    fn clear_halt(&self, endpoint: u8) -> rusb::Result<()> {
        rusb::DeviceHandle::clear_halt(self, endpoint)
    }

    fn release_interface(&self, iface: u8) -> rusb::Result<()> {
        rusb::DeviceHandle::release_interface(self, iface)
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::UsbTransport;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    /// C-defined wire expectations for boundary tests — transcribed
    /// independently from the Rust enums (`commands.rs`) so a
    /// Rust-to-wire mismatch cannot hide. Sources: the
    /// `airspy_vendor_request` enum (`airspy_commands.h`), the
    /// `receiver_mode_t` enum, `LIBUSB_CTRL_TIMEOUT_MS` (airspy.c),
    /// and the vendor `bmRequestType` compositions.
    pub(crate) mod wire {
        use std::time::Duration;

        /// `AIRSPY_RECEIVER_MODE = 1` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE: u8 = 1;
        /// `RECEIVER_MODE_OFF = 0` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE_OFF: u16 = 0;
        /// `RECEIVER_MODE_RX = 1` (`airspy_commands.h`).
        pub(crate) const RECEIVER_MODE_RX: u16 = 1;
        /// `AIRSPY_SET_FREQ = 13` (`airspy_commands.h`).
        pub(crate) const SET_FREQ: u8 = 13;
        /// `AIRSPY_BOARD_ID_READ = 9` (`airspy_commands.h`).
        pub(crate) const BOARD_ID_READ: u8 = 9;
        /// OUT|VENDOR|DEVICE (airspy.c's host-to-device transfers).
        pub(crate) const VENDOR_OUT: u8 = 0x40;
        /// IN|VENDOR|DEVICE (airspy.c's device-to-host transfers).
        pub(crate) const VENDOR_IN: u8 = 0xC0;
        /// `LIBUSB_CTRL_TIMEOUT_MS = 500` (airspy.c).
        pub(crate) const CTRL_TIMEOUT: Duration = Duration::from_millis(500);
        /// The samplerate fallback pair `airspy_open_init` installs
        /// when the firmware query fails: `{10000000, 2500000}`
        /// (airspy.c).
        pub(crate) const FALLBACK_SAMPLERATES: [u32; 2] = [10_000_000, 2_500_000];
    }

    /// Poll delay served while the bulk script is exhausted — a
    /// mock-only pacing value with no C equivalent (the real device
    /// blocks in libusb instead).
    const EXHAUSTED_BULK_POLL: Duration = Duration::from_millis(5);

    /// One recorded bulk read's parameters.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct BulkCall {
        pub(crate) endpoint: u8,
        pub(crate) buf_len: usize,
        pub(crate) timeout: Duration,
    }

    /// One recorded control transfer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct ControlCall {
        pub(crate) request_type: u8,
        pub(crate) request: u8,
        pub(crate) value: u16,
        pub(crate) index: u16,
        /// Payload written (OUT) or buffer length requested (IN).
        pub(crate) data: Vec<u8>,
        pub(crate) timeout: Duration,
    }

    /// A scripted response for one control transfer, consumed in
    /// order. `Ok` carries bytes to return: for IN requests they fill
    /// the caller's buffer; for OUT requests the value is the
    /// transferred-byte count.
    type Scripted = rusb::Result<Vec<u8>>;

    /// One scripted bulk-read outcome.
    #[derive(Debug, Clone)]
    pub(crate) enum BulkRead {
        /// Fill the whole buffer with this byte (a complete transfer).
        Fill(u8),
        /// Transfer only this many bytes — clamped to strictly less
        /// than the buffer length so a mis-scripted value can never
        /// masquerade as a complete transfer.
        Short(usize),
        /// Fail with this USB error.
        Fail(rusb::Error),
    }

    /// Recording, scriptable [`UsbTransport`] for boundary tests.
    /// Read and write responses queue separately so a control write
    /// can never consume a response scripted for a read (or vice
    /// versa), keeping tests independent of unrelated call ordering.
    #[derive(Debug, Default)]
    pub(crate) struct MockTransport {
        pub(crate) calls: Mutex<Vec<ControlCall>>,
        pub(crate) bulk_calls: Mutex<Vec<BulkCall>>,
        write_responses: Mutex<VecDeque<Scripted>>,
        read_responses: Mutex<VecDeque<Scripted>>,
        bulk: Mutex<VecDeque<BulkRead>>,
    }

    impl MockTransport {
        /// Queue scripted responses for control WRITES (consumed
        /// FIFO). Unscripted writes succeed in full, so receiver-mode
        /// chatter needn't be scripted everywhere; `Ok` values are
        /// transferred-byte counts.
        pub(crate) fn script_writes(&self, responses: Vec<Scripted>) {
            *self.write_responses.lock().expect("mock lock") = responses.into();
        }

        /// Queue scripted responses for control READS (consumed FIFO).
        /// Unscripted reads fail with `NoDevice` so missing
        /// expectations surface loudly; `Ok` bytes fill the caller's
        /// buffer.
        pub(crate) fn script_reads(&self, responses: Vec<Scripted>) {
            *self.read_responses.lock().expect("mock lock") = responses.into();
        }

        /// Queue bulk-read outcomes (consumed FIFO); once exhausted,
        /// further reads time out — which the reader loop tolerates —
        /// so the stream stays alive for the consumer to drain
        /// (terminal outcomes are scripted explicitly).
        pub(crate) fn script_bulk(&self, reads: Vec<BulkRead>) {
            *self.bulk.lock().expect("mock lock") = reads.into();
        }

        /// Drain the recorded calls (e.g. to discard construction-time
        /// traffic before the assertion window).
        pub(crate) fn take_recorded(&self) -> Vec<ControlCall> {
            std::mem::take(&mut *self.calls.lock().expect("mock lock"))
        }

        fn next_write_response(&self) -> Option<Scripted> {
            self.write_responses.lock().expect("mock lock").pop_front()
        }

        fn next_read_response(&self) -> Option<Scripted> {
            self.read_responses.lock().expect("mock lock").pop_front()
        }
    }

    impl UsbTransport for MockTransport {
        fn write_control(
            &self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            buf: &[u8],
            timeout: Duration,
        ) -> rusb::Result<usize> {
            self.calls.lock().expect("mock lock").push(ControlCall {
                request_type,
                request,
                value,
                index,
                data: buf.to_vec(),
                timeout,
            });
            match self.next_write_response() {
                None => Ok(buf.len()),
                Some(Ok(bytes)) => Ok(bytes.len()),
                Some(Err(e)) => Err(e),
            }
        }

        fn read_control(
            &self,
            request_type: u8,
            request: u8,
            value: u16,
            index: u16,
            buf: &mut [u8],
            timeout: Duration,
        ) -> rusb::Result<usize> {
            self.calls.lock().expect("mock lock").push(ControlCall {
                request_type,
                request,
                value,
                index,
                data: vec![0; buf.len()],
                timeout,
            });
            match self.next_read_response() {
                // Unscripted reads fail loudly: silently returning
                // zeroed data would hide missing test expectations.
                None => Err(rusb::Error::NoDevice),
                Some(Ok(bytes)) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
                Some(Err(e)) => Err(e),
            }
        }

        fn read_bulk(
            &self,
            endpoint: u8,
            buf: &mut [u8],
            timeout: Duration,
        ) -> rusb::Result<usize> {
            self.bulk_calls.lock().expect("mock lock").push(BulkCall {
                endpoint,
                buf_len: buf.len(),
                timeout,
            });
            match self.bulk.lock().expect("mock lock").pop_front() {
                Some(BulkRead::Fill(byte)) => {
                    buf.fill(byte);
                    Ok(buf.len())
                }
                // Strictly shorter than the buffer, per the variant's
                // contract.
                Some(BulkRead::Short(n)) => Ok(n.min(buf.len().saturating_sub(1))),
                Some(BulkRead::Fail(e)) => Err(e),
                // Exhausted script: keep the stream alive via the
                // tolerated timeout path (brief sleep avoids a busy
                // poll loop).
                None => {
                    std::thread::sleep(EXHAUSTED_BULK_POLL);
                    Err(rusb::Error::Timeout)
                }
            }
        }

        fn clear_halt(&self, _endpoint: u8) -> rusb::Result<()> {
            Ok(())
        }

        fn release_interface(&self, _iface: u8) -> rusb::Result<()> {
            Ok(())
        }
    }
}

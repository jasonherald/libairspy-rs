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
        /// Transfer only this many bytes (a short transfer).
        Short(usize),
        /// Fail with this USB error.
        Fail(rusb::Error),
    }

    /// Recording, scriptable [`UsbTransport`] for boundary tests.
    #[derive(Debug, Default)]
    pub(crate) struct MockTransport {
        pub(crate) calls: Mutex<Vec<ControlCall>>,
        responses: Mutex<VecDeque<Scripted>>,
        bulk: Mutex<VecDeque<BulkRead>>,
    }

    impl MockTransport {
        /// Queue the next scripted control responses (consumed FIFO).
        /// Unscripted writes succeed in full (so receiver-mode chatter
        /// needn't be scripted everywhere); unscripted reads fail with
        /// `NoDevice` so missing expectations surface loudly.
        pub(crate) fn script(&self, responses: Vec<Scripted>) {
            *self.responses.lock().expect("mock lock") = responses.into();
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

        fn next_response(&self) -> Option<Scripted> {
            self.responses.lock().expect("mock lock").pop_front()
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
            match self.next_response() {
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
            match self.next_response() {
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
            _endpoint: u8,
            buf: &mut [u8],
            _timeout: Duration,
        ) -> rusb::Result<usize> {
            match self.bulk.lock().expect("mock lock").pop_front() {
                Some(BulkRead::Fill(byte)) => {
                    buf.fill(byte);
                    Ok(buf.len())
                }
                Some(BulkRead::Short(n)) => Ok(n.min(buf.len())),
                Some(BulkRead::Fail(e)) => Err(e),
                // Exhausted script: keep the stream alive via the
                // tolerated timeout path (brief sleep avoids a busy
                // poll loop).
                None => {
                    std::thread::sleep(Duration::from_millis(5));
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

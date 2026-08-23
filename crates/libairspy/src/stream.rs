//! The bulk-transfer streaming engine, ported from `airspy.c`'s
//! transfer machinery: `airspy_start_rx` / `airspy_stop_rx` /
//! `airspy_is_streaming`, the libusb transfer callback's 8-slot swap
//! ring, and the producer/consumer thread pair.
//!
//! ## Deviation from the C transfer model
//!
//! C queues 16 asynchronous 256 KiB URBs and swaps completed buffers
//! into the ring from libusb's event loop. rusb's safe API exposes
//! only synchronous bulk reads, so this port uses a dedicated reader
//! thread issuing back-to-back `read_bulk` calls into recycled
//! buffers — the same strategy librtlsdr-rs ships. The ring, drop
//! accounting, and stop semantics are unchanged. Whether one queued
//! URB sustains 10 MSPS gaplessly is a hardware-validation (M6)
//! question; if drops appear there, an async-URB upgrade gets its own
//! issue.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::commands::{Command, ReceiverMode};
use crate::device::Device;
use crate::error::{Error, Result};

/// `RAW_BUFFER_COUNT` in airspy.c — slots in the received-samples ring.
pub(crate) const RAW_BUFFER_COUNT: usize = 8;

/// `device->buffer_size` in `airspy_open_init` (airspy.c) — bytes per
/// bulk transfer.
pub(crate) const BUFFER_SIZE: usize = 262_144;

/// `LIBUSB_ENDPOINT_IN | 1` — the bulk sample endpoint
/// (`create_io_threads` / `airspy_start_rx` in airspy.c).
pub(crate) const BULK_ENDPOINT: u8 = 0x81;

/// The 500 ms event/read timeout from `transfer_threadproc`'s
/// `struct timeval timeout = { 0, 500000 }` (airspy.c).
pub(crate) const EVENT_TIMEOUT: Duration = Duration::from_millis(500);

/// One delivered block of samples — the `airspy_transfer` view handed
/// to the `airspy_start_rx` callback.
///
/// Until the sample-type pipeline lands, `samples` carries the raw
/// bulk bytes (`AIRSPY_SAMPLE_RAW` semantics).
#[derive(Debug)]
pub struct Transfer<'a> {
    /// Raw sample bytes for this block.
    pub samples: &'a [u8],
    /// `dropped_buffers * sample_count` — samples lost to a full ring
    /// since the previous delivery (`airspy_transfer.dropped_samples`).
    pub dropped_samples: u64,
}

/// The 8-slot ring from `airspy_libusb_transfer_callback` +
/// `consumer_threadproc`, with C's buffer swap expressed as a bounded
/// recycling pool: producers take free buffers, push them filled, and
/// consumers recycle them after delivery — zero steady-state
/// allocation, exactly one owner per buffer.
pub(crate) struct SampleQueue {
    state: Mutex<QueueState>,
    cv: Condvar,
    capacity: usize,
}

struct QueueState {
    /// Filled buffers awaiting the consumer, each carrying the
    /// dropped-buffer count accumulated before it was queued
    /// (`dropped_buffers_queue` in airspy.c).
    filled: VecDeque<(Vec<u8>, u32)>,
    /// Recycled buffers available to the producer.
    free: Vec<Vec<u8>>,
    /// `device->dropped_buffers` — buffers discarded since the last
    /// successful queue insert.
    dropped: u32,
    /// Set by [`SampleQueue::shutdown`]; wakes and finishes waiters.
    shutdown: bool,
}

impl SampleQueue {
    /// A queue with `slots` ring positions and a pool of `pool`
    /// preallocated buffers.
    pub(crate) fn with_pool(slots: usize, pool: usize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                filled: VecDeque::with_capacity(slots),
                free: (0..pool).map(|_| vec![0u8; BUFFER_SIZE]).collect(),
                dropped: 0,
                shutdown: false,
            }),
            cv: Condvar::new(),
            capacity: slots,
        }
    }

    /// The production configuration: `RAW_BUFFER_COUNT` slots plus one
    /// extra buffer so the producer always has one in flight.
    pub(crate) fn for_streaming() -> Self {
        Self::with_pool(RAW_BUFFER_COUNT, RAW_BUFFER_COUNT + 1)
    }

    /// Take a free buffer without blocking. `None` when the pool is
    /// exhausted (bounded by construction). Test-only: production
    /// paths use the blocking [`SampleQueue::acquire_free`].
    #[cfg(test)]
    pub(crate) fn try_acquire_free(&self) -> Option<Vec<u8>> {
        match self.state.lock() {
            Ok(mut s) => s.free.pop(),
            Err(_) => None,
        }
    }

    /// Take a free buffer, blocking until one is recycled or the queue
    /// shuts down (`None`). Replaces a busy-wait in the reader: with
    /// the sync read model, an exhausted pool means the consumer holds
    /// every buffer, and the reader must sleep rather than spin.
    pub(crate) fn acquire_free(&self) -> Option<Vec<u8>> {
        let mut s = self.state.lock().ok()?;
        loop {
            if let Some(buf) = s.free.pop() {
                return Some(buf);
            }
            if s.shutdown {
                return None;
            }
            s = self.cv.wait(s).ok()?;
        }
    }

    /// Queue a filled buffer. On a full ring the buffer is recycled
    /// and the drop counter increments (C: `dropped_buffers++`, the
    /// transfer resubmits).
    pub(crate) fn push_filled(&self, buf: Vec<u8>) {
        let Ok(mut s) = self.state.lock() else { return };
        if s.filled.len() >= self.capacity {
            s.dropped = s.dropped.saturating_add(1);
            s.free.push(buf);
            drop(s);
            // The recycled buffer may unblock a waiting reader.
            self.cv.notify_all();
            return;
        }
        let dropped = std::mem::take(&mut s.dropped);
        s.filled.push_back((buf, dropped));
        drop(s);
        // One condvar serves both directions (filled and free);
        // notify_all so a waiting consumer AND a buffer-starved
        // reader can each re-check their predicate.
        self.cv.notify_all();
    }

    /// Blocking pop: waits until a filled buffer arrives or
    /// [`SampleQueue::shutdown`] runs. `None` means shut down.
    pub(crate) fn pop_filled(&self) -> Option<(Vec<u8>, u32)> {
        let mut s = self.state.lock().ok()?;
        loop {
            if let Some(entry) = s.filled.pop_front() {
                return Some(entry);
            }
            if s.shutdown {
                return None;
            }
            s = self.cv.wait(s).ok()?;
        }
    }

    /// Return a consumed buffer to the free pool, waking a
    /// buffer-starved reader.
    pub(crate) fn recycle(&self, buf: Vec<u8>) {
        if let Ok(mut s) = self.state.lock() {
            s.free.push(buf);
            drop(s);
            self.cv.notify_all();
        }
    }

    /// Wake all waiters and make further pops return `None`
    /// (`kill_io_threads`' condvar signal in airspy.c).
    pub(crate) fn shutdown(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.shutdown = true;
        }
        self.cv.notify_all();
    }
}

/// State shared between the reader thread, the consumer thread, and
/// the owning [`Device`] — the streaming fields of `airspy_device_t`.
pub(crate) struct StreamShared {
    /// `device->streaming`.
    pub(crate) streaming: AtomicBool,
    /// `device->stop_requested`.
    pub(crate) stop_requested: AtomicBool,
    pub(crate) queue: SampleQueue,
}

impl StreamShared {
    pub(crate) fn new(queue: SampleQueue) -> Self {
        Self {
            streaming: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            queue,
        }
    }

    fn running(&self) -> bool {
        self.streaming.load(Ordering::SeqCst) && !self.stop_requested.load(Ordering::SeqCst)
    }
}

/// `consumer_threadproc`: pop filled buffers, hand them to the
/// callback, recycle. A `false` return stops streaming. Sample-type
/// conversion slots in here when the pipeline lands.
pub(crate) fn run_consumer(
    shared: &StreamShared,
    callback: &mut (impl FnMut(Transfer<'_>) -> bool + ?Sized),
) {
    // C computes dropped_samples as dropped_buffers * sample_count
    // where sample_count is the u16 count per buffer.
    let sample_count = (BUFFER_SIZE / 2) as u64;
    while shared.running() {
        let Some((buf, dropped)) = shared.queue.pop_filled() else {
            break;
        };
        let keep_going = callback(Transfer {
            samples: &buf,
            dropped_samples: u64::from(dropped) * sample_count,
        });
        shared.queue.recycle(buf);
        if !keep_going {
            shared.streaming.store(false, Ordering::SeqCst);
        }
    }
    shared.streaming.store(false, Ordering::SeqCst);
}

/// The reader half — C's transfer thread plus libusb callback,
/// collapsed into a synchronous `read_bulk` loop (see the module
/// docs).
fn run_reader(shared: &StreamShared, handle: &rusb::DeviceHandle<rusb::Context>) {
    while shared.running() {
        // Blocks until the consumer recycles a buffer or stop shuts
        // the queue down. While blocked no reads are issued, so USB
        // data lost in that window is uncounted — unlike C, whose
        // still-completing transfers increment dropped_buffers; see
        // the module docs on the sync-read deviation.
        let Some(mut buf) = shared.queue.acquire_free() else {
            break;
        };
        match handle.read_bulk(BULK_ENDPOINT, &mut buf, EVENT_TIMEOUT) {
            // C requires actual_length == length; a short transfer
            // stops streaming.
            Ok(n) if n == BUFFER_SIZE => shared.queue.push_filled(buf),
            Err(rusb::Error::Timeout | rusb::Error::Interrupted) => {
                // The C event loop tolerates timeouts/EINTR and keeps
                // polling while streaming.
                shared.queue.recycle(buf);
            }
            Ok(n) => {
                tracing::warn!(
                    bytes = n,
                    expected = BUFFER_SIZE,
                    "short bulk transfer; stopping stream"
                );
                shared.queue.recycle(buf);
                shared.streaming.store(false, Ordering::SeqCst);
            }
            Err(err) => {
                tracing::warn!(%err, "bulk read failed; stopping stream");
                shared.queue.recycle(buf);
                shared.streaming.store(false, Ordering::SeqCst);
            }
        }
    }
    shared.streaming.store(false, Ordering::SeqCst);
    shared.queue.shutdown();
}

/// Worker-thread handles held by a streaming [`Device`].
pub(crate) struct StreamWorkers {
    pub(crate) shared: Arc<StreamShared>,
    reader: Option<JoinHandle<()>>,
    consumer: Option<JoinHandle<()>>,
}

impl Device {
    /// `airspy_set_receiver_mode`: `AIRSPY_RECEIVER_MODE` with the
    /// mode in wValue.
    fn set_receiver_mode(&self, mode: ReceiverMode) -> Result<()> {
        let n = self.vendor_out(Command::ReceiverMode, mode as u16, 0, &[])?;
        if n != 0 {
            // C: result != 0 → AIRSPY_ERROR_LIBUSB.
            return Err(Error::Usb(rusb::Error::Other));
        }
        Ok(())
    }

    /// Start streaming (`airspy_start_rx`): receiver off, clear the
    /// bulk endpoint halt, receiver on, then spawn the reader and
    /// consumer threads. Returns [`Error::Busy`] while streaming.
    ///
    /// The callback runs on the consumer thread; return `true` to
    /// keep streaming, `false` to stop (C's nonzero-return stop).
    pub fn start_rx(
        &mut self,
        callback: impl FnMut(Transfer<'_>) -> bool + Send + 'static,
    ) -> Result<()> {
        // C gates on `streaming || stop_requested` only, so a device
        // whose callback stopped the stream can be restarted; reap
        // finished workers before the busy check.
        if let Some(workers) = self.stream_workers() {
            if workers.shared.running() {
                return Err(Error::Busy);
            }
            let mut finished = self
                .take_stream_workers()
                .unwrap_or_else(|| unreachable!("workers just observed"));
            finished.shared.queue.shutdown();
            if let Some(t) = finished.reader.take() {
                let _ = t.join();
            }
            if let Some(t) = finished.consumer.take() {
                let _ = t.join();
            }
        }
        // Converter resets and drop-counter zeroing from
        // airspy_start_rx happen via fresh queue/converter state here.
        self.set_receiver_mode(ReceiverMode::Off)?;
        // C ignores the clear-halt result.
        let _ = self.usb_handle().clear_halt(BULK_ENDPOINT);
        self.set_receiver_mode(ReceiverMode::Rx)?;

        let shared = Arc::new(StreamShared::new(SampleQueue::for_streaming()));
        shared.streaming.store(true, Ordering::SeqCst);

        let consumer_shared = Arc::clone(&shared);
        let mut cb = callback;
        let consumer = std::thread::Builder::new()
            .name("airspy-consumer".into())
            .spawn(move || run_consumer(&consumer_shared, &mut cb))
            .map_err(|_| Error::Thread)?;

        let reader_shared = Arc::clone(&shared);
        let reader_handle = self.usb_handle_arc();
        let spawn_result = std::thread::Builder::new()
            .name("airspy-reader".into())
            .spawn(move || run_reader(&reader_shared, &reader_handle));
        let Ok(reader) = spawn_result else {
            // Don't orphan the already-running consumer.
            shared.streaming.store(false, Ordering::SeqCst);
            shared.queue.shutdown();
            let _ = consumer.join();
            return Err(Error::Thread);
        };

        self.set_stream_workers(Some(StreamWorkers {
            shared,
            reader: Some(reader),
            consumer: Some(consumer),
        }));
        Ok(())
    }

    /// Stop streaming (`airspy_stop_rx`): request stop, switch the
    /// receiver off, then join the worker threads.
    pub fn stop_rx(&mut self) -> Result<()> {
        let Some(mut workers) = self.take_stream_workers() else {
            return Ok(());
        };
        workers.shared.stop_requested.store(true, Ordering::SeqCst);
        let mode_result = self.set_receiver_mode(ReceiverMode::Off);

        // kill_io_threads: clear flags, wake the consumer, join both.
        workers.shared.streaming.store(false, Ordering::SeqCst);
        workers.shared.queue.shutdown();
        if let Some(t) = workers.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = workers.consumer.take() {
            let _ = t.join();
        }
        workers.shared.stop_requested.store(false, Ordering::SeqCst);

        mode_result
    }

    /// `airspy_is_streaming`: true between a successful
    /// [`Device::start_rx`] and a stop request.
    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.stream_workers().is_some_and(|w| w.shared.running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[test]
    fn constants_match_c() {
        // RAW_BUFFER_COUNT (8), device->buffer_size (262144), and the
        // bulk endpoint (LIBUSB_ENDPOINT_IN | 1) from airspy.c.
        assert_eq!(RAW_BUFFER_COUNT, 8);
        assert_eq!(BUFFER_SIZE, 262_144);
        assert_eq!(BULK_ENDPOINT, 0x81);
        assert_eq!(EVENT_TIMEOUT, core::time::Duration::from_millis(500));
    }

    fn make_queue() -> SampleQueue {
        SampleQueue::with_pool(3, 4) // 3 slots, 4-buffer pool, tiny for tests
    }

    #[test]
    fn buffers_pass_through_in_fifo_order() {
        let q = make_queue();
        let mut a = q.acquire_free().expect("pool has buffers");
        a[0] = 1;
        let mut b = q.acquire_free().expect("pool has buffers");
        b[0] = 2;
        q.push_filled(a);
        q.push_filled(b);
        let (first, dropped) = q.pop_filled().expect("filled");
        assert_eq!((first[0], dropped), (1, 0));
        let (second, dropped) = q.pop_filled().expect("filled");
        assert_eq!((second[0], dropped), (2, 0));
    }

    #[test]
    fn full_queue_drops_and_next_entry_carries_the_count() {
        let q = make_queue();
        for _ in 0..3 {
            let buf = q.acquire_free().expect("pool");
            q.push_filled(buf);
        }
        // Queue full: this push is dropped, its buffer recycled
        // (C: dropped_buffers++ and the transfer resubmits).
        let buf = q.acquire_free().expect("pool");
        q.push_filled(buf);

        // Drain one slot, then push again: the new entry carries the
        // accumulated drop count and the counter resets (C's
        // dropped_buffers_queue[head] = dropped_buffers; dropped = 0).
        let (buf, dropped) = q.pop_filled().expect("filled");
        assert_eq!(dropped, 0);
        q.recycle(buf);
        let buf = q.acquire_free().expect("pool");
        q.push_filled(buf);
        let (_, d0) = q.pop_filled().expect("filled");
        assert_eq!(d0, 0); // second slot, filled before the drop
        let (_, d1) = q.pop_filled().expect("filled");
        assert_eq!(d1, 0); // third slot
        let (_, d2) = q.pop_filled().expect("filled");
        assert_eq!(d2, 1); // the post-drop push carries the count
    }

    #[test]
    fn pool_is_bounded() {
        let q = make_queue();
        let held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        assert!(q.try_acquire_free().is_none(), "pool must be bounded");
        drop(held);
    }

    #[test]
    fn blocking_acquire_wakes_on_recycle() {
        let q = Arc::new(make_queue());
        let held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.acquire_free());
        std::thread::sleep(core::time::Duration::from_millis(50));
        let mut held = held;
        q.recycle(held.pop().expect("held"));
        assert!(waiter.join().expect("join").is_some());
    }

    #[test]
    fn blocking_acquire_wakes_none_on_shutdown() {
        let q = Arc::new(make_queue());
        let _held: Vec<_> = (0..4)
            .map(|_| q.try_acquire_free().expect("pool"))
            .collect();
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.acquire_free());
        std::thread::sleep(core::time::Duration::from_millis(50));
        q.shutdown();
        assert!(waiter.join().expect("join").is_none());
    }

    #[test]
    fn shutdown_wakes_blocking_pop() {
        let q = Arc::new(make_queue());
        let q2 = Arc::clone(&q);
        let waiter = std::thread::spawn(move || q2.pop_filled());
        std::thread::sleep(core::time::Duration::from_millis(50));
        q.shutdown();
        assert!(waiter.join().expect("join").is_none());
    }

    #[test]
    fn consumer_loop_delivers_transfers_and_stops_on_false() {
        // Mirrors consumer_threadproc: pops buffers, hands the caller
        // a Transfer with the accumulated dropped-sample count, and a
        // callback returning false clears `streaming` (C: nonzero
        // return → device->streaming = false).
        let shared = Arc::new(StreamShared::new(SampleQueue::with_pool(3, 4)));
        shared.streaming.store(true, Ordering::SeqCst);

        for fill in [1u8, 2, 3] {
            let mut buf = shared.queue.acquire_free().expect("pool");
            buf[0] = fill;
            shared.queue.push_filled(buf);
        }

        let mut seen = Vec::new();
        let shared2 = Arc::clone(&shared);
        let consumer = std::thread::spawn(move || {
            run_consumer(&shared2, &mut |transfer: Transfer<'_>| {
                seen.push((transfer.samples[0], transfer.dropped_samples));
                seen.len() < 2 // stop after the second delivery
            });
            seen
        });

        let seen = consumer.join().expect("join");
        assert_eq!(seen, vec![(1, 0), (2, 0)]);
        assert!(!shared.streaming.load(Ordering::SeqCst));
    }
}

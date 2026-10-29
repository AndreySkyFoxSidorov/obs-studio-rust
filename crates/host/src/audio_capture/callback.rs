//! Preallocated, single-producer callback handoff. Construct and retire both
//! endpoints on the native control owner, after the callback has quiesced.
use super::{AudioCaptureError, AudioCaptureLimits, NativeAudioFormat, NativePacketFlags};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use std::{
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Raw device time. Interpretation/conversion belongs to the worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackTime {
    Unavailable,
    /// DirectShow graph-relative presentation time, not a QPC timestamp.
    DirectShow100ns { value:i64, generation:Option<u64> },
    HostNanoseconds(u64),
    MachTicks(u64),
    CoreMedia {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackStamp {
    pub device: CallbackTime,
    pub received_ticks: u64,
}

struct Slot {
    samples: Vec<f32>,
    owner: Arc<()>,
}
/// One complete interleaved PCM packet, never a truncated prefix. Recycle it on
/// the processing worker when its samples have been consumed.
pub struct CallbackPacket {
    pub format: NativeAudioFormat,
    pub frames: usize,
    pub stamp: CallbackStamp,
    pub flags: NativePacketFlags,
    count: usize,
    slot: Slot,
}
impl CallbackPacket {
    pub fn samples(&self) -> &[f32] {
        &self.slot.samples[..self.count]
    }
}

pub struct CallbackSender {
    free: Consumer<Slot>,
    ready: Producer<CallbackPacket>,
    parked: Option<Slot>,
    limits: AudioCaptureLimits,
    pending_dropped: u64,
    total_dropped: u64,
    drops: Arc<AtomicU64>,
}
pub struct CallbackReceiver {
    free: Producer<Slot>,
    ready: Consumer<CallbackPacket>,
    owner: Arc<()>,
    drops: Arc<AtomicU64>,
    retained_bytes: usize,
}

/// All allocation, initialization and budget admission happen here, outside the
/// callback. Both rings have capacity N; exactly N PCM buffers exist.
pub fn pool(
    limits: AudioCaptureLimits,
) -> Result<(CallbackSender, CallbackReceiver), AudioCaptureError> {
    if !(1..=32).contains(&limits.callback_slots)
        || !(1..=4096).contains(&limits.max_frames)
        || !(1..=8).contains(&limits.max_channels)
        || !(4..=128 * 1024).contains(&limits.max_packet_bytes)
        || !(1..=4 * 1024 * 1024).contains(&limits.max_native_buffer_bytes)
    {
        return Err(AudioCaptureError::Limit("invalid callback pool limits"));
    }
    let count = limits
        .max_frames
        .checked_mul(limits.max_channels)
        .ok_or(AudioCaptureError::Limit("callback sample count"))?;
    let pcm_bytes = count
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_mul(limits.callback_slots))
        .ok_or(AudioCaptureError::Limit("callback PCM pool bytes"))?;
    // Include both ring payload arrays, their public ring layouts, endpoint
    // storage and conservative Arc headers. Allocator bookkeeping/rounding is
    // not a cross-platform process-resident-memory guarantee.
    let ring_entries = size_of::<Slot>()
        .checked_add(size_of::<CallbackPacket>())
        .and_then(|bytes| bytes.checked_mul(limits.callback_slots))
        .ok_or(AudioCaptureError::Limit("callback ring metadata"))?;
    let fixed = size_of::<RingBuffer<Slot>>()
        + size_of::<RingBuffer<CallbackPacket>>()
        + size_of::<CallbackSender>()
        + size_of::<CallbackReceiver>()
        + size_of::<[usize; 8]>()
        + size_of::<AtomicU64>();
    let retained_bytes = pcm_bytes
        .checked_add(ring_entries)
        .and_then(|bytes| bytes.checked_add(fixed))
        .ok_or(AudioCaptureError::Limit("callback pool bytes"))?;
    if retained_bytes > limits.max_native_buffer_bytes {
        return Err(AudioCaptureError::Limit("callback pool byte budget"));
    }
    let (mut free_writer, free_reader) = RingBuffer::new(limits.callback_slots);
    let (ready_writer, ready_reader) = RingBuffer::new(limits.callback_slots);
    let owner = Arc::new(());
    let drops = Arc::new(AtomicU64::new(0));
    for _ in 0..limits.callback_slots {
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(count)
            .map_err(|_| AudioCaptureError::Limit("callback PCM allocation"))?;
        samples.resize(count, 0.0);
        // Reserve-exact may legally return a larger capacity. Charge that
        // actual advertised excess before retaining the slot.
        if samples.capacity() > count {
            return Err(AudioCaptureError::Limit("callback PCM excess allocation"));
        }
        if free_writer
            .push(Slot {
                samples,
                owner: owner.clone(),
            })
            .is_err()
        {
            return Err(AudioCaptureError::Invalid("callback pool initialization"));
        }
    }
    Ok((
        CallbackSender {
            free: free_reader,
            ready: ready_writer,
            parked: None,
            limits,
            pending_dropped: 0,
            total_dropped: 0,
            drops: drops.clone(),
        },
        CallbackReceiver {
            free: free_writer,
            ready: ready_reader,
            owner,
            drops,
            retained_bytes,
        },
    ))
}
impl CallbackSender {
    /// Mark an unsupported native packet/topology without copying or allocating.
    /// Only this producer writes the counters, so saturating stores need no CAS
    /// retry loop. The worker may read the cumulative diagnostic atomically.
    pub fn reject(&mut self) {
        self.pending_dropped = self.pending_dropped.saturating_add(1);
        self.total_dropped = self.total_dropped.saturating_add(1);
        self.drops.store(self.total_dropped, Ordering::Relaxed);
    }
    pub fn dropped_packets(&self) -> u64 {
        self.total_dropped
    }
    /// Copy exactly frames*channels samples into one already initialized slot.
    /// The closure and its captures must themselves avoid allocation, blocking,
    /// panic, logging and expensive final destruction. No closure is invoked
    /// for rejected counts, an exhausted pool or an abandoned receiver.
    pub fn submit(
        &mut self,
        format: NativeAudioFormat,
        frames: usize,
        stamp: CallbackStamp,
        mut flags: NativePacketFlags,
        copy: impl FnOnce(&mut [f32]),
    ) -> bool {
        let count = frames.checked_mul(usize::from(format.channels));
        if frames == 0
            || frames > self.limits.max_frames
            || format.validate(self.limits).is_err()
            || count
                .and_then(|count| count.checked_mul(size_of::<f32>()))
                .is_none_or(|bytes| bytes > self.limits.max_packet_bytes)
            || self.ready.is_abandoned()
        {
            self.reject();
            return false;
        }
        let count = match count {
            Some(count) => count,
            None => {
                self.reject();
                return false;
            }
        };
        let mut slot = match self.parked.take().or_else(|| self.free.pop().ok()) {
            Some(slot) => slot,
            None => {
                self.reject();
                return false;
            }
        };
        // Pool construction and admission imply this bound; keep a non-panicking
        // defensive guard without retiring the buffer on the callback thread.
        if count > slot.samples.len() {
            self.parked = Some(slot);
            self.reject();
            return false;
        }
        copy(&mut slot.samples[..count]);
        if self.pending_dropped != 0 {
            flags.discontinuity = true;
            flags.queue_dropped = flags.queue_dropped.saturating_add(self.pending_dropped);
        }
        let packet = CallbackPacket {
            format,
            frames,
            stamp,
            flags,
            count,
            slot,
        };
        match self.ready.push(packet) {
            Ok(()) => {
                self.pending_dropped = 0;
                true
            }
            Err(PushError::Full(packet)) => {
                // With N slots and two N-entry rings this cannot occur under
                // the ownership protocol. Preserve the returned buffer anyway.
                self.parked = Some(packet.slot);
                self.reject();
                false
            }
        }
    }
}
impl CallbackReceiver {
    pub fn pop(&mut self) -> Option<CallbackPacket> {
        self.ready.pop().ok()
    }
    /// Worker-only return. False means foreign ownership or a violated pool
    /// protocol; any rejected packet is retired here, never in submit.
    pub fn recycle(&mut self, packet: CallbackPacket) -> bool {
        if !Arc::ptr_eq(&packet.slot.owner, &self.owner) {
            return false;
        }
        self.free.push(packet.slot).is_ok()
    }
    pub fn dropped_packets(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };
    thread_local! {
        static TRACKING:Cell<bool>=const {Cell::new(false)};
        static ALLOCATIONS:Cell<(usize,usize)>=const {Cell::new((0,0))};
    }
    struct TrackingAllocator;
    #[global_allocator]
    static ALLOCATOR: TrackingAllocator = TrackingAllocator;
    fn allocation(allocate: bool) {
        if TRACKING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|counts| {
                let (a, d) = counts.get();
                counts.set(if allocate {
                    (a.saturating_add(1), d)
                } else {
                    (a, d.saturating_add(1))
                });
            });
        }
    }
    // Test-only passthrough instrumentation. The production pool contains no
    // unsafe code; these forwarding calls are required by GlobalAlloc's API.
    unsafe impl GlobalAlloc for TrackingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            allocation(true);
            // SAFETY: forward the original allocation layout to System.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            allocation(true);
            // SAFETY: forward the original allocation layout to System.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            allocation(false);
            // SAFETY: the caller's original pointer/layout pair is unchanged.
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            allocation(true);
            allocation(false);
            // SAFETY: the caller's original allocation arguments are unchanged.
            unsafe { System.realloc(pointer, layout, size) }
        }
    }
    fn measured<T>(work: impl FnOnce() -> T) -> (T, (usize, usize)) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TRACKING.with(|tracking| tracking.set(false));
            }
        }
        ALLOCATIONS.with(|counts| counts.set((0, 0)));
        TRACKING.with(|tracking| tracking.set(true));
        let reset = Reset;
        let value = work();
        drop(reset);
        (value, ALLOCATIONS.with(Cell::get))
    }
    fn format() -> NativeAudioFormat {
        NativeAudioFormat {
            sample_rate: 48000,
            channels: 2,
            channel_mask: 3,
        }
    }
    fn stamp() -> CallbackStamp {
        CallbackStamp {
            device: CallbackTime::CoreMedia {
                value: 17,
                timescale: 48000,
                flags: 5,
                epoch: 2,
            },
            received_ticks: 999,
        }
    }
    fn limits(slots: usize) -> AudioCaptureLimits {
        AudioCaptureLimits {
            callback_slots: slots,
            ..Default::default()
        }
    }
    #[test]
    fn pool_admission_charges_pcm_and_metadata_and_rejects_oversized_limits() {
        let (_, receiver) = pool(limits(8)).unwrap();
        assert!(receiver.retained_bytes() > 8 * 4096 * 8 * 4);
        assert!(receiver.retained_bytes() <= AudioCaptureLimits::default().max_native_buffer_bytes);
        assert!(
            pool(AudioCaptureLimits {
                max_frames: 16,
                ..limits(32)
            })
            .is_ok()
        );
        for limits in [
            limits(0),
            limits(33),
            limits(32),
            AudioCaptureLimits {
                max_frames: 4097,
                ..limits(1)
            },
            AudioCaptureLimits {
                max_channels: 9,
                ..limits(1)
            },
            AudioCaptureLimits {
                max_native_buffer_bytes: 16,
                ..limits(1)
            },
            AudioCaptureLimits {
                max_packet_bytes: 3,
                ..limits(1)
            },
        ] {
            assert!(pool(limits).is_err());
        }
    }
    #[test]
    fn full_pool_drops_whole_packets_and_marks_next_accepted_packet_after_recycling() {
        let (mut sender, mut receiver) = pool(limits(2)).unwrap();
        for value in [1.0, 2.0] {
            assert!(sender.submit(
                format(),
                2,
                stamp(),
                NativePacketFlags::default(),
                |samples| samples.fill(value)
            ));
        }
        let mut copied = false;
        assert!(
            !sender.submit(format(), 2, stamp(), NativePacketFlags::default(), |_| {
                copied = true
            })
        );
        assert!(!copied);
        let first = receiver.pop().unwrap();
        let pointer = first.samples().as_ptr();
        let capacity = first.slot.samples.capacity();
        assert!(!sender.submit(
            format(),
            2,
            stamp(),
            NativePacketFlags::default(),
            |_| panic!("held packet is not a free slot")
        ));
        assert!(receiver.recycle(first));
        assert!(sender.submit(
            format(),
            2,
            stamp(),
            NativePacketFlags {
                queue_dropped: 3,
                ..Default::default()
            },
            |samples| samples.copy_from_slice(&[3.0, 4.0, 5.0, 6.0])
        ));
        let second = receiver.pop().unwrap();
        assert_eq!(second.samples(), &[2.0; 4]);
        assert!(!second.flags.discontinuity);
        assert!(receiver.recycle(second));
        let accepted = receiver.pop().unwrap();
        assert_eq!(accepted.samples(), &[3.0, 4.0, 5.0, 6.0]);
        assert_eq!(accepted.samples().as_ptr(), pointer);
        assert_eq!(accepted.slot.samples.capacity(), capacity);
        assert_eq!(accepted.stamp, stamp());
        assert_eq!(accepted.format, format());
        assert_eq!(accepted.frames, 2);
        assert!(accepted.flags.discontinuity);
        assert_eq!(accepted.flags.queue_dropped, 5);
        assert_eq!(receiver.dropped_packets(), 2);
        assert!(receiver.recycle(accepted));
        assert!(sender.submit(
            format(),
            1,
            stamp(),
            NativePacketFlags::default(),
            |samples| samples.fill(7.0)
        ));
        assert_eq!(receiver.pop().unwrap().flags.queue_dropped, 0);
    }
    #[test]
    fn rejects_invalid_format_counts_and_explicit_native_topology_without_copy() {
        let (mut sender, mut receiver) = pool(limits(1)).unwrap();
        for (format, frames) in [
            (format(), 0),
            (format(), 4097),
            (format(), usize::MAX),
            (
                NativeAudioFormat {
                    channels: 9,
                    ..format()
                },
                1,
            ),
            (
                NativeAudioFormat {
                    channel_mask: 1,
                    ..format()
                },
                1,
            ),
        ] {
            assert!(!sender.submit(
                format,
                frames,
                stamp(),
                NativePacketFlags::default(),
                |_| panic!("invalid callback must not copy")
            ));
        }
        sender.reject();
        assert_eq!(sender.dropped_packets(), 6);
        assert!(sender.submit(
            format(),
            2048,
            stamp(),
            NativePacketFlags::default(),
            |samples| for (index, sample) in samples.iter_mut().enumerate() {
                *sample = index as f32;
            }
        ));
        let packet = receiver.pop().unwrap();
        assert_eq!(packet.samples().len(), 4096);
        assert_eq!(packet.samples()[4095], 4095.0);
        assert_eq!(packet.flags.queue_dropped, 6);
        assert!(packet.flags.discontinuity);
    }
    #[test]
    fn unexpected_full_ready_ring_parks_buffer_and_reuses_it_without_loss() {
        let (mut sender, mut receiver) = pool(limits(1)).unwrap();
        // Inject an extra ready entry outside the callback to exercise the
        // defensive path, which valid N-slot ownership cannot reach.
        let slot = Slot {
            samples: vec![0.0; 32768],
            owner: receiver.owner.clone(),
        };
        let packet = CallbackPacket {
            format: format(),
            frames: 1,
            stamp: stamp(),
            flags: NativePacketFlags::default(),
            count: 2,
            slot,
        };
        assert!(sender.ready.push(packet).is_ok());
        let (accepted, allocations) = measured(|| {
            sender.submit(
                format(),
                1,
                stamp(),
                NativePacketFlags::default(),
                |samples| samples.fill(4.0),
            )
        });
        assert!(!accepted);
        assert_eq!(allocations, (0, 0));
        let pointer = sender.parked.as_ref().unwrap().samples.as_ptr();
        drop(receiver.pop().unwrap());
        assert!(sender.submit(
            format(),
            1,
            stamp(),
            NativePacketFlags::default(),
            |samples| samples.fill(8.0)
        ));
        let packet = receiver.pop().unwrap();
        assert_eq!(packet.samples().as_ptr(), pointer);
        assert_eq!(packet.samples(), &[8.0; 2]);
        assert_eq!(packet.flags.queue_dropped, 1);
        assert!(packet.flags.discontinuity);
        assert!(receiver.recycle(packet));
    }
    #[test]
    fn successful_rejected_full_and_stopped_receiver_submissions_allocate_or_retire_nothing() {
        let (mut sender, mut receiver) = pool(limits(1)).unwrap();
        let (accepted, counts) = measured(|| {
            sender.submit(
                format(),
                2,
                stamp(),
                NativePacketFlags::default(),
                |samples| samples.copy_from_slice(&[0.1, 0.2, 0.3, 0.4]),
            )
        });
        assert!(accepted);
        assert_eq!(counts, (0, 0));
        let (accepted, counts) = measured(|| {
            sender.submit(format(), 2, stamp(), NativePacketFlags::default(), |_| {
                panic!("full callback pool must not copy")
            })
        });
        assert!(!accepted);
        assert_eq!(counts, (0, 0));
        let ((), counts) = measured(|| sender.reject());
        assert_eq!(counts, (0, 0));
        let (accepted, counts) = measured(|| {
            sender.submit(
                format(),
                usize::MAX,
                stamp(),
                NativePacketFlags::default(),
                |_| panic!("oversize callback must not copy"),
            )
        });
        assert!(!accepted);
        assert_eq!(counts, (0, 0));
        let packet = receiver.pop().unwrap();
        assert!(receiver.recycle(packet));
        let (accepted, counts) = measured(|| {
            sender.submit(
                format(),
                2,
                stamp(),
                NativePacketFlags::default(),
                |samples| samples.fill(0.5),
            )
        });
        assert!(accepted);
        assert_eq!(counts, (0, 0));
        drop(receiver);
        let (accepted, counts) = measured(|| {
            sender.submit(format(), 2, stamp(), NativePacketFlags::default(), |_| {
                panic!("abandoned receiver must not copy")
            })
        });
        assert!(!accepted);
        assert_eq!(counts, (0, 0));
        // Only the control owner now drops sender and its retained buffers.
    }
    #[test]
    fn pending_and_cumulative_drop_counts_saturate_without_wrapping() {
        let (mut sender, mut receiver) = pool(limits(1)).unwrap();
        sender.pending_dropped = u64::MAX;
        sender.total_dropped = u64::MAX;
        sender.reject();
        assert_eq!(sender.dropped_packets(), u64::MAX);
        assert_eq!(receiver.dropped_packets(), u64::MAX);
        assert!(sender.submit(
            format(),
            1,
            stamp(),
            NativePacketFlags {
                queue_dropped: 9,
                ..Default::default()
            },
            |samples| samples.fill(0.0)
        ));
        let packet = receiver.pop().unwrap();
        assert_eq!(packet.flags.queue_dropped, u64::MAX);
        assert!(packet.flags.discontinuity);
    }
}

//! Bounded upper mux: native100ns precision refinement of OBS's strict peer
//! watermark and video-first ties. See doc/migration/streaming-watermark.md.
use super::*;
use crate::rtmp::{AudioPacket, EncodedPacket, VideoPacket};
//2s*120FPS plus bounded native reply/drain reservations per track.
// Metadata slots only: encoded-byte/future-credit caps remain unchanged.
//288 per track covers240video AUs/2s@120FPS plus native4-frame AVC
// reply, AAC accepted/residual/two-EOS temporal slack, and drain reservations.
pub(crate) const MAX_OUTBOX_PACKETS: usize = 576;
pub(crate) const MAX_OUTBOX_BYTES: usize = 16 * 1024 * 1024;
const TRACK_SLOTS: usize = MAX_OUTBOX_PACKETS/2;
// Preserve previous per-track encoded-byte budgets independently of metadata.
const TRACK_BYTE_SLOTS:usize=5;
const REPLY_PACKETS: usize = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Track {
    Video,
    Audio,
}
impl Track {
    fn max_packet(self) -> usize {
        match self {
            Self::Video => 2 * 1024 * 1024,
            Self::Audio => 64 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Encode,
    Finish,
}
pub(crate) struct Packet {
    pub track: Track,
    pub bytes: Arc<[u8]>,
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
}
impl Packet {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.bytes.is_empty() || self.bytes.len() > self.track.max_packet() {
            return Err(Error::Limit("encoded AU transport profile"));
        }
        if self.duration <= 0
            || self.pts != self.dts
            || self.pts.checked_add(self.duration).is_none()
        {
            return Err(Error::Invalid("encoded native timing"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InterleaveWait {
    #[default]
    None,
    FirstVideo,
    FirstAudio,
    VideoWatermark,
    AudioWatermark,
    Network,
}
/// Small copied runtime metadata. No AU bytes, owned strings or codec handles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InterleaveState {
    pub video_packets: usize,
    pub audio_packets: usize,
    pub video_bytes: usize,
    pub audio_bytes: usize,
    pub video_reserved_packets: usize,
    pub audio_reserved_packets: usize,
    pub video_reserved_bytes: usize,
    pub audio_reserved_bytes: usize,
    pub highest_video_100ns: Option<i128>,
    pub highest_audio_100ns: Option<i128>,
    pub video_drained: bool,
    pub audio_drained: bool,
    pub waiting: InterleaveWait,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BatchStats {
    pub packets: u64,
    pub bytes: u64,
    pub drained: bool,
}
#[derive(Clone, Copy)]
struct Reservation {
    operation: Operation,
    packets: usize,
}
struct Queue {
    slots: [Option<Packet>; TRACK_SLOTS],
    head: usize,
    len: usize,
    bytes: usize,
    highest: Option<i128>,
    last_native: Option<i64>,
    reservation: Option<Reservation>,
    produced: u64,
    drained: bool,
}
impl Queue {
    fn new() -> Self {
        Self {
            slots: [const { None }; TRACK_SLOTS],
            head: 0,
            len: 0,
            bytes: 0,
            highest: None,
            last_native: None,
            reservation: None,
            produced: 0,
            drained: false,
        }
    }
    fn front(&self) -> Option<&Packet> {
        self.slots[self.head].as_ref()
    }
    fn push(&mut self, packet: Packet) {
        let index = (self.head + self.len) % TRACK_SLOTS;
        debug_assert!(self.slots[index].is_none());
        self.slots[index] = Some(packet);
        self.len += 1;
    }
    fn pop(&mut self) -> Result<Packet, Error> {
        let value = self.slots[self.head]
            .take()
            .ok_or(Error::State("validated queue head absent"))?;
        self.head = (self.head + 1) % TRACK_SLOTS;
        self.len -= 1;
        self.bytes -= value.bytes.len();
        Ok(value)
    }
    fn reserved_packets(&self) -> usize {
        self.reservation.map_or(0, |r| r.packets)
    }
    fn reserved_bytes(&self, track: Track) -> usize {
        self.reserved_packets() * track.max_packet()
    }
}
pub(crate) struct Outbox {
    key: StreamKey,
    audio: bool,
    audio_offset: Option<i64>,
    first_video: Option<i128>,
    first_audio: Option<i128>,
    shift: Option<i128>,
    video: Queue,
    audio_queue: Queue,
    last_sent: Option<i128>,
    audio_pcm_frames: u64,
    audio_future: Option<usize>,
    video_inputs: u64,
    video_future: Option<usize>,
}
impl Outbox {
    pub(crate) fn new(key: StreamKey, audio: bool) -> Self {
        Self {
            key,
            audio,
            audio_offset: None,
            first_video: None,
            first_audio: None,
            shift: None,
            video: Queue::new(),
            audio_queue: Queue::new(),
            last_sent: None,
            audio_pcm_frames: 0,
            audio_future: None,
            video_inputs: 0,
            video_future: None,
        }
    }
    fn queue(&self, track: Track) -> Result<&Queue, Error> {
        match track {
            Track::Video => Ok(&self.video),
            Track::Audio if self.audio => Ok(&self.audio_queue),
            Track::Audio => Err(Error::Scope),
        }
    }
    fn queue_mut(&mut self, track: Track) -> Result<&mut Queue, Error> {
        match track {
            Track::Video => Ok(&mut self.video),
            Track::Audio if self.audio => Ok(&mut self.audio_queue),
            Track::Audio => Err(Error::Scope),
        }
    }
    pub(crate) fn audio_offset(&mut self, offset: i64) -> Result<(), Error> {
        if !self.audio
            || self.audio_offset.is_some_and(|old| old != offset)
            || self.first_audio.is_some()
        {
            return Err(Error::State("audio origin already fixed"));
        }
        self.audio_offset = Some(offset);
        Ok(())
    }
    fn adjusted(&self, track: Track, dts: i64) -> Result<i128, Error> {
        i128::from(dts)
            .checked_add(if track == Track::Audio {
                i128::from(
                    self.audio_offset
                        .ok_or(Error::State("AAC before audio anchor"))?,
                )
            } else {
                0
            })
            .ok_or(Error::Limit("adjusted DTS range"))
    }
    #[cfg(test)]
    pub(crate) fn can_reserve(&self, track: Track) -> bool {
        self.queue(track).is_ok_and(|q| {
            !q.drained
                && q.reservation.is_none()
                && q.len <= TRACK_SLOTS - REPLY_PACKETS
                && q.bytes <= TRACK_BYTE_SLOTS * track.max_packet() - REPLY_PACKETS * track.max_packet()
        })
    }
    /// General four-AU codec reservation; certified AAC uses cumulative credits.
    #[cfg(test)]
    pub(crate) fn reserve(&mut self, track: Track, operation: Operation) -> Result<bool, Error> {
        if (track == Track::Audio && self.audio_future.is_some())
            || (track == Track::Video && self.video_future.is_some())
        {
            return Err(Error::State(
                "AAC cumulative credit requires its typed reservation",
            ));
        }
        self.reserve_packets(track, operation, REPLY_PACKETS)
    }
    fn reserve_packets(
        &mut self,
        track: Track,
        operation: Operation,
        packets: usize,
    ) -> Result<bool, Error> {
        if packets > REPLY_PACKETS {
            return Err(Error::Limit("codec reply reservation"));
        }
        let q = self.queue(track)?;
        if q.drained {
            return Err(Error::State("codec track already drained"));
        }
        if q.reservation.is_some() {
            return Err(Error::State("codec operation already reserved"));
        }
        if q.len > TRACK_SLOTS - packets || q.bytes > (TRACK_BYTE_SLOTS - packets) * track.max_packet() {
            return Ok(false);
        }
        self.queue_mut(track)?.reservation = Some(Reservation { operation, packets });
        Ok(true)
    }
    // Native Baseline AVC removes exactly one accepted Stamp per AU and
    // rejects unmatched input at real drain. Frame INDEX gaps are not inputs.
    fn next_video_credit(&self) -> Result<(u64, usize), Error> {
        let next = self
            .video_inputs
            .checked_add(1)
            .ok_or(Error::Limit("AVC input credit overflow"))?;
        let future = next
            .checked_sub(self.video.produced)
            .ok_or(Error::State("AVC output exceeds accepted frames"))?;
        if future > REPLY_PACKETS as u64 {
            return Err(Error::Limit("AVC native accepted-frame backlog"));
        }
        Ok((
            next,
            usize::try_from(future).map_err(|_| Error::Limit("AVC future credit"))?,
        ))
    }
    /// A/V producer wall skew is bounded separately from native codec delay.
    /// Credits remain authoritative for bytes and native pending AU counts.
    pub(crate) fn video_time_admitted(&self,pts:i128,frame_duration:i128)->Result<bool,Error>{
        if pts<0||frame_duration<=0{return Err(Error::Invalid("video skew timestamp"));}
        if !self.audio{return Ok(true);}
        let floor=self.audio_queue.highest.or(self.first_video).unwrap_or(pts);
        let skew=i128::from(crate::audio_output::MAX_AUDIO_EMISSION_SKEW_NS/100);
        let native_tail=frame_duration.checked_mul(REPLY_PACKETS as i128).and_then(|n|n.checked_add(6*1024*10_000_000/48_000)).ok_or(Error::Limit("native codec time credit"))?;
        Ok(pts<=floor+skew+native_tail)
    }
    fn future_bytes_fit(&self,track:Track,future:usize)->Result<bool,Error>{
        let q=self.queue(track)?;
        Ok(future.checked_mul(track.max_packet()).and_then(|n|q.bytes.checked_add(n)).is_some_and(|n|n<=TRACK_BYTE_SLOTS*track.max_packet()))
    }
    pub(crate) fn can_reserve_video_encode(&self) -> Result<bool, Error> {
        let (_, future) = self.next_video_credit()?;
        Ok(self.future_bytes_fit(Track::Video,future)? && !self.video.drained
            && self.video.reservation.is_none()
            && self
                .video
                .len
                .checked_add(future)
                .is_some_and(|n| n <= TRACK_SLOTS))
    }
    pub(crate) fn reserve_video_encode(&mut self) -> Result<bool, Error> {
        let (next, future) = self.next_video_credit()?;
        if !self.future_bytes_fit(Track::Video,future)?{return Ok(false);}
        if self
            .video
            .len
            .checked_add(future)
            .is_none_or(|n| n > TRACK_SLOTS)
        {
            return Ok(false);
        }
        if !self.reserve_packets(Track::Video, Operation::Encode, future.min(REPLY_PACKETS))? {
            return Ok(false);
        }
        self.video_inputs = next;
        self.video_future = Some(future);
        Ok(true)
    }
    pub(crate) fn reserve_video_finish(&mut self) -> Result<bool, Error> {
        let future = usize::try_from(
            self.video_inputs
                .checked_sub(self.video.produced)
                .ok_or(Error::State("AVC output exceeds accepted frames"))?,
        )
        .map_err(|_| Error::Limit("AVC tail credit"))?;
        if !self.future_bytes_fit(Track::Video,future)?{return Err(Error::State("AVC future byte credit lost"));}
        if self
            .video
            .len
            .checked_add(future)
            .is_none_or(|n| n > TRACK_SLOTS)
        {
            return Err(Error::State("AVC future tail credit was not retained"));
        }
        if !self.reserve_packets(Track::Video, Operation::Finish, future.min(REPLY_PACKETS))? {
            return Ok(false);
        }
        self.video_future = Some(future);
        Ok(true)
    }
    /// Native AAC bounds total output by accepted blocks, plus two EOS AUs.
    /// Preserve room for ALL remaining final AUs before admitting more PCM.
    pub(crate) fn reserve_audio_encode(
        &mut self,
        first: u64,
        frames: usize,
    ) -> Result<bool, Error> {
        let q = self.queue(Track::Audio)?;
        if frames == 0 || frames > crate::native_aac::FRAME_LEN || first != self.audio_pcm_frames {
            return Err(Error::Invalid("AAC contiguous input credit"));
        }
        let next = first
            .checked_add(frames as u64)
            .ok_or(Error::Limit("AAC input credit overflow"))?;
        let (reply, future) = self.audio_credit(next)?;
        if q.len.checked_add(future).is_none_or(|n| n > TRACK_SLOTS) || !self.future_bytes_fit(Track::Audio,future)? {
            if q.len == 0 {
                // No queued packet can leave to create capacity. Waiting for
                // peer/network progress cannot fix this native-lookahead profile.
                return Err(Error::Limit(
                    "AAC lookahead exceeds bounded metadata tail-credit profile",
                ));
            }
            return Ok(false);
        }
        if !self.reserve_packets(Track::Audio, Operation::Encode, reply)? {
            return Ok(false);
        }
        self.audio_pcm_frames = next;
        self.audio_future = Some(future);
        Ok(true)
    }
    fn audio_credit(&self, frames: u64) -> Result<(usize, usize), Error> {
        let q = self.queue(Track::Audio)?;
        let blocks = frames / crate::native_aac::FRAME_LEN as u64;
        let ceiling = blocks
            .checked_add(u64::from(frames % crate::native_aac::FRAME_LEN as u64 != 0))
            .and_then(|n| n.checked_add(2))
            .ok_or(Error::Limit("AAC final credit overflow"))?;
        let future = ceiling
            .checked_sub(q.produced)
            .ok_or(Error::State("AAC output exceeds input credit"))?;
        let before_eos = blocks
            .checked_sub(q.produced)
            .ok_or(Error::State("AAC output exceeds accepted blocks"))?;
        Ok((
            usize::try_from(before_eos.min(REPLY_PACKETS as u64))
                .map_err(|_| Error::Limit("AAC reply credit"))?,
            usize::try_from(future).map_err(|_| Error::Limit("AAC future credit"))?,
        ))
    }
    pub(crate) fn reserve_audio_finish(&mut self) -> Result<bool, Error> {
        let (_, future) = self.audio_credit(self.audio_pcm_frames)?;
        let q = self.queue(Track::Audio)?;
        if q.len.checked_add(future).is_none_or(|n| n > TRACK_SLOTS) || !self.future_bytes_fit(Track::Audio,future)? {
            return Err(Error::State("AAC future tail credit was not retained"));
        }
        if !self.reserve_packets(Track::Audio, Operation::Finish, future.min(REPLY_PACKETS))? {
            return Ok(false);
        }
        self.audio_future = Some(future);
        Ok(true)
    }
    /// Whole reply admission is atomic. An invalid reply preserves the reservation
    /// and all previous packets/high-water marks; the owner reports its real failure.
    pub(crate) fn admit_batch(
        &mut self,
        track: Track,
        packets: Vec<Packet>,
        drained: bool,
    ) -> Result<BatchStats, Error> {
        let q = self.queue(track)?;
        if q.drained {
            return Err(Error::State("packet after codec EOS"));
        }
        let operation = q
            .reservation
            .ok_or(Error::State("unsolicited codec packet reply"))?;
        if drained != (operation.operation == Operation::Finish) {
            return Err(Error::State("codec EOS does not match reserved operation"));
        }
        if packets.len() > operation.packets {
            return Err(Error::Limit("codec packet batch"));
        }
        let mut last = q.last_native;
        let mut highest = q.highest;
        let mut added = 0usize;
        let mut first = None;
        for packet in &packets {
            if packet.track != track {
                return Err(Error::Scope);
            }
            packet.validate()?;
            if last.is_some_and(|previous| packet.dts <= previous) {
                return Err(Error::Invalid("unordered encoded DTS"));
            }
            let at = self.adjusted(track, packet.dts)?;
            if at > i128::from(u64::MAX) / 100 {
                return Err(Error::Limit("encoded ns range"));
            }
            if self.last_sent.is_some_and(|sent| at < sent) {
                return Err(Error::Invalid("encoded DTS precedes published watermark"));
            }
            first.get_or_insert(at);
            highest = Some(at);
            last = Some(packet.dts);
            added = added
                .checked_add(packet.bytes.len())
                .ok_or(Error::Limit("encoded outbox bytes"))?;
        }
        let count = q
            .len
            .checked_add(packets.len())
            .ok_or(Error::Limit("encoded outbox count"))?;
        let bytes = q
            .bytes
            .checked_add(added)
            .ok_or(Error::Limit("encoded outbox bytes"))?;
        let peer = if track == Track::Video {
            &self.audio_queue
        } else {
            &self.video
        };
        if count > TRACK_SLOTS
            || bytes > TRACK_BYTE_SLOTS * track.max_packet()
            || count + peer.len > MAX_OUTBOX_PACKETS
            || bytes
                .checked_add(peer.bytes)
                .is_none_or(|v| v > MAX_OUTBOX_BYTES)
        {
            return Err(Error::Limit("encoded track reservation"));
        }
        let first_video = self
            .first_video
            .or(if track == Track::Video { first } else { None });
        let first_audio = self
            .first_audio
            .or(if track == Track::Audio { first } else { None });
        let shift = self.shift.or_else(|| {
            if let Some(first_video) = first_video
                && (!self.audio || first_audio.is_some())
            {
                Some((-first_video.min(first_audio.unwrap_or(i128::MAX))).max(0))
            } else {
                None
            }
        });
        if let Some(shift) = shift {
            for packet in self
                .video
                .slots
                .iter()
                .chain(self.audio_queue.slots.iter())
                .filter_map(Option::as_ref)
                .chain(packets.iter())
            {
                let at = self.adjusted(packet.track, packet.dts)?;
                u64::try_from(
                    (at + shift)
                        .checked_mul(100)
                        .ok_or(Error::Limit("encoded ns overflow"))?,
                )
                .map_err(|_| Error::Limit("encoded ns range"))?;
            }
        }
        let produced = q
            .produced
            .checked_add(packets.len() as u64)
            .ok_or(Error::Limit("codec produced packet counter"))?;
        let future = if track == Track::Audio {
            self.audio_future
                .map(|credit| {
                    credit
                        .checked_sub(packets.len())
                        .ok_or(Error::State("AAC reply exceeds cumulative credit"))
                })
                .transpose()?
        } else {
            None
        };
        if future.is_some_and(|n| !drained && count.checked_add(n).is_none_or(|n| n > TRACK_SLOTS))
        {
            return Err(Error::State("AAC future tail credit exceeded"));
        }
        let video_future = if track == Track::Video && self.video_future.is_some() {
            Some(
                usize::try_from(
                    self.video_inputs
                        .checked_sub(produced)
                        .ok_or(Error::State("AVC reply exceeds accepted input"))?,
                )
                .map_err(|_| Error::Limit("AVC tail credit"))?,
            )
        } else {
            None
        };
        if video_future.is_some_and(|n| {
            (drained && n != 0) || count.checked_add(n).is_none_or(|n| n > TRACK_SLOTS)
        }) {
            return Err(Error::State("AVC EOS or future input coverage"));
        }
        if track == Track::Audio && self.audio_future.is_some() && drained {
            let covered = self.audio_pcm_frames / crate::native_aac::FRAME_LEN as u64
                + u64::from(self.audio_pcm_frames % crate::native_aac::FRAME_LEN as u64 != 0);
            if produced < covered {
                return Err(Error::State("AAC EOS before admitted PCM coverage"));
            }
        }
        let stats = BatchStats {
            packets: packets.len() as u64,
            bytes: added as u64,
            drained,
        };
        let q = self.queue_mut(track)?;
        q.bytes = bytes;
        q.highest = highest;
        q.last_native = last;
        q.reservation = None;
        q.drained = drained;
        q.produced = produced;
        for packet in packets {
            q.push(packet);
        }
        if track == Track::Audio && self.audio_future.is_some() {
            self.audio_future = if drained { Some(0) } else { future };
        }
        if track == Track::Video && self.video_future.is_some() {
            self.video_future = video_future;
        }
        if let Some(first) = first {
            match track {
                Track::Video => {
                    self.first_video.get_or_insert(first);
                }
                Track::Audio => {
                    self.first_audio.get_or_insert(first);
                }
            }
        }
        if self.shift.is_none()
            && self.first_video.is_some()
            && (!self.audio || self.first_audio.is_some())
        {
            let minimum = self
                .first_video
                .unwrap()
                .min(self.first_audio.unwrap_or(i128::MAX));
            self.shift = Some((-minimum).max(0));
        }
        Ok(stats)
    }
    pub(crate) fn gated(&self) -> bool {
        self.shift.is_none()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.video.len == 0 && self.audio_queue.len == 0
    }
    fn selected(&self) -> Result<Option<(Track, i128)>, Error> {
        let video = self
            .video
            .front()
            .map(|p| self.adjusted(Track::Video, p.dts))
            .transpose()?;
        let audio = self
            .audio_queue
            .front()
            .map(|p| self.adjusted(Track::Audio, p.dts))
            .transpose()?;
        Ok(match (video, audio) {
            (Some(v), Some(a)) => Some(if v <= a {
                (Track::Video, v)
            } else {
                (Track::Audio, a)
            }),
            (Some(v), None) => Some((Track::Video, v)),
            (None, Some(a)) => Some((Track::Audio, a)),
            _ => None,
        })
    }
    fn eligible(&self, track: Track, at: i128) -> bool {
        if track == Track::Video {
            !self.audio
                || self.audio_queue.drained
                || self.audio_queue.highest.is_some_and(|h| h > at)
        } else {
            self.video.drained || self.video.highest.is_some_and(|h| h > at)
        }
    }
    pub(crate) fn state(&self) -> InterleaveState {
        let waiting = if self.first_video.is_none() {
            InterleaveWait::FirstVideo
        } else if self.audio && self.first_audio.is_none() {
            InterleaveWait::FirstAudio
        } else {
            match self.selected() {
                Ok(Some((track, at))) if !self.eligible(track, at) => {
                    if track == Track::Video {
                        InterleaveWait::AudioWatermark
                    } else {
                        InterleaveWait::VideoWatermark
                    }
                }
                _ => InterleaveWait::None,
            }
        };
        InterleaveState {
            video_packets: self.video.len,
            audio_packets: self.audio_queue.len,
            video_bytes: self.video.bytes,
            audio_bytes: self.audio_queue.bytes,
            video_reserved_packets: self
                .video_future
                .unwrap_or_else(|| self.video.reserved_packets()),
            audio_reserved_packets: self
                .audio_future
                .unwrap_or_else(|| self.audio_queue.reserved_packets()),
            video_reserved_bytes: self.video_future.map_or_else(
                || self.video.reserved_bytes(Track::Video),
                |n| n * Track::Video.max_packet(),
            ),
            audio_reserved_bytes: self.audio_future.map_or_else(
                || self.audio_queue.reserved_bytes(Track::Audio),
                |n| n * Track::Audio.max_packet(),
            ),
            highest_video_100ns: self.video.highest,
            highest_audio_100ns: self.audio_queue.highest,
            video_drained: self.video.drained,
            audio_drained: !self.audio || self.audio_queue.drained,
            waiting,
        }
    }
    /// Exact common origin/shift. No per-track zeroing or native field rewriting.
    pub(crate) fn front(&self) -> Result<Option<EncodedPacket>, Error> {
        let Some(shift) = self.shift else {
            return Ok(None);
        };
        let Some((track, at)) = self.selected()? else {
            return Ok(None);
        };
        if !self.eligible(track, at) {
            return Ok(None);
        }
        let packet = self
            .queue(track)?
            .front()
            .ok_or(Error::State("selected queue empty"))?;
        let offset = if track == Track::Audio {
            i128::from(self.audio_offset.ok_or(Error::State("audio offset"))?)
        } else {
            0
        };
        let ns = |ticks: i64| -> Result<u64, Error> {
            u64::try_from(
                (i128::from(ticks) + offset + shift)
                    .checked_mul(100)
                    .ok_or(Error::Limit("encoded ns overflow"))?,
            )
            .map_err(|_| Error::Limit("encoded ns range"))
        };
        let dts = ns(packet.dts)?;
        let generation = self.key.generation(track == Track::Audio);
        match track {
            Track::Video => VideoPacket::new(
                generation,
                dts,
                i128::from(ns(packet.pts)?),
                packet.bytes.clone(),
            )
            .map(EncodedPacket::Video),
            Track::Audio => {
                AudioPacket::new(generation, dts, packet.bytes.clone()).map(EncodedPacket::Audio)
            }
        }
        .map(Some)
        .map_err(|e| Error::Transport(e.to_string()))
    }
    pub(crate) fn pop(&mut self) -> Result<(), Error> {
        let (track, at) = self.selected()?.ok_or(Error::State("empty outbox"))?;
        if self.shift.is_none() || !self.eligible(track, at) {
            return Err(Error::State("outbox head has no peer watermark"));
        }
        if self.last_sent.is_some_and(|old| at < old) {
            return Err(Error::Invalid("global encoded DTS rewind"));
        }
        self.queue_mut(track)?.pop()?;
        self.last_sent = Some(at);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    include!("watermark_timing_tests.rs");
}

// Fixed inline packet slots are charged separately from the owner's bounded
// control metadata; increasing the temporal slot window does not raise payload
// byte ceilings or introduce a growable queue allocation.
const _: () = assert!(
    std::mem::size_of::<Outbox>()
        <= 4096 + MAX_OUTBOX_PACKETS * std::mem::size_of::<Option<Packet>>()
);

#[cfg(test)]
mod audio_credit_tests {
    include!("watermark_audio_credit_tests.rs");
}

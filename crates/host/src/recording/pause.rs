//! Bounded-history common recording clock; original Program PCM may arrive2s late. All updates run on the recording
//! owner after a FIFO admission barrier shared by video and Program PCM.
use super::RecordingError;
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingControlKey {pub recording:u64,pub session:u64,pub serial:u64}
#[derive(Clone,Debug,serde::Serialize)]
pub struct RecordingPauseReceipt {
    pub key:RecordingControlKey,pub paused:bool,pub requested_ns:u64,
    pub effective_ns:u64,pub removed_ns:u64,
}
struct PauseSpan {start:u64,end:u64,removed_before:u64}
pub(super) struct AudioSpan {pub first:u64,pub frames:usize,pub output_first:u64}
const MAX_PENDING_PAUSES:usize=128;
pub(super) struct PauseClock {
    spans:std::collections::VecDeque<PauseSpan>,retired_removed:u64,
    origin:u64,pub removed_ns:u64,pub removed_audio_frames:u64,
    pub resume_floor_ns:u64,pub paused_at:Option<u64>,
    pub video_end_ns:u64,pub audio_end_ns:u64,last_control_ns:u64,
}
impl PauseClock {
    pub fn new(origin:u64)->Self {Self {spans:std::collections::VecDeque::with_capacity(MAX_PENDING_PAUSES),retired_removed:0,origin,removed_ns:0,removed_audio_frames:0,resume_floor_ns:origin,paused_at:None,video_end_ns:origin,audio_end_ns:origin,last_control_ns:origin}}
    pub fn audio_grid(ns:u64,rate:u32)->Result<u64,RecordingError> {
        u64::try_from((u128::from(ns)*u128::from(rate)).div_ceil(1_000_000_000)).map_err(|_|RecordingError::invalid("recording pause sample grid overflow"))
    }
    pub fn audio_ns(frame:u64,rate:u32)->Result<u64,RecordingError> {
        u64::try_from((u128::from(frame)*1_000_000_000).div_ceil(u128::from(rate))).map_err(|_|RecordingError::invalid("recording pause sample time overflow"))
    }
    /// Return only originally unpaused samples; wall-time pause state never
    /// discards a valid delayed pre-pause prefix. History retires on the audio
    /// cursor, not on GUI Resume. At most129 small ranges, no retained PCM.
    pub fn audio_spans(&mut self,first:u64,end:u64,rate:u32)->Result<Vec<AudioSpan>,RecordingError>{
        let origin=Self::audio_grid(self.origin,rate)?;let mut cursor=first.max(origin);let mut removed=self.retired_removed;
        let mut output=Vec::with_capacity(self.spans.len()+1);
        let mut append=|a:u64,b:u64,removed:u64|->Result<(),RecordingError>{
            if b>a {let offset=u64::try_from(u128::from(removed)*u128::from(rate)/1_000_000_000).map_err(|_|RecordingError::invalid("pause audio offset overflow"))?;
                let mapped=a.checked_sub(origin).and_then(|v|v.checked_sub(offset)).ok_or_else(||RecordingError::invalid("pause audio mapping underflow"))?;
                output.push(AudioSpan {first:a,frames:usize::try_from(b-a).map_err(|_|RecordingError::invalid("pause audio span overflow"))?,output_first:mapped});}Ok(())};
        for span in &self.spans {
            let start=Self::audio_grid(span.start,rate)?;let stop=Self::audio_grid(span.end,rate)?;
            if cursor<start{append(cursor,end.min(start),removed)?;}
            if end<=start{cursor=end;break;}
            cursor=cursor.max(stop);removed=span.removed_before+span.end-span.start;
            if cursor>=end{break;}
        }
        let end=self.paused_at.map(|at|Self::audio_grid(at,rate)).transpose()?.map_or(end,|at|end.min(at));
        if cursor<end{append(cursor,end,removed)?;}
        while self.spans.front().is_some_and(|span|Self::audio_grid(span.end,rate).is_ok_and(|stop|stop<=end)){
            let span=self.spans.pop_front().unwrap();self.retired_removed=span.removed_before+span.end-span.start;
        }
        Ok(output)
    }
    pub fn change(&mut self,key:RecordingControlKey,paused:bool,requested:u64,rate:Option<u32>)->Result<RecordingPauseReceipt,RecordingError> {
        if requested<self.origin {return Err(RecordingError::invalid("recording pause time precedes the recording origin"));}
        let effective=requested.max(self.video_end_ns).max(self.audio_end_ns).max(self.last_control_ns);
        if effective.saturating_sub(self.origin)>24*60*60*1_000_000_000 {return Err(RecordingError::invalid("recording pause timeline exceeds24hours"));}
        if paused {
            if self.paused_at.is_some() {return Err(RecordingError::invalid("recording is already paused"));}
            if self.spans.len()>=MAX_PENDING_PAUSES{return Err(RecordingError::invalid("recording pause history is full until delayed Program audio catches up"));}
            self.paused_at=Some(effective);
        } else {
            let start=self.paused_at.ok_or_else(||RecordingError::invalid("recording is not paused"))?;
            let removed=self.removed_ns.checked_add(effective-start).ok_or_else(||RecordingError::invalid("recording paused duration overflow"))?;
            let audio_removed=if let Some(rate)=rate {
                // Quantize the cumulative common-clock offset once. Rounding
                // every pause separately would accumulate A/V sample drift.
                u64::try_from(u128::from(removed)*u128::from(rate)/1_000_000_000).map_err(|_|RecordingError::invalid("recording paused sample count overflow"))?
            } else {self.removed_audio_frames};
            self.spans.push_back(PauseSpan {start,end:effective,removed_before:self.removed_ns});
            self.removed_ns=removed;self.removed_audio_frames=audio_removed;self.resume_floor_ns=effective;self.paused_at=None;
        }
        self.last_control_ns=effective;
        Ok(RecordingPauseReceipt {key,paused,requested_ns:requested,effective_ns:effective,removed_ns:self.removed_ns})
    }
}

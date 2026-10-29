//! Typed composition boundary. Native facades keep actual COM/socket ownership.
use super::{
    timing::{Packet, Track},
    video::{NativeRenderer, Renderer},
    *,
};
use crate::{native_aac as aac, native_h264 as h264, rtmp};
pub(crate) enum CodecEvent {
    Ready(Arc<[u8]>),
    Packets {
        header: Arc<[u8]>,
        packets: Vec<Packet>,
        drained: bool,
        aac: Option<aac::StreamStats>,
    },
    Closed,
}
pub(crate) trait VideoPort {
    fn busy(&self) -> bool;
    fn closed(&self) -> bool;
    fn encode(&mut self, index: u64, bytes: Box<[u8]>) -> Result<(), Error>;
    fn finish(&mut self) -> Result<(), Error>;
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error>;
    fn close(&mut self);
}
pub(crate) trait AudioPort {
    fn busy(&self) -> bool;
    fn closed(&self) -> bool;
    fn encode(&mut self, first: u64, samples: Box<[i16]>) -> Result<(), Error>;
    fn finish(&mut self) -> Result<(), Error>;
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error>;
    fn close(&mut self);
}
pub(crate) enum PublishEvent {
    Publishing,
    Progress { packets: u64 },
    Stopped { packets: u64 },
    Cancelled,
}
pub(crate) trait PublisherPort {
    fn poll(&mut self) -> Result<Option<PublishEvent>, Error>;
    /// false means bounded network backpressure, not packet admission.
    fn packet(&mut self, packet: &rtmp::EncodedPacket) -> Result<bool, Error>;
    fn stop(&mut self);
    fn cancel(&mut self);
    fn finished(&self) -> bool;
    /// Called only when finished, on service owner; must establish actual return.
    fn join(self: Box<Self>) -> Result<(), Error>;
}
pub(crate) trait Factory: Send {
    fn video(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn VideoPort>, Error>;
    fn audio(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn AudioPort>, Error>;
    fn publisher(
        &mut self,
        config: &Config,
        key: StreamKey,
        video: Arc<[u8]>,
        audio: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn PublisherPort>, Error>;
    fn renderer(&mut self) -> Box<dyn Renderer>;
}
/// Production factory keeps the root capability across output operations.
pub(crate) struct RenderFactory {pub render_gpu:crate::render_gpu::RenderGpu}
impl Factory for RenderFactory {
    fn video(&mut self,config:&Config,key:StreamKey)->Result<Box<dyn VideoPort>,Error> {
        NativeFactory.video(config,key)
    }
    fn audio(&mut self,config:&Config,key:StreamKey)->Result<Box<dyn AudioPort>,Error> {
        NativeFactory.audio(config,key)
    }
    fn publisher(&mut self,config:&Config,key:StreamKey,video:Arc<[u8]>,audio:Option<Arc<[u8]>>)->Result<Box<dyn PublisherPort>,Error> {
        NativeFactory.publisher(config,key,video,audio)
    }
    fn renderer(&mut self)->Box<dyn Renderer> {
        Box::new(NativeRenderer::new(self.render_gpu.clone()))
    }
}
pub(crate) struct NativeFactory;
impl Factory for NativeFactory {
    fn video(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn VideoPort>, Error> {
        let config = config.video_config(key)?;
        let worker =
            h264::EncoderWorker::native(config.clone()).map_err(|e| Error::H264(e.to_string()))?;
        Ok(Box::new(NativeVideo { config, worker }))
    }
    fn audio(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn AudioPort>, Error> {
        let config = config
            .audio_config(key)?
            .ok_or(Error::State("audio disabled"))?;
        let worker =
            aac::EncoderWorker::native(config.clone()).map_err(|e| Error::Aac(e.to_string()))?;
        Ok(Box::new(NativeAudio { config, worker }))
    }
    fn publisher(
        &mut self,
        config: &Config,
        key: StreamKey,
        video: Arc<[u8]>,
        audio: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn PublisherPort>, Error> {
        let options = rtmp::ConnectOptions {
            connection: key.output,
            target: config.resolve()?,
            video_generation: key.generation(false),
            video: rtmp::VideoConfig::new(video).map_err(|e| transport_error(e))?,
            audio: audio
                .map(|a| rtmp::AudioConfig::new(a).map(|a| (key.generation(true), a)))
                .transpose()
                .map_err(|e| transport_error(e))?,
            timestamp_origin_ns: 0,
            limits: rtmp::Limits::default(),
            timeouts: rtmp::Timeouts::default(),
        };
        if config.stop.as_ref().is_some_and(|stop|stop.load(std::sync::atomic::Ordering::Acquire)){return Err(Error::Cancelled);}
        rtmp::Publisher::start(options)
            .map(|worker| {
                Box::new(NativePublisher {
                    worker: Some(worker),
                    failure: None,
                }) as Box<dyn PublisherPort>
            })
            .map_err(|e| transport_error(e))
    }
    fn renderer(&mut self) -> Box<dyn Renderer> {
        Box::new(NativeRenderer::default())
    }
}
struct NativeVideo {
    config: h264::Config,
    worker: h264::EncoderWorker,
}
impl VideoPort for NativeVideo {
    fn busy(&self) -> bool {
        self.worker.is_busy()
    }
    fn closed(&self) -> bool {
        self.worker.is_closed()
    }
    fn encode(&mut self, index: u64, bytes: Box<[u8]>) -> Result<(), Error> {
        let (pts, duration) = self
            .config
            .frame_timing(index)
            .map_err(|e| Error::H264(e.to_string()))?;
        let input = h264::Nv12Input::new(&self.config, pts, duration, bytes)
            .map_err(|e| Error::H264(e.to_string()))?;
        self.worker
            .submit(input)
            .map_err(|e| Error::H264(e.error.to_string()))
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.worker.finish().map_err(|e| Error::H264(e.to_string()))
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        let Some(event) = self.worker.poll() else {
            return Ok(None);
        };
        Ok(Some(match event {
            h264::Event::Ready {
                generation,
                configuration,
            } => {
                if generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                CodecEvent::Ready(Arc::from(
                    configuration
                        .ok_or(Error::State("deferred AVC header unsupported"))?
                        .avcc(),
                ))
            }
            h264::Event::Packets(batch) => {
                if batch.generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                let header = Arc::from(
                    batch
                        .configuration
                        .ok_or(Error::State("missing AVC header"))?
                        .avcc(),
                );
                let mut packets = Vec::with_capacity(batch.packets.len());
                for p in batch.packets {
                    if p.generation() != self.config.generation() {
                        return Err(Error::Scope);
                    }
                    packets.push(Packet {
                        track: Track::Video,
                        bytes: Arc::from(p.bytes()),
                        pts: p.pts(),
                        dts: p.dts(),
                        duration: p.duration(),
                    });
                }
                CodecEvent::Packets {
                    header,
                    packets,
                    drained: batch.drained,
                    aac: None,
                }
            }
            h264::Event::Failed { error, .. } => return Err(Error::H264(error.to_string())),
            h264::Event::Closed { generation } => {
                if generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                CodecEvent::Closed
            }
        }))
    }
    fn close(&mut self) {
        self.worker.request_close();
    }
}
struct NativeAudio {
    config: aac::Config,
    worker: aac::EncoderWorker,
}
impl AudioPort for NativeAudio {
    fn busy(&self) -> bool {
        self.worker.is_busy()
    }
    fn closed(&self) -> bool {
        self.worker.is_closed()
    }
    fn encode(&mut self, first: u64, samples: Box<[i16]>) -> Result<(), Error> {
        let input = aac::PcmInput::new(&self.config, first, samples)
            .map_err(|e| Error::Aac(e.to_string()))?;
        self.worker
            .submit(input)
            .map_err(|e| Error::Aac(e.error.to_string()))
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.worker.finish().map_err(|e| Error::Aac(e.to_string()))
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        let Some(event) = self.worker.poll() else {
            return Ok(None);
        };
        Ok(Some(match event {
            aac::Event::Ready {
                generation,
                configuration,
            } => {
                if generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                CodecEvent::Ready(Arc::from(configuration.asc()))
            }
            aac::Event::Packets(batch) => {
                if batch.generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                let header = Arc::from(batch.configuration.asc());
                let mut packets = Vec::with_capacity(batch.packets.len());
                for p in batch.packets {
                    if p.generation() != self.config.generation() {
                        return Err(Error::Scope);
                    }
                    packets.push(Packet {
                        track: Track::Audio,
                        bytes: Arc::from(p.bytes()),
                        pts: p.pts(),
                        dts: p.dts(),
                        duration: p.duration(),
                    });
                }
                CodecEvent::Packets {
                    header,
                    packets,
                    drained: batch.drained,
                    aac: Some(batch.stats),
                }
            }
            aac::Event::Failed { error, .. } => return Err(Error::Aac(error.to_string())),
            aac::Event::Closed { generation } => {
                if generation != self.config.generation() {
                    return Err(Error::Scope);
                }
                CodecEvent::Closed
            }
        }))
    }
    fn close(&mut self) {
        self.worker.request_close();
    }
}
struct NativePublisher {
    worker: Option<rtmp::Publisher>,
    failure: Option<Error>,
}
impl PublisherPort for NativePublisher {
    fn poll(&mut self) -> Result<Option<PublishEvent>, Error> {
        let w = self
            .worker
            .as_mut()
            .ok_or(Error::State("publisher retired"))?;
        let Some(event) = w.poll() else {
            return Ok(None);
        };
        let key = w.key();
        Ok(Some(match event {
            rtmp::Event::Publishing { key: k } if key == k => PublishEvent::Publishing,
            rtmp::Event::Progress { key: k, stats } if key == k => PublishEvent::Progress {
                packets: stats.packets,
            },
            rtmp::Event::Stopped { key: k, stats } if key == k => PublishEvent::Stopped {
                packets: stats.packets,
            },
            rtmp::Event::Failed {
                key: k,
                error: rtmp::Error::Cancelled,
            } if key == k => PublishEvent::Cancelled,
            rtmp::Event::Failed { key: k, error } if key == k => {
                let error = transport_error(error);
                self.failure = Some(error.clone());
                return Err(error);
            }
            _ => return Err(Error::Scope),
        }))
    }
    fn packet(&mut self, packet: &rtmp::EncodedPacket) -> Result<bool, Error> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        match self
            .worker
            .as_mut()
            .ok_or(Error::State("publisher retired"))?
            .try_packet(packet)
        {
            Ok(()) => Ok(true),
            Err(rtmp::Error::Backpressure) => Ok(false),
            // Native RTMP holds its input receiver through terminal publication.
            // An admission Closed race therefore has an observable true cause.
            Err(rtmp::Error::Closed) => terminal_cause_after_closed(|| self.poll()),
            Err(e) => Err(transport_error(e)),
        }
    }
    fn stop(&mut self) {
        if let Some(w) = &mut self.worker {
            w.stop();
        }
    }
    fn cancel(&mut self) {
        if let Some(w) = &mut self.worker {
            w.cancel();
        }
    }
    fn finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(rtmp::Publisher::is_finished)
    }
    fn join(mut self: Box<Self>) -> Result<(), Error> {
        if let Some(w) = self.worker.take() {
            w.join().map_err(|e| transport_error(e))?;
        }
        Ok(())
    }
}

/// At most8 bounded RTMP events can precede/contain the reserved terminal.
/// Returning the actual failure preserves SDK/server errors across send races.
fn terminal_cause_after_closed(
    mut poll: impl FnMut() -> Result<Option<PublishEvent>, Error>,
) -> Result<bool, Error> {
    for _ in 0..8 {
        match poll()? {
            Some(PublishEvent::Progress { .. } | PublishEvent::Publishing) => {}
            Some(PublishEvent::Cancelled) => return Err(Error::Cancelled),
            Some(PublishEvent::Stopped { .. }) => {
                return Err(Error::State("publisher already stopped"));
            }
            None => break,
        }
    }
    Err(Error::State(
        "publisher retired without its reserved terminal cause",
    ))
}
#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn closed_admission_observes_original_terminal_failure_after_pending_progress() {
        let expected = Error::Transport("rtmp_invalid: original exact server/SDK failure".into());
        let mut values = std::collections::VecDeque::from([
            Ok(Some(PublishEvent::Progress { packets: 3 })),
            Err(expected.clone()),
        ]);
        assert_eq!(
            terminal_cause_after_closed(|| values.pop_front().unwrap_or(Ok(None))),
            Err(expected)
        );
    }
}

fn transport_error(error:crate::rtmp::Error)->Error{if matches!(error,crate::rtmp::Error::Cancelled){return Error::Cancelled;}if error.retryable_transport(){Error::RetryableTransport(error.to_string())}else{Error::Transport(error.to_string())}}

//! Modeless runtime statistics. Counters never modify scene or output state.
use eframe::egui;
use skylive_host::{process_statistics::{Sample, Worker}, recording::RecordingStatus, streaming::Status};
use std::time::{Duration, Instant};

pub struct Snapshot {
    pub session: u64,
    pub canvas: (u32, u32, u32, u32),
    pub preview: crate::preview::Metrics,
    pub program: crate::preview::Metrics,
    pub recording: RecordingStatus,
    pub streaming: Option<Status>,
    pub replay: skylive_host::replay_buffer::ReplayStatus,
    pub filters: skylive_host::render_gpu::SharedFilterStats,
    pub virtual_camera_frames: u64,
    pub virtual_camera_active: bool,
    pub virtual_camera_run: u64,
}

#[derive(Default)]
pub struct State {
    open: bool,
    session: Option<u64>,
    worker: Option<Worker>,
    attempted: bool,
    next_sample: Option<Instant>,
    sample: Option<Sample>,
    cpu_percent: Option<f64>,
    problem: Option<String>,
    reset: Option<Snapshot>,
    stream_sample: Option<(skylive_host::streaming::StreamKey, Instant, u64)>,
    bitrate_kbps: Option<f64>,
}
impl State {
    pub fn open(&mut self) {
        if !self.open {self.sample=None;self.cpu_percent=None;self.next_sample=None;}
        self.open = true;
    }
    pub fn is_open(&self) -> bool { self.open }
    fn poll_process(&mut self, ctx: &egui::Context) {
        if self.worker.is_none() && !self.attempted {
            self.attempted = true;
            match Worker::new() {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => self.problem = Some(error.to_string()),
            }
        }
        if let Some(worker) = &mut self.worker {
            if let Some(result) = worker.poll() {
                match result {
                    Ok(sample) if sample.sampled_at.elapsed() > Duration::from_secs(3) => {
                        self.sample = None;
                        self.cpu_percent = None;
                    }
                    Ok(sample) => {
                        self.cpu_percent = self.sample.and_then(|previous| {
                            let seconds = sample.sampled_at.checked_duration_since(previous.sampled_at)?.as_secs_f64();
                            let ticks = sample.cpu_time_100ns.checked_sub(previous.cpu_time_100ns)?;
                            (seconds > 0.0).then(|| (ticks as f64 / 10_000_000.0 / seconds / sample.logical_processors.max(1) as f64 * 100.0).clamp(0.0,100.0))
                        });
                        self.sample = Some(sample);
                        self.problem = None;
                    }
                    Err(error) => { self.sample = None; self.cpu_percent = None; self.problem = Some(error); }
                }
            }
            if !worker.is_busy() && self.next_sample.is_none_or(|at| Instant::now() >= at) {
                if let Err(error) = worker.request() { self.problem = Some(error); }
                self.next_sample = Some(Instant::now() + Duration::from_secs(1));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    pub fn show(&mut self, ctx: &egui::Context, current: Snapshot) {
        if !self.open { return; }
        self.poll_process(ctx);
        if self.session != Some(current.session) {
            self.session = Some(current.session);
            self.reset = None;
            self.stream_sample = None;
            self.bitrate_kbps = None;
        }
        if let Some(status) = &current.streaming {
            let now = Instant::now();
            match self.stream_sample {
                Some((key, at, bytes)) if key == status.key => {
                    let elapsed = now.duration_since(at).as_secs_f64();
                    if elapsed >= 1.0 {
                        self.bitrate_kbps = status.encoded_bytes.checked_sub(bytes).map(|delta| delta as f64 * 8.0 / elapsed / 1000.0);
                        self.stream_sample = Some((key, now, status.encoded_bytes));
                    }
                }
                _ => {self.stream_sample = Some((status.key, now, status.encoded_bytes)); self.bitrate_kbps = None;}
            }
        } else { self.stream_sample = None; self.bitrate_kbps = None; }
        let mut open = self.open;
        let mut reset = false;
        egui::Window::new(crate::i18n::tr_context(ctx,"Stats"))
            .id(egui::Id::new("runtime-statistics-window"))
            .open(&mut open).collapsible(false).default_width(640.0)
            .default_pos(ctx.screen_rect().center()-egui::vec2(320.0,220.0))
            .show(ctx, |ui| {
                let actions=[crate::views::modal_footer::Action::new("Reset",true,"Reset the displayed counter baselines. Capture and outputs keep running."),
                    crate::views::modal_footer::Action::new("Close",true,"Close the statistics window.")];
                let height=(ctx.screen_rect().height()-crate::views::modal_footer::required_height(ui,&actions)-150.0).max(96.0);
                egui::ScrollArea::vertical().id_salt("runtime-statistics-body").max_height(height).show(ui,|ui| {
                    ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
                    egui::Grid::new("runtime-statistics-grid").striped(true).min_col_width(220.0).show(ui,|ui| {
                        row(ui,"CPU usage",self.cpu_percent.map_or_else(||"—".into(),|value|format!("{value:.1}%")),"Process CPU time normalized across the available logical processors.");
                        row(ui,"Memory usage",self.sample.map_or_else(||"—".into(),|s|format!("{:.1} MiB",s.working_set_bytes as f64/1048576.0)),"Current process working set, sampled on the statistics worker.");
                        let (w,h,n,d)=current.canvas;
                        row(ui,"Canvas / target FPS",format!("{w} × {h} · {n}/{d}"),"Configured canvas and exact target frame rate; this is not a measured rendering rate.");
                        let old=self.reset.as_ref();
                        ui.strong(crate::i18n::tr(ui,"Source filters"));ui.end_row();
                        row(ui,"Filter computations",delta(current.filters.computations,old.map(|s|s.filters.computations)),"Source-filter computations performed by the shared service.");
                        row(ui,"Shared filter hits",delta(current.filters.hits,old.map(|s|s.filters.hits)),"Completed filter results reused for the exact same input frame and settings.");
                        row(ui,"Shared filter joins",delta(current.filters.joined,old.map(|s|s.filters.joined)),"Render callers waiting for the same in-progress filter computation instead of starting another one.");
                        row(ui,"Filter service memory",format!("{:.1} MiB",current.filters.charged_bytes as f64/1048576.0),"Memory credits held by active filter work, retained results and their render leases.");
                        for (label,metrics,baseline) in [("Preview",current.preview,old.map(|s|s.preview)),("Program",current.program,old.map(|s|s.program))] {
                            ui.strong(crate::i18n::tr(ui,label)); ui.end_row();
                            row(ui,"Submitted preview jobs",metrics.submitted.saturating_sub(baseline.map_or(0,|m|m.submitted)).to_string(),"Preview jobs actually admitted by the worker. Obsolete completions are not published.");
                            row(ui,"Published preview images",metrics.published.saturating_sub(baseline.map_or(0,|m|m.published)).to_string(),"Images actually published to this pane. Static scenes do not need repeated image publication.");
                            row(ui,"Preview failures",metrics.failures.saturating_sub(baseline.map_or(0,|m|m.failures)).to_string(),"Actual reported preview worker failures since the displayed baseline.");
                            row(ui,"Last preview latency",metrics.last_latency_ms.map_or_else(||"—".into(),|v|format!("{v:.1} ms")),"Time from preview admission until its completed image was received by the UI; includes queue and polling time.");
                        }
                        ui.strong(crate::i18n::tr(ui,"Recording"));ui.end_row();
                        let recording_base=old.filter(|s|s.recording.path==current.recording.path).map(|s|&s.recording);
                        row(ui,"Video frames",delta(current.recording.video_frames,recording_base.map(|s|s.video_frames)),"Frames accepted by the recording encoder.");
                        row(ui,"Skipped video frames",delta(current.recording.skipped_video_frames,recording_base.map(|s|s.skipped_video_frames)),"Recording frames skipped by its real pacing and admission policy.");
                        row(ui,"Audio gap frames",delta(current.recording.audio_gap_frames,recording_base.map(|s|s.audio_gap_frames)),"Missing recording audio frames filled by the output policy.");
                        if let Some(status)=&current.streaming {
                            ui.strong(crate::i18n::tr(ui,"Streaming"));ui.end_row();
                            let base=old.and_then(|s|s.streaming.as_ref()).filter(|s|s.key==status.key);
                            row(ui,"Video frames",delta(status.video_frames,base.map(|s|s.video_frames)),"Frames accepted by the streaming encoder.");
                            row(ui,"Skipped video frames",delta(status.skipped_video_frames,base.map(|s|s.skipped_video_frames)),"Streaming frames skipped by its real pacing and admission policy.");
                            row(ui,"Encoded bytes",delta(status.encoded_bytes,base.map(|s|s.encoded_bytes)),"Actual encoded stream payload bytes; excludes transport overhead.");
                            row(ui,"Encoded bitrate",self.bitrate_kbps.map_or_else(||"—".into(),|v|format!("{v:.0} kb/s")),"Encoded payload rate over the last statistics interval; this is not network throughput.");
                            row(ui,"Published packets",delta(status.packets_published,base.map(|s|s.packets_published)),"Encoded packets successfully published by the streaming owner.");
                        }
                        ui.strong(crate::i18n::tr(ui,"Replay Buffer"));ui.end_row();
                        let state=if current.replay.stopping {"Stopping Replay Buffer…"}else if current.replay.starting {"Starting Replay Buffer…"}
                            else if current.replay.ready {"Ready"}else if current.replay.active {"Buffering…"}else{"Stopped"};
                        row(ui,"State",crate::i18n::tr(ui,state),"Current Replay Buffer state reported by its owner.");
                        row(ui,"Retained duration",format!("{:.2} s",current.replay.duration_100ns as f64/10_000_000.0),"Encoded time span retained by Replay Buffer. Memory pressure can shorten it.");
                        row(ui,"Encoded bytes",format!("{:.1} MiB",current.replay.stored_bytes as f64/1048576.0),"Encoded payloads currently retained in the replay ring.");
                        row(ui,"Replay memory",format!("{:.1} MiB",current.replay.charged_bytes as f64/1048576.0),"Accounted replay memory, including payloads pinned by an in-progress save.");
                        ui.strong(crate::i18n::tr(ui,"Virtual Camera"));ui.end_row();
                        row(ui,"State",crate::i18n::tr(ui,if current.virtual_camera_active {"Active"}else{"Stopped"}),"Current virtual-camera producer state.");
                        row(ui,"Video frames",delta(current.virtual_camera_frames,old.filter(|s|s.virtual_camera_run==current.virtual_camera_run).map(|s|s.virtual_camera_frames)),"Program frames written to the virtual-camera producer. A receiving application is reported separately in camera settings.");
                    });
                    if let Some(problem)=&self.problem {ui.colored_label(ui.visuals().error_fg_color,problem);}
                });
                ui.separator();
                match crate::views::modal_footer::show(ui,&actions) {Some(0)=>reset=true,Some(1)=>self.open=false,_=>{}}
            });
        self.open &= open;
        if reset { self.reset=Some(current); }
    }
}
fn delta(value:u64,base:Option<u64>)->String {value.saturating_sub(base.unwrap_or(0)).to_string()}
fn row(ui:&mut egui::Ui,label:&str,value:String,help:&str) {
    ui.label(crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,help));
    ui.monospace(value).on_hover_text(crate::i18n::tr(ui,help));
    ui.end_row();
}

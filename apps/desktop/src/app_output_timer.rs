//! Frontend-owned output timer. Deadlines use a monotonic clock and only
//! request the existing recording/streaming owners to stop their own output.
//! Closing the window does not cancel an armed timer.
use super::*;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Timer {
    hours:u32,
    minutes:u32,
    seconds:u32,
    auto:bool,
    pending:bool,
    running:bool,
    seen_ready:bool,
    remaining:Duration,
    last:Option<Instant>,
}
impl Timer {
    fn configured(&self)->Duration {
        Duration::from_secs((u64::from(self.hours)*3600+u64::from(self.minutes)*60+u64::from(self.seconds)).max(1))
    }
    fn arm(&mut self,now:Instant) {
        self.remaining=self.configured();
        self.running=true;
        self.pending=false;
        self.last=Some(now);
    }
    fn clear(&mut self) {
        self.pending=false;
        self.running=false;
        self.remaining=Duration::ZERO;
        self.last=None;
    }
    fn advance(&mut self,active:bool,ready:bool,paused:bool,visible:bool,now:Instant)->bool {
        if !active {
            self.clear();
            self.seen_ready=false;
            return false;
        }
        if ready&&!self.seen_ready {
            self.seen_ready=true;
            if self.pending||self.auto||visible {self.arm(now);}
        } else if ready&&self.pending&&!self.running {
            self.arm(now);
        }
        if !self.running {return false;}
        let elapsed=self.last.replace(now).map_or(Duration::ZERO,|last|now.saturating_duration_since(last));
        if !paused {self.remaining=self.remaining.saturating_sub(elapsed);}
        if self.remaining.is_zero() {
            self.clear();
            return true;
        }
        false
    }
}
#[derive(Default)]
pub(super) struct Runtime {
    pub open:bool,
    loaded:bool,
    stream:Timer,
    record:Timer,
    pause_record_timer:bool,
}
fn timer_row(ui:&mut egui::Ui,timer:&mut Timer,key:&str,fallback:&str,active:bool)->bool {
    ui.group(|ui|{
        ui.label(crate::i18n::tr_key_or(ui,key,fallback));
        ui.horizontal(|ui|{
            ui.add_enabled(!timer.running,egui::DragValue::new(&mut timer.hours).range(0..=99).speed(0.1).suffix(" h"));
            ui.add_enabled(!timer.running,egui::DragValue::new(&mut timer.minutes).range(0..=59).speed(0.1).suffix(" m"));
            ui.add_enabled(!timer.running,egui::DragValue::new(&mut timer.seconds).range(0..=59).speed(0.1).suffix(" s"));
        });
        let seconds=timer.remaining.as_secs().saturating_add(u64::from(timer.remaining.subsec_nanos()!=0));
        ui.label(format!("{:02}:{:02}:{:02}",seconds/3600,seconds/60%60,seconds%60));
        let caption=if active&&timer.running {"Stop"} else {"Start"};
        ui.button(crate::i18n::tr(ui,caption)).clicked()
    }).inner
}
impl StudioApp {
    pub(super) fn output_timer_frame(&mut self,ctx:&egui::Context) {
        if !self.output_timer.loaded {
            let settings=self.session.snapshot().preferences().output_timer();
            self.output_timer.stream.hours=settings.stream_seconds/3600;
            self.output_timer.stream.minutes=settings.stream_seconds/60%60;
            self.output_timer.stream.seconds=settings.stream_seconds%60;
            self.output_timer.record.hours=settings.record_seconds/3600;
            self.output_timer.record.minutes=settings.record_seconds/60%60;
            self.output_timer.record.seconds=settings.record_seconds%60;
            self.output_timer.stream.auto=settings.auto_stream;
            self.output_timer.record.auto=settings.auto_record;
            self.output_timer.pause_record_timer=settings.pause_record;
            self.output_timer.loaded=true;
        }
        let now=Instant::now();
        let stream=&self.streaming.view;
        let stream_active=stream.active&&!stream.finishing;
        let stream_ready=stream_active&&matches!(stream.phase,Some(skylive_host::streaming::Phase::Publishing));
        let record=&self.recording.view;
        let record_active=record.active&&!record.finishing;
        let record_ready=record_active&&!record.starting;
        let stop_stream=self.output_timer.stream.advance(stream_active,stream_ready,false,self.output_timer.open,now);
        let stop_record=self.output_timer.record.advance(record_active,record_ready,record.paused&&self.output_timer.pause_record_timer,self.output_timer.open,now);
        if stop_stream {self.stop_streaming();}
        if stop_record {self.stop_recording();}
        if self.output_timer.stream.running||self.output_timer.stream.pending||self.output_timer.record.running||self.output_timer.record.pending {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.output_timer.open {return;}
        let mut open=true;
        let mut stream_clicked=false;
        let mut record_clicked=false;
        egui::Window::new(crate::i18n::tr_context(ctx,"Output Timer"))
            .id(egui::Id::new("output-timer"))
            .open(&mut open)
            .resizable(false)
            .show(ctx,|ui|{
                stream_clicked=timer_row(ui,&mut self.output_timer.stream,"Module.frontend-tools.OutputTimer.Stream","Stop streaming after:",stream_active);
                ui.checkbox(&mut self.output_timer.stream.auto,crate::i18n::tr_key_or(ui,"Module.frontend-tools.OutputTimer.Stream.EnableEverytime","Enable streaming timer every time"));
                ui.add_space(8.0);
                record_clicked=timer_row(ui,&mut self.output_timer.record,"Module.frontend-tools.OutputTimer.Record","Stop recording after:",record_active);
                ui.checkbox(&mut self.output_timer.record.auto,crate::i18n::tr_key_or(ui,"Module.frontend-tools.OutputTimer.Record.EnableEverytime","Enable recording timer every time"));
                ui.checkbox(&mut self.output_timer.pause_record_timer,crate::i18n::tr_key_or(ui,"Module.frontend-tools.OutputTimer.Record.PauseTimer","Pause timer when recording is paused"));
                ui.separator();
                if ui.button(crate::i18n::tr(ui,"Close")).clicked(){self.output_timer.open=false;}
            });
        self.output_timer.open &= open;
        let settings=skylive_core::OutputTimerPreferences {
            stream_seconds:self.output_timer.stream.hours*3600+self.output_timer.stream.minutes*60+self.output_timer.stream.seconds,
            record_seconds:self.output_timer.record.hours*3600+self.output_timer.record.minutes*60+self.output_timer.record.seconds,
            auto_stream:self.output_timer.stream.auto,
            auto_record:self.output_timer.record.auto,
            pause_record:self.output_timer.pause_record_timer,
        };
        let preferences=self.session.snapshot().preferences().clone();
        if settings!=preferences.output_timer() {
            match preferences.with_output_timer(settings) {
                Ok(preferences)=>{self.publish_command_at(None,Command::SetUserPreferences {preferences},false);},
                Err(error)=>self.message(true,error.to_string()),
            }
        }
        if stream_clicked {
            if stream_active&&self.output_timer.stream.running {self.output_timer.stream.clear();self.stop_streaming();}
            else if stream_ready {self.output_timer.stream.arm(now);}
            else if !stream_active {self.start_streaming();self.output_timer.stream.pending=true;}
            else {self.output_timer.stream.pending=true;}
        }
        if record_clicked {
            if record_active&&self.output_timer.record.running {self.output_timer.record.clear();self.stop_recording();}
            else if record_ready {self.output_timer.record.arm(now);}
            else if !record_active {self.start_recording();self.output_timer.record.pending=true;}
            else {self.output_timer.record.pending=true;}
        }
    }
}

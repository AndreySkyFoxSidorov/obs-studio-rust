//! Process counters sampled on an owned worker, never during a GUI frame.
use serde::Serialize;
use std::{sync::mpsc, thread, time::Instant};

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Sample {
    #[serde(skip)]
    pub sampled_at: Instant,
    pub cpu_time_100ns: u64,
    pub working_set_bytes: usize,
    pub logical_processors: usize,
}

pub fn sample() -> Result<Sample, String> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::{Foundation::FILETIME, System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        }};
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let process = unsafe { GetCurrentProcess() };
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            .map_err(|error| error.to_string())?;
        let ticks = |value: FILETIME| (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
        let cpu_time_100ns = ticks(kernel).checked_add(ticks(user)).ok_or("Process CPU counter overflow")?;
        let mut memory = PROCESS_MEMORY_COUNTERS::default();
        memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let length = memory.cb;
        unsafe { GetProcessMemoryInfo(process, &mut memory, length) }.map_err(|error| error.to_string())?;
        Ok(Sample {sampled_at: Instant::now(), cpu_time_100ns, working_set_bytes: memory.WorkingSetSize,
            logical_processors: thread::available_parallelism().map_err(|error| error.to_string())?.get()})
    }
    #[cfg(not(target_os = "windows"))]
    { Err("Process statistics are currently available on Windows.".into()) }
}

pub struct Worker {
    sender: Option<mpsc::SyncSender<()>>,
    results: mpsc::Receiver<Result<Sample, String>>,
    busy: bool,
}
impl Worker {
    pub fn new() -> std::io::Result<Self> {
        let (sender, input) = mpsc::sync_channel(1);
        let (output, results) = mpsc::sync_channel(1);
        thread::Builder::new().name("skylive-process-statistics".into()).spawn(move || {
            while input.recv().is_ok() {
                if output.send(sample()).is_err() { break; }
            }
        })?;
        Ok(Self { sender: Some(sender), results, busy: false })
    }
    pub fn request(&mut self) -> Result<(), String> {
        if self.busy { return Err("A process-statistics sample is already pending".into()); }
        self.sender.as_ref().ok_or("Process-statistics worker is closed")?
            .try_send(()).map_err(|error| error.to_string())?;
        self.busy = true;
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<Sample, String>> {
        match self.results.try_recv() {
            Ok(value) => {self.busy = false; Some(value)}
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) if self.busy => {
                self.busy = false;
                Some(Err("Process-statistics worker disconnected".into()))
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
    pub fn is_busy(&self) -> bool { self.busy }
}
impl Drop for Worker { fn drop(&mut self) { self.sender.take(); } }

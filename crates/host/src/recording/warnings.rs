//! Bounded immutable warnings for the latest successfully encoded frame.
use super::owner::RecordingFrame;
use crate::preview::SourceDiagnostic;
use crate::transition::{CompositionKey, OutputKey};
use serde::Serialize;
use std::{mem::size_of, sync::Arc};

pub const MAX_RECORDING_WARNINGS: usize = 32;
pub const MAX_RECORDING_WARNING_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct RecordingSourceName {
    pub source: skylive_core::Id,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordingWarnings {
    pub session: u64,
    pub revision: u64,
    pub time_ns: u64,
    pub resource_generation: u64,
    pub capture_epoch: u64,
    pub capture_generation: u64,
    pub output: OutputKey,
    pub composition: CompositionKey,
    pub diagnostics: Vec<SourceDiagnostic>,
    pub source_names: Vec<RecordingSourceName>,
    pub omitted: u64,
}

impl RecordingWarnings {
    pub(crate) fn from_frame(
        frame: &RecordingFrame,
        diagnostics: Vec<SourceDiagnostic>,
    ) -> Option<Arc<Self>> {
        if diagnostics.is_empty() {
            return None;
        }
        let mut retained = Vec::with_capacity(diagnostics.len().min(MAX_RECORDING_WARNINGS));
        let mut source_names = Vec::<RecordingSourceName>::new();
        let scenes = frame.output.scenes();
        let mut bytes = size_of::<Self>() + 2 * size_of::<usize>();
        let mut omitted = 0u64;
        for mut issue in diagnostics {
            let source_name = if source_names
                .iter()
                .any(|entry| entry.source == issue.source)
            {
                None
            } else {
                frame
                    .model
                    .source(issue.source)
                    .or_else(|| {
                        scenes
                            .iter()
                            .find_map(|scene| scene.source(&frame.model, issue.source))
                    })
                    .map(|source| source.name())
            };
            let charge = size_of::<SourceDiagnostic>()
                .saturating_add(issue.detail.len())
                .saturating_add(issue.code.len())
                .saturating_add(
                    source_name.map_or(0, |name| size_of::<RecordingSourceName>() + name.len()),
                );
            if retained.len() == MAX_RECORDING_WARNINGS
                || charge > MAX_RECORDING_WARNING_BYTES.saturating_sub(bytes)
            {
                omitted = omitted.saturating_add(1);
                continue;
            }
            // Retained charge follows bytes, not caller-controlled String capacity.
            issue.detail = issue.detail.into_boxed_str().into_string();
            bytes += charge;
            if let Some(name) = source_name {
                source_names.push(RecordingSourceName {
                    source: issue.source,
                    name: name.to_owned().into_boxed_str().into_string(),
                });
            }
            retained.push(issue);
        }
        Some(Arc::new(Self {
            session: frame.session,
            revision: frame.model.revision(),
            time_ns: frame.time.0,
            resource_generation: frame.resource_generation,
            capture_epoch: frame.capture.epoch(),
            capture_generation: frame.capture.generation(),
            output: frame.output.identity(),
            composition: frame.output.sample_key(),
            diagnostics: retained.into_boxed_slice().into_vec(),
            source_names: source_names.into_boxed_slice().into_vec(),
            omitted,
        }))
    }

    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.diagnostics.capacity() * size_of::<SourceDiagnostic>()
            + self
                .diagnostics
                .iter()
                .map(|issue| issue.detail.capacity() + issue.code.len())
                .sum::<usize>()
            + self.source_names.capacity() * size_of::<RecordingSourceName>()
            + self
                .source_names
                .iter()
                .map(|entry| entry.name.capacity())
                .sum::<usize>()
    }

    pub fn source_name(&self, source: skylive_core::Id) -> Option<&str> {
        self.source_names
            .iter()
            .find(|entry| entry.source == source)
            .map(|entry| entry.name.as_str())
    }
}

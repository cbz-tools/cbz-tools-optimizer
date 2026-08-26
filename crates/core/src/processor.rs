use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;

use crate::archive::{archive_kind, archive_metadata, ArchiveEntry, ArchiveKind, ArchiveReader};
use crate::resize::{is_image, resize_image_bytes};
use crate::{OptimizeConfig, OverwriteMode, ProgressEvent};

/// Outcome of processing a single archive.
enum ArchiveOutcome {
    Done { input_bytes: u64, output_bytes: u64 },
    Skipped,
    Failed,
}

struct ArchivePlan {
    output_path: PathBuf,
    skip: bool,
    error: Option<String>,
}

struct OutputResolution {
    output_path: PathBuf,
    skip: bool,
}

const OPTIMIZER_IDENTIFIER: &str = "cbz-opt";
static WORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// Cross-process processing of the same final output is unsupported; same-batch
// output conflicts are rejected before processing.
struct WorkOwnership {
    work_path: PathBuf,
}

impl WorkOwnership {
    fn create(output_path: &Path) -> Result<(Self, File)> {
        let parent = output_path.parent().unwrap_or(Path::new("."));
        let output_name = output_path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| std::borrow::Cow::Borrowed("output"));
        let process_id = std::process::id();
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let work_path = parent.join(format!(
            ".{OPTIMIZER_IDENTIFIER}-{output_name}-{process_id}-{sequence}.work"
        ));
        let work_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&work_path)
            .with_context(|| {
                format!(
                    "Failed to create optimizer work file: {}",
                    work_path.display()
                )
            })?;
        // Never reuse or scan for stale work files; create_new leaves an
        // interrupted file available for inspection instead.
        Ok((Self { work_path }, work_file))
    }
}

/// Input-stage permits are shared by every archive in one process_archives
/// call. A permit covers one entry from the reader through transform and is
/// released once the transformed result has entered the completed stage.
struct InputLimiter {
    state: Mutex<usize>,
    available: Condvar,
    limit: usize,
}

impl InputLimiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(0),
            available: Condvar::new(),
            limit,
        }
    }

    fn acquire(self: &Arc<Self>, cancelled: &AtomicBool) -> Option<InputPermit> {
        let mut in_flight = self.state.lock().expect("input limiter poisoned");
        while *in_flight >= self.limit && !cancelled.load(Ordering::Acquire) {
            in_flight = self
                .available
                .wait(in_flight)
                .expect("input limiter poisoned");
        }
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        *in_flight += 1;
        Some(InputPermit {
            limiter: Arc::clone(self),
        })
    }

    fn notify_all(&self) {
        self.available.notify_all();
    }

    fn release(&self) {
        let mut in_flight = self.state.lock().expect("input limiter poisoned");
        *in_flight -= 1;
        self.available.notify_one();
    }
}

struct InputPermit {
    limiter: Arc<InputLimiter>,
}

impl Drop for InputPermit {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

/// Completed-stage permits are shared by every archive in one
/// process_archives call. A permit is reserved by the reader before it reads
/// an entry and remains held until that entry's input-order write completes.
/// Reserving in reader order ensures an early entry can never wait behind later
/// completed entries for the final completed-stage slot.
struct CompletedLimiter {
    state: Mutex<usize>,
    available: Condvar,
    limit: usize,
}

impl CompletedLimiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(0),
            available: Condvar::new(),
            limit,
        }
    }

    fn acquire(self: &Arc<Self>, cancelled: &AtomicBool) -> Option<CompletedPermit> {
        let mut in_flight = self.state.lock().expect("completed limiter poisoned");
        while *in_flight >= self.limit && !cancelled.load(Ordering::Acquire) {
            in_flight = self
                .available
                .wait(in_flight)
                .expect("completed limiter poisoned");
        }
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        *in_flight += 1;
        Some(CompletedPermit {
            limiter: Arc::clone(self),
        })
    }

    fn notify_all(&self) {
        self.available.notify_all();
    }

    fn release(&self) {
        let mut in_flight = self.state.lock().expect("completed limiter poisoned");
        *in_flight -= 1;
        self.available.notify_one();
    }
}

struct CompletedPermit {
    limiter: Arc<CompletedLimiter>,
}

impl Drop for CompletedPermit {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

struct PipelineLimits {
    input: Arc<InputLimiter>,
    completed: Arc<CompletedLimiter>,
}

struct WorkItem {
    index: usize,
    entry: ArchiveEntry,
    _input_permit: InputPermit,
    _completed_permit: CompletedPermit,
}

struct ProcessedEntry {
    index: usize,
    entry: ArchiveEntry,
    _completed_permit: CompletedPermit,
}

enum PipelineMessage {
    Entry(ProcessedEntry),
    End(Result<()>),
}

/// Entry point for parallel processing of multiple ZIP/CBZ/RAR/CBR files.
///
/// `on_progress` must be `Send + Sync` as it is called across threads.
/// Returns (succeeded, skipped, failed).
pub fn process_archives<F>(
    archive_paths: &[PathBuf],
    config: &OptimizeConfig,
    on_progress: F,
) -> (usize, usize, usize)
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    // Thread pool (0 = auto = half of logical CPUs, minimum 1).
    let effective_threads = if config.threads == 0 {
        (num_cpus() / 2).max(1)
    } else {
        config.threads
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(effective_threads)
        .build()
        .expect("rayon pool");

    let plans = resolve_archive_plans(archive_paths, config);
    let on_progress = Arc::new(on_progress);
    let config = Arc::new(config.clone());
    // These two global stages bound memory and work across all archives in
    // this process_archives call. The reader reserves one completed-stage
    // permit in original order before reading each entry, so every entry in
    // the input/read+transform stage is already included in the completed
    // window. There are therefore at most effective_threads * 2 distinct
    // entry buffers globally, while effective_threads limits entries being
    // read or transformed at once. Completed buffers cannot grow with the
    // archive and the ordered-output window is deliberately twice the active
    // input capacity.
    let pipeline_limits = Arc::new(PipelineLimits {
        input: Arc::new(InputLimiter::new(effective_threads.max(1))),
        completed: Arc::new(CompletedLimiter::new(
            effective_threads.saturating_mul(2).max(1),
        )),
    });

    let outcomes: Vec<ArchiveOutcome> = std::thread::scope(|scope| {
        let next_index = Arc::new(AtomicUsize::new(0));
        let worker_count = effective_threads.min(archive_paths.len());
        let handles: Vec<_> = (0..worker_count)
            .map(|_| {
                let cb = Arc::clone(&on_progress);
                let cfg = Arc::clone(&config);
                let pipeline_limits = Arc::clone(&pipeline_limits);
                let pool = &pool;
                let next_index = Arc::clone(&next_index);
                let plans = &plans;

                scope.spawn(move || {
                    let mut outcomes = Vec::new();
                    loop {
                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        let Some((path, plan)) = archive_paths.get(index).zip(plans.get(index))
                        else {
                            break;
                        };

                        let outcome =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                // Catch panics to prevent them from propagating.
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        process_one_archive(
                                            path,
                                            plan,
                                            &cfg,
                                            Arc::clone(&cb),
                                            &pipeline_limits,
                                            pool,
                                        )
                                    }));

                                match result {
                                    Ok(Ok(Some((out, input_bytes)))) => {
                                        let output_bytes =
                                            out.metadata().map(|m| m.len()).unwrap_or(0);
                                        cb(ProgressEvent::ZipDone {
                                            path: path.display().to_string(),
                                            output_path: out.display().to_string(),
                                            input_bytes,
                                            output_bytes,
                                        });
                                        ArchiveOutcome::Done {
                                            input_bytes,
                                            output_bytes,
                                        }
                                    }
                                    Ok(Ok(None)) => ArchiveOutcome::Skipped,
                                    Ok(Err(e)) => {
                                        cb(ProgressEvent::ZipError {
                                            path: path.display().to_string(),
                                            message: e.to_string(),
                                        });
                                        ArchiveOutcome::Failed
                                    }
                                    Err(_panic) => {
                                        cb(ProgressEvent::ZipError {
                                            path: path.display().to_string(),
                                            message: "Unexpected error occurred".to_string(),
                                        });
                                        ArchiveOutcome::Failed
                                    }
                                }
                            })) {
                                Ok(outcome) => outcome,
                                Err(_panic) => ArchiveOutcome::Failed,
                            };
                        outcomes.push((index, outcome));
                    }
                    outcomes
                })
            })
            .collect();

        let mut outcomes: Vec<_> = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect();
        outcomes.sort_by_key(|(index, _)| *index);
        outcomes.into_iter().map(|(_, outcome)| outcome).collect()
    });

    let succeeded = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Done { .. }))
        .count();
    let skipped = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Skipped))
        .count();
    let failed = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Failed))
        .count();
    let total_input_bytes: u64 = outcomes
        .iter()
        .map(|o| match o {
            ArchiveOutcome::Done { input_bytes, .. } => *input_bytes,
            _ => 0,
        })
        .sum();
    let total_output_bytes: u64 = outcomes
        .iter()
        .map(|o| match o {
            ArchiveOutcome::Done { output_bytes, .. } => *output_bytes,
            _ => 0,
        })
        .sum();

    on_progress(ProgressEvent::AllDone {
        total_zips: outcomes.len(),
        succeeded,
        skipped,
        failed,
        total_input_bytes,
        total_output_bytes,
    });

    (succeeded, skipped, failed)
}

/// Backward-compatible ZIP-named entry point for existing library callers.
pub fn process_zips<F>(
    archive_paths: &[PathBuf],
    config: &OptimizeConfig,
    on_progress: F,
) -> (usize, usize, usize)
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    process_archives(archive_paths, config, on_progress)
}

fn process_one_archive<F>(
    archive_path: &Path,
    plan: &ArchivePlan,
    config: &OptimizeConfig,
    on_progress: Arc<F>,
    pipeline_limits: &PipelineLimits,
    pool: &rayon::ThreadPool,
) -> Result<Option<(PathBuf, u64)>>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    if let Some(error) = &plan.error {
        anyhow::bail!("{error}");
    }

    let input_bytes = fs::metadata(archive_path)
        .with_context(|| format!("Failed to stat: {}", archive_path.display()))?
        .len();
    let metadata = archive_metadata(archive_path)?;
    on_progress(ProgressEvent::ZipStarted {
        path: archive_path.display().to_string(),
        image_count: metadata.image_count,
    });

    if plan.skip {
        on_progress(ProgressEvent::ZipSkipped {
            path: archive_path.display().to_string(),
            reason: "Output file already exists (skip mode)".to_string(),
        });
        return Ok(None);
    }

    let (ownership, work_file) = WorkOwnership::create(&plan.output_path)?;
    let result = stream_archive_to_work_file(
        archive_path,
        work_file,
        config,
        Arc::clone(&on_progress),
        metadata.entry_count,
        pipeline_limits,
        pool,
    )
    .and_then(|()| {
        finalize_work_file(
            &ownership.work_path,
            &plan.output_path,
            &config.overwrite_mode,
        )
    });

    match result {
        Ok(()) => Ok(Some((plan.output_path.clone(), input_bytes))),
        Err(error) => {
            // Keep the work file for post-crash/error inspection.
            drop(ownership);
            Err(error)
        }
    }
}

fn stream_archive_to_work_file<F>(
    archive_path: &Path,
    work_file: File,
    config: &OptimizeConfig,
    on_progress: Arc<F>,
    total_entries: usize,
    pipeline_limits: &PipelineLimits,
    pool: &rayon::ThreadPool,
) -> Result<()>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    let writer = zip::ZipWriter::new(work_file);
    let input_limiter = &pipeline_limits.input;
    let completed_limiter = &pipeline_limits.completed;
    let input_capacity = input_limiter.limit;
    let completed_capacity = completed_limiter.limit;
    let (work_sender, work_receiver) = std::sync::mpsc::sync_channel::<WorkItem>(input_capacity);
    let (result_sender, result_receiver) =
        std::sync::mpsc::sync_channel::<PipelineMessage>(completed_capacity);
    let cancelled = Arc::new(AtomicBool::new(false));
    let archive_path_string = archive_path.display().to_string();

    std::thread::scope(|thread_scope| {
        let writer_cancelled = Arc::clone(&cancelled);
        let writer_completed_limiter = Arc::clone(completed_limiter);
        let writer_input_limiter = Arc::clone(input_limiter);
        let writer_handle = thread_scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_ordered_entries(result_receiver, writer, &writer_cancelled)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("archive writer thread panicked")));
            if result.is_err() {
                writer_cancelled.store(true, Ordering::Release);
                writer_input_limiter.notify_all();
                writer_completed_limiter.notify_all();
            }
            result
        });

        let shared_work_receiver = Arc::new(Mutex::new(work_receiver));
        // A fixed worker set removes the per-entry OS-thread and scheduling
        // overhead. Workers acquire the completed-stage window before using
        // the Rayon pool, so both bounded stages remain globally accounted for.
        let mut worker_handles = Vec::with_capacity(input_capacity);
        for _ in 0..input_capacity {
            let work_receiver = Arc::clone(&shared_work_receiver);
            let result_sender = result_sender.clone();
            let worker_cancelled = Arc::clone(&cancelled);
            let worker_input_limiter = Arc::clone(input_limiter);
            let worker_completed_limiter = Arc::clone(completed_limiter);
            let config = config.clone();
            let archive_path = archive_path_string.clone();
            let on_progress = Arc::clone(&on_progress);
            worker_handles.push(thread_scope.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        let work = {
                            let receiver = work_receiver
                                .lock()
                                .expect("archive work receiver poisoned");
                            receiver.recv()
                        };
                        let Ok(work) = work else {
                            break;
                        };
                        if worker_cancelled.load(Ordering::Acquire) {
                            drop(work);
                            continue;
                        }

                        let WorkItem {
                            index,
                            entry,
                            _input_permit: input_permit,
                            _completed_permit: completed_permit,
                        } = work;
                        let processed = pool.install(|| {
                            process_entry(
                                entry,
                                index,
                                total_entries,
                                &config,
                                &archive_path,
                                &*on_progress,
                            )
                        });
                        let message = PipelineMessage::Entry(ProcessedEntry {
                            index,
                            entry: processed,
                            _completed_permit: completed_permit,
                        });
                        if send_cancellable(&result_sender, message, &worker_cancelled).is_err() {
                            drop(input_permit);
                            worker_cancelled.store(true, Ordering::Release);
                            worker_input_limiter.notify_all();
                            worker_completed_limiter.notify_all();
                            break;
                        }
                        // The result now owns the completed-stage permit; only
                        // the input/read+transform permit is released here.
                        drop(input_permit);
                    }
                }));
                if result.is_err() {
                    worker_cancelled.store(true, Ordering::Release);
                    worker_input_limiter.notify_all();
                    worker_completed_limiter.notify_all();
                }
            }));
        }

        let producer_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let mut reader = ArchiveReader::open(archive_path)?;
                let mut index = 0;
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let Some(input_permit) = input_limiter.acquire(&cancelled) else {
                        break;
                    };
                    let Some(completed_permit) = completed_limiter.acquire(&cancelled) else {
                        drop(input_permit);
                        break;
                    };
                    let Some(entry) = reader.next_entry()? else {
                        drop(completed_permit);
                        drop(input_permit);
                        break;
                    };
                    let entry_index = index;
                    index += 1;
                    let work = WorkItem {
                        index: entry_index,
                        entry,
                        _input_permit: input_permit,
                        _completed_permit: completed_permit,
                    };
                    if send_cancellable(&work_sender, work, &cancelled).is_err() {
                        break;
                    }
                }
                Ok(())
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("archive reader panicked")));
        let producer_failed = producer_result.is_err();
        drop(work_sender);
        let _ = send_cancellable(
            &result_sender,
            PipelineMessage::End(producer_result),
            &cancelled,
        );
        if producer_failed {
            cancelled.store(true, Ordering::Release);
            input_limiter.notify_all();
            completed_limiter.notify_all();
        }
        drop(result_sender);
        for handle in worker_handles {
            let _ = handle.join();
        }
        writer_handle
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("archive writer thread panicked")))
    })
}

fn send_cancellable<T>(
    sender: &std::sync::mpsc::SyncSender<T>,
    mut message: T,
    cancelled: &AtomicBool,
) -> std::result::Result<(), T> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(message);
        }
        match sender.try_send(message) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                message = returned;
                std::thread::yield_now();
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(returned)) => return Err(returned),
        }
    }
}

fn process_entry<F>(
    entry: ArchiveEntry,
    index: usize,
    total_entries: usize,
    config: &OptimizeConfig,
    archive_path: &str,
    on_progress: &F,
) -> ArchiveEntry
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    if entry.is_directory {
        return entry;
    }

    let ArchiveEntry {
        name,
        data,
        last_modified,
        ..
    } = entry;
    let (out_data, out_name) = if is_image(&name) {
        match resize_image_bytes(&data, &name, config) {
            Ok((resized, ext)) => {
                let output_name = if ext == ".gif" && name.to_lowercase().ends_with(".gif") {
                    name.clone()
                } else {
                    replace_extension(&name, ext)
                };
                (resized, output_name)
            }
            Err(e) => {
                log::warn!("Resize failed for {name}: {e}");
                (data, name.clone())
            }
        }
    } else {
        (data, name.clone())
    };

    on_progress(ProgressEvent::ImageDone {
        zip_path: archive_path.to_string(),
        image_index: index + 1,
        total: total_entries,
    });

    ArchiveEntry {
        name: out_name,
        data: out_data,
        is_directory: false,
        last_modified,
    }
}

fn write_ordered_entries(
    receiver: std::sync::mpsc::Receiver<PipelineMessage>,
    mut writer: zip::ZipWriter<File>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let mut pending = BTreeMap::new();
    let mut next_index = 0;
    let mut producer_result = None;

    while let Ok(message) = receiver.recv() {
        match message {
            PipelineMessage::Entry(entry) => {
                pending.insert(entry.index, entry);
                while let Some(entry) = pending.remove(&next_index) {
                    write_entry(&mut writer, entry.entry)?;
                    drop(entry._completed_permit);
                    next_index += 1;
                }
            }
            PipelineMessage::End(result) => producer_result = Some(result),
        }
    }

    producer_result.unwrap_or_else(|| Err(anyhow::anyhow!("archive reader ended unexpectedly")))?;
    if !pending.is_empty() {
        anyhow::bail!("archive entries were not completed in order");
    }
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("archive processing was cancelled");
    }

    // The final output is not touched until this finish succeeds.
    let finished_file = writer.finish()?;
    finished_file.sync_all()?;
    drop(finished_file);
    Ok(())
}

fn write_entry(writer: &mut zip::ZipWriter<File>, entry: ArchiveEntry) -> Result<()> {
    if entry.is_directory {
        writer.add_directory(&entry.name, archive_directory_options(entry.last_modified))?;
    } else {
        writer.start_file(&entry.name, archive_file_options(entry.last_modified))?;
        writer.write_all(&entry.data)?;
    }
    Ok(())
}

fn resolve_archive_plans(archive_paths: &[PathBuf], config: &OptimizeConfig) -> Vec<ArchivePlan> {
    let mut plans: Vec<ArchivePlan> = archive_paths
        .iter()
        .map(|path| match resolve_output_path(path, config) {
            Ok(resolution) => ArchivePlan {
                output_path: resolution.output_path,
                skip: resolution.skip,
                error: None,
            },
            Err(error) => ArchivePlan {
                output_path: PathBuf::new(),
                skip: false,
                error: Some(error.to_string()),
            },
        })
        .collect();

    let mut by_output: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, plan) in plans.iter().enumerate() {
        if plan.error.is_none() {
            by_output
                .entry(output_path_key(&plan.output_path))
                .or_default()
                .push(index);
        }
    }
    for indices in by_output.values().filter(|indices| indices.len() > 1) {
        let output = &plans[indices[0]].output_path;
        let inputs = indices
            .iter()
            .map(|index| archive_paths[*index].display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "Same-batch output conflict: inputs [{inputs}] resolve to output {}",
            output.display()
        );
        for index in indices {
            plans[*index].error = Some(message.clone());
        }
    }
    plans
}

/// Resolve the final path before any archive processing begins.
fn resolve_output_path(input: &Path, config: &OptimizeConfig) -> Result<OutputResolution> {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = match archive_kind(input) {
        Some(ArchiveKind::Rar) => "cbz".to_owned(),
        _ => input
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let filename = format!("{}{}.{}", stem, config.output_suffix, ext);
    let base_path = match &config.output_dir {
        Some(dir) => dir.join(&filename),
        None => input.parent().unwrap_or(Path::new(".")).join(&filename),
    };

    match config.overwrite_mode {
        OverwriteMode::Skip => Ok(OutputResolution {
            skip: base_path.exists(),
            output_path: base_path,
        }),
        OverwriteMode::Overwrite => Ok(OutputResolution {
            output_path: base_path,
            skip: false,
        }),
        OverwriteMode::Rename => {
            if !base_path.exists() {
                return Ok(OutputResolution {
                    output_path: base_path,
                    skip: false,
                });
            }
            let base_dir: &Path = config
                .output_dir
                .as_deref()
                .unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")));
            for n in 1..=9999 {
                let renamed = format!("{}{}({}).{}", stem, config.output_suffix, n, ext);
                let candidate = base_dir.join(&renamed);
                if !candidate.exists() {
                    return Ok(OutputResolution {
                        output_path: candidate,
                        skip: false,
                    });
                }
            }
            anyhow::bail!("Could not find available filename after 9999 attempts")
        }
    }
}

fn output_path_key(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    let key = normalized.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        key.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}

fn finalize_work_file(work_path: &Path, output_path: &Path, mode: &OverwriteMode) -> Result<()> {
    match mode {
        OverwriteMode::Overwrite => finalize_overwrite(work_path, output_path)?,
        OverwriteMode::Skip | OverwriteMode::Rename => {
            if output_path.exists() {
                anyhow::bail!(
                    "Output file appeared while processing: {}",
                    output_path.display()
                );
            }
            fs::rename(work_path, output_path).with_context(|| {
                format!("Failed to finalize output file: {}", output_path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn finalize_overwrite(work_path: &Path, output_path: &Path) -> Result<()> {
    let Some(output_name) = output_path.file_name() else {
        anyhow::bail!("Output path has no filename: {}", output_path.display());
    };
    let parent = output_path.parent().unwrap_or(Path::new("."));
    let backup_path = parent.join(format!(".cbz-opt-{}.backup", output_name.to_string_lossy()));
    // A backup without a destination means a previous process was interrupted
    // after staging the old output but before installing its finished work.
    // Preserve it as the rollback copy; it is removed only after this run's
    // work file has been installed successfully.
    if backup_path.exists() && output_path.exists() {
        fs::remove_file(&backup_path).with_context(|| {
            format!(
                "Failed to remove superseded optimizer backup: {}",
                backup_path.display()
            )
        })?;
    }
    if output_path.exists() {
        fs::rename(output_path, &backup_path).with_context(|| {
            format!("Failed to stage existing output: {}", output_path.display())
        })?;
    }

    match fs::rename(work_path, output_path) {
        Ok(()) => {
            if backup_path.exists() {
                if let Err(error) = fs::remove_file(&backup_path) {
                    log::warn!(
                        "Could not remove optimizer backup {} after successful replacement: {error}",
                        backup_path.display()
                    );
                }
            }
            Ok(())
        }
        Err(error) => {
            let restore = if backup_path.exists() {
                fs::rename(&backup_path, output_path)
            } else {
                Ok(())
            };
            match restore {
                Ok(()) => Err(error).with_context(|| {
                    format!("Failed to finalize output file: {}", output_path.display())
                }),
                Err(restore_error) => Err(anyhow::anyhow!(
                    "Failed to finalize output {} ({error}); rollback also failed ({restore_error}); backup remains at {}",
                    output_path.display(),
                    backup_path.display()
                )),
            }
        }
    }
}

#[cfg(not(windows))]
fn finalize_overwrite(work_path: &Path, output_path: &Path) -> Result<()> {
    fs::rename(work_path, output_path)
        .with_context(|| format!("Failed to finalize output file: {}", output_path.display()))
}

/// Build file output options without discarding a valid input entry timestamp.
/// Missing or invalid timestamps intentionally retain zip's normal safe default.
fn archive_file_options(last_modified: Option<zip::DateTime>) -> SimpleFileOptions {
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));
    apply_last_modified(options, last_modified)
}

/// Build directory output options without discarding a valid input timestamp.
fn archive_directory_options(last_modified: Option<zip::DateTime>) -> SimpleFileOptions {
    apply_last_modified(SimpleFileOptions::default(), last_modified)
}

fn apply_last_modified(
    options: SimpleFileOptions,
    last_modified: Option<zip::DateTime>,
) -> SimpleFileOptions {
    match last_modified.filter(zip::DateTime::is_valid) {
        Some(last_modified) => options.last_modified_time(last_modified),
        None => options,
    }
}

/// Replace the extension of an entry name, preserving any directory prefix.
fn replace_extension(name: &str, new_ext: &str) -> String {
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    match path.parent() {
        Some(parent) if parent != std::path::Path::new("") => {
            format!("{}/{}{}", parent.display(), stem, new_ext)
        }
        _ => format!("{}{}", stem, new_ext),
    }
}

/// Logical CPU count (without rayon dependency).
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

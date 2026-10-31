//! Formflux desktop application (egui).
//!
//! The UI only collects parameters and renders state.  Every operation runs in
//! a background Tokio runtime and reports through an unbounded channel, so a
//! slow conversion never blocks painting.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use formflux_core::cancel::CancelToken;
use formflux_core::data::{self, DataFormat};
use formflux_core::detect::FfmpegTools;
use formflux_core::hash::{self, HashAlgorithm};
use formflux_core::image::{
    self as imglib, ImageAdjustments, ImageOptions, ResizeMode, ResizeOptions,
};
use formflux_core::media::{self, ConvertOptions};
use formflux_core::progress::{ProgressEvent, ProgressSink};
use formflux_core::text::{self, TextFormat};
use formflux_core::time as timelib;
use formflux_shared::config::AppConfig;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Media,
    Image,
    Document,
    Hash,
    Color,
    Tools,
    Data,
    Generate,
    Time,
}

impl Tab {
    const ALL: [Tab; 9] = [
        Tab::Media,
        Tab::Image,
        Tab::Document,
        Tab::Hash,
        Tab::Color,
        Tab::Tools,
        Tab::Data,
        Tab::Generate,
        Tab::Time,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Media => "媒体 Media",
            Tab::Image => "图像 Image",
            Tab::Document => "文档 Document",
            Tab::Hash => "哈希 Hash",
            Tab::Color => "颜色 Colour",
            Tab::Tools => "工具箱 Tools",
            Tab::Data => "数据 Data",
            Tab::Generate => "生成 Generate",
            Tab::Time => "时间 Time",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

struct TaskView {
    id: u64,
    label: String,
    progress: f32,
    message: String,
    state: TaskState,
    cancel: CancelToken,
}

struct TaskUpdate {
    id: u64,
    progress: f32,
    message: String,
    state: Option<TaskState>,
    /// Optional payload to show in the preview pane (hash list, OCR text).
    result: Option<String>,
}

#[derive(Clone)]
struct GuiProgress {
    id: u64,
    tx: UnboundedSender<TaskUpdate>,
    percent: Arc<Mutex<f32>>,
}

impl ProgressSink for GuiProgress {
    fn report(&self, event: ProgressEvent) {
        if let Ok(mut value) = self.percent.lock() {
            *value = event.percent as f32;
        }
        let _ = self.tx.send(TaskUpdate {
            id: self.id,
            progress: event.percent as f32,
            message: event.message.or(Some(event.stage)).unwrap_or_default(),
            state: Some(TaskState::Running),
            result: None,
        });
    }
}

struct Runner {
    runtime: tokio::runtime::Runtime,
    tx: UnboundedSender<TaskUpdate>,
    rx: UnboundedReceiver<TaskUpdate>,
    semaphore: Arc<Semaphore>,
    next_id: AtomicU64,
}

impl Runner {
    fn new(jobs: usize) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Ok(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(jobs.max(1))
                .build()?,
            tx,
            rx,
            semaphore: Arc::new(Semaphore::new(jobs.max(1))),
            next_id: AtomicU64::new(1),
        })
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn spawn<F>(&self, label: &str, cancel: CancelToken, job: F) -> TaskView
    where
        F: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        let id = self.id();
        let tx = self.tx.clone();
        let semaphore = self.semaphore.clone();
        let job_label = label.to_string();
        let job_cancel = cancel.clone();
        self.runtime.spawn(async move {
            let _permit = match semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 0.0,
                        message: "runner is shutting down".to_string(),
                        state: Some(TaskState::Failed),
                        result: None,
                    });
                    return;
                }
            };
            let _ = tx.send(TaskUpdate {
                id,
                progress: 0.0,
                message: "running".to_string(),
                state: Some(TaskState::Running),
                result: None,
            });
            match job.await {
                Ok(message) => {
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 100.0,
                        message,
                        state: Some(TaskState::Done),
                        result: None,
                    });
                }
                Err(_error) if job_cancel.is_cancelled() => {
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 0.0,
                        message: "cancelled".to_string(),
                        state: Some(TaskState::Cancelled),
                        result: None,
                    });
                }
                Err(error) => {
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 0.0,
                        message: error.to_string(),
                        state: Some(TaskState::Failed),
                        result: None,
                    });
                }
            }
        });
        TaskView {
            id,
            label: job_label,
            progress: 0.0,
            message: "queued".to_string(),
            state: TaskState::Queued,
            cancel,
        }
    }
}

struct App {
    tab: Tab,
    config: AppConfig,
    runner: Runner,
    tasks: Vec<TaskView>,
    inputs: Vec<PathBuf>,
    output_dir: String,
    result: String,
    status: String,
    preview: Option<egui::TextureHandle>,
    preview_for: Option<PathBuf>,

    media_format: String,
    media_stream_copy: bool,
    media_crf: u8,
    image_format: String,
    image_quality: u8,
    image_width: u32,
    image_height: u32,
    hash_algorithm: String,
    text_target: String,
    color_input: String,
    color_secondary: String,
    color_target: String,
    data_target: String,
    time_input: String,
    time_unit: String,
    time_zone: String,
    split_size: String,
    password_length: usize,
    radix_input: String,
    radix_from: String,
    radix_to: String,
    ocr_lang: String,
    ocr_psm: u8,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> anyhow::Result<Self> {
        install_cjk_font(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        let jobs = config.effective_jobs();
        Ok(Self {
            tab: Tab::Image,
            runner: Runner::new(jobs)?,
            config,
            tasks: Vec::new(),
            inputs: Vec::new(),
            output_dir: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string(),
            result: String::new(),
            status: "Ready. Drop files anywhere or use Add files.".to_string(),
            preview: None,
            preview_for: None,
            media_format: "mp4".to_string(),
            media_stream_copy: false,
            media_crf: 23,
            image_format: String::new(),
            image_quality: 90,
            image_width: 0,
            image_height: 0,
            hash_algorithm: "sha256".to_string(),
            text_target: "html".to_string(),
            color_input: "#ff8800".to_string(),
            color_secondary: "#ffffff".to_string(),
            color_target: "hsl".to_string(),
            data_target: "yaml".to_string(),
            time_input: "0".to_string(),
            time_unit: "s".to_string(),
            time_zone: "UTC".to_string(),
            split_size: "100M".to_string(),
            password_length: 24,
            radix_input: "255".to_string(),
            radix_from: "10".to_string(),
            radix_to: "16".to_string(),
            ocr_lang: "eng".to_string(),
            ocr_psm: 3,
        })
    }

    fn add_inputs(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if !self.inputs.contains(&path) {
                self.inputs.push(path);
            }
        }
        self.refresh_preview();
    }

    fn refresh_preview(&mut self) {
        self.preview = None;
        self.preview_for = None;
    }

    fn output_path(&self) -> PathBuf {
        PathBuf::from(self.output_dir.trim())
    }

    fn image_options(&self) -> ImageOptions {
        ImageOptions {
            format: self.image_format.clone(),
            quality: self.image_quality,
            resize: if self.image_width > 0 || self.image_height > 0 {
                Some(ResizeOptions {
                    width: (self.image_width > 0).then_some(self.image_width),
                    height: (self.image_height > 0).then_some(self.image_height),
                    mode: ResizeMode::Fit,
                })
            } else {
                None
            },
            adjustments: ImageAdjustments::default(),
            ..ImageOptions::default()
        }
    }

    fn spawn_image(&mut self) {
        if self.inputs.is_empty() {
            self.status = "Add at least one image first.".to_string();
            return;
        }
        let inputs = self.inputs.clone();
        let output = self.output_path();
        let options = self.image_options();
        let id = self.runner.id();
        let cancel = CancelToken::new();
        let progress = GuiProgress {
            id,
            tx: self.runner.tx.clone(),
            percent: Arc::new(Mutex::new(0.0)),
        };
        let tx = self.runner.tx.clone();
        let cancel_job = cancel.clone();
        let future = async move {
            let result = tokio::task::spawn_blocking(move || {
                imglib::batch_convert(
                    &inputs,
                    &output,
                    &options,
                    false,
                    self_jobs(),
                    &cancel_job,
                    &progress,
                )
            })
            .await?;
            match result {
                Ok(outcomes) => {
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 100.0,
                        message: format!("converted {} image(s)", outcomes.len()),
                        state: Some(TaskState::Done),
                        result: None,
                    });
                    Ok(format!("Converted {} image(s).", outcomes.len()))
                }
                Err(error) => Err(error.into()),
            }
        };
        let task = self.runner.spawn("Image batch", cancel, future);
        self.tasks.push(task);
        self.status = "Image batch queued.".to_string();
    }
}

fn self_jobs() -> usize {
    formflux_shared::platform::logical_cpus()
}

fn install_cjk_font(ctx: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "formflux-cjk".to_string(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "formflux-cjk".to_string());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(update) = self.runner.rx.try_recv() {
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == update.id) {
                task.progress = update.progress;
                if !update.message.is_empty() {
                    task.message = update.message;
                }
                if let Some(result) = update.result {
                    self.result = result;
                }
                if let Some(state) = update.state {
                    if state == TaskState::Done {
                        self.status = task.message.clone();
                    }
                    task.state = state;
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(120));

        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_inputs(dropped);
        }

        egui::SidePanel::left("navigation")
            .resizable(false)
            .exact_width(170.0)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.heading("Formflux");
                ui.label(egui::RichText::new("offline toolbox").small().weak());
                ui.add_space(16.0);
                for tab in Tab::ALL {
                    if ui.selectable_label(self.tab == tab, tab.label()).clicked() {
                        self.tab = tab;
                    }
                }
                ui.add_space(20.0);
                ui.separator();
                ui.label(format!(
                    "FFmpeg: {}",
                    if self.config.tools.ffmpeg_path.is_some() {
                        "configured"
                    } else {
                        "PATH"
                    }
                ));
                ui.label(format!("jobs: {}", self.config.effective_jobs()));
            });

        egui::TopBottomPanel::bottom("tasks")
            .resizable(true)
            .default_height(180.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Task queue");
                    if ui.button("Clear finished").clicked() {
                        self.tasks.retain(|task| {
                            !matches!(task.state, TaskState::Done | TaskState::Failed)
                        });
                    }
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.tasks.is_empty() {
                        ui.weak("No tasks yet.");
                    }
                    let mut cancel_id = None;
                    for task in &self.tasks {
                        ui.horizontal(|ui| {
                            ui.label(format!("{:<14}", task.label));
                            ui.add(
                                egui::ProgressBar::new(task.progress / 100.0)
                                    .desired_width(240.0)
                                    .show_percentage(),
                            );
                            ui.label(format!("{:?}", task.state));
                            ui.weak(&task.message);
                            if matches!(task.state, TaskState::Queued | TaskState::Running)
                                && ui.small_button("Cancel").clicked()
                            {
                                cancel_id = Some(task.id);
                            }
                        });
                    }
                    if let Some(id) = cancel_id {
                        if let Some(task) = self.tasks.iter().find(|task| task.id == id) {
                            task.cancel.cancel();
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add files").clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        self.add_inputs(files);
                    }
                }
                if ui.button("Add folder").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        match formflux_core::io::list_files(&folder, false, true) {
                            Ok(files) => self.add_inputs(files),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                }
                if ui.button("Clear").clicked() {
                    self.inputs.clear();
                    self.refresh_preview();
                }
                ui.separator();
                ui.label("Output");
                ui.add(egui::TextEdit::singleline(&mut self.output_dir).desired_width(320.0));
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.status).weak());
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(260.0);
                        ui.heading("Inputs");
                        if self.inputs.is_empty() {
                            ui.weak("Drop files here or add a folder.");
                        }
                        for path in self.inputs.iter().take(30) {
                            ui.label(
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("file"),
                            )
                            .on_hover_text(path.display().to_string());
                        }
                        if self.inputs.len() > 30 {
                            ui.weak(format!("… and {} more", self.inputs.len() - 30));
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| match self.tab {
                        Tab::Media => self.media_tab(ui),
                        Tab::Image => self.image_tab(ui),
                        Tab::Document => self.document_tab(ui),
                        Tab::Hash => self.hash_tab(ui),
                        Tab::Color => self.color_tab(ui),
                        Tab::Tools => self.tools_tab(ui),
                        Tab::Data => self.data_tab(ui),
                        Tab::Generate => self.generate_tab(ui),
                        Tab::Time => self.time_tab(ui),
                    });
                });
            });
        });
    }
}

impl App {
    fn media_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Media conversion");
        egui::ComboBox::from_label("Format")
            .selected_text(&self.media_format)
            .show_ui(ui, |ui| {
                for format in ["mp4", "webm", "mkv", "mov", "mp3", "flac", "wav", "ogg"] {
                    ui.selectable_value(&mut self.media_format, format.to_string(), format);
                }
            });
        ui.checkbox(&mut self.media_stream_copy, "Stream copy (no re-encode)");
        ui.add(egui::Slider::new(&mut self.media_crf, 0..=51).text("CRF"));
        if ui.button("Convert").clicked() {
            self.spawn_media();
        }
    }

    fn spawn_media(&mut self) {
        let Some(input) = self.inputs.first().cloned() else {
            self.status = "Add a media file first.".to_string();
            return;
        };
        let config = self.config.clone();
        let output = self.output_path();
        let options = ConvertOptions {
            format: self.media_format.clone(),
            stream_copy: self.media_stream_copy,
            crf: (!self.media_stream_copy).then_some(self.media_crf),
            ..ConvertOptions::default()
        };
        let id = self.runner.id();
        let tx = self.runner.tx.clone();
        let cancel = CancelToken::new();
        let cancel_job = cancel.clone();
        let progress = GuiProgress {
            id,
            tx: tx.clone(),
            percent: Arc::new(Mutex::new(0.0)),
        };
        let future = async move {
            let tools = FfmpegTools::require(&config)?;
            let progress_dyn: Box<dyn ProgressSink> = Box::new(progress);
            let outcome = media::convert(
                &tools,
                &input,
                &output,
                &options,
                false,
                &cancel_job,
                progress_dyn.as_ref(),
            )
            .await?;
            let message = format!("Wrote {}", outcome.output.display());
            let _ = tx.send(TaskUpdate {
                id,
                progress: 100.0,
                message: message.clone(),
                state: Some(TaskState::Done),
                result: Some(message.clone()),
            });
            Ok(message)
        };
        self.tasks
            .push(self.runner.spawn("Media convert", cancel, future));
        self.status = "Media conversion queued.".to_string();
    }

    fn image_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Image processing");
        egui::ComboBox::from_label("Output format")
            .selected_text(if self.image_format.is_empty() {
                "same as input"
            } else {
                &self.image_format
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.image_format, String::new(), "same as input");
                for format in ["jpg", "png", "webp", "bmp", "tiff", "ico", "gif"] {
                    ui.selectable_value(&mut self.image_format, format.to_string(), format);
                }
            });
        ui.add(egui::Slider::new(&mut self.image_quality, 1..=100).text("Quality"));
        ui.horizontal(|ui| {
            ui.label("Width");
            ui.add(egui::DragValue::new(&mut self.image_width).range(0..=16384));
            ui.label("Height");
            ui.add(egui::DragValue::new(&mut self.image_height).range(0..=16384));
        });
        if ui.button("Convert / compress").clicked() {
            self.spawn_image();
        }
        if let Some(texture) = &self.preview {
            ui.add(egui::Image::new(texture).max_width(360.0));
        } else if let Some(path) = self.inputs.first() {
            if self.preview_for.as_ref() != Some(path) {
                // Decoding is for a thumbnail only, so refuse enormous files
                // rather than allocating gigabytes on the UI thread.
                let small_enough = path
                    .metadata()
                    .map(|metadata| metadata.len() <= 64 * 1024 * 1024)
                    .unwrap_or(false);
                if !small_enough {
                    ui.weak("Preview skipped for a large file.");
                    return;
                }
                if let Ok(image) = image::open(path) {
                    let image = image.thumbnail(480, 320).to_rgba8();
                    let size = [image.width() as usize, image.height() as usize];
                    let texture = ui.ctx().load_texture(
                        "preview",
                        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                        Default::default(),
                    );
                    self.preview = Some(texture);
                    self.preview_for = Some(path.clone());
                }
            }
            if let Some(texture) = &self.preview {
                ui.add(egui::Image::new(texture).max_width(360.0));
            }
        }
    }

    fn document_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Document conversion");
        egui::ComboBox::from_label("Target")
            .selected_text(&self.text_target)
            .show_ui(ui, |ui| {
                for target in ["md", "html", "txt"] {
                    ui.selectable_value(&mut self.text_target, target.to_string(), target);
                }
            });
        if ui.button("Convert").clicked() {
            let Some(input) = self.inputs.first().cloned() else {
                self.status = "Add a document first.".to_string();
                return;
            };
            let output = self.output_path();
            let target = match TextFormat::parse(&self.text_target) {
                Ok(target) => target,
                Err(error) => {
                    self.status = error.to_string();
                    return;
                }
            };
            match text::convert_document(&input, &output, target, false) {
                Ok(path) => {
                    self.result = path.display().to_string();
                    self.status = format!("Wrote {}", path.display());
                }
                Err(error) => self.status = error.to_string(),
            }
        }
        if !self.result.is_empty() {
            ui.add(
                egui::TextEdit::multiline(&mut self.result)
                    .desired_rows(12)
                    .desired_width(f32::INFINITY),
            );
        }
        ui.separator();
        ui.heading("Offline OCR");
        ui.label("Uses the first input image and an external Tesseract install.");
        ui.horizontal(|ui| {
            ui.label("Language");
            ui.add(egui::TextEdit::singleline(&mut self.ocr_lang).desired_width(80.0));
            ui.label("PSM");
            ui.add(egui::DragValue::new(&mut self.ocr_psm).range(0..=13));
            if ui.button("Recognize text").clicked() {
                let Some(input) = self.inputs.first().cloned() else {
                    self.status = "Add an image first.".to_string();
                    return;
                };
                let config = self.config.clone();
                let language = self.ocr_lang.trim().to_string();
                let psm = self.ocr_psm;
                let output_dir = self.output_path();
                let id = self.runner.id();
                let tx = self.runner.tx.clone();
                let cancel = CancelToken::new();
                let future = async move {
                    let result =
                        formflux_core::ocr::ocr_with_config(&config, &input, Some(&language), psm)
                            .await?;
                    let target = formflux_shared::fs::destination_for(
                        &input,
                        &output_dir,
                        Some("txt"),
                        false,
                    )?;
                    formflux_core::io::write_text(&target, &result.text, false)?;
                    let message = format!("OCR wrote {}", target.display());
                    let _ = tx.send(TaskUpdate {
                        id,
                        progress: 100.0,
                        message: message.clone(),
                        state: Some(TaskState::Done),
                        result: Some(result.text),
                    });
                    Ok(message)
                };
                self.tasks.push(self.runner.spawn("OCR", cancel, future));
                self.status = "OCR queued.".to_string();
            }
        });
    }

    fn hash_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("File hashes");
        egui::ComboBox::from_label("Algorithm")
            .selected_text(&self.hash_algorithm)
            .show_ui(ui, |ui| {
                for algorithm in ["md5", "sha1", "sha256", "sha384", "sha512", "blake2b"] {
                    ui.selectable_value(&mut self.hash_algorithm, algorithm.to_string(), algorithm);
                }
            });
        if ui.button("Hash inputs").clicked() {
            if self.inputs.is_empty() {
                self.status = "Add files first.".to_string();
                return;
            }
            let inputs = self.inputs.clone();
            let algorithm = match HashAlgorithm::parse(&self.hash_algorithm) {
                Ok(value) => value,
                Err(error) => {
                    self.status = error.to_string();
                    return;
                }
            };
            let id = self.runner.id();
            let tx = self.runner.tx.clone();
            let cancel = CancelToken::new();
            let progress = GuiProgress {
                id,
                tx: tx.clone(),
                percent: Arc::new(Mutex::new(0.0)),
            };
            let cancel_job = cancel.clone();
            let future = async move {
                let result = tokio::task::spawn_blocking(move || {
                    hash::hash_paths(&inputs, algorithm, false, true, &cancel_job, &progress)
                })
                .await?;
                match result {
                    Ok(hashes) => {
                        let text = hashes
                            .iter()
                            .map(|item| format!("{}  {}", item.digest, item.path.display()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = tx.send(TaskUpdate {
                            id,
                            progress: 100.0,
                            message: format!("hashed {} file(s)", hashes.len()),
                            state: Some(TaskState::Done),
                            result: Some(text.clone()),
                        });
                        Ok(text)
                    }
                    Err(error) => Err(error.into()),
                }
            };
            self.tasks.push(self.runner.spawn("Hash", cancel, future));
            self.status = "Hash task queued.".to_string();
        }
    }

    fn color_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Colour tools");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.color_input).desired_width(220.0));
            let mut pick = formflux_core::color::parse(&self.color_input)
                .map(|color| {
                    egui::Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
                })
                .unwrap_or(egui::Color32::WHITE);
            if ui.color_edit_button_srgba(&mut pick).changed() {
                self.color_input = format!("#{:02x}{:02x}{:02x}", pick.r(), pick.g(), pick.b());
            }
        });
        egui::ComboBox::from_label("To")
            .selected_text(&self.color_target)
            .show_ui(ui, |ui| {
                for space in ["hex", "rgb", "rgba", "hsl", "hsla", "cmyk", "lab"] {
                    ui.selectable_value(&mut self.color_target, space.to_string(), space);
                }
            });
        if ui.button("Convert").clicked() {
            match formflux_core::color::parse(&self.color_input) {
                Ok(color) => {
                    let space = match formflux_core::color::ColorSpace::parse(&self.color_target) {
                        Ok(space) => space,
                        Err(error) => {
                            self.status = error.to_string();
                            return;
                        }
                    };
                    self.result = formflux_core::color::format(color, space);
                    self.status = self.result.clone();
                }
                Err(error) => self.status = error.to_string(),
            }
        }
        if !self.result.is_empty() {
            ui.monospace(&self.result);
        }
        ui.separator();
        ui.heading("WCAG contrast");
        ui.horizontal(|ui| {
            ui.label("Background");
            ui.add(egui::TextEdit::singleline(&mut self.color_secondary).desired_width(120.0));
            if ui.button("Check").clicked() {
                let foreground = formflux_core::color::parse(&self.color_input);
                let background = formflux_core::color::parse(&self.color_secondary);
                match (foreground, background) {
                    (Ok(foreground), Ok(background)) => {
                        let wcag = formflux_core::color::contrast(foreground, background);
                        self.result = format!(
                            "ratio {:.2}:1  AA normal {} ({})  AAA normal {} ({})",
                            wcag.ratio,
                            "4.5:1",
                            if wcag.aa_normal { "pass" } else { "fail" },
                            "7:1",
                            if wcag.aaa_normal { "pass" } else { "fail" }
                        );
                        self.status = self.result.clone();
                    }
                    (Err(error), _) | (_, Err(error)) => self.status = error.to_string(),
                }
            }
        });
    }

    fn tools_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("File toolbox");
        ui.horizontal(|ui| {
            ui.label("Split chunk size");
            ui.add(egui::TextEdit::singleline(&mut self.split_size).desired_width(100.0));
            if ui.button("Split first input").clicked() {
                let Some(input) = self.inputs.first().cloned() else {
                    self.status = "Add a file first.".to_string();
                    return;
                };
                let size = match formflux_shared::units::parse_bytes(&self.split_size) {
                    Ok(size) => size,
                    Err(error) => {
                        self.status = error.to_string();
                        return;
                    }
                };
                match formflux_core::io::split_file(
                    &input,
                    &self.output_path(),
                    size,
                    &CancelToken::never(),
                    &formflux_core::progress::NoopProgress,
                ) {
                    Ok(parts) => {
                        self.status = format!("Wrote {} chunk(s).", parts.len());
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
        });
        if ui.button("Remove empty directories").clicked() {
            let dir = self.output_path();
            match formflux_core::tools::clean_empty_dirs(&dir, true) {
                Ok(dirs) => self.status = format!("Would remove {} empty dir(s).", dirs.len()),
                Err(error) => self.status = error.to_string(),
            }
        }
        ui.separator();
        ui.heading("Radix");
        ui.horizontal(|ui| {
            ui.label("Value");
            ui.add(egui::TextEdit::singleline(&mut self.radix_input).desired_width(180.0));
            ui.label("From");
            ui.add(egui::TextEdit::singleline(&mut self.radix_from).desired_width(40.0));
            ui.label("To");
            ui.add(egui::TextEdit::singleline(&mut self.radix_to).desired_width(40.0));
            if ui.button("Convert").clicked() {
                let result = formflux_core::radix::Radix::parse(&self.radix_from)
                    .and_then(|from| {
                        formflux_core::radix::Radix::parse(&self.radix_to).map(|to| (from, to))
                    })
                    .and_then(|(from, to)| {
                        formflux_core::radix::convert(&self.radix_input, Some(from), to)
                    });
                match result {
                    Ok(value) => {
                        self.result = value.clone();
                        self.status = format!("Radix: {value}");
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
        });
        if !self.result.is_empty() {
            ui.monospace(&self.result);
        }
    }

    fn data_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Data conversion");
        ui.label("Uses the first input file and the output directory.");
        egui::ComboBox::from_label("Target")
            .selected_text(&self.data_target)
            .show_ui(ui, |ui| {
                for target in ["json", "yaml", "xml"] {
                    ui.selectable_value(&mut self.data_target, target.to_string(), target);
                }
            });
        if ui.button("Convert").clicked() {
            let Some(input) = self.inputs.first().cloned() else {
                self.status = "Add a JSON/YAML/XML file first.".to_string();
                return;
            };
            let target = match DataFormat::parse(&self.data_target) {
                Ok(target) => target,
                Err(error) => {
                    self.status = error.to_string();
                    return;
                }
            };
            match data::convert_file(&input, &self.output_path(), target, true, false) {
                Ok(path) => self.status = format!("Wrote {}", path.display()),
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    fn generate_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Generate");
        if ui.button("New UUID v4").clicked() {
            self.result = formflux_core::generate::uuid_v4();
            self.status = self.result.clone();
        }
        if ui.button("New UUID v7").clicked() {
            self.result = formflux_core::generate::uuid_v7();
            self.status = self.result.clone();
        }
        ui.add(egui::Slider::new(&mut self.password_length, 8..=128).text("Password length"));
        if ui.button("Password").clicked() {
            let options = formflux_core::generate::PasswordOptions {
                length: self.password_length,
                ..Default::default()
            };
            match formflux_core::generate::generate_password(&options) {
                Ok(password) => {
                    self.result = password;
                    self.status = "Password generated.".to_string();
                }
                Err(error) => self.status = error.to_string(),
            }
        }
        if ui.button("QR to output/qr.png").clicked() {
            let target = self.output_path().join("qr.png");
            match formflux_core::generate::write_qr(&self.result, &target, 8, 4, false) {
                Ok(()) => self.status = format!("Wrote {}", target.display()),
                Err(error) => self.status = error.to_string(),
            }
        }
        if !self.result.is_empty() {
            ui.monospace(&self.result);
        }
    }

    fn time_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Time and time zones");
        if ui.button("Now").clicked() {
            match timelib::convert_timestamp(
                timelib::now(timelib::TimeUnit::Seconds),
                timelib::TimeUnit::Seconds,
                &self.time_zone,
                "iso8601",
            ) {
                Ok(info) => {
                    self.result = format!("{} ({})", info.rfc3339, info.timezone);
                    self.status = self.result.clone();
                }
                Err(error) => self.status = error.to_string(),
            }
        }
        ui.horizontal(|ui| {
            ui.label("Timestamp");
            ui.add(egui::TextEdit::singleline(&mut self.time_input).desired_width(160.0));
            egui::ComboBox::from_id_salt("unit")
                .selected_text(&self.time_unit)
                .show_ui(ui, |ui| {
                    for unit in ["s", "ms", "us"] {
                        ui.selectable_value(&mut self.time_unit, unit.to_string(), unit);
                    }
                });
            ui.label("Zone");
            ui.add(egui::TextEdit::singleline(&mut self.time_zone).desired_width(140.0));
        });
        if ui.button("Convert").clicked() {
            let value = self.time_input.trim().parse::<i64>();
            let unit = timelib::TimeUnit::parse(&self.time_unit);
            match (value, unit) {
                (Ok(value), Ok(unit)) => {
                    match timelib::convert_timestamp(value, unit, &self.time_zone, "iso8601") {
                        Ok(info) => {
                            self.result = format!(
                                "{}\n{} s / {} ms / {} us",
                                info.rfc3339,
                                info.unix_seconds,
                                info.unix_milliseconds,
                                info.unix_microseconds
                            );
                            self.status = "Converted.".to_string();
                        }
                        Err(error) => self.status = error.to_string(),
                    }
                }
                (Err(error), _) => self.status = format!("invalid timestamp: {error}"),
                (_, Err(error)) => self.status = error.to_string(),
            }
        }
        if !self.result.is_empty() {
            ui.monospace(&self.result);
        }
    }
}

fn main() -> eframe::Result {
    let config = match AppConfig::load_or_create_default() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("warning: could not load configuration: {error}");
            AppConfig::default()
        }
    };
    let _ = formflux_shared::logging::init(&config.log_level, false, false);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Formflux",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, config)?))),
    )
}

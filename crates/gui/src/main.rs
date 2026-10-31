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
use formflux_shared::config::{AppConfig, Language};
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

    fn label(self, lang: Language) -> &'static str {
        lang.pick(
            match self {
                Tab::Media => "Media",
                Tab::Image => "Image",
                Tab::Document => "Documents",
                Tab::Hash => "Hashes",
                Tab::Color => "Colour",
                Tab::Tools => "Toolbox",
                Tab::Data => "Data",
                Tab::Generate => "Generate",
                Tab::Time => "Time",
            },
            match self {
                Tab::Media => "媒体",
                Tab::Image => "图像",
                Tab::Document => "文档",
                Tab::Hash => "哈希",
                Tab::Color => "颜色",
                Tab::Tools => "工具箱",
                Tab::Data => "数据",
                Tab::Generate => "生成",
                Tab::Time => "时间",
            },
        )
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::Media => "▶",
            Tab::Image => "◈",
            Tab::Document => "¶",
            Tab::Hash => "#",
            Tab::Color => "◐",
            Tab::Tools => "⚙",
            Tab::Data => "{}",
            Tab::Generate => "✦",
            Tab::Time => "◷",
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

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0, 150, 136);
const ACCENT_SOFT: egui::Color32 = egui::Color32::from_rgb(0, 184, 169);
const DANGER: egui::Color32 = egui::Color32::from_rgb(220, 84, 84);

fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    style.visuals.hyperlink_color = ACCENT_SOFT;
    style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.hovered.weak_bg_fill = ACCENT.gamma_multiply(0.18);
    style.visuals.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.28);
    ctx.set_style(style);
}

fn sidebar_fill(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(20, 24, 29)
    } else {
        egui::Color32::from_rgb(238, 242, 245)
    }
}

fn card_fill(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(30, 35, 41)
    } else {
        egui::Color32::from_rgb(255, 255, 255)
    }
}

fn subtle_fill(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(37, 43, 50)
    } else {
        egui::Color32::from_rgb(246, 248, 250)
    }
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(ACCENT)
        .corner_radius(7)
        .min_size(egui::vec2(170.0, 34.0)),
    )
}

fn status_dot(ui: &mut egui::Ui, ok: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.0, if ok { ACCENT_SOFT } else { DANGER });
}

fn dashed_rect(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let dash = 8.0;
    let gap = 5.0;
    let mut x = rect.left();
    while x < rect.right() {
        let end = (x + dash).min(rect.right());
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(end, rect.top())],
            egui::Stroke::new(1.5_f32, color),
        );
        painter.line_segment(
            [egui::pos2(x, rect.bottom()), egui::pos2(end, rect.bottom())],
            egui::Stroke::new(1.5_f32, color),
        );
        x += dash + gap;
    }
    let mut y = rect.top();
    while y < rect.bottom() {
        let end = (y + dash).min(rect.bottom());
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.left(), end)],
            egui::Stroke::new(1.5_f32, color),
        );
        painter.line_segment(
            [egui::pos2(rect.right(), y), egui::pos2(rect.right(), end)],
            egui::Stroke::new(1.5_f32, color),
        );
        y += dash + gap;
    }
}

struct App {
    tab: Tab,
    lang: Language,
    config: AppConfig,
    jobs: usize,
    ffmpeg_ready: bool,
    tesseract_ready: bool,
    about_open: bool,
    settings_open: bool,
    keep_aspect: bool,
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
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        initial_inputs: Vec<PathBuf>,
    ) -> anyhow::Result<Self> {
        install_cjk_font(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        configure_style(&cc.egui_ctx);
        let jobs = config.effective_jobs();
        let lang = config.ui.language;
        let ffmpeg = formflux_core::detect::inspect(
            "ffmpeg",
            config.tools.ffmpeg_path.as_deref(),
            &["-version"],
        );
        let tesseract = formflux_core::detect::inspect(
            "tesseract",
            config.tools.tesseract_path.as_deref(),
            &["--version"],
        );
        let output_dir = config
            .ui
            .last_output_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .display()
            .to_string();
        let mut app = Self {
            tab: Tab::Image,
            lang,
            runner: Runner::new(jobs)?,
            config,
            jobs,
            ffmpeg_ready: ffmpeg.available,
            tesseract_ready: tesseract.available,
            about_open: false,
            settings_open: false,
            keep_aspect: true,
            tasks: Vec::new(),
            inputs: Vec::new(),
            output_dir,
            result: String::new(),
            status: lang
                .pick(
                    "Ready. Drop files anywhere or use Add files.",
                    "就绪。拖入文件，或点击“添加文件”。",
                )
                .to_string(),
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
        };
        if !initial_inputs.is_empty() {
            app.add_inputs(initial_inputs);
        }
        Ok(app)
    }

    fn t<'a>(&self, en: &'a str, zh: &'a str) -> &'a str {
        self.lang.pick(en, zh)
    }

    fn set_language(&mut self, lang: Language) {
        self.lang = lang;
        self.config.ui.language = lang;
        let _ = self.config.save_default();
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

        let dark = ctx.style().visuals.dark_mode;
        let lang = self.lang;

        egui::SidePanel::left("navigation")
            .resizable(false)
            .exact_width(218.0)
            .frame(
                egui::Frame::new()
                    .fill(sidebar_fill(dark))
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(ACCENT)
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(11, 7))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("F")
                                    .heading()
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );
                        });
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Formflux").heading().strong());
                        ui.label(
                            egui::RichText::new(lang.pick("offline toolbox", "离线工具箱"))
                                .small()
                                .weak(),
                        );
                    });
                });
                ui.add_space(18.0);
                for tab in Tab::ALL {
                    let selected = self.tab == tab;
                    let text =
                        egui::RichText::new(format!("{}   {}", tab.icon(), tab.label(self.lang)))
                            .size(14.0)
                            .color(if selected {
                                egui::Color32::WHITE
                            } else {
                                ui.visuals().text_color()
                            });
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            ACCENT
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(7)
                        .min_size(egui::vec2(ui.available_width(), 34.0));
                    if ui.add(button).clicked() {
                        self.tab = tab;
                    }
                }
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.lang == Language::En, "EN")
                        .clicked()
                    {
                        self.set_language(Language::En);
                    }
                    if ui
                        .selectable_label(self.lang == Language::Zh, "中文")
                        .clicked()
                    {
                        self.set_language(Language::Zh);
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new(format!("v{} · MIT", formflux_shared::VERSION))
                            .small()
                            .weak(),
                    );
                    if ui
                        .small_button(self.t("About Formflux", "关于 Formflux"))
                        .clicked()
                    {
                        self.about_open = true;
                    }
                    ui.add_space(4.0);
                });
            });

        egui::TopBottomPanel::bottom("status")
            .exact_height(34.0)
            .frame(
                egui::Frame::new()
                    .fill(subtle_fill(dark))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    status_dot(ui, self.ffmpeg_ready);
                    ui.label(
                        egui::RichText::new(if self.ffmpeg_ready {
                            self.t("FFmpeg: Ready", "FFmpeg：就绪")
                        } else {
                            self.t("FFmpeg: Missing", "FFmpeg：缺失")
                        })
                        .small(),
                    );
                    ui.add_space(8.0);
                    status_dot(ui, self.tesseract_ready);
                    ui.label(
                        egui::RichText::new(if self.tesseract_ready {
                            self.t("Tesseract: Ready", "Tesseract：就绪")
                        } else {
                            self.t("Tesseract: Optional", "Tesseract：可选")
                        })
                        .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(format!("{} {}", self.t("About", "关于"), "ⓘ"))
                            .clicked()
                        {
                            self.about_open = true;
                        }
                        if ui
                            .button(format!("{} {}", self.t("Settings", "设置"), "⚙"))
                            .clicked()
                        {
                            self.settings_open = true;
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{}: {}  ·  v{}",
                                self.t("jobs", "并发"),
                                self.jobs,
                                formflux_shared::VERSION
                            ))
                            .small()
                            .weak(),
                        );
                    });
                });
            });

        egui::TopBottomPanel::bottom("tasks")
            .resizable(true)
            .default_height(186.0)
            .min_height(120.0)
            .frame(
                egui::Frame::new()
                    .fill(card_fill(dark))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(self.t("Task queue", "任务队列")).heading());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(self.t("Clear finished", "清除已完成")).clicked() {
                            self.tasks.retain(|task| {
                                !matches!(
                                    task.state,
                                    TaskState::Done | TaskState::Failed | TaskState::Cancelled
                                )
                            });
                        }
                    });
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.tasks.is_empty() {
                        ui.add_space(10.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("◌")
                                    .size(30.0)
                                    .color(ACCENT.gamma_multiply(0.7)),
                            );
                            ui.label(
                                egui::RichText::new(self.t(
                                    "No tasks yet. Drop a file above to begin.",
                                    "当前没有任务。把文件拖到上方即可开始。",
                                ))
                                .weak(),
                            );
                        });
                    }
                    let mut cancel_id = None;
                    for task in &self.tasks {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  {}",
                                    task_icon(&task.state),
                                    task.label
                                ))
                                .strong(),
                            );
                            ui.add(
                                egui::ProgressBar::new(task.progress / 100.0)
                                    .desired_width(230.0)
                                    .show_percentage()
                                    .fill(task_color(&task.state)),
                            );
                            ui.label(
                                egui::RichText::new(task_state_label(self.lang, &task.state))
                                    .small()
                                    .color(task_color(&task.state)),
                            );
                            ui.label(egui::RichText::new(&task.message).small().weak());
                            if matches!(task.state, TaskState::Queued | TaskState::Running)
                                && ui.small_button(self.t("Cancel", "取消")).clicked()
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

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(if dark {
                        egui::Color32::from_rgb(24, 28, 33)
                    } else {
                        egui::Color32::from_rgb(247, 249, 251)
                    })
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(card_fill(dark))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button(format!("＋  {}", self.t("Add files", "添加文件")))
                                .clicked()
                            {
                                if let Some(files) = rfd::FileDialog::new().pick_files() {
                                    self.add_inputs(files);
                                }
                            }
                            if ui
                                .button(format!("▣  {}", self.t("Add folder", "添加文件夹")))
                                .clicked()
                            {
                                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                    match formflux_core::io::list_files(&folder, false, true) {
                                        Ok(files) => self.add_inputs(files),
                                        Err(error) => self.status = error.to_string(),
                                    }
                                }
                            }
                            if ui.button(self.t("Clear", "清空")).clicked() {
                                self.inputs.clear();
                                self.refresh_preview();
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("📁").clicked() {
                                        let mut dialog = rfd::FileDialog::new();
                                        if let Some(dir) = self.config.ui.last_output_dir.clone() {
                                            dialog = dialog.set_directory(dir);
                                        }
                                        if let Some(folder) = dialog.pick_folder() {
                                            self.output_dir = folder.display().to_string();
                                            self.config.ui.last_output_dir = Some(folder);
                                            let _ = self.config.save_default();
                                        }
                                    }
                                    let output_hint = lang.pick("Output folder", "输出目录");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.output_dir)
                                            .desired_width(300.0)
                                            .hint_text(output_hint),
                                    );
                                    ui.label(
                                        egui::RichText::new(self.t("Output", "输出"))
                                            .small()
                                            .weak(),
                                    );
                                },
                            );
                        });
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(&self.status)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    });

                ui.add_space(10.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Frame::new()
                        .fill(card_fill(dark))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(self.t("Inputs", "输入")).heading());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(self.inputs.len().to_string())
                                                .small()
                                                .weak(),
                                        );
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            if self.inputs.is_empty() {
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 154.0),
                                    egui::Sense::click(),
                                );
                                let hovered = response.hovered();
                                let fill = if hovered {
                                    ACCENT.gamma_multiply(0.14)
                                } else {
                                    subtle_fill(dark)
                                };
                                ui.painter().rect_filled(rect, 9.0, fill);
                                dashed_rect(
                                    ui.painter(),
                                    rect.shrink(1.0),
                                    if hovered {
                                        ACCENT_SOFT
                                    } else {
                                        ui.visuals().weak_text_color()
                                    },
                                );
                                ui.painter().text(
                                    rect.center() - egui::vec2(0.0, 22.0),
                                    egui::Align2::CENTER_CENTER,
                                    "＋",
                                    egui::FontId::proportional(30.0),
                                    if hovered {
                                        ACCENT_SOFT
                                    } else {
                                        ui.visuals().weak_text_color()
                                    },
                                );
                                ui.painter().text(
                                    rect.center() + egui::vec2(0.0, 18.0),
                                    egui::Align2::CENTER_CENTER,
                                    self.t("Drop files here", "把文件拖到这里"),
                                    egui::FontId::proportional(14.0),
                                    ui.visuals().text_color(),
                                );
                                ui.painter().text(
                                    rect.center() + egui::vec2(0.0, 40.0),
                                    egui::Align2::CENTER_CENTER,
                                    self.t(
                                        "or click to browse · drag folders",
                                        "或点击选择文件 · 可拖入文件夹",
                                    ),
                                    egui::FontId::proportional(11.0),
                                    ui.visuals().weak_text_color(),
                                );
                                if response
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                                        self.add_inputs(files);
                                    }
                                }
                            }
                            for path in self.inputs.iter().take(40) {
                                let name = path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("file");
                                egui::Frame::new()
                                    .fill(subtle_fill(dark))
                                    .corner_radius(6)
                                    .inner_margin(egui::Margin::symmetric(8, 6))
                                    .show(ui, |ui| {
                                        ui.set_min_width(250.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(file_icon(name))
                                                    .color(ACCENT_SOFT),
                                            );
                                            ui.label(
                                                egui::RichText::new(shorten_middle(name, 34))
                                                    .small(),
                                            )
                                            .on_hover_text(path.display().to_string());
                                        });
                                    });
                            }
                            if self.inputs.len() > 40 {
                                ui.weak(format!(
                                    "… {} {}",
                                    self.inputs.len() - 40,
                                    self.t("more", "个更多")
                                ));
                            }
                        });

                    ui.add_space(10.0);
                    egui::Frame::new()
                        .fill(card_fill(dark))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            ui.set_width(width);
                            match self.tab {
                                Tab::Media => self.media_tab(ui),
                                Tab::Image => self.image_tab(ui),
                                Tab::Document => self.document_tab(ui),
                                Tab::Hash => self.hash_tab(ui),
                                Tab::Color => self.color_tab(ui),
                                Tab::Tools => self.tools_tab(ui),
                                Tab::Data => self.data_tab(ui),
                                Tab::Generate => self.generate_tab(ui),
                                Tab::Time => self.time_tab(ui),
                            }
                        });
                });
            });

        self.about_window(ctx);
        self.settings_window(ctx);
    }
}

fn task_icon(state: &TaskState) -> &'static str {
    match state {
        TaskState::Queued => "◷",
        TaskState::Running => "▶",
        TaskState::Done => "✓",
        TaskState::Failed => "!",
        TaskState::Cancelled => "×",
    }
}

fn task_color(state: &TaskState) -> egui::Color32 {
    match state {
        TaskState::Queued => egui::Color32::GRAY,
        TaskState::Running => ACCENT_SOFT,
        TaskState::Done => egui::Color32::from_rgb(60, 179, 113),
        TaskState::Failed => DANGER,
        TaskState::Cancelled => egui::Color32::from_rgb(200, 160, 60),
    }
}

fn task_state_label(lang: Language, state: &TaskState) -> &'static str {
    match state {
        TaskState::Queued => lang.pick("queued", "排队中"),
        TaskState::Running => lang.pick("running", "处理中"),
        TaskState::Done => lang.pick("done", "已完成"),
        TaskState::Failed => lang.pick("failed", "失败"),
        TaskState::Cancelled => lang.pick("cancelled", "已取消"),
    }
}

fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "webm" | "mkv" | "mov" | "avi" | "gif" => "▶",
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tiff" | "ico" => "◈",
        "md" | "html" | "txt" | "csv" | "tsv" => "¶",
        "json" | "yaml" | "yml" | "xml" => "{}",
        "ttf" | "otf" => "A",
        _ => "•",
    }
}

fn shorten_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max / 2 - 1).collect();
    let tail: String = text.chars().skip(count - (max / 2 - 1)).collect();
    format!("{head}…{tail}")
}

impl App {
    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.about_open;
        let lang = self.lang;
        egui::Window::new(lang.pick("About Formflux", "关于 Formflux"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.vertical_centered(|ui| {
                    egui::Frame::new()
                        .fill(ACCENT)
                        .corner_radius(12)
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("F")
                                    .heading()
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );
                        });
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Formflux").heading().strong());
                    ui.label(
                        egui::RichText::new(format!("v{} · MIT", formflux_shared::VERSION)).weak(),
                    );
                });
                ui.add_space(8.0);
                ui.separator();
                egui::Grid::new("about-grid")
                    .num_columns(2)
                    .spacing([14.0, 7.0])
                    .show(ui, |ui| {
                        ui.label(lang.pick("Author", "作者"));
                        ui.label(egui::RichText::new(formflux_shared::AUTHOR).strong());
                        ui.end_row();
                        ui.label(lang.pick("Repository", "仓库"));
                        ui.hyperlink_to(formflux_shared::REPOSITORY, formflux_shared::REPOSITORY);
                        ui.end_row();
                        ui.label(lang.pick("License", "许可证"));
                        ui.label(formflux_shared::LICENSE);
                        ui.end_row();
                        ui.label(lang.pick("Network", "网络"));
                        ui.label(lang.pick("100% offline · no telemetry", "100% 离线 · 无遥测"));
                        ui.end_row();
                    });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(lang.pick(
                        "Offline media, image, document, hash, colour, data and developer tools.",
                        "离线多媒体、图像、文档、哈希、颜色、数据与开发者工具箱。",
                    ))
                    .small()
                    .weak(),
                );
            });
        self.about_open = open;
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let lang = self.lang;
        let mut jobs = if self.config.runtime.jobs == 0 {
            self.config.effective_jobs()
        } else {
            self.config.runtime.jobs
        };
        let mut changed = false;
        egui::Window::new(lang.pick("Settings", "设置"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(330.0);
                ui.label(lang.pick("Concurrent jobs", "并发任务数"));
                changed = ui
                    .add(egui::Slider::new(&mut jobs, 1..=64).show_value(true))
                    .changed();
                ui.label(
                    egui::RichText::new(lang.pick(
                        "Applies to newly started tasks; restart to change queue capacity.",
                        "对新任务生效；调整队列容量后需重启。",
                    ))
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.keep_aspect,
                    lang.pick("Lock image aspect ratio", "锁定图像宽高比"),
                );
                ui.separator();
                ui.label(lang.pick("Configuration", "配置文件"));
                ui.monospace(
                    AppConfig::default_path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "unknown".to_string()),
                );
            });
        if changed {
            self.config.runtime.jobs = jobs;
            self.jobs = jobs;
            let _ = self.config.save_default();
        }
        self.settings_open = open;
    }
}

impl App {
    fn media_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.heading(lang.pick("Media conversion", "媒体转换"));
        egui::ComboBox::from_label(lang.pick("Format", "格式"))
            .selected_text(&self.media_format)
            .show_ui(ui, |ui| {
                for format in ["mp4", "webm", "mkv", "mov", "mp3", "flac", "wav", "ogg"] {
                    ui.selectable_value(&mut self.media_format, format.to_string(), format);
                }
            });
        ui.checkbox(
            &mut self.media_stream_copy,
            lang.pick("Stream copy (no re-encode)", "流复制（不重新编码）"),
        );
        ui.add(egui::Slider::new(&mut self.media_crf, 0..=51).text("CRF"));
        if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
        self.tasks.push(self.runner.spawn(
            self.lang.pick("Media convert", "媒体转换"),
            cancel,
            future,
        ));
        self.status = self
            .lang
            .pick("Media conversion queued.", "媒体转换已排队。")
            .to_string();
    }

    fn image_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.heading(lang.pick("Image processing", "图像处理"));
        egui::ComboBox::from_label(lang.pick("Output format", "输出格式"))
            .selected_text(if self.image_format.is_empty() {
                lang.pick("same as input", "与输入相同")
            } else {
                &self.image_format
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.image_format,
                    String::new(),
                    lang.pick("same as input", "与输入相同"),
                );
                for format in ["jpg", "png", "webp", "bmp", "tiff", "ico", "gif"] {
                    ui.selectable_value(&mut self.image_format, format.to_string(), format);
                }
            });
        ui.add(
            egui::Slider::new(&mut self.image_quality, 1..=100).text(lang.pick("Quality", "质量")),
        );
        ui.horizontal(|ui| {
            ui.label(lang.pick("Width", "宽度"));
            let auto_label = lang.pick("Auto", "自动").to_owned();
            ui.add(
                egui::DragValue::new(&mut self.image_width)
                    .range(0..=16384)
                    .custom_formatter(|n, _| {
                        if n == 0.0 {
                            auto_label.clone()
                        } else {
                            format!("{n:.0}")
                        }
                    }),
            );
            ui.label(lang.pick("Height", "高度"));
            ui.add(
                egui::DragValue::new(&mut self.image_height)
                    .range(0..=16384)
                    .custom_formatter(|n, _| {
                        if n == 0.0 {
                            auto_label.clone()
                        } else {
                            format!("{n:.0}")
                        }
                    }),
            );
            ui.label(if self.keep_aspect { "🔒" } else { "🔓" })
                .on_hover_text(lang.pick(
                    "Aspect lock is configured in Settings.",
                    "宽高比锁定可在“设置”中调整。",
                ));
        });
        if primary_button(ui, lang.pick("Convert / compress", "转换 / 压缩")).clicked() {
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
                    ui.weak(lang.pick(
                        "Preview skipped for a large file.",
                        "文件过大，已跳过预览。",
                    ));
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
        let lang = self.lang;
        ui.heading(lang.pick("Document conversion", "文档转换"));
        egui::ComboBox::from_label(lang.pick("Target", "目标格式"))
            .selected_text(&self.text_target)
            .show_ui(ui, |ui| {
                for target in ["md", "html", "txt"] {
                    ui.selectable_value(&mut self.text_target, target.to_string(), target);
                }
            });
        if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
        ui.heading(lang.pick("Offline OCR", "离线 OCR"));
        ui.label(lang.pick(
            "Uses the first input image and an external Tesseract install.",
            "使用第一个输入图像和外部 Tesseract。",
        ));
        ui.horizontal(|ui| {
            ui.label(lang.pick("Language", "语言"));
            ui.add(egui::TextEdit::singleline(&mut self.ocr_lang).desired_width(80.0));
            ui.label("PSM");
            ui.add(egui::DragValue::new(&mut self.ocr_psm).range(0..=13));
            if ui.button(lang.pick("Recognize text", "识别文字")).clicked() {
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
                self.tasks
                    .push(self.runner.spawn(lang.pick("OCR", "OCR"), cancel, future));
                self.status = lang.pick("OCR queued.", "OCR 已排队。").to_string();
            }
        });
    }

    fn hash_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.heading(lang.pick("File hashes", "文件哈希"));
        egui::ComboBox::from_label(lang.pick("Algorithm", "算法"))
            .selected_text(&self.hash_algorithm)
            .show_ui(ui, |ui| {
                for algorithm in ["md5", "sha1", "sha256", "sha384", "sha512", "blake2b"] {
                    ui.selectable_value(&mut self.hash_algorithm, algorithm.to_string(), algorithm);
                }
            });
        if primary_button(ui, lang.pick("Hash inputs", "计算哈希")).clicked() {
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
            self.tasks
                .push(self.runner.spawn(lang.pick("Hash", "哈希"), cancel, future));
            self.status = lang
                .pick("Hash task queued.", "哈希任务已排队。")
                .to_string();
        }
    }

    fn color_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.heading(lang.pick("Colour tools", "颜色工具"));
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
        egui::ComboBox::from_label(lang.pick("To", "转换为"))
            .selected_text(&self.color_target)
            .show_ui(ui, |ui| {
                for space in ["hex", "rgb", "rgba", "hsl", "hsla", "cmyk", "lab"] {
                    ui.selectable_value(&mut self.color_target, space.to_string(), space);
                }
            });
        if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
            ui.label(lang.pick("Background", "背景色"));
            ui.add(egui::TextEdit::singleline(&mut self.color_secondary).desired_width(120.0));
            if ui.button(lang.pick("Check", "检查")).clicked() {
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
        let lang = self.lang;
        ui.heading(lang.pick("File toolbox", "文件工具箱"));
        ui.horizontal(|ui| {
            ui.label(lang.pick("Split chunk size", "分卷大小"));
            ui.add(egui::TextEdit::singleline(&mut self.split_size).desired_width(100.0));
            if ui
                .button(lang.pick("Split first input", "分割第一个输入"))
                .clicked()
            {
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
        if ui
            .button(lang.pick("Remove empty directories", "清理空目录"))
            .clicked()
        {
            let dir = self.output_path();
            match formflux_core::tools::clean_empty_dirs(&dir, true) {
                Ok(dirs) => self.status = format!("Would remove {} empty dir(s).", dirs.len()),
                Err(error) => self.status = error.to_string(),
            }
        }
        ui.separator();
        ui.heading(lang.pick("Radix", "进制转换"));
        ui.horizontal(|ui| {
            ui.label(lang.pick("Value", "数值"));
            ui.add(egui::TextEdit::singleline(&mut self.radix_input).desired_width(180.0));
            ui.label(lang.pick("From", "原进制"));
            ui.add(egui::TextEdit::singleline(&mut self.radix_from).desired_width(40.0));
            ui.label(lang.pick("To", "目标进制"));
            ui.add(egui::TextEdit::singleline(&mut self.radix_to).desired_width(40.0));
            if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
        let lang = self.lang;
        ui.heading(lang.pick("Data conversion", "数据转换"));
        ui.label(lang.pick(
            "Uses the first input file and the output directory.",
            "使用第一个输入文件和输出目录。",
        ));
        egui::ComboBox::from_label(lang.pick("Target", "目标格式"))
            .selected_text(&self.data_target)
            .show_ui(ui, |ui| {
                for target in ["json", "yaml", "xml"] {
                    ui.selectable_value(&mut self.data_target, target.to_string(), target);
                }
            });
        if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
        let lang = self.lang;
        ui.heading(lang.pick("Generate", "生成"));
        if ui
            .button(lang.pick("New UUID v4", "生成 UUID v4"))
            .clicked()
        {
            self.result = formflux_core::generate::uuid_v4();
            self.status = self.result.clone();
        }
        if ui
            .button(lang.pick("New UUID v7", "生成 UUID v7"))
            .clicked()
        {
            self.result = formflux_core::generate::uuid_v7();
            self.status = self.result.clone();
        }
        ui.add(
            egui::Slider::new(&mut self.password_length, 8..=128)
                .text(lang.pick("Password length", "密码长度")),
        );
        if ui.button(lang.pick("Password", "密码")).clicked() {
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
        if ui
            .button(lang.pick("QR to output/qr.png", "二维码写入 output/qr.png"))
            .clicked()
        {
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
        let lang = self.lang;
        ui.heading(lang.pick("Time and time zones", "时间与时区"));
        if ui.button(lang.pick("Now", "当前时间")).clicked() {
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
            ui.label(lang.pick("Timestamp", "时间戳"));
            ui.add(egui::TextEdit::singleline(&mut self.time_input).desired_width(160.0));
            egui::ComboBox::from_id_salt("unit")
                .selected_text(&self.time_unit)
                .show_ui(ui, |ui| {
                    for unit in ["s", "ms", "us"] {
                        ui.selectable_value(&mut self.time_unit, unit.to_string(), unit);
                    }
                });
            ui.label(lang.pick("Zone", "时区"));
            ui.add(egui::TextEdit::singleline(&mut self.time_zone).desired_width(140.0));
        });
        if primary_button(ui, lang.pick("Convert", "转换")).clicked() {
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
    let initial_inputs: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    eframe::run_native(
        "Formflux",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, config, initial_inputs)?))),
    )
}

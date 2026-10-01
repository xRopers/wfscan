//! Loading the selected file for the preview pane, off the UI thread. A newer request
//! replaces an older one; a stale result is dropped.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use egui::{ColorImage, TextureHandle};
use wfscan_core::formats::audio::{self, Pcm};
use wfscan_core::formats::model::{self, Model, render::View};
use wfscan_core::formats::texture::{self, Shape};
use wfscan_core::{Cache, ExportError, ExportInput, Kind, Oodle, OodleDecoder, Part, with_input};

use crate::widgets::TextLines;

/// The largest side a texture preview is decoded at.
const TEXTURE_SIZE: u32 = 1024;
/// How much of each part is kept for the hex and text views.
const RAW_BYTES: usize = 4 << 20;
/// How many peaks a sound's waveform is drawn from.
const WAVEFORM_PEAKS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub package: String,
    pub path: String,
    /// Session generation, so a reopened cache reloads.
    pub generation: u64,
}

pub enum Content {
    Texture(ColorImage),
    Model(Arc<Model>),
    /// A decoded sound, and its loudest sample in each stretch for the waveform.
    Audio { pcm: Arc<Pcm>, peaks: Vec<f32> },
}

/// One of a file's parts as it is in the cache once decompressed, for the hex and text
/// views: its first bytes, and how long it really is.
pub struct RawPart {
    pub part: Part,
    pub bytes: Vec<u8>,
    pub text: TextLines,
    pub size: usize,
}

impl RawPart {
    fn new(part: Part, data: &[u8]) -> Self {
        let bytes = data[..data.len().min(RAW_BYTES)].to_vec();
        Self { part, text: TextLines::new(&bytes, RAW_BYTES), bytes, size: data.len() }
    }

    pub fn truncated(&self) -> bool {
        self.size > self.bytes.len()
    }
}

pub struct Loaded {
    /// One line about what the file is.
    pub summary: String,
    /// The common header's attribute text.
    pub attributes: String,
    /// What the file is as a picture, a model or a sound; `None` for a kind with no
    /// viewer, an error for a file its viewer couldn't read.
    pub content: Option<Result<Content, String>>,
    /// The parts the file has, in H, B, F order (a part that can't be read is left out).
    pub raw: Vec<RawPart>,
}

#[derive(Default)]
pub struct Previewer {
    pending: Option<(Key, Receiver<Result<Loaded, String>>)>,
    pub key: Option<Key>,
    pub current: Option<Result<Loaded, String>>,
    pub texture: Option<TextureHandle>,
    /// The view and size the model picture in `texture` was drawn with.
    pub drawn: Option<(View, [usize; 2])>,
    pub view: View,
}

impl Previewer {
    /// Load a file unless it is already shown or loading.
    pub fn request(&mut self, key: Key, cache: Arc<Cache>, oodle: Option<Arc<Oodle>>, kind: Option<Kind>, wake: impl Fn() + Send + 'static) {
        if self.key.as_ref() == Some(&key) || self.pending.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (package, path) = (key.package.clone(), key.path.clone());
        std::thread::spawn(move || {
            let decoder = oodle.as_deref().map(|o| o as &dyn OodleDecoder);
            let loaded = with_input(&cache, &package, &path, decoder, |input| load(input, kind)).map_err(|e| e.to_string());
            let _ = tx.send(loaded);
            wake();
        });
        self.pending = Some((key, rx));
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Take a finished load, if there is one.
    pub fn poll(&mut self) {
        let Some((_, rx)) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the preview failed unexpectedly".into()),
        };
        let (key, _) = self.pending.take().expect("checked above");
        self.key = Some(key);
        self.current = Some(result);
        self.texture = None;
        self.drawn = None;
        self.view = View::default();
    }
}

/// `1:05.3`, or `1:02:03` from an hour up.
pub fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    let (h, m, s) = (tenths / 36000, tenths / 600 % 60, tenths / 10 % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}.{}", tenths % 10) }
}

fn reason(e: ExportError) -> String {
    match e {
        ExportError::Unsupported(reason) | ExportError::Failed(reason) => reason,
    }
}

fn load(input: &ExportInput, kind: Option<Kind>) -> Loaded {
    let attributes = input.header.attributes.clone();
    let (summary, content) = match kind {
        Some(Kind::Texture) => match texture::read(input) {
            Ok(t) => {
                let shape = match t.shape {
                    Shape::Flat => String::new(),
                    Shape::Cube => ", cube map (first face shown)".into(),
                    Shape::Array(n) => format!(", array of {n} (first layer shown)"),
                    Shape::Volume(n) => format!(", volume of {n} slices (first shown)"),
                };
                let summary = format!("{}×{} {:?}, {} mips{shape}", t.width, t.height, t.format, t.mips);
                let image = texture::decode_rgba(&t, 0, TEXTURE_SIZE)
                    .map(|rgba| Content::Texture(ColorImage::from_rgba_unmultiplied([rgba.width as usize, rgba.height as usize], &rgba.pixels)));
                (summary, Some(image))
            }
            Err(e) => ("texture".to_string(), Some(Err(reason(e)))),
        },
        Some(Kind::Model) => match model::read(input) {
            Ok(m) => {
                let bones = m.skeleton.as_ref().map_or(String::new(), |s| format!(", {} bones", s.bones.len()));
                let summary = format!("{} vertices, {} triangles, {} meshes{bones}", m.positions.len(), m.triangle_count(), m.meshes.len());
                (summary, Some(Ok(Content::Model(Arc::new(m)))))
            }
            Err(e) => ("model".to_string(), Some(Err(reason(e)))),
        },
        Some(Kind::Audio) => match audio::read(input) {
            Ok(sound) => {
                let channels = match sound.channels {
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n} channels"),
                };
                let summary = format!("{}, {channels}, {} Hz, {}", sound.codec.name(), sound.sample_rate, clock(sound.seconds()));
                let content = audio::decode(&sound, None).map(|pcm| Content::Audio { peaks: pcm.peaks(WAVEFORM_PEAKS), pcm: Arc::new(pcm) });
                (summary, Some(content))
            }
            Err(e) => ("sound".to_string(), Some(Err(reason(e)))),
        },
        other => {
            let what = other.map_or_else(|| format!("type {}", input.type_id()), |k| format!("{k} (type {})", input.type_id()));
            (format!("{what}: no viewer yet"), None)
        }
    };
    let mut raw = vec![RawPart::new(Part::H, input.h)];
    for part in [Part::B, Part::F] {
        if input.has(part)
            && let Ok(data) = input.read(part)
        {
            raw.push(RawPart::new(part, &data));
        }
    }
    Loaded { summary, attributes, content, raw }
}

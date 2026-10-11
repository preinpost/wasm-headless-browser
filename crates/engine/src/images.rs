//! Images: fetched, measured and decoded for layout and screenshots.
//!
//! A page's images are requested when its load finishes, all at once, as a
//! browser fetches them while it loads; nothing waits for them then. Layout
//! waits when it needs an `<img>`'s intrinsic size, and painting when it
//! needs the pixels, each for at most [`WAIT`]. An image that arrives later
//! bumps [`ImageStore::generation`], which is part of the layout cache's key,
//! so the next layout uses it.
//!
//! PNG, JPEG, GIF (its first frame) and WebP are decoded, and SVG is drawn
//! by resvg (svg.rs) at twice its own size, so it stays sharp when a page
//! scales it up a little.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::fetch::{self, Client, Request};
use crate::network::ResourceType;

/// The longest layout or painting waits for images still on their way.
pub const WAIT: Duration = Duration::from_secs(5);

/// Decoded pixels kept, in bytes, before the oldest are dropped. They are
/// decoded again from the bytes when needed.
const DECODED_BUDGET: usize = 128 * 1024 * 1024;

/// The largest image decoded, in pixels: a 8K frame. Larger ones are left
/// undecoded rather than risk the memory.
const MAX_PIXELS: u64 = 8192 * 4320;

/// Straight-alpha RGBA, row by row.
pub struct Image {
    /// The pixels' size.
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The size the image has of its own, in CSS pixels: the pixels' for a
    /// raster image, the SVG's own for a drawn one.
    pub natural: (f64, f64),
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

enum Entry {
    Loading(fetch::Pending),
    /// The response body and its dimensions, or `None` when it could not be
    /// fetched or read.
    Loaded(Option<Loaded>),
}

struct Loaded {
    bytes: Arc<Vec<u8>>,
    size: (u32, u32),
    decoded: Option<Arc<Image>>,
    /// When it was last decoded, for dropping the oldest.
    used: u64,
}

/// One target's images, by absolute URL.
pub struct ImageStore {
    client: Client,
    entries: Mutex<HashMap<String, Entry>>,
    generation: AtomicU64,
    clock: AtomicU64,
}

impl std::fmt::Debug for ImageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageStore {{ {} images }}", self.entries().len())
    }
}

impl ImageStore {
    pub fn new(client: Client) -> Self {
        ImageStore { client, entries: Mutex::new(HashMap::new()), generation: AtomicU64::new(0), clock: AtomicU64::new(0) }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Forgets every image, when the document that used them is gone.
    pub fn clear(&self) {
        self.entries().clear();
    }

    /// Counts images that have arrived, so a layout made before one did is
    /// not reused after.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Starts fetching `url` unless it was already asked for. `data:` URLs
    /// are read at once.
    pub fn request(&self, url: &str) {
        let mut entries = self.entries();
        if !entries.contains_key(url) {
            let pending = self.client.start(Request::get(url).of_kind(ResourceType::Image));
            entries.insert(url.to_string(), Entry::Loading(pending));
        }
    }

    /// The intrinsic size of `url`, fetching it if nobody has, and waiting
    /// until `deadline` if it is on its way.
    pub fn size(&self, url: &str, deadline: Instant) -> Option<(u32, u32)> {
        self.request(url);
        self.settle(url, deadline);
        match self.entries().get(url) {
            Some(Entry::Loaded(Some(loaded))) => Some(loaded.size),
            _ => None,
        }
    }

    /// Where `url` stands, without waiting: `None` while it is on its way,
    /// then its size, or `Some(None)` when it failed or is not an image.
    pub fn state(&self, url: &str) -> Option<Option<(u32, u32)>> {
        self.settle(url, Instant::now());
        match self.entries().get(url)? {
            Entry::Loading(_) => None,
            Entry::Loaded(loaded) => Some(loaded.as_ref().map(|l| l.size)),
        }
    }

    /// Waits up to [`WAIT`] for `url` and gives its size.
    pub fn wait(&self, url: &str) -> Option<(u32, u32)> {
        self.size(url, Instant::now() + WAIT)
    }

    /// Requests every one of `urls`, then waits for them together, until one
    /// deadline [`WAIT`] away.
    pub fn wait_all(&self, urls: &[String]) {
        for url in urls {
            self.request(url);
        }
        let deadline = Instant::now() + WAIT;
        for url in urls {
            self.settle(url, deadline);
        }
    }

    /// The pixels of `url`, decoded once and kept.
    pub fn pixels(&self, url: &str, deadline: Instant) -> Option<Arc<Image>> {
        self.request(url);
        self.settle(url, deadline);
        let bytes = {
            let mut entries = self.entries();
            let Some(Entry::Loaded(Some(loaded))) = entries.get_mut(url) else { return None };
            loaded.used = self.clock.fetch_add(1, Ordering::Relaxed);
            if let Some(image) = &loaded.decoded {
                return Some(Arc::clone(image));
            }
            Arc::clone(&loaded.bytes)
        };
        // Decoded without the lock: a large JPEG takes tens of milliseconds.
        let image = Arc::new(decode(&bytes)?);
        let mut entries = self.entries();
        if let Some(Entry::Loaded(Some(loaded))) = entries.get_mut(url) {
            loaded.decoded = Some(Arc::clone(&image));
        }
        evict(&mut entries);
        Some(image)
    }

    /// Turns a finished fetch into dimensions, waiting for it until
    /// `deadline`.
    fn settle(&self, url: &str, deadline: Instant) {
        let pending = {
            let mut entries = self.entries();
            match entries.remove(url) {
                Some(Entry::Loading(pending)) => pending,
                Some(other) => {
                    entries.insert(url.to_string(), other);
                    return;
                }
                None => return,
            }
        };
        let left = deadline.saturating_duration_since(Instant::now());
        let loaded = match pending.wait_for(left) {
            Ok(result) => Entry::Loaded(result.ok().filter(|r| (200..300).contains(&r.status)).and_then(|r| {
                let size = dimensions(&r.body)?;
                Some(Loaded { bytes: Arc::new(r.body), size, decoded: None, used: 0 })
            })),
            // Still on its way: put it back for the next caller.
            Err(pending) => Entry::Loading(pending),
        };
        let arrived = matches!(loaded, Entry::Loaded(_));
        self.entries().insert(url.to_string(), loaded);
        if arrived {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Drops the least recently used decoded pixels until they fit the budget.
fn evict(entries: &mut HashMap<String, Entry>) {
    loop {
        let mut total = 0;
        let mut oldest: Option<(&String, u64)> = None;
        for (url, entry) in entries.iter() {
            if let Entry::Loaded(Some(Loaded { decoded: Some(image), used, .. })) = entry {
                total += image.rgba.len();
                if oldest.is_none_or(|(_, u)| *used < u) {
                    oldest = Some((url, *used));
                }
            }
        }
        if total <= DECODED_BUDGET {
            return;
        }
        let Some((url, _)) = oldest else { return };
        let url = url.clone();
        if let Some(Entry::Loaded(Some(loaded))) = entries.get_mut(&url) {
            loaded.decoded = None;
        }
    }
}

fn reader(bytes: &[u8]) -> Option<image::ImageReader<Cursor<&[u8]>>> {
    image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()
}

/// Width and height from the header alone.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if crate::svg::is_svg(bytes) {
        return crate::svg::size(bytes);
    }
    reader(bytes)?.into_dimensions().ok()
}

/// The largest side an SVG is drawn at.
const MAX_SVG_SIDE: u32 = 4096;

pub fn decode(bytes: &[u8]) -> Option<Image> {
    let (width, height) = dimensions(bytes)?;
    if crate::svg::is_svg(bytes) {
        let scale = (2.0f64).min(MAX_SVG_SIDE as f64 / width.max(height) as f64);
        let side = |v: u32| ((v as f64 * scale).round() as u32).max(1);
        return crate::svg::rasterize(bytes, side(width), side(height));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return None;
    }
    let rgba = reader(bytes)?.decode().ok()?.into_rgba8();
    let natural = (rgba.width() as f64, rgba.height() as f64);
    Some(Image { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw(), natural })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 PNG, red then half-transparent blue, made with image's encoder.
    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        let pixels = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        pixels.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn decodes_png_and_jpeg() {
        let image = decode(&png()).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(image.pixel(1, 0), [0, 0, 255, 128]);

        let mut jpeg = Vec::new();
        let gray = image::RgbImage::from_pixel(16, 8, image::Rgb([200, 200, 200]));
        gray.write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        assert_eq!(dimensions(&jpeg), Some((16, 8)));
        let decoded = decode(&jpeg).unwrap();
        assert!(decoded.pixel(3, 3)[0].abs_diff(200) < 4);
        let svg = decode(b"<svg xmlns='http://www.w3.org/2000/svg' width='3' height='2'/>").unwrap();
        assert_eq!((svg.natural, svg.width), ((3.0, 2.0), 6), "drawn at twice its size");
    }

    #[test]
    fn data_urls_load_without_the_network() {
        use base64::Engine as _;
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png()));
        let client = Client::new(Arc::new(Mutex::new(crate::cookies::CookieJar::new())));
        let store = ImageStore::new(client);
        let deadline = Instant::now() + WAIT;
        assert_eq!(store.size(&url, deadline), Some((2, 1)));
        assert_eq!(store.generation(), 1);
        assert_eq!(store.pixels(&url, deadline).unwrap().pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(store.size("data:text/plain,not an image", deadline), None);
    }
}

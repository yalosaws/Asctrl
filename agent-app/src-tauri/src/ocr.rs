//! Reading text off the screen with the OCR engine built into Windows 10/11 (Windows.Media.Ocr), for programs
//! whose controls UI Automation can't see: games, canvases, remote desktops, many web and Electron pages.
//! Coordinates are physical screen pixels.

use crate::capture::{self, Area};
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

pub struct Word {
    pub text: String,
    pub rect: (i32, i32, i32, i32),
}

pub struct Line {
    pub words: Vec<Word>,
}

impl Line {
    /// The line as text: Latin words separated by spaces, CJK characters run together
    pub fn text(&self) -> String {
        let mut s = String::new();
        for w in &self.words {
            let cjk = |c: Option<char>| c.is_some_and(|c| c as u32 >= 0x2E80);
            if !s.is_empty() && !(cjk(s.chars().last()) && cjk(w.text.chars().next())) {
                s.push(' ');
            }
            s.push_str(&w.text);
        }
        s
    }

    pub fn rect(&self) -> (i32, i32, i32, i32) {
        union(self.words.iter().map(|w| w.rect))
    }
}

fn union(rs: impl Iterator<Item = (i32, i32, i32, i32)>) -> (i32, i32, i32, i32) {
    rs.fold((i32::MAX, i32::MAX, i32::MIN, i32::MIN), |a, r| (a.0.min(r.0), a.1.min(r.1), a.2.max(r.2), a.3.max(r.3)))
}

fn engine() -> Result<OcrEngine, String> {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // Fails (a null engine) when none of the user's languages has an OCR pack installed
    OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| format!("Windows OCR is not available for this user's languages: {}", e.message()))
}

/// Read every line of text in `area`
pub fn read(area: Area) -> Result<Vec<Line>, String> {
    let engine = engine()?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(10000) as i32;
    // Small regions are enlarged: the engine reads text best at 20+ pixels tall
    let up = if (area.width as i64 * area.height as i64) < 900 * 700 { 2.0 } else { 1.0 };
    let k = (up as f64).min(max as f64 / area.width.max(area.height) as f64);
    let (w, h) = (((area.width as f64 * k) as i32).max(1), ((area.height as f64 * k) as i32).max(1));
    let px = capture::pixels(area, w, h, true)?;
    let ocr = |e: windows::core::Error| format!("OCR failed: {}", e.message());
    let writer = DataWriter::new().map_err(ocr)?;
    writer.WriteBytes(&px).map_err(ocr)?;
    let buf = writer.DetachBuffer().map_err(ocr)?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, w, h).map_err(ocr)?;
    let result = engine.RecognizeAsync(&bmp).map_err(ocr)?.join().map_err(ocr)?;
    let to = |x: f32, o: i32| o + (x as f64 / k).round() as i32;
    let mut out = vec![];
    for line in result.Lines().map_err(ocr)? {
        let mut words = vec![];
        for word in line.Words().map_err(ocr)? {
            let r = word.BoundingRect().map_err(ocr)?;
            let text = word.Text().map_err(ocr)?.to_string();
            words.push(Word { text, rect: (to(r.X, area.left), to(r.Y, area.top), to(r.X + r.Width, area.left), to(r.Y + r.Height, area.top)) });
        }
        if !words.is_empty() {
            out.push(Line { words });
        }
    }
    Ok(out)
}

/// Lower-case, without white space: how text is compared, so "Save  As" matches "save as" and spaced-out CJK matches
fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

pub struct Hit {
    pub rect: (i32, i32, i32, i32),
    pub line: String,
    /// The whole line is the query (a button label), not just part of a sentence
    pub exact: bool,
}

/// Where `query` appears, top to bottom; the box covers just the matching words
pub fn find(lines: &[Line], query: &str) -> Vec<Hit> {
    let q = norm(query);
    if q.is_empty() {
        return vec![];
    }
    let qn = q.chars().count();
    let mut hits = vec![];
    for line in lines {
        // Each normalised character, with the word it came from
        let mut chars: Vec<char> = vec![];
        let mut owner: Vec<usize> = vec![];
        for (i, w) in line.words.iter().enumerate() {
            for c in norm(&w.text).chars() {
                chars.push(c);
                owner.push(i);
            }
        }
        let qc: Vec<char> = q.chars().collect();
        let mut s = 0;
        while s + qn <= chars.len() {
            if chars[s..s + qn] == qc[..] {
                let (a, b) = (owner[s], owner[s + qn - 1]);
                hits.push(Hit { rect: union(line.words[a..=b].iter().map(|w| w.rect)), line: line.text(), exact: qn == chars.len() });
                s += qn;
            } else {
                s += 1;
            }
        }
    }
    // Reading order: top to bottom, then left to right
    hits.sort_by_key(|h| (h.rect.1 / 8, h.rect.0));
    hits
}

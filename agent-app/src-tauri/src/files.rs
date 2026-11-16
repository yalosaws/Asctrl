//! Remote file read, write, edit, directory listing, find by name, and search by content.
//! Encoding is detected automatically: UTF-8 (with or without BOM), UTF-16, otherwise the system ANSI code page.
//! Writes keep the original encoding.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar, WideCharToMultiByte};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;

const MAX_READ: u64 = 20 * 1024 * 1024;
const MAX_SEARCH_FILE: u64 = 10 * 1024 * 1024;
const MAX_LINE: usize = 2000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Enc {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Ansi,
}

impl Enc {
    fn name(self) -> &'static str {
        match self {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 BOM",
            Enc::Utf16Le => "UTF-16 LE",
            Enc::Utf16Be => "UTF-16 BE",
            Enc::Ansi => "ANSI (system code page)",
        }
    }

    pub fn parse(s: &str) -> Result<Option<Enc>, String> {
        Ok(Some(match s.to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "" => return Ok(None),
            "utf8" => Enc::Utf8,
            "utf8bom" => Enc::Utf8Bom,
            "utf16" | "utf16le" | "unicode" => Enc::Utf16Le,
            "ansi" | "gbk" | "default" => Enc::Ansi,
            other => return Err(format!("Unsupported encoding: {other} (use utf8 / utf8bom / utf16le / ansi)")),
        }))
    }
}

fn ansi_decode(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, None);
        let mut w = vec![0u16; n.max(0) as usize];
        MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, Some(&mut w));
        String::from_utf16_lossy(&w)
    }
}

/// Fails when the text has characters the system code page can't represent: writing them would
/// silently turn them into "?" and lose the user's data.
fn ansi_encode(s: &str) -> Result<Vec<u8>, String> {
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.is_empty() {
        return Ok(vec![]);
    }
    unsafe {
        let mut lossy = windows::core::BOOL(0);
        let n = WideCharToMultiByte(CP_ACP, 0, &w, None, windows::core::PCSTR::null(), None);
        let mut b = vec![0u8; n.max(0) as usize];
        WideCharToMultiByte(CP_ACP, 0, &w, Some(&mut b), windows::core::PCSTR::null(), Some(&mut lossy));
        if lossy.as_bool() {
            return Err("The text has characters the system ANSI code page can't represent; write the file with encoding utf8 instead".into());
        }
        Ok(b)
    }
}

/// Looks like a binary file (has NUL bytes and is not UTF-16)
fn looks_binary(b: &[u8]) -> bool {
    b.iter().take(8192).any(|&c| c == 0)
}

pub fn decode(b: &[u8]) -> Result<(String, Enc), String> {
    if let Some(r) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Ok((String::from_utf8_lossy(r).into_owned(), Enc::Utf8Bom));
    }
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        let le = b[0] == 0xFF;
        let u: Vec<u16> =
            b[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return Ok((String::from_utf16_lossy(&u), if le { Enc::Utf16Le } else { Enc::Utf16Be }));
    }
    if looks_binary(b) {
        return Err("This is a binary file and can't be read as text. Use download_file to fetch it if needed".into());
    }
    match std::str::from_utf8(b) {
        Ok(s) => Ok((s.to_string(), Enc::Utf8)),
        Err(_) => Ok((ansi_decode(b), Enc::Ansi)),
    }
}

pub fn encode(s: &str, enc: Enc) -> Result<Vec<u8>, String> {
    Ok(match enc {
        Enc::Utf8 => s.as_bytes().to_vec(),
        Enc::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat(),
        Enc::Utf16Le => [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_le_bytes())).collect(),
        Enc::Utf16Be => [0xFE, 0xFF].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_be_bytes())).collect(),
        Enc::Ansi => ansi_encode(s)?,
    })
}

/// Encoding of an existing file, judged from its first 64 KB (enough to tell, and a multi-gigabyte
/// log is not read into memory just to be overwritten). None when it is missing, empty or binary.
fn sniff_enc(path: &Path) -> Option<Enc> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut head).ok()?;
    if head.is_empty() {
        return None;
    }
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(Enc::Utf8Bom);
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return Some(Enc::Utf16Le);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Some(Enc::Utf16Be);
    }
    if looks_binary(&head) {
        return None;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => Some(Enc::Utf8),
        // A multi-byte character cut off by the 64 KB boundary is not an encoding error
        Err(e) if e.error_len().is_none() => Some(Enc::Utf8),
        Err(_) => Some(Enc::Ansi),
    }
}

fn read_text(path: &Path) -> Result<(String, Enc), String> {
    let m = std::fs::metadata(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    if m.is_dir() {
        return Err(format!("{} is a folder; use list_files", path.display()));
    }
    if m.len() > MAX_READ {
        return Err(format!("File is too large ({} MB); use search_files to search it or download_file to fetch it", m.len() / 1024 / 1024));
    }
    let b = std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    decode(&b)
}

pub fn read(path: &Path, offset: usize, limit: usize) -> Result<String, String> {
    let (text, enc) = read_text(path)?;
    let lines: Vec<&str> = text.split('\n').collect();
    let total = if text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
    let start = offset.max(1);
    let mut out = format!("{} ({}, {total} lines)\n", path.display(), enc.name());
    if total == 0 {
        out.push_str("(empty file)");
        return Ok(out);
    }
    if start > total {
        return Err(format!("offset {start} is past the end of the file ({total} lines)"));
    }
    let end = (start - 1 + limit).min(total);
    for (i, l) in lines[start - 1..end].iter().enumerate() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        let shown: String = l.chars().take(MAX_LINE).collect();
        let cut = if l.chars().count() > MAX_LINE { "… (line truncated)" } else { "" };
        let _ = writeln!(out, "{:>6}\t{shown}{cut}", start + i);
    }
    if end < total {
        let _ = write!(out, "… {} more lines; continue with offset={}", total - end, end + 1);
    }
    Ok(out)
}

pub fn write(path: &Path, content: &str, enc: Option<Enc>) -> Result<String, String> {
    // No encoding given: existing files keep theirs, new files use UTF-8
    let enc = match enc {
        Some(e) => e,
        None => sniff_enc(path).unwrap_or(Enc::Utf8),
    };
    let bytes = encode(content, enc)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, &bytes).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(format!("Wrote {} ({}, {} bytes)", path.display(), enc.name(), bytes.len()))
}

pub fn edit(path: &Path, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("oldText must not be empty".into());
    }
    if old == new {
        return Err("oldText and newText are identical; nothing to change".into());
    }
    let (text, enc) = read_text(path)?;
    // If the file uses CRLF but the input uses \n, convert to \r\n
    let crlf = text.contains("\r\n");
    let fix = |s: &str| if crlf && !s.contains('\r') { s.replace('\n', "\r\n") } else { s.to_string() };
    let (old, new) = (fix(old), fix(new));
    let n = text.matches(&old).count();
    if n == 0 {
        return Err("oldText was not found in the file (it must match exactly, including spaces and indentation). Check with read_file first".into());
    }
    if n > 1 && !replace_all {
        return Err(format!("oldText appears {n} times in the file. Include more context to make it unique, or pass replaceAll: true"));
    }
    let out = if replace_all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    std::fs::write(path, encode(&out, enc)?).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(format!("Edited {}: {} replacement(s) ({})", path.display(), if replace_all { n } else { 1 }, enc.name()))
}

fn fmt_time(t: SystemTime) -> String {
    let Ok(d) = t.duration_since(UNIX_EPOCH) else { return String::new() };
    let ticks = (d.as_secs() + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut local = FILETIME::default();
    let mut st = SYSTEMTIME::default();
    unsafe {
        if FileTimeToLocalFileTime(&ft, &mut local).is_err() || FileTimeToSystemTime(&local, &mut st).is_err() {
            return String::new();
        }
    }
    format!("{}-{:02}-{:02} {:02}:{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute)
}

fn fmt_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

pub fn list_dir(path: &Path) -> Result<String, String> {
    let rd = std::fs::read_dir(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut items: Vec<(bool, String, u64, String)> = rd
        .filter_map(Result::ok)
        .map(|e| {
            let m = e.metadata().ok();
            let is_dir = m.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
            let time = m.and_then(|m| m.modified().ok()).map(fmt_time).unwrap_or_default();
            (is_dir, e.file_name().to_string_lossy().into_owned(), size, time)
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let total = items.len();
    let mut out = format!("{} ({total} items)\n", path.display());
    for (is_dir, name, size, time) in items.iter().take(1000) {
        let s = if *is_dir { "<DIR>".to_string() } else { fmt_size(*size) };
        let _ = writeln!(out, "{time:<16}  {s:>10}  {name}{}", if *is_dir { "\\" } else { "" });
    }
    if total > 1000 {
        let _ = write!(out, "… showing the first 1000 items only");
    }
    Ok(out)
}

/// Wildcard match (* and ?), case-insensitive
fn wildcard(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

pub struct Matcher(Vec<Vec<char>>);

impl Matcher {
    /// Separate multiple patterns with ; or , e.g. "*.log;*.txt"
    pub fn new(pattern: &str) -> Matcher {
        Matcher(
            pattern
                .split([';', ','])
                .map(|p| p.trim().to_lowercase())
                .filter(|p| !p.is_empty())
                .map(|p| p.chars().collect())
                .collect(),
        )
    }

    pub fn is_match(&self, name: &str) -> bool {
        if self.0.is_empty() {
            return true;
        }
        let n: Vec<char> = name.to_lowercase().chars().collect();
        self.0.iter().any(|p| wildcard(p, &n))
    }
}

/// Walk files recursively (skipping symlinks / junctions to avoid loops).
/// Returns false once the callback asked to stop, so the stop unwinds through every level:
/// otherwise each parent folder would carry on with its remaining entries and the whole tree
/// would still be walked after the result limit was reached.
fn walk(root: &Path, depth: usize, f: &mut dyn FnMut(&Path, &std::fs::Metadata) -> bool) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else { return true };
    for e in rd.filter_map(Result::ok) {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() {
            if depth > 0 && !walk(&p, depth - 1, f) {
                return false;
            }
        } else if let Ok(m) = e.metadata()
            && !f(&p, &m) {
                return false;
            }
    }
    true
}

pub fn find(root: &Path, pattern: &str, max: usize) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let m = Matcher::new(pattern);
    let mut hits: Vec<(PathBuf, u64, SystemTime)> = vec![];
    let mut more = false;
    walk(root, 40, &mut |p, meta| {
        if m.is_match(&p.file_name().unwrap_or_default().to_string_lossy()) {
            if hits.len() >= max {
                more = true;
                return false;
            }
            hits.push((p.to_path_buf(), meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)));
        }
        true
    });
    hits.sort_by_key(|h| std::cmp::Reverse(h.2));
    let mut out = format!("Found {}{} under {} (newest first)\n", hits.len(), if more { "+" } else { "" }, root.display());
    for (p, size, t) in &hits {
        let _ = writeln!(out, "{:<16}  {:>10}  {}", fmt_time(*t), fmt_size(*size), p.display());
    }
    if more {
        let _ = write!(out, "… too many results; showing {max}. Narrow the search or raise maxResults");
    }
    Ok(out)
}

// ---------------- Disk usage ----------------

/// A full scan of a big drive takes about a minute; stop well before the technician's call times out
const DU_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(300);
/// OneDrive "online-only" placeholders report their full size but take no space on disk
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;

struct DuDir {
    path: PathBuf,
    size: u64,
    files: u64,
    /// Largest subfolders only (kept down to the requested depth)
    children: Vec<DuDir>,
    /// Subfolders left out of `children`: (count, total size)
    rest: (usize, u64),
}

struct DuScan {
    top_files: std::collections::BinaryHeap<std::cmp::Reverse<(u64, PathBuf)>>,
    max_files: usize,
    denied: u64,
    denied_eg: Vec<PathBuf>,
    cloud: u64,
    deadline: std::time::Instant,
    timed_out: bool,
}

fn du_dir(path: &Path, depth: usize, per_level: usize, s: &mut DuScan) -> DuDir {
    use std::os::windows::fs::MetadataExt;
    let mut d = DuDir { path: path.to_path_buf(), size: 0, files: 0, children: vec![], rest: (0, 0) };
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => {
            s.denied += 1;
            if s.denied_eg.len() < 5 {
                s.denied_eg.push(path.to_path_buf());
            }
            return d;
        }
    };
    let mut subs = vec![];
    for e in rd.filter_map(Result::ok) {
        if s.timed_out || std::time::Instant::now() > s.deadline {
            s.timed_out = true;
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        // Junctions and symlinks point at folders counted elsewhere (or on another drive)
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let c = du_dir(&e.path(), depth.saturating_sub(1), per_level, s);
            d.size += c.size;
            d.files += c.files;
            subs.push(c);
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        if m.file_attributes() & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_OFFLINE) != 0 {
            s.cloud += m.len();
            continue;
        }
        d.size += m.len();
        d.files += 1;
        if s.max_files > 0 {
            s.top_files.push(std::cmp::Reverse((m.len(), e.path())));
            if s.top_files.len() > s.max_files {
                s.top_files.pop();
            }
        }
    }
    if depth > 0 {
        subs.sort_by_key(|s| std::cmp::Reverse(s.size));
        for c in subs.drain(per_level.min(subs.len())..) {
            d.rest.0 += 1;
            d.rest.1 += c.size;
        }
        d.children = subs;
    }
    d
}

fn dir_name(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with('\\') { s } else { s + "\\" }
}

fn du_write(out: &mut String, d: &DuDir, indent: usize) {
    for c in &d.children {
        let pct = if d.size > 0 { c.size as f64 * 100.0 / d.size as f64 } else { 0.0 };
        let _ = writeln!(out, "{:indent$}{:>10}  {pct:>3.0}%  {}", "", fmt_size(c.size), dir_name(&c.path), indent = indent);
        du_write(out, c, indent + 2);
    }
    if d.rest.0 > 0 {
        let _ = writeln!(out, "{:indent$}{:>10}        ({} smaller folders)", "", fmt_size(d.rest.1), d.rest.0, indent = indent);
    }
}

/// Where the space under a folder goes: the largest subfolders down to `depth` levels, and the largest files
pub fn disk_usage(root: &Path, depth: usize, per_level: usize, max_files: usize) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    std::fs::read_dir(root).map_err(|e| format!("Failed to open {}: {e}", root.display()))?;
    let started = std::time::Instant::now();
    let mut s = DuScan {
        top_files: Default::default(),
        max_files,
        denied: 0,
        denied_eg: vec![],
        cloud: 0,
        deadline: started + DU_TIME_LIMIT,
        timed_out: false,
    };
    let d = du_dir(root, depth, per_level, &mut s);

    let mut out = format!(
        "{} — {} in {} files (scanned in {:.0} s)\n",
        dir_name(root),
        fmt_size(d.size),
        d.files,
        started.elapsed().as_secs_f64().ceil()
    );
    if s.timed_out {
        let _ = writeln!(out, "⚠ Stopped after {} s: the totals below are incomplete. Scan a subfolder for exact numbers.", DU_TIME_LIMIT.as_secs());
    }
    if depth > 0 {
        let _ = writeln!(out, "\nLargest folders (size, share of the parent):");
        du_write(&mut out, &d, 0);
    }
    if max_files > 0 {
        let _ = writeln!(out, "\nLargest files:");
        for std::cmp::Reverse((size, p)) in s.top_files.into_sorted_vec() {
            let t = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()).map(fmt_time).unwrap_or_default();
            let _ = writeln!(out, "{:>10}  {t:<16}  {}", fmt_size(size), p.display());
        }
    }
    if s.cloud > 0 {
        let _ = writeln!(out, "\nNot counted: {} of online-only cloud files (OneDrive etc.) that take no disk space.", fmt_size(s.cloud));
    }
    if s.denied > 0 {
        let eg: Vec<String> = s.denied_eg.iter().map(|p| p.display().to_string()).collect();
        let _ = writeln!(
            out,
            "\n{} folders could not be read, so the totals are too low (e.g. {}).{}",
            s.denied,
            eg.join("; "),
            if crate::is_elevated() { "" } else { " Call request_admin and scan again for a complete picture." }
        );
    }
    Ok(out)
}

pub fn search(root: &Path, pattern: &str, glob: &str, ignore_case: bool, max: usize, context: usize) -> Result<String, String> {
    let re = regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build().map_err(|e| format!("Invalid regular expression: {e}"))?;
    let m = Matcher::new(glob);
    let mut out = String::new();
    let (mut hits, mut files, mut more) = (0usize, 0usize, false);
    let mut scan = |p: &Path, meta: &std::fs::Metadata| -> bool {
        if meta.len() > MAX_SEARCH_FILE || !m.is_match(&p.file_name().unwrap_or_default().to_string_lossy()) {
            return true;
        }
        let Ok(b) = std::fs::read(p) else { return true };
        let Ok((text, _)) = decode(&b) else { return true };
        let lines: Vec<&str> = text.lines().collect();
        let mut first = true;
        let mut last_shown = 0usize;
        for (i, l) in lines.iter().enumerate() {
            if !re.is_match(l) {
                continue;
            }
            if hits >= max {
                more = true;
                return false;
            }
            if first {
                files += 1;
                let _ = writeln!(out, "{}", p.display());
                first = false;
            }
            hits += 1;
            let from = i.saturating_sub(context).max(last_shown);
            let to = (i + context + 1).min(lines.len());
            for (j, line) in lines.iter().enumerate().take(to).skip(from) {
                let shown: String = line.chars().take(400).collect();
                let _ = writeln!(out, "{:>6}{} {shown}", j + 1, if j == i { ":" } else { "-" });
            }
            last_shown = to;
        }
        true
    };
    match std::fs::metadata(root) {
        Ok(meta) if meta.is_file() => {
            scan(root, &meta);
        }
        Ok(_) => {
            walk(root, 40, &mut scan);
        }
        Err(e) => return Err(format!("Failed to open {}: {e}", root.display())),
    }
    if hits == 0 {
        return Ok(format!("No matches for \"{pattern}\" under {}", root.display()));
    }
    let head = format!("{hits} matches in {files} files{}\n", if more { " (limit reached; there are more)" } else { "" });
    Ok(head + &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_match() {
        let m = Matcher::new("*.log;a?c.txt");
        assert!(m.is_match("Setup.LOG"));
        assert!(m.is_match("abc.txt"));
        assert!(!m.is_match("abcd.txt"));
        assert!(Matcher::new("").is_match("x"));
    }

    #[test]
    fn walk_stops_everywhere_once_asked() {
        let root = std::env::temp_dir().join(format!("asctrl-walk-{}", std::process::id()));
        for d in ["a", "b", "c"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            for f in ["1.txt", "2.txt"] {
                std::fs::write(root.join(d).join(f), "x").unwrap();
            }
        }
        let mut seen = 0;
        let finished = walk(&root, 40, &mut |_, _| {
            seen += 1;
            seen < 2
        });
        let _ = std::fs::remove_dir_all(&root);
        assert!(!finished);
        assert_eq!(seen, 2, "the callback must not run again after it asked to stop");
    }

    #[test]
    fn sniff_matches_decode() {
        let dir = std::env::temp_dir().join(format!("asctrl-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.txt");
        for enc in [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be] {
            std::fs::write(&p, encode("你好\r\nabc", enc).unwrap()).unwrap();
            assert_eq!(sniff_enc(&p), Some(enc));
        }
        // A 3-byte character straddling the 64 KB boundary must still read as UTF-8
        std::fs::write(&p, format!("{}你好", "a".repeat(64 * 1024 - 1))).unwrap();
        assert_eq!(sniff_enc(&p), Some(Enc::Utf8));
        std::fs::write(&p, [0x61, 0xD6, 0xD0, 0xCE, 0xC4]).unwrap(); // GBK bytes, invalid as UTF-8
        assert_eq!(sniff_enc(&p), Some(Enc::Ansi));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encodings_roundtrip() {
        for enc in [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be] {
            let (s, e) = decode(&encode("你好\r\nabc", enc).unwrap()).unwrap();
            assert_eq!((s.as_str(), e), ("你好\r\nabc", enc));
        }
    }
}

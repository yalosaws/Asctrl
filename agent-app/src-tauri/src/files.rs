//! 远程文件的读、写、改、列目录、按名字找、按内容搜。
//! 自动识别编码：UTF-8（带不带 BOM）、UTF-16，否则当作系统 ANSI 编码（中文系统是 GBK）。
//! 写回时保持原来的编码。

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
            Enc::Ansi => "ANSI(GBK)",
        }
    }

    pub fn parse(s: &str) -> Result<Option<Enc>, String> {
        Ok(Some(match s.to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "" => return Ok(None),
            "utf8" => Enc::Utf8,
            "utf8bom" => Enc::Utf8Bom,
            "utf16" | "utf16le" | "unicode" => Enc::Utf16Le,
            "ansi" | "gbk" | "default" => Enc::Ansi,
            other => return Err(format!("不支持的编码：{other}（可选 utf8 / utf8bom / utf16le / ansi）")),
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

fn ansi_encode(s: &str) -> Vec<u8> {
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.is_empty() {
        return vec![];
    }
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, 0, &w, None, windows::core::PCSTR::null(), None);
        let mut b = vec![0u8; n.max(0) as usize];
        WideCharToMultiByte(CP_ACP, 0, &w, Some(&mut b), windows::core::PCSTR::null(), None);
        b
    }
}

/// 看起来像二进制文件（有 NUL 字节且不是 UTF-16）
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
            b[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return Ok((String::from_utf16_lossy(&u), if le { Enc::Utf16Le } else { Enc::Utf16Be }));
    }
    if looks_binary(b) {
        return Err("这是二进制文件，不能当文本读。需要的话用 download_file 取回".into());
    }
    match std::str::from_utf8(b) {
        Ok(s) => Ok((s.to_string(), Enc::Utf8)),
        Err(_) => Ok((ansi_decode(b), Enc::Ansi)),
    }
}

pub fn encode(s: &str, enc: Enc) -> Vec<u8> {
    match enc {
        Enc::Utf8 => s.as_bytes().to_vec(),
        Enc::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], s.as_bytes()].concat(),
        Enc::Utf16Le => [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_le_bytes())).collect(),
        Enc::Utf16Be => [0xFE, 0xFF].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_be_bytes())).collect(),
        Enc::Ansi => ansi_encode(s),
    }
}

fn read_text(path: &Path) -> Result<(String, Enc), String> {
    let m = std::fs::metadata(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    if m.is_dir() {
        return Err(format!("{} 是文件夹，用 list_dir 查看", path.display()));
    }
    if m.len() > MAX_READ {
        return Err(format!("文件太大（{} MB），用 search_files 搜内容或 download_file 取回", m.len() / 1024 / 1024));
    }
    let b = std::fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    decode(&b)
}

pub fn read(path: &Path, offset: usize, limit: usize) -> Result<String, String> {
    let (text, enc) = read_text(path)?;
    let lines: Vec<&str> = text.split('\n').collect();
    let total = if text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
    let start = offset.max(1);
    let mut out = format!("{}（{}，共 {total} 行）\n", path.display(), enc.name());
    if total == 0 {
        out.push_str("(空文件)");
        return Ok(out);
    }
    if start > total {
        return Err(format!("offset {start} 超过文件总行数 {total}"));
    }
    let end = (start - 1 + limit).min(total);
    for (i, l) in lines[start - 1..end].iter().enumerate() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        let shown: String = l.chars().take(MAX_LINE).collect();
        let cut = if l.chars().count() > MAX_LINE { "…(本行已截断)" } else { "" };
        let _ = writeln!(out, "{:>6}\t{shown}{cut}", start + i);
    }
    if end < total {
        let _ = write!(out, "…还有 {} 行，用 offset={} 继续读", total - end, end + 1);
    }
    Ok(out)
}

pub fn write(path: &Path, content: &str, enc: Option<Enc>) -> Result<String, String> {
    // 不指定编码时：已有文件沿用原编码，新文件用 UTF-8
    let enc = match enc {
        Some(e) => e,
        None => std::fs::read(path).ok().and_then(|b| decode(&b).ok()).map(|(_, e)| e).unwrap_or(Enc::Utf8),
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let bytes = encode(content, enc);
    std::fs::write(path, &bytes).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    Ok(format!("已写入 {}（{}，{} 字节）", path.display(), enc.name(), bytes.len()))
}

pub fn edit(path: &Path, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("oldText 不能为空".into());
    }
    if old == new {
        return Err("oldText 和 newText 一样，没有要改的".into());
    }
    let (text, enc) = read_text(path)?;
    // 文件是 CRLF 换行而传进来的是 \n 时，自动换成 \r\n
    let crlf = text.contains("\r\n");
    let fix = |s: &str| if crlf && !s.contains('\r') { s.replace('\n', "\r\n") } else { s.to_string() };
    let (old, new) = (fix(old), fix(new));
    let n = text.matches(&old).count();
    if n == 0 {
        return Err("文件里找不到 oldText（要和文件内容完全一致，包括空格和缩进）。先 read_file 确认".into());
    }
    if n > 1 && !replace_all {
        return Err(format!("oldText 在文件里出现了 {n} 次。多带几行上下文让它唯一，或者传 replaceAll: true"));
    }
    let out = if replace_all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    std::fs::write(path, encode(&out, enc)).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    Ok(format!("已修改 {}，替换了 {} 处（{}）", path.display(), if replace_all { n } else { 1 }, enc.name()))
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
    let rd = std::fs::read_dir(path).map_err(|e| format!("打开 {} 失败：{e}", path.display()))?;
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
    let mut out = format!("{}（{total} 项）\n", path.display());
    for (is_dir, name, size, time) in items.iter().take(1000) {
        let s = if *is_dir { "<文件夹>".to_string() } else { fmt_size(*size) };
        let _ = writeln!(out, "{time:<16}  {s:>10}  {name}{}", if *is_dir { "\\" } else { "" });
    }
    if total > 1000 {
        let _ = write!(out, "…只显示前 1000 项");
    }
    Ok(out)
}

/// 通配符匹配（* 和 ?），不区分大小写
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
    /// 多个模式用 ; 或 , 分隔，例如 "*.log;*.txt"
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

/// 递归遍历文件（跳过符号链接 / 目录联接，避免死循环）
fn walk(root: &Path, depth: usize, f: &mut dyn FnMut(&Path, &std::fs::Metadata) -> bool) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.filter_map(Result::ok) {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, f);
            }
        } else if let Ok(m) = e.metadata() {
            if !f(&p, &m) {
                return;
            }
        }
    }
}

pub fn find(root: &Path, pattern: &str, max: usize) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("{} 不是文件夹", root.display()));
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
    hits.sort_by(|a, b| b.2.cmp(&a.2));
    let mut out = format!("在 {} 下找到 {} 个{}（按修改时间从新到旧）\n", root.display(), hits.len(), if more { "以上" } else { "" });
    for (p, size, t) in &hits {
        let _ = writeln!(out, "{:<16}  {:>10}  {}", fmt_time(*t), fmt_size(*size), p.display());
    }
    if more {
        let _ = write!(out, "…结果太多，只显示 {max} 个。缩小范围或加大 maxResults");
    }
    Ok(out)
}

pub fn search(root: &Path, pattern: &str, glob: &str, ignore_case: bool, max: usize, context: usize) -> Result<String, String> {
    let re = regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build().map_err(|e| format!("正则表达式有误：{e}"))?;
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
        Ok(_) => walk(root, 40, &mut scan),
        Err(e) => return Err(format!("打开 {} 失败：{e}", root.display())),
    }
    if hits == 0 {
        return Ok(format!("在 {} 下没有找到匹配「{pattern}」的内容", root.display()));
    }
    let head = format!("{hits} 处匹配，{files} 个文件{}\n", if more { "（已到上限，还有更多）" } else { "" });
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
    fn encodings_roundtrip() {
        for enc in [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be] {
            let (s, e) = decode(&encode("你好\r\nabc", enc)).unwrap();
            assert_eq!((s.as_str(), e), ("你好\r\nabc", enc));
        }
    }
}

//! UI Automation: read a window's controls and operate them directly (press a button, fill a box, tick a
//! checkbox, open a list, scroll) without moving the user's mouse or needing a screenshot.
//! Coordinates are physical screen pixels.

use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VariantClear, VariantToBooleanWithDefault, VariantToInt32WithDefault, VariantToStringAlloc};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{BSTR, Interface};

/// UI Automation objects are free-threaded: usable from any thread in the multithreaded apartment
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

struct Entry {
    el: Agile<IUIAutomationElement>,
    rect: RECT,
    /** Short name for the cursor's tag on screen */
    tag: String,
}

static AUTOMATION: Mutex<Option<Agile<IUIAutomation>>> = Mutex::new(None);
/// Elements numbered by the last list_elements; ids index into this
static LISTED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Where the last listed window's operable controls are, for the overlay to frame (screen pixels), and
/// whether that is simply the whole window
static WORK_AREA: Mutex<Option<(RECT, bool)>> = Mutex::new(None);

/// The area holding every operable control of the last listed window: ((left, top, right, bottom), whole window)
pub fn work_area() -> Option<((i32, i32, i32, i32), bool)> {
    WORK_AREA.lock().unwrap().map(|(r, whole)| ((r.left, r.top, r.right, r.bottom), whole))
}

fn automation() -> Result<IUIAutomation, String> {
    // Every thread that touches UI Automation joins the MTA; a thread already in it gets S_FALSE, which is fine
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut g = AUTOMATION.lock().unwrap();
    if let Some(a) = &*g {
        return Ok(a.0.clone());
    }
    let a: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| format!("UI Automation is not available: {e}"))?;
    // A hung program would otherwise block each call for the default 20 seconds or more
    if let Ok(a2) = a.cast::<IUIAutomation2>() {
        unsafe {
            let _ = a2.SetConnectionTimeout(4000);
            let _ = a2.SetTransactionTimeout(6000);
        }
    }
    *g = Some(Agile(a.clone()));
    Ok(a)
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

const TYPES: [&str; 41] = [
    "Button", "Calendar", "CheckBox", "ComboBox", "Edit", "Link", "Image", "ListItem", "List", "Menu", "MenuBar", "MenuItem",
    "ProgressBar", "RadioButton", "ScrollBar", "Slider", "Spinner", "StatusBar", "Tab", "TabItem", "Text", "ToolBar", "ToolTip",
    "Tree", "TreeItem", "Custom", "Group", "Thumb", "DataGrid", "DataItem", "Document", "SplitButton", "Window", "Pane", "Header",
    "HeaderItem", "Table", "TitleBar", "Separator", "SemanticZoom", "AppBar",
];

fn type_name(t: UIA_CONTROLTYPE_ID) -> &'static str {
    TYPES.get((t.0 - 50000) as usize).copied().unwrap_or("Element")
}

/// Properties fetched for the whole tree in one cross-process call
const PROPS: [UIA_PROPERTY_ID; 16] = [
    UIA_NamePropertyId,
    UIA_ControlTypePropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_ValueValuePropertyId,
    UIA_IsScrollPatternAvailablePropertyId,
    UIA_IsTextPatternAvailablePropertyId,
];
const STATE_PROPS: [UIA_PROPERTY_ID; 3] =
    [UIA_ToggleToggleStatePropertyId, UIA_ExpandCollapseExpandCollapseStatePropertyId, UIA_SelectionItemIsSelectedPropertyId];

struct Var(VARIANT);

impl Drop for Var {
    fn drop(&mut self) {
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

fn prop(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<Var> {
    unsafe { el.GetCachedPropertyValue(id) }.ok().map(Var)
}

fn flag(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    prop(el, id).is_some_and(|v| unsafe { VariantToBooleanWithDefault(&v.0, false) }.as_bool())
}

fn int(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    // An unset property comes back as "not supported" (an IUnknown), which the default turns into None
    prop(el, id).map(|v| unsafe { VariantToInt32WithDefault(&v.0, -1) }).filter(|&n| n >= 0)
}

fn string(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> String {
    let Some(v) = prop(el, id) else { return String::new() };
    match unsafe { VariantToStringAlloc(&v.0) } {
        Ok(p) => {
            let s = unsafe { p.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            s
        }
        Err(_) => String::new(),
    }
}

fn clip(s: &str, n: usize) -> String {
    let s = s.replace(['\r', '\n', '\t'], " ");
    let s = s.trim();
    if s.chars().count() > n { s.chars().take(n).collect::<String>() + "…" } else { s.to_string() }
}

struct Info {
    line: String,
    rect: RECT,
    show: bool,
    tag: String,
    /** Something to press, tick, pick, open or fill in (not just text to read or an area to scroll) */
    operable: bool,
    /** The window's own frame (title bar and its buttons), which never counts toward the work area */
    chrome: bool,
}

fn describe(el: &IUIAutomationElement, to_shot: &dyn Fn(i32, i32) -> Option<(i32, i32)>, offscreen: bool) -> Info {
    let ty = unsafe { el.CachedControlType() }.map(type_name).unwrap_or("Element");
    let name = unsafe { el.CachedName() }.map(|b: BSTR| b.to_string()).unwrap_or_default();
    let rect = unsafe { el.CachedBoundingRectangle() }.unwrap_or_default();
    let off = flag(el, UIA_IsOffscreenPropertyId);
    let has_value = flag(el, UIA_IsValuePatternAvailablePropertyId);
    let value = if has_value { string(el, UIA_ValueValuePropertyId) } else { String::new() };

    let mut can = vec![];
    if flag(el, UIA_IsInvokePatternAvailablePropertyId) {
        can.push("click");
    }
    let toggle = flag(el, UIA_IsTogglePatternAvailablePropertyId);
    if toggle {
        can.push("toggle");
    }
    let sel = flag(el, UIA_IsSelectionItemPatternAvailablePropertyId);
    if sel {
        can.push("select");
    }
    let expand = flag(el, UIA_IsExpandCollapsePatternAvailablePropertyId);
    if expand {
        can.push("expand");
    }
    if has_value && !flag(el, UIA_ValueIsReadOnlyPropertyId) {
        can.push("set_value");
    }
    if flag(el, UIA_IsScrollPatternAvailablePropertyId) {
        can.push("scroll");
    }
    if flag(el, UIA_IsTextPatternAvailablePropertyId) {
        can.push("text");
    }

    let mut state = vec![];
    if !flag(el, UIA_IsEnabledPropertyId) {
        state.push("disabled");
    }
    if flag(el, UIA_HasKeyboardFocusPropertyId) {
        state.push("focused");
    }
    if toggle {
        match int(el, UIA_ToggleToggleStatePropertyId) {
            Some(0) => state.push("unchecked"),
            Some(1) => state.push("checked"),
            Some(2) => state.push("mixed"),
            _ => {}
        }
    }
    if expand {
        match int(el, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
            Some(0) => state.push("collapsed"),
            Some(1) | Some(2) => state.push("expanded"),
            _ => {}
        }
    }
    if sel && flag(el, UIA_SelectionItemIsSelectedPropertyId) {
        state.push("selected");
    }
    if off {
        state.push("offscreen");
    }

    let empty = rect.right <= rect.left || rect.bottom <= rect.top;
    let noise = matches!(ty, "ScrollBar" | "Thumb" | "Separator" | "TitleBar");
    let useful = !name.trim().is_empty() || !value.trim().is_empty() || !can.is_empty() || flag(el, UIA_IsKeyboardFocusablePropertyId);
    let show = useful && !noise && !empty && (offscreen || !off);

    let mut line = ty.to_string();
    if !name.trim().is_empty() {
        line += &format!(" \"{}\"", clip(&name, 80));
    }
    if !value.trim().is_empty() && value.trim() != name.trim() {
        line += &format!(" value=\"{}\"", clip(&value, 80));
    }
    if !state.is_empty() {
        line += &format!(" ({})", state.join(", "));
    }
    if !can.is_empty() {
        line += &format!(" {{{}}}", can.join(","));
    }
    if !empty {
        let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
        line += &match to_shot(cx, cy) {
            Some((x, y)) => format!(" @({x},{y})"),
            None => format!(" @screen({cx},{cy})"),
        };
    }
    // The name if it has one, else what it holds, else what kind of control it is
    let tag = [name.trim(), value.trim(), ty].into_iter().find(|s| !s.is_empty()).map(|s| clip(s, 24)).unwrap_or_default();
    let operable = can.iter().any(|c| matches!(*c, "click" | "toggle" | "select" | "expand" | "set_value"));
    Info { line, rect, show, tag, operable, chrome: ty == "TitleBar" }
}

/// Number the useful controls of a window and describe them, one per line, indented by nesting
pub fn list(hwnd: HWND, query: &str, max: usize, offscreen: bool, to_shot: &dyn Fn(i32, i32) -> Option<(i32, i32)>) -> Result<String, String> {
    if hwnd.is_invalid() {
        return Err("No window is in front; pass handle or title".into());
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == std::process::id() {
        return Err("The window in front is the assist app itself; pass handle or title of the window to read".into());
    }
    crate::input::elevation_block(hwnd, "reading and operating its controls")?;
    let auto = automation()?;
    let root = unsafe { auto.ElementFromHandle(hwnd) }.map_err(|e| format!("Cannot read this window: {e}"))?;
    let req = unsafe { auto.CreateCacheRequest() }.map_err(|e| e.to_string())?;
    unsafe {
        for p in PROPS.iter().chain(STATE_PROPS.iter()) {
            let _ = req.AddProperty(*p);
        }
        req.SetTreeScope(TreeScope_Subtree).map_err(|e| e.to_string())?;
        req.SetTreeFilter(&auto.ControlViewCondition().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    let root = unsafe { root.BuildUpdatedCache(&req) }.map_err(|e| format!("Cannot read this window's controls: {e}"))?;
    let title = unsafe { root.CachedName() }.map(|b| b.to_string()).unwrap_or_default();

    let q = query.trim().to_lowercase();
    let mut lines = vec![];
    let mut entries = vec![];
    let mut more = 0usize;
    // The work area: everything operable in the window, title bar aside, kept inside the window
    let bounds = unsafe { root.CachedBoundingRectangle() }.unwrap_or_default();
    let mut work: Option<RECT> = None;
    // Depth-first, children in order; depth counts only the ancestors that were printed
    let mut stack: Vec<(IUIAutomationElement, usize, bool)> = vec![(root, 0, false)];
    while let Some((el, depth, in_chrome)) = stack.pop() {
        let info = describe(&el, to_shot, offscreen);
        let chrome = in_chrome || info.chrome;
        if info.show && info.operable && !chrome && !flag(&el, UIA_IsOffscreenPropertyId) {
            let r = info.rect;
            work = Some(match work {
                None => r,
                Some(w) => RECT { left: w.left.min(r.left), top: w.top.min(r.top), right: w.right.max(r.right), bottom: w.bottom.max(r.bottom) },
            });
        }
        let shown = info.show && (q.is_empty() || info.line.to_lowercase().contains(&q));
        if shown {
            if entries.len() < max {
                let id = entries.len() + 1;
                let indent = if q.is_empty() { "  ".repeat(depth) } else { String::new() };
                lines.push(format!("{indent}[{id}] {}", info.line));
                entries.push(Entry { el: Agile(el.clone()), rect: info.rect, tag: info.tag });
            } else {
                more += 1;
            }
        }
        let next = depth + usize::from(shown && q.is_empty());
        if let Ok(kids) = unsafe { el.GetCachedChildren() } {
            let n = unsafe { kids.Length() }.unwrap_or(0);
            for i in (0..n).rev() {
                if let Ok(k) = unsafe { kids.GetElement(i) } {
                    stack.push((k, next, chrome));
                }
            }
        }
    }
    *LISTED.lock().unwrap() = entries;
    // When the controls fill most of the window, frame the window itself, by its visible edge
    let visible = crate::winmgr::rect_of(hwnd.0 as isize).map(|(left, top, right, bottom)| RECT { left, top, right, bottom }).unwrap_or(bounds);
    *WORK_AREA.lock().unwrap() = work
        .map(|w| RECT { left: w.left.max(visible.left), top: w.top.max(visible.top), right: w.right.min(visible.right), bottom: w.bottom.min(visible.bottom) })
        .filter(|w| w.right > w.left && w.bottom > w.top)
        .map(|w| {
            let (ww, wh) = ((visible.right - visible.left).max(1), (visible.bottom - visible.top).max(1));
            let most = (w.right - w.left) * 100 / ww >= 80 && (w.bottom - w.top) * 100 / wh >= 75;
            if most { (visible, true) } else { (w, false) }
        });

    let mut out = format!(
        "Controls of \"{}\" ({} listed). Operate them with element_action and the id in brackets; @(x,y) is the centre on the last screenshot.\n\
         {{…}} shows what each one supports.",
        clip(&title, 80),
        lines.len()
    );
    if lines.is_empty() {
        out += if q.is_empty() {
            "\n(nothing readable: this window draws its own UI, e.g. a game or a remote desktop. Use screenshot and click instead)"
        } else {
            "\n(no control matches; try a shorter query or none)"
        };
    } else {
        out += "\n";
        out += &lines.join("\n");
    }
    if more > 0 {
        out += &format!("\n… {more} more not shown; narrow it down with query");
    }
    Ok(out)
}

/// Screen rectangle and short name of a listed element, for the on-screen animation
pub fn mark_of(id: usize) -> Option<((i32, i32, i32, i32), String)> {
    let g = LISTED.lock().unwrap();
    let e = g.get(id.checked_sub(1)?)?;
    let r = e.rect;
    Some(((r.left, r.top, r.right, r.bottom), e.tag.clone()))
}

fn pattern<T: Interface>(el: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    unsafe { el.GetCurrentPatternAs::<T>(id) }.ok()
}

fn fail(what: &str, e: windows::core::Error) -> String {
    format!("{what} failed: {}", e.message())
}

/// The nearest element at or above `el` that can scroll
fn scroller(auto: &IUIAutomation, el: &IUIAutomationElement) -> Option<IUIAutomationScrollPattern> {
    let walker = unsafe { auto.ControlViewWalker() }.ok()?;
    let mut cur = el.clone();
    for _ in 0..30 {
        if let Some(p) = pattern::<IUIAutomationScrollPattern>(&cur, UIA_ScrollPatternId) {
            return Some(p);
        }
        cur = unsafe { walker.GetParentElement(&cur) }.ok()?;
    }
    None
}

/// Operate a listed element. action: click (default), toggle, select, expand, collapse, set_value, focus,
/// scroll (value: up/down/left/right, optionally ":page"), scroll_into_view, text
pub fn act(id: usize, action: &str, value: &str) -> Result<String, String> {
    let el = {
        let g = LISTED.lock().unwrap();
        let e = id.checked_sub(1).and_then(|i| g.get(i)).ok_or(format!("No element [{id}]; call list_elements first"))?;
        e.el.0.clone()
    };
    let auto = automation()?;
    // A stale element (its window closed or the control was rebuilt) fails here with a clear message
    unsafe { el.CurrentProcessId() }.map_err(|_| format!("Element [{id}] no longer exists; call list_elements again"))?;
    if let Ok(h) = unsafe { el.CurrentNativeWindowHandle() } {
        if !h.is_invalid() {
            crate::input::elevation_block(h, "operating its controls")?;
        }
    }
    match action {
        "" | "click" | "invoke" => {
            if let Some(p) = pattern::<IUIAutomationInvokePattern>(&el, UIA_InvokePatternId) {
                unsafe { p.Invoke() }.map_err(|e| fail("Click", e))?;
                return Ok(format!("Clicked [{id}]"));
            }
            if let Some(p) = pattern::<IUIAutomationTogglePattern>(&el, UIA_TogglePatternId) {
                unsafe { p.Toggle() }.map_err(|e| fail("Toggle", e))?;
                return Ok(format!("Toggled [{id}]"));
            }
            if let Some(p) = pattern::<IUIAutomationSelectionItemPattern>(&el, UIA_SelectionItemPatternId) {
                unsafe { p.Select() }.map_err(|e| fail("Select", e))?;
                return Ok(format!("Selected [{id}]"));
            }
            if let Some(p) = pattern::<IUIAutomationExpandCollapsePattern>(&el, UIA_ExpandCollapsePatternId) {
                let collapsed = unsafe { p.CurrentExpandCollapseState() }.map(|s| s == ExpandCollapseState_Collapsed).unwrap_or(true);
                if collapsed {
                    unsafe { p.Expand() }.map_err(|e| fail("Expand", e))?;
                    return Ok(format!("Expanded [{id}]"));
                }
                unsafe { p.Collapse() }.map_err(|e| fail("Collapse", e))?;
                return Ok(format!("Collapsed [{id}]"));
            }
            if let Some(p) = pattern::<IUIAutomationLegacyIAccessiblePattern>(&el, UIA_LegacyIAccessiblePatternId) {
                if unsafe { p.CurrentDefaultAction() }.is_ok_and(|a| !a.is_empty()) {
                    unsafe { p.DoDefaultAction() }.map_err(|e| fail("Click", e))?;
                    return Ok(format!("Clicked [{id}] (default action)"));
                }
            }
            Err(format!("[{id}] can't be clicked without the mouse; use click at its @(x,y) instead"))
        }
        "toggle" => {
            let p = pattern::<IUIAutomationTogglePattern>(&el, UIA_TogglePatternId).ok_or(format!("[{id}] is not a checkbox or toggle"))?;
            unsafe { p.Toggle() }.map_err(|e| fail("Toggle", e))?;
            let s = match unsafe { p.CurrentToggleState() } {
                Ok(s) if s == ToggleState_On => "checked",
                Ok(s) if s == ToggleState_Off => "unchecked",
                _ => "mixed",
            };
            Ok(format!("[{id}] is now {s}"))
        }
        "select" => {
            let p = pattern::<IUIAutomationSelectionItemPattern>(&el, UIA_SelectionItemPatternId)
                .ok_or(format!("[{id}] can't be selected (not a list, tab or radio item)"))?;
            unsafe { p.Select() }.map_err(|e| fail("Select", e))?;
            Ok(format!("Selected [{id}]"))
        }
        "expand" | "collapse" => {
            let p = pattern::<IUIAutomationExpandCollapsePattern>(&el, UIA_ExpandCollapsePatternId)
                .ok_or(format!("[{id}] can't be expanded or collapsed"))?;
            if action == "expand" {
                unsafe { p.Expand() }.map_err(|e| fail("Expand", e))?;
            } else {
                unsafe { p.Collapse() }.map_err(|e| fail("Collapse", e))?;
            }
            Ok(format!("{} [{id}]", if action == "expand" { "Expanded" } else { "Collapsed" }))
        }
        "set_value" => {
            if let Some(p) = pattern::<IUIAutomationValuePattern>(&el, UIA_ValuePatternId) {
                unsafe { p.SetValue(&BSTR::from(value)) }.map_err(|e| fail("Setting the value", e))?;
                return Ok(format!("Set [{id}] to \"{}\"", clip(value, 80)));
            }
            if let Some(p) = pattern::<IUIAutomationRangeValuePattern>(&el, UIA_RangeValuePatternId) {
                let n: f64 = value.trim().parse().map_err(|_| format!("[{id}] is a slider; value must be a number"))?;
                unsafe { p.SetValue(n) }.map_err(|e| fail("Setting the value", e))?;
                return Ok(format!("Set [{id}] to {n}"));
            }
            Err(format!("[{id}] doesn't take a value; focus it and use type_text instead"))
        }
        "focus" => {
            unsafe { el.SetFocus() }.map_err(|e| fail("Focus", e))?;
            Ok(format!("Focused [{id}]"))
        }
        "scroll_into_view" => {
            let p = pattern::<IUIAutomationScrollItemPattern>(&el, UIA_ScrollItemPatternId).ok_or(format!("[{id}] can't be scrolled into view"))?;
            unsafe { p.ScrollIntoView() }.map_err(|e| fail("Scroll", e))?;
            Ok(format!("Scrolled [{id}] into view"))
        }
        "scroll" => {
            let p = scroller(&auto, &el).ok_or(format!("Nothing around [{id}] can scroll; use scroll at its @(x,y) instead"))?;
            let v = value.trim().to_lowercase();
            let (dir, page) = match v.split_once(':') {
                Some((d, m)) => (d.to_string(), m == "page"),
                None => (v.clone(), false),
            };
            let (inc, dec) = if page {
                (ScrollAmount_LargeIncrement, ScrollAmount_LargeDecrement)
            } else {
                (ScrollAmount_SmallIncrement, ScrollAmount_SmallDecrement)
            };
            let none = ScrollAmount_NoAmount;
            let (h, vv) = match dir.as_str() {
                "" | "down" => (none, inc),
                "up" => (none, dec),
                "right" => (inc, none),
                "left" => (dec, none),
                other => return Err(format!("Unknown scroll direction: {other} (use up, down, left or right)")),
            };
            unsafe { p.Scroll(h, vv) }.map_err(|e| fail("Scroll", e))?;
            let pos = unsafe { p.CurrentVerticalScrollPercent() }.unwrap_or(-1.0);
            Ok(if pos >= 0.0 { format!("Scrolled; now {pos:.0}% down") } else { "Scrolled".into() })
        }
        "text" => {
            if let Some(p) = pattern::<IUIAutomationTextPattern>(&el, UIA_TextPatternId) {
                let t = unsafe { p.DocumentRange().and_then(|r| r.GetText(50_000)) }.map_err(|e| fail("Reading the text", e))?;
                return Ok(t.to_string());
            }
            let name = unsafe { el.CurrentName() }.map(|b| b.to_string()).unwrap_or_default();
            let value = pattern::<IUIAutomationValuePattern>(&el, UIA_ValuePatternId)
                .and_then(|p| unsafe { p.CurrentValue() }.ok())
                .map(|b| b.to_string())
                .unwrap_or_default();
            Ok(if value.is_empty() || value == name {
                if name.is_empty() { "(no text)".into() } else { name }
            } else if name.is_empty() {
                value
            } else {
                format!("{name}\n{value}")
            })
        }
        other => Err(format!("Unknown action: {other}")),
    }
}

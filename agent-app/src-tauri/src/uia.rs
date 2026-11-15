//! UI Automation: read a window's controls as a scene (a pruned tree of the things a person would point at),
//! give each control a reference that stays the same from one reading to the next, say what changed between
//! two readings, and operate controls directly (press, tick, pick, fill, scroll) without the mouse.
//! Coordinates are physical screen pixels.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{
    VARIANT, VariantClear, VariantGetElementCount, VariantToBooleanWithDefault, VariantToInt32Array, VariantToInt32WithDefault, VariantToStringAlloc,
};
use windows::Win32::UI::Accessibility::*;
use windows::core::{BSTR, Interface};

pub type Rect = (i32, i32, i32, i32);

/// UI Automation objects are free-threaded: usable from any thread in the multithreaded apartment
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

static AUTOMATION: Mutex<Option<Agile<IUIAutomation>>> = Mutex::new(None);

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
            let _ = a2.SetConnectionTimeout(3000);
            let _ = a2.SetTransactionTimeout(5000);
        }
    }
    *g = Some(Agile(a.clone()));
    Ok(a)
}

// ---- What a control is and can do ----

pub mod flag {
    // State
    pub const DISABLED: u32 = 1;
    pub const FOCUSED: u32 = 1 << 1;
    pub const CHECKED: u32 = 1 << 2;
    pub const UNCHECKED: u32 = 1 << 3;
    pub const MIXED: u32 = 1 << 4;
    pub const EXPANDED: u32 = 1 << 5;
    pub const COLLAPSED: u32 = 1 << 6;
    pub const SELECTED: u32 = 1 << 7;
    pub const OFFSCREEN: u32 = 1 << 8;
    pub const READONLY: u32 = 1 << 9;
    pub const PASSWORD: u32 = 1 << 10;
    /// The bits that make up a control's visible state (compared between two readings)
    pub const STATE: u32 = (1 << 11) - 1;
    // Abilities
    pub const INVOKE: u32 = 1 << 12;
    pub const TOGGLE: u32 = 1 << 13;
    pub const SELECT: u32 = 1 << 14;
    pub const EXPAND: u32 = 1 << 15;
    /// Has a value that can be set (a text box, an editable combo)
    pub const VALUE: u32 = 1 << 16;
    pub const RANGE: u32 = 1 << 17;
    pub const SCROLL: u32 = 1 << 18;
    pub const TEXT: u32 = 1 << 19;
    pub const FOCUSABLE: u32 = 1 << 20;
    /// Drawn by a browser engine (Chrome, Edge, Electron, WebView2, Firefox): real mouse clicks are the reliable way in
    pub const WEB: u32 = 1 << 21;
    /// Something a click does something to
    pub const ACTIONABLE: u32 = INVOKE | TOGGLE | SELECT | EXPAND;
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

/// Controls a person operates, shown even when they have no name
fn interactive_role(role: &str) -> bool {
    matches!(
        role,
        "Button" | "CheckBox" | "ComboBox" | "Edit" | "Link" | "ListItem" | "MenuItem" | "RadioButton" | "Slider" | "Spinner" | "TabItem" | "TreeItem" | "SplitButton" | "DataItem"
    )
}

/// One control in a scene
/// Where to point at a control: its middle — except a check box or radio button with a caption beside it,
/// which is aimed at its mark, as a person would. The middle of a wide one is often empty space past the
/// caption, and in some programs that space is not part of the control at all.
pub fn aim(role: &str, (l, t, r, b): Rect) -> (i32, i32) {
    let (w, h) = (r - l, b - t);
    if matches!(role, "CheckBox" | "RadioButton") && h > 0 && w > h * 2 {
        return (l + (h / 3).clamp(4, 10), (t + b) / 2);
    }
    ((l + r) / 2, (t + b) / 2)
}

#[derive(Clone, Debug)]
pub struct Node {
    /// Reference number: the same control keeps it from one reading to the next (printed as e<n>)
    pub rf: u32,
    /// Nesting among the controls shown
    pub depth: usize,
    pub role: &'static str,
    pub name: String,
    pub value: String,
    pub rect: Rect,
    pub flags: u32,
}

impl Node {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    /// Where to point at it
    pub fn center(&self) -> (i32, i32) {
        aim(self.role, self.rect)
    }

    /// Something to press, tick, pick, open or fill in (not just text to read)
    pub fn operable(&self) -> bool {
        self.has(flag::ACTIONABLE | flag::VALUE | flag::RANGE) || interactive_role(self.role)
    }

    pub fn editable(&self) -> bool {
        (self.has(flag::VALUE) && !self.has(flag::READONLY)) || matches!(self.role, "Edit" | "Document")
    }

    fn state_words(&self) -> Vec<&'static str> {
        state_words(self.flags)
    }

    /// `Button "Save"` — how a control is named in messages
    pub fn label(&self) -> String {
        let mut s = self.role.to_string();
        if !self.name.is_empty() {
            s += &format!(" \"{}\"", clip(&self.name, 60));
        }
        s + &format!(" [e{}]", self.rf)
    }

    /// One line of a scene listing: `[e12] Button "Save" (disabled) @(640,480)`
    pub fn line(&self, at: &dyn Fn(i32, i32) -> String) -> String {
        let mut s = format!("[e{}] {}", self.rf, self.role);
        let long = if self.role == "Text" || self.role == "Document" { 160 } else { 80 };
        if !self.name.is_empty() {
            s += &format!(" \"{}\"", clip(&self.name, long));
        }
        if !self.value.is_empty() && self.value != self.name {
            s += &format!(" = \"{}\"", clip(&self.value, long));
        }
        let st = self.state_words();
        if !st.is_empty() {
            s += &format!(" ({})", st.join(", "));
        }
        let (cx, cy) = self.center();
        s + " " + &at(cx, cy)
    }
}

fn state_words(flags: u32) -> Vec<&'static str> {
    let table = [
        (flag::DISABLED, "disabled"),
        (flag::FOCUSED, "focused"),
        (flag::CHECKED, "checked"),
        (flag::UNCHECKED, "unchecked"),
        (flag::MIXED, "mixed"),
        (flag::EXPANDED, "expanded"),
        (flag::COLLAPSED, "collapsed"),
        (flag::SELECTED, "selected"),
        (flag::READONLY, "read-only"),
        (flag::PASSWORD, "password"),
        (flag::OFFSCREEN, "offscreen"),
    ];
    table.iter().filter(|(f, _)| flags & f != 0).map(|(_, w)| *w).collect()
}

/// Line breaks as "\n". Rich-edit controls (Notepad, WordPad, many text boxes) end lines with a lone "\r",
/// which most displays drop, gluing the lines together.
pub fn newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn clip(s: &str, n: usize) -> String {
    let s = s.replace(['\r', '\n', '\t'], " ");
    let s = s.trim();
    if s.chars().count() > n { s.chars().take(n).collect::<String>() + "…" } else { s.to_string() }
}

/// A window's controls at one moment
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub hwnd: isize,
    pub title: String,
    pub nodes: Vec<Node>,
    /// Open menus, drop-downs and other popups of the window that were read with it
    pub popups: usize,
    /// How long the reading took: a slow window is not re-read after every action
    pub cost: Duration,
}

impl Scene {
    /// Little to go on: the program draws its own UI (a game, a canvas, a remote desktop), so pixels say more
    pub fn thin(&self) -> bool {
        self.nodes.iter().filter(|n| n.operable() || !n.name.is_empty()).count() < 4
    }
}

// ---- The registry: references that outlive a single reading ----

struct Entry {
    el: Agile<IUIAutomationElement>,
    hwnd: isize,
    label: String,
}

/// What a control looked like in the reading the AI last saw
#[derive(Clone, PartialEq)]
struct Sig {
    role: &'static str,
    name: String,
    value: String,
    state: u32,
}

struct Known {
    sigs: HashMap<u32, Sig>,
    /// References in listing order, to say what went away
    order: Vec<u32>,
    cost: Duration,
}

#[derive(Default)]
struct Registry {
    next: u32,
    by_key: HashMap<Vec<i32>, u32>,
    entries: HashMap<u32, Entry>,
    keys: HashMap<u32, Vec<i32>>,
    known: HashMap<isize, Known>,
    /// The scene the overlay frames its work area from
    work: Option<(Rect, bool)>,
}

static REG: Mutex<Option<Registry>> = Mutex::new(None);

fn with_reg<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = REG.lock().unwrap();
    f(g.get_or_insert_with(Registry::default))
}

/// More references than this and the oldest are forgotten
const MAX_REFS: usize = 40_000;

impl Registry {
    fn assign(&mut self, key: Vec<i32>, el: &IUIAutomationElement, hwnd: isize) -> u32 {
        let rf = match (!key.is_empty()).then(|| self.by_key.get(&key).copied()).flatten() {
            Some(rf) => rf,
            None => {
                self.next += 1;
                if !key.is_empty() {
                    self.by_key.insert(key.clone(), self.next);
                    self.keys.insert(self.next, key);
                }
                self.next
            }
        };
        self.entries.insert(rf, Entry { el: Agile(el.clone()), hwnd, label: String::new() });
        rf
    }

    fn trim(&mut self) {
        if self.entries.len() <= MAX_REFS {
            return;
        }
        // Controls of windows that are gone first; then the oldest half
        let dead: Vec<u32> = self.entries.iter().filter(|(_, e)| !crate::winmgr::exists(e.hwnd)).map(|(rf, _)| *rf).collect();
        for rf in dead {
            self.drop_ref(rf);
        }
        self.known.retain(|h, _| crate::winmgr::exists(*h));
        if self.entries.len() > MAX_REFS {
            let cut = self.next.saturating_sub((MAX_REFS / 2) as u32);
            let old: Vec<u32> = self.entries.keys().filter(|rf| **rf < cut).copied().collect();
            for rf in old {
                self.drop_ref(rf);
            }
        }
    }

    fn drop_ref(&mut self, rf: u32) {
        self.entries.remove(&rf);
        if let Some(k) = self.keys.remove(&rf) {
            self.by_key.remove(&k);
        }
    }
}

/// Forget every reference and reading (a new session starts from e1)
pub fn reset() {
    *REG.lock().unwrap() = None;
}

/// The area holding every operable control of the last scene read: ((left, top, right, bottom), whole window)
pub fn work_area() -> Option<(Rect, bool)> {
    with_reg(|r| r.work)
}

/// Whether the AI has a reading of this window to compare against, and how long reading it takes
pub fn known_cost(hwnd: isize) -> Option<Duration> {
    with_reg(|r| r.known.get(&hwnd).map(|k| k.cost))
}

// ---- Reading ----

/// Properties fetched for the whole tree in one cross-process call
const PROPS: [UIA_PROPERTY_ID; 25] = [
    UIA_RuntimeIdPropertyId,
    UIA_NamePropertyId,
    UIA_ControlTypePropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_FrameworkIdPropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_IsRangeValuePatternAvailablePropertyId,
    UIA_IsScrollPatternAvailablePropertyId,
    UIA_IsTextPatternAvailablePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_ValueValuePropertyId,
    UIA_RangeValueValuePropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_AutomationIdPropertyId,
];

struct Var(VARIANT);

impl Drop for Var {
    fn drop(&mut self) {
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

fn prop(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<Var> {
    unsafe { el.GetCachedPropertyValue(id) }.ok().map(Var)
}

fn flag_of(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    prop(el, id).is_some_and(|v| unsafe { VariantToBooleanWithDefault(&v.0, false) }.as_bool())
}

fn int(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    // An unset property comes back as "not supported" (an IUnknown), which the default turns into None
    prop(el, id).map(|v| unsafe { VariantToInt32WithDefault(&v.0, -1) }).filter(|&n| n >= 0)
}

fn var_string(v: &Var) -> String {
    match unsafe { VariantToStringAlloc(&v.0) } {
        Ok(p) => {
            let s = unsafe { p.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            s
        }
        Err(_) => String::new(),
    }
}

fn string(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> String {
    prop(el, id).map(|v| var_string(&v)).unwrap_or_default()
}

/// The identity UI Automation gives a control for as long as it exists; empty when the program gives none
fn runtime_id(el: &IUIAutomationElement) -> Vec<i32> {
    let Some(v) = prop(el, UIA_RuntimeIdPropertyId) else { return vec![] };
    let n = unsafe { VariantGetElementCount(&v.0) } as usize;
    if n == 0 || n > 64 {
        return vec![];
    }
    let mut buf = vec![0i32; n];
    let mut got = 0u32;
    match unsafe { VariantToInt32Array(&v.0, &mut buf, &mut got) } {
        Ok(()) => {
            buf.truncate(got as usize);
            buf
        }
        Err(_) => vec![],
    }
}

/// A control as read from the cache, before deciding whether it is worth showing
struct Raw {
    role: &'static str,
    name: String,
    value: String,
    rect: Rect,
    flags: u32,
    key: Vec<i32>,
}

fn read(el: &IUIAutomationElement) -> Raw {
    let role = unsafe { el.CachedControlType() }.map(type_name).unwrap_or("Element");
    let name = unsafe { el.CachedName() }.map(|b: BSTR| b.to_string()).unwrap_or_default().trim().to_string();
    let r: RECT = unsafe { el.CachedBoundingRectangle() }.unwrap_or_default();
    let mut f = 0u32;
    let mut set = |on: bool, bit: u32| {
        if on {
            f |= bit
        }
    };
    let has_value = flag_of(el, UIA_IsValuePatternAvailablePropertyId);
    let password = flag_of(el, UIA_IsPasswordPropertyId);
    let read_only = has_value && flag_of(el, UIA_ValueIsReadOnlyPropertyId);
    let toggle = flag_of(el, UIA_IsTogglePatternAvailablePropertyId);
    let expand = flag_of(el, UIA_IsExpandCollapsePatternAvailablePropertyId);
    let select = flag_of(el, UIA_IsSelectionItemPatternAvailablePropertyId);
    let range = flag_of(el, UIA_IsRangeValuePatternAvailablePropertyId);
    set(!flag_of(el, UIA_IsEnabledPropertyId), flag::DISABLED);
    set(flag_of(el, UIA_HasKeyboardFocusPropertyId), flag::FOCUSED);
    set(flag_of(el, UIA_IsOffscreenPropertyId), flag::OFFSCREEN);
    set(password, flag::PASSWORD);
    set(flag_of(el, UIA_IsInvokePatternAvailablePropertyId), flag::INVOKE);
    set(toggle, flag::TOGGLE);
    set(select, flag::SELECT);
    set(expand, flag::EXPAND);
    set(has_value && !read_only, flag::VALUE);
    // A read-only value on something that is not just a label is worth saying (a locked field)
    set(read_only && matches!(role, "Edit" | "ComboBox" | "Document" | "Spinner"), flag::READONLY);
    set(range, flag::RANGE);
    set(flag_of(el, UIA_IsScrollPatternAvailablePropertyId), flag::SCROLL);
    set(flag_of(el, UIA_IsTextPatternAvailablePropertyId), flag::TEXT);
    set(flag_of(el, UIA_IsKeyboardFocusablePropertyId), flag::FOCUSABLE);
    if toggle {
        match int(el, UIA_ToggleToggleStatePropertyId) {
            Some(0) => f |= flag::UNCHECKED,
            Some(1) => f |= flag::CHECKED,
            Some(2) => f |= flag::MIXED,
            _ => {}
        }
    }
    if expand {
        match int(el, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
            Some(0) => f |= flag::COLLAPSED,
            Some(1) | Some(2) => f |= flag::EXPANDED,
            _ => {}
        }
    }
    if select && flag_of(el, UIA_SelectionItemIsSelectedPropertyId) {
        f |= flag::SELECTED;
    }
    let fw = string(el, UIA_FrameworkIdPropertyId);
    if matches!(fw.as_str(), "Chrome" | "Gecko" | "InternetExplorer" | "MicrosoftEdge") {
        f |= flag::WEB;
    }
    // A password box's value is dots at best; never carry it around
    let value = if password {
        String::new()
    } else if has_value {
        string(el, UIA_ValueValuePropertyId).trim().to_string()
    } else if range {
        prop(el, UIA_RangeValueValuePropertyId).map(|v| var_string(&v)).unwrap_or_default()
    } else {
        String::new()
    };
    Raw { role, name, value, rect: (r.left, r.top, r.right, r.bottom), flags: f, key: runtime_id(el) }
}

/// Whether a control earns a line. Unnamed containers don't (their children move up a level); scroll bars,
/// separators and the window's own title bar are never worth a line.
fn worth_showing(raw: &Raw, parent_name: &str) -> bool {
    let (l, t, r, b) = raw.rect;
    if r <= l || b <= t {
        return false;
    }
    if matches!(raw.role, "ScrollBar" | "Thumb" | "Separator" | "TitleBar" | "ToolTip") {
        return false;
    }
    let labeled = !raw.name.is_empty() || !raw.value.is_empty();
    let actionable = raw.flags & (flag::ACTIONABLE | flag::VALUE | flag::RANGE) != 0;
    // The text inside a button repeats the button
    if !actionable && !raw.name.is_empty() && raw.name == parent_name && matches!(raw.role, "Text" | "Image") {
        return false;
    }
    labeled || actionable || (interactive_role(raw.role) && raw.flags & flag::FOCUSABLE != 0)
}

/// Stop reading a window after this many controls: nothing a person works with has more on screen
const MAX_NODES: usize = 6000;

struct Reader {
    auto: IUIAutomation,
    req: IUIAutomationCacheRequest,
}

impl Reader {
    fn new(offscreen: bool) -> Result<Reader, String> {
        let auto = automation()?;
        let err = |e: windows::core::Error| e.to_string();
        let req = unsafe { auto.CreateCacheRequest() }.map_err(err)?;
        unsafe {
            for p in PROPS.iter() {
                let _ = req.AddProperty(*p);
            }
            req.SetTreeScope(TreeScope_Subtree).map_err(err)?;
            let control = auto.ControlViewCondition().map_err(err)?;
            // What is scrolled out of view is left out by the program itself, before anything crosses over:
            // a long list or web page costs what is on screen, not what it holds
            let filter = if offscreen {
                control
            } else {
                let onscreen = auto.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false)).map_err(err)?;
                auto.CreateAndCondition(&control, &onscreen).map_err(err)?
            };
            req.SetTreeFilter(&filter).map_err(err)?;
        }
        Ok(Reader { auto, req })
    }

    /// Read one top-level window into `nodes` (references not yet assigned)
    fn read_window(&self, hwnd: isize, base_depth: usize, offscreen: bool, out: &mut Vec<(Raw, usize, IUIAutomationElement)>) -> Result<String, String> {
        let root = unsafe { self.auto.ElementFromHandle(HWND(hwnd as *mut _)) }.map_err(|e| format!("Cannot read this window: {}", e.message()))?;
        let root = unsafe { root.BuildUpdatedCache(&self.req) }.map_err(|e| format!("Cannot read this window's controls: {}", e.message()))?;
        let title = unsafe { root.CachedName() }.map(|b| b.to_string()).unwrap_or_default();
        // Depth-first, children in order; depth counts only the ancestors that were shown
        let mut stack: Vec<(IUIAutomationElement, usize, String, bool)> = vec![(root, base_depth, String::new(), true)];
        while let Some((el, depth, parent_name, is_root)) = stack.pop() {
            if out.len() >= MAX_NODES {
                break;
            }
            let raw = read(&el);
            // The window's own frame: its buttons are focus_window's job
            if raw.role == "TitleBar" {
                continue;
            }
            let hidden = !offscreen && raw.flags & flag::OFFSCREEN != 0;
            let shown = !is_root && !hidden && worth_showing(&raw, &parent_name);
            let (next_depth, next_parent) = if shown { (depth + 1, raw.name.clone()) } else { (depth, parent_name) };
            if let Ok(kids) = unsafe { el.GetCachedChildren() } {
                let n = unsafe { kids.Length() }.unwrap_or(0);
                for i in (0..n).rev() {
                    if let Ok(k) = unsafe { kids.GetElement(i) } {
                        stack.push((k, next_depth, next_parent.clone(), false));
                    }
                }
            }
            if shown {
                out.push((raw, depth, el));
            }
        }
        Ok(title)
    }
}

/// Read a window's controls (and those of its open menus and popups). `offscreen` also reads what is
/// scrolled out of view. The scene is not yet "what the AI saw": call `remember` when it is shown.
pub fn snapshot(hwnd: isize, offscreen: bool) -> Result<Scene, String> {
    if hwnd == 0 || !crate::winmgr::exists(hwnd) {
        return Err("No window is in front; say which window".into());
    }
    if crate::winmgr::pid_of(hwnd) == std::process::id() {
        return Err("That window is the assist app itself; say which window to read".into());
    }
    crate::input::elevation_block(HWND(hwnd as *mut _), "reading and operating its controls")?;
    if crate::winmgr::is_minimized(hwnd) {
        return Ok(Scene { hwnd, title: crate::winmgr::title_of(hwnd), ..Default::default() });
    }
    let start = Instant::now();
    let reader = Reader::new(offscreen)?;
    let mut raw: Vec<(Raw, usize, IUIAutomationElement)> = vec![];
    let title = reader.read_window(hwnd, 0, offscreen, &mut raw)?;
    // Popups are read after, one level in, so a listing shows them as their own blocks
    let mut popups = 0;
    for p in crate::winmgr::satellites(hwnd) {
        let before = raw.len();
        if reader.read_window(p, 0, offscreen, &mut raw).is_ok() && raw.len() > before {
            popups += 1;
        }
    }

    // The work area: everything operable in the main window, kept inside the window's visible edge
    let visible = crate::winmgr::rect_of(hwnd);
    let mut work: Option<Rect> = None;
    let nodes: Vec<Node> = with_reg(|reg| {
        let nodes = raw
            .into_iter()
            .map(|(r, depth, el)| {
                let rf = reg.assign(r.key, &el, hwnd);
                let n = Node { rf, depth, role: r.role, name: r.name, value: r.value, rect: r.rect, flags: r.flags };
                if let Some(e) = reg.entries.get_mut(&rf) {
                    e.label = n.label();
                }
                n
            })
            .collect();
        reg.trim();
        nodes
    });
    for n in nodes.iter().filter(|n| n.operable() && !n.has(flag::OFFSCREEN)) {
        let r = n.rect;
        work = Some(match work {
            None => r,
            Some(w) => (w.0.min(r.0), w.1.min(r.1), w.2.max(r.2), w.3.max(r.3)),
        });
    }
    let work = match (work, visible) {
        (Some(w), Some(v)) => {
            let w = (w.0.max(v.0), w.1.max(v.1), w.2.min(v.2), w.3.min(v.3));
            if w.2 <= w.0 || w.3 <= w.1 {
                None
            } else {
                // When the controls fill most of the window, frame the window itself, by its visible edge
                let most = (w.2 - w.0) * 100 / (v.2 - v.0).max(1) >= 80 && (w.3 - w.1) * 100 / (v.3 - v.1).max(1) >= 75;
                Some(if most { (v, true) } else { (w, false) })
            }
        }
        _ => None,
    };
    with_reg(|reg| reg.work = work);
    Ok(Scene { hwnd, title: if title.is_empty() { crate::winmgr::title_of(hwnd) } else { title }, nodes, popups, cost: start.elapsed() })
}

// ---- Showing a scene, and what changed since the last one shown ----

/// How a scene is written out
pub struct Render<'a> {
    /// Only lines containing this (lower-case) text; flat, without nesting
    pub query: &'a str,
    pub max: usize,
    /// How a screen point is written (on the latest screenshot when it is there)
    pub at: &'a dyn Fn(i32, i32) -> String,
}

/// The scene as lines. When it is longer than `max`, plain text goes first (controls matter more), then the tail.
pub fn render(scene: &Scene, r: &Render) -> String {
    let q = r.query.trim().to_lowercase();
    let lines: Vec<(usize, String, bool)> = scene.nodes.iter().map(|n| (n.depth, n.line(r.at), n.operable())).collect();
    if !q.is_empty() {
        let hits: Vec<&String> = lines.iter().map(|l| &l.1).filter(|l| l.to_lowercase().contains(&q)).collect();
        if hits.is_empty() {
            return format!("(no control matches \"{}\"; try a shorter query, or none)", r.query.trim());
        }
        let more = hits.len().saturating_sub(r.max);
        let mut out = hits.iter().take(r.max).map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
        if more > 0 {
            out += &format!("\n… {more} more match; narrow the query");
        }
        return out;
    }
    if lines.is_empty() {
        return "(no readable controls: this window draws its own UI, e.g. a game, a canvas or a remote desktop — work from the screenshot or the text read off the pixels)".into();
    }
    let total = lines.len();
    let (kept, dropped_text): (Vec<&(usize, String, bool)>, usize) = if total > r.max {
        let k: Vec<_> = lines.iter().filter(|l| l.2).collect();
        let d = total - k.len();
        (k, d)
    } else {
        (lines.iter().collect(), 0)
    };
    let more = kept.len().saturating_sub(r.max);
    // Nesting is only kept when nothing was dropped from between the lines
    let mut out = kept.iter().take(r.max).map(|l| if dropped_text == 0 { format!("{}{}", "  ".repeat(l.0.min(12)), l.1) } else { l.1.clone() }).collect::<Vec<_>>().join("\n");
    if dropped_text > 0 {
        out += &format!("\n… {dropped_text} lines of plain text left out to fit; read them with read_text, or list with query");
    }
    if more > 0 {
        out += &format!("\n… {more} more controls; narrow it down with query");
    }
    out
}

/// Record a scene as what the AI now knows about its window; later changes are told against it
pub fn remember(scene: &Scene) {
    let sigs = scene.nodes.iter().map(|n| (n.rf, Sig { role: n.role, name: n.name.clone(), value: n.value.clone(), state: n.flags & flag::STATE })).collect();
    let order = scene.nodes.iter().map(|n| n.rf).collect();
    with_reg(|reg| {
        reg.known.insert(scene.hwnd, Known { sigs, order, cost: scene.cost });
    });
}

/// What differs between the reading the AI last saw of this window and `scene`, as lines; None when the AI
/// has no reading of it. `Some("")` means nothing changed.
pub fn changes(scene: &Scene, max: usize, at: &dyn Fn(i32, i32) -> String) -> Option<String> {
    let (old_sigs, old_order) = with_reg(|reg| reg.known.get(&scene.hwnd).map(|k| (k.sigs.clone(), k.order.clone())))?;
    let now: HashMap<u32, &Node> = scene.nodes.iter().map(|n| (n.rf, n)).collect();
    let mut changed = vec![];
    let mut added: Vec<&Node> = vec![];
    for n in &scene.nodes {
        match old_sigs.get(&n.rf) {
            None => added.push(n),
            Some(o) => {
                let mut what = vec![];
                if o.name != n.name {
                    what.push(format!("now \"{}\"", clip(&n.name, 80)));
                }
                if o.value != n.value {
                    what.push(format!("value \"{}\" → \"{}\"", clip(&o.value, 50), clip(&n.value, 80)));
                }
                let state = n.flags & flag::STATE;
                if o.state != state {
                    let gained = state_words(state & !o.state);
                    let lost = state_words(o.state & !state);
                    // "unchecked → checked" reads better than "+checked −unchecked"
                    what.extend(gained.iter().map(|w| format!("now {w}")));
                    what.extend(lost.iter().filter(|w| !matches!(**w, "checked" | "unchecked" | "mixed" | "expanded" | "collapsed") || gained.is_empty()).map(|w| format!("no longer {w}")));
                }
                if !what.is_empty() {
                    changed.push(format!("~ [e{}] {} \"{}\": {}", n.rf, n.role, clip(if o.name.is_empty() { &n.name } else { &o.name }, 50), what.join(", ")));
                }
            }
        }
    }
    let removed: Vec<(u32, &Sig)> = old_order.iter().filter(|rf| !now.contains_key(rf)).filter_map(|rf| old_sigs.get(rf).map(|s| (*rf, s))).collect();
    if changed.is_empty() && added.is_empty() && removed.is_empty() {
        return Some(String::new());
    }
    // Mostly new: the window shows something else now (another page, another view). List it as it is.
    if !scene.nodes.is_empty() && added.len() * 10 >= scene.nodes.len() * 7 && added.len() > 6 {
        return Some(format!("The window now shows ({} controls):\n{}", scene.nodes.len(), render(scene, &Render { query: "", max, at })));
    }
    let mut out: Vec<String> = vec![];
    let mut room = max;
    for c in changed.iter().take(room) {
        out.push(c.clone());
    }
    if changed.len() > room {
        out.push(format!("… {} more changed", changed.len() - room));
    }
    room = room.saturating_sub(changed.len());
    // New controls keep their nesting, shifted so the shallowest starts at the margin
    let base = added.iter().map(|n| n.depth).min().unwrap_or(0);
    let (shown, text_cut): (Vec<&&Node>, usize) = if added.len() > room {
        let k: Vec<_> = added.iter().filter(|n| n.operable()).collect();
        let cut = added.len() - k.len();
        (k, cut)
    } else {
        (added.iter().collect(), 0)
    };
    for n in shown.iter().take(room) {
        out.push(format!("+ {}{}", "  ".repeat((n.depth - base).min(8)), n.line(at)));
    }
    let hidden = shown.len().saturating_sub(room) + text_cut;
    if hidden > 0 {
        out.push(format!("… {hidden} more new (observe shows everything)"));
    }
    if !removed.is_empty() {
        let names: Vec<String> = removed.iter().take(6).map(|(_, s)| if s.name.is_empty() { s.role.to_string() } else { format!("{} \"{}\"", s.role, clip(&s.name, 30)) }).collect();
        let more = if removed.len() > 6 { format!(", … {} more", removed.len() - 6) } else { String::new() };
        out.push(format!("- gone: {}{more}", names.join(", ")));
    }
    Some(out.join("\n"))
}

// ---- Operating a control by its reference ----

/// A live control, looked up by reference
pub struct Live {
    el: IUIAutomationElement,
    /// The window whose scene it was read in
    pub hwnd: isize,
    pub label: String,
}

/// The control behind a reference. Fails with a clear message when it no longer exists.
pub fn live(rf: u32) -> Result<Live, String> {
    let found = with_reg(|reg| reg.entries.get(&rf).map(|e| (e.el.0.clone(), e.hwnd, e.label.clone())));
    let Some((el, hwnd, label)) = found else {
        return Err(format!("There is no e{rf}. References come from observe, find, and the replies of actions"));
    };
    // A stale element (its window closed, or the control was rebuilt) fails here
    if unsafe { el.CurrentProcessId() }.is_err() {
        return Err(format!("e{rf} ({label}) no longer exists: its window closed or the view changed. Look again with observe"));
    }
    Ok(Live { el, hwnd, label })
}

fn pattern<T: Interface>(el: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    unsafe { el.GetCurrentPatternAs::<T>(id) }.ok()
}

fn fail(what: &str, e: windows::core::Error) -> String {
    format!("{what} failed: {}", e.message())
}

/// Run a UI Automation call that may not return until a dialog it opens is closed (some programs handle
/// the press before answering). After `patience` the call is left to finish on its own and counts as done.
fn patient(patience: Duration, f: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<bool, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let _ = tx.send(f());
    });
    match rx.recv_timeout(patience) {
        Ok(r) => r.map(|_| true),
        Err(_) => Ok(false),
    }
}

impl Live {
    pub fn rect(&self) -> Option<Rect> {
        let r = unsafe { self.el.CurrentBoundingRectangle() }.ok()?;
        (r.right > r.left && r.bottom > r.top).then_some((r.left, r.top, r.right, r.bottom))
    }

    pub fn enabled(&self) -> bool {
        unsafe { self.el.CurrentIsEnabled() }.map(|b| b.as_bool()).unwrap_or(true)
    }

    pub fn offscreen(&self) -> bool {
        unsafe { self.el.CurrentIsOffscreen() }.map(|b| b.as_bool()).unwrap_or(false)
    }

    pub fn role(&self) -> &'static str {
        unsafe { self.el.CurrentControlType() }.map(type_name).unwrap_or("Element")
    }

    pub fn is_password(&self) -> bool {
        unsafe { self.el.CurrentIsPassword() }.map(|b| b.as_bool()).unwrap_or(false)
    }

    pub fn has_focus(&self) -> bool {
        unsafe { self.el.CurrentHasKeyboardFocus() }.map(|b| b.as_bool()).unwrap_or(false)
    }

    /// Drawn by a browser engine, where a real click is the reliable one
    pub fn is_web(&self) -> bool {
        unsafe { self.el.CurrentFrameworkId() }.map(|b| matches!(b.to_string().as_str(), "Chrome" | "Gecko" | "InternetExplorer" | "MicrosoftEdge")).unwrap_or(false)
    }

    /// The top-level window the control is in right now (a popup's own window, when it lives in one)
    pub fn window(&self) -> isize {
        let auto = automation().ok();
        let mut cur = Some(self.el.clone());
        let walker = auto.and_then(|a| unsafe { a.ControlViewWalker() }.ok());
        for _ in 0..40 {
            let Some(el) = cur else { break };
            if let Ok(h) = unsafe { el.CurrentNativeWindowHandle() }
                && !h.is_invalid()
            {
                return crate::winmgr::root_of(h.0 as isize);
            }
            cur = walker.as_ref().and_then(|w| unsafe { w.GetParentElement(&el) }.ok());
        }
        self.hwnd
    }

    /// Bring the control into view if it is scrolled out; true when it could be asked to
    pub fn scroll_into_view(&self) -> bool {
        pattern::<IUIAutomationScrollItemPattern>(&self.el, UIA_ScrollItemPatternId).is_some_and(|p| unsafe { p.ScrollIntoView() }.is_ok())
    }

    /// Press the control the way a click would, through UI Automation: buttons, links and menu items are
    /// invoked; checkboxes toggle; list, tab and radio items are selected; what opens is opened or closed.
    /// Err when it has no such action (the caller clicks with the mouse instead).
    pub fn press(&self) -> Result<String, String> {
        self.guard()?;
        let label = self.label.clone();
        let el = Agile(self.el.clone());
        // What a press means depends on what the control is, in this order
        let kind = if pattern::<IUIAutomationInvokePattern>(&self.el, UIA_InvokePatternId).is_some() {
            0
        } else if pattern::<IUIAutomationTogglePattern>(&self.el, UIA_TogglePatternId).is_some() {
            1
        } else if pattern::<IUIAutomationSelectionItemPattern>(&self.el, UIA_SelectionItemPatternId).is_some() {
            2
        } else if pattern::<IUIAutomationExpandCollapsePattern>(&self.el, UIA_ExpandCollapsePatternId).is_some() {
            3
        } else if pattern::<IUIAutomationLegacyIAccessiblePattern>(&self.el, UIA_LegacyIAccessiblePatternId).is_some_and(|p| unsafe { p.CurrentDefaultAction() }.is_ok_and(|a| !a.is_empty())) {
            4
        } else {
            return Err(format!("{label} has no press action of its own"));
        };
        let returned = patient(Duration::from_millis(1200), move || {
            let el = el;
            let el = &el.0;
            unsafe {
                match kind {
                    0 => pattern::<IUIAutomationInvokePattern>(el, UIA_InvokePatternId).ok_or("gone")?.Invoke().map_err(|e| fail("Pressing it", e)),
                    1 => pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId).ok_or("gone")?.Toggle().map_err(|e| fail("Toggling it", e)),
                    2 => pattern::<IUIAutomationSelectionItemPattern>(el, UIA_SelectionItemPatternId).ok_or("gone")?.Select().map_err(|e| fail("Selecting it", e)),
                    3 => {
                        let p = pattern::<IUIAutomationExpandCollapsePattern>(el, UIA_ExpandCollapsePatternId).ok_or("gone")?;
                        let collapsed = p.CurrentExpandCollapseState().map(|s| s == ExpandCollapseState_Collapsed).unwrap_or(true);
                        if collapsed { p.Expand() } else { p.Collapse() }.map_err(|e| fail("Opening it", e))
                    }
                    _ => pattern::<IUIAutomationLegacyIAccessiblePattern>(el, UIA_LegacyIAccessiblePatternId).ok_or("gone")?.DoDefaultAction().map_err(|e| fail("Pressing it", e)),
                }
            }
        })?;
        let verb = ["Pressed", "Toggled", "Selected", "Opened", "Pressed"][kind];
        Ok(if returned { format!("{verb} {label}") } else { format!("{verb} {label} (the program is still busy with it: a dialog is probably open)") })
    }

    fn guard(&self) -> Result<(), String> {
        if let Ok(h) = unsafe { self.el.CurrentNativeWindowHandle() }
            && !h.is_invalid()
        {
            crate::input::elevation_block(h, "operating its controls")?;
        }
        Ok(())
    }

    pub fn focus(&self) -> Result<(), String> {
        self.guard()?;
        unsafe { self.el.SetFocus() }.map_err(|e| fail("Focusing it", e))
    }

    /// The control's current value, when it has one
    pub fn value(&self) -> Option<String> {
        if self.is_password() {
            return None;
        }
        if let Some(p) = pattern::<IUIAutomationValuePattern>(&self.el, UIA_ValuePatternId) {
            return unsafe { p.CurrentValue() }.ok().map(|b| b.to_string());
        }
        if let Some(p) = pattern::<IUIAutomationRangeValuePattern>(&self.el, UIA_RangeValuePatternId) {
            return unsafe { p.CurrentValue() }.ok().map(|v| format!("{v}"));
        }
        None
    }

    /// The control's full text: a document's or text box's content, else its value, else its name
    pub fn text(&self, max: i32) -> String {
        if let Some(p) = pattern::<IUIAutomationTextPattern>(&self.el, UIA_TextPatternId)
            && let Ok(t) = unsafe { p.DocumentRange().and_then(|r| r.GetText(max)) }
        {
            let t = newlines(&t.to_string());
            if !t.trim().is_empty() {
                return t;
            }
        }
        let name = unsafe { self.el.CurrentName() }.map(|b| b.to_string()).unwrap_or_default();
        match self.value().map(|v| newlines(&v)) {
            Some(v) if !v.is_empty() && v != name => {
                if name.is_empty() {
                    v
                } else {
                    format!("{name}\n{v}")
                }
            }
            _ => name,
        }
    }

    fn toggle_state(&self) -> Option<&'static str> {
        let p = pattern::<IUIAutomationTogglePattern>(&self.el, UIA_TogglePatternId)?;
        Some(match unsafe { p.CurrentToggleState() } {
            Ok(s) if s == ToggleState_On => "checked",
            Ok(s) if s == ToggleState_Off => "unchecked",
            _ => "mixed",
        })
    }

    /// Make the control hold `value`, whatever kind of control it is:
    /// a text box takes the text; a slider or spinner a number; a checkbox or switch on / off; a radio button,
    /// tab or list item "selected"; something that opens "expanded" / "collapsed"; a combo box or list the
    /// name of one of its items. Idempotent: a box already ticked stays ticked.
    /// Err(None) means it takes no value this way and the caller should type into it instead.
    pub fn set(&self, value: &str) -> Result<String, Option<String>> {
        self.guard().map_err(Some)?;
        if !self.enabled() {
            return Err(Some(format!("{} is disabled", self.label)));
        }
        let label = &self.label;
        let v = value.trim();
        let word = v.to_lowercase();
        let yes = matches!(word.as_str(), "on" | "true" | "yes" | "checked" | "check" | "1" | "selected");
        let no = matches!(word.as_str(), "off" | "false" | "no" | "unchecked" | "uncheck" | "0");
        let role = self.role();

        // On / off
        if let Some(p) = pattern::<IUIAutomationTogglePattern>(&self.el, UIA_TogglePatternId)
            && matches!(role, "CheckBox" | "Button" | "MenuItem" | "ListItem" | "TreeItem" | "DataItem" | "Custom" | "RadioButton")
            && (yes || no)
        {
            let want = if yes { "checked" } else { "unchecked" };
            // A three-state box may need two presses to get there
            for _ in 0..3 {
                if self.toggle_state() == Some(want) {
                    return Ok(format!("{label} is {want}"));
                }
                unsafe { p.Toggle() }.map_err(|e| Some(fail("Toggling it", e)))?;
                std::thread::sleep(Duration::from_millis(30));
            }
            return match self.toggle_state() {
                Some(s) if s == want => Ok(format!("{label} is {want}")),
                s => Err(Some(format!("{label} stays {} after toggling", s.unwrap_or("unknown")))),
            };
        }
        // Open / closed
        if matches!(word.as_str(), "expanded" | "expand" | "open" | "collapsed" | "collapse" | "closed")
            && let Some(p) = pattern::<IUIAutomationExpandCollapsePattern>(&self.el, UIA_ExpandCollapsePatternId)
        {
            let open = matches!(word.as_str(), "expanded" | "expand" | "open");
            unsafe { if open { p.Expand() } else { p.Collapse() } }.map_err(|e| Some(fail(if open { "Opening it" } else { "Closing it" }, e)))?;
            return Ok(format!("{label} is {}", if open { "expanded" } else { "collapsed" }));
        }
        // Picked
        if yes
            && let Some(p) = pattern::<IUIAutomationSelectionItemPattern>(&self.el, UIA_SelectionItemPatternId)
        {
            unsafe { p.Select() }.map_err(|e| Some(fail("Selecting it", e)))?;
            return Ok(format!("{label} is selected"));
        }
        // A number
        if let Some(p) = pattern::<IUIAutomationRangeValuePattern>(&self.el, UIA_RangeValuePatternId)
            && pattern::<IUIAutomationValuePattern>(&self.el, UIA_ValuePatternId).is_none()
        {
            let n: f64 = v.parse().map_err(|_| Some(format!("{label} takes a number")))?;
            let (lo, hi) = unsafe { (p.CurrentMinimum().unwrap_or(f64::MIN), p.CurrentMaximum().unwrap_or(f64::MAX)) };
            if n < lo || n > hi {
                return Err(Some(format!("{label} takes {lo} to {hi}")));
            }
            unsafe { p.SetValue(n) }.map_err(|e| Some(fail("Setting the value", e)))?;
            return Ok(format!("{label} is now {}", unsafe { p.CurrentValue() }.unwrap_or(n)));
        }
        // One of its items, by name (a combo box, a list, a tab strip)
        if matches!(role, "ComboBox" | "List" | "Tab" | "Tree" | "Menu" | "MenuBar" | "DataGrid" | "Table" | "ToolBar")
            && let Some(done) = self.pick(v)
        {
            return done.map_err(Some);
        }
        // Text
        if let Some(p) = pattern::<IUIAutomationValuePattern>(&self.el, UIA_ValuePatternId) {
            if unsafe { p.CurrentIsReadOnly() }.map(|b| b.as_bool()).unwrap_or(false) {
                return Err(Some(format!("{label} is read-only")));
            }
            unsafe { p.SetValue(&BSTR::from(value)) }.map_err(|e| Some(fail("Setting the value", e)))?;
            // Read it back: some programs (and many web pages) take the call and ignore it
            if self.is_password() {
                return Ok(format!("Filled {label}"));
            }
            // A web page applies the value a moment after the call returns: read again for a short while
            let same = |a: &str, b: &str| newlines(a).trim_end() == newlines(b).trim_end();
            let started = std::time::Instant::now();
            loop {
                let got = unsafe { p.CurrentValue() }.map(|b| b.to_string()).unwrap_or_default();
                if same(&got, value) {
                    return Ok(format!("{label} now reads \"{}\"", clip(&got, 80)));
                }
                if started.elapsed() >= Duration::from_millis(150) {
                    return Err(None);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        Err(None)
    }

    /// Pick the item named `name` in a container: opened if it has to be, the item selected or pressed, closed
    /// again. None when the container has no such way in.
    fn pick(&self, name: &str) -> Option<Result<String, String>> {
        let auto = automation().ok()?;
        let label = &self.label;
        let expander = pattern::<IUIAutomationExpandCollapsePattern>(&self.el, UIA_ExpandCollapsePatternId);
        let was_closed = expander.as_ref().is_some_and(|p| unsafe { p.CurrentExpandCollapseState() }.is_ok_and(|s| s == ExpandCollapseState_Collapsed));
        if was_closed && let Some(p) = &expander {
            let _ = unsafe { p.Expand() };
            std::thread::sleep(Duration::from_millis(120));
        }
        let want = norm(name);
        let find = || -> Option<(IUIAutomationElement, String)> {
            let all = unsafe { self.el.FindAll(TreeScope_Descendants, &auto.ControlViewCondition().ok()?) }.ok()?;
            let n = unsafe { all.Length() }.unwrap_or(0).min(3000);
            let mut partial = None;
            for i in 0..n {
                let Ok(k) = (unsafe { all.GetElement(i) }) else { continue };
                let kn = unsafe { k.CurrentName() }.map(|b| b.to_string()).unwrap_or_default();
                let knn = norm(&kn);
                if knn.is_empty() {
                    continue;
                }
                if knn == want {
                    return Some((k, kn));
                }
                if partial.is_none() && knn.contains(&want) {
                    partial = Some((k, kn));
                }
            }
            partial
        };
        let mut item = find();
        if item.is_none() && was_closed {
            // The list may still be filling
            std::thread::sleep(Duration::from_millis(250));
            item = find();
        }
        let Some((item, item_name)) = item else {
            if was_closed && let Some(p) = &expander {
                let _ = unsafe { p.Collapse() };
            }
            // No items at all: not a container to pick from this way (an editable combo takes text instead)
            return (expander.is_some() || self.role() != "ComboBox").then(|| Err(format!("{label} has no item \"{name}\"")));
        };
        let r = if let Some(p) = pattern::<IUIAutomationSelectionItemPattern>(&item, UIA_SelectionItemPatternId) {
            unsafe { p.Select() }.map_err(|e| fail("Selecting the item", e))
        } else if let Some(p) = pattern::<IUIAutomationInvokePattern>(&item, UIA_InvokePatternId) {
            unsafe { p.Invoke() }.map_err(|e| fail("Pressing the item", e))
        } else {
            Err(format!("The item \"{item_name}\" of {label} can't be picked without the mouse; open it and click the item"))
        };
        if was_closed && let Some(p) = &expander {
            std::thread::sleep(Duration::from_millis(40));
            let _ = unsafe { p.Collapse() };
        }
        Some(r.map(|_| format!("{label} is now \"{}\"", clip(&item_name, 60))))
    }

    /// One explicit action. action: press, toggle, select, expand, collapse, focus, scroll_into_view,
    /// scroll (value: up/down/left/right, optionally ":page")
    pub fn act(&self, action: &str, value: &str) -> Result<String, String> {
        self.guard()?;
        let label = &self.label;
        let el = &self.el;
        match action {
            "" | "press" | "click" | "invoke" => self.press().map_err(|e| format!("{e}; use click (it falls back to the mouse)")),
            "toggle" => {
                let p = pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId).ok_or(format!("{label} is not a checkbox or toggle"))?;
                unsafe { p.Toggle() }.map_err(|e| fail("Toggling it", e))?;
                Ok(format!("{label} is now {}", self.toggle_state().unwrap_or("toggled")))
            }
            "select" => {
                let p = pattern::<IUIAutomationSelectionItemPattern>(el, UIA_SelectionItemPatternId).ok_or(format!("{label} can't be selected (not a list, tab or radio item)"))?;
                unsafe { p.Select() }.map_err(|e| fail("Selecting it", e))?;
                Ok(format!("Selected {label}"))
            }
            "expand" | "collapse" => {
                let p = pattern::<IUIAutomationExpandCollapsePattern>(el, UIA_ExpandCollapsePatternId).ok_or(format!("{label} can't be expanded or collapsed"))?;
                if action == "expand" {
                    unsafe { p.Expand() }.map_err(|e| fail("Expanding it", e))?;
                } else {
                    unsafe { p.Collapse() }.map_err(|e| fail("Collapsing it", e))?;
                }
                Ok(format!("{} {label}", if action == "expand" { "Expanded" } else { "Collapsed" }))
            }
            "focus" => {
                self.focus()?;
                Ok(format!("Focused {label}"))
            }
            "scroll_into_view" => {
                if !self.scroll_into_view() {
                    return Err(format!("{label} can't be scrolled into view this way; use scroll"));
                }
                Ok(format!("Scrolled {label} into view"))
            }
            "scroll" => {
                let auto = automation()?;
                let p = scroller(&auto, el).ok_or(format!("Nothing around {label} scrolls this way; use scroll (the mouse wheel)"))?;
                let v = value.trim().to_lowercase();
                let (dir, page) = match v.split_once(':') {
                    Some((d, m)) => (d.to_string(), m == "page"),
                    None => (v.clone(), false),
                };
                let (inc, dec) = if page { (ScrollAmount_LargeIncrement, ScrollAmount_LargeDecrement) } else { (ScrollAmount_SmallIncrement, ScrollAmount_SmallDecrement) };
                let none = ScrollAmount_NoAmount;
                let (h, vv) = match dir.as_str() {
                    "" | "down" => (none, inc),
                    "up" => (none, dec),
                    "right" => (inc, none),
                    "left" => (dec, none),
                    other => return Err(format!("Unknown scroll direction: {other} (use up, down, left or right)")),
                };
                unsafe { p.Scroll(h, vv) }.map_err(|e| fail("Scrolling", e))?;
                let pos = unsafe { p.CurrentVerticalScrollPercent() }.unwrap_or(-1.0);
                Ok(if pos >= 0.0 { format!("Scrolled; now {pos:.0}% down") } else { "Scrolled".into() })
            }
            other => Err(format!("Unknown action: {other} (use press, toggle, select, expand, collapse, focus, scroll or scroll_into_view)")),
        }
    }
}

/// Lower-case, without white space, accelerator marks or trailing dots: how names are compared
pub fn norm(s: &str) -> String {
    let s = s.split('\t').next().unwrap_or(s);
    s.chars().filter(|c| !c.is_whitespace() && *c != '&').flat_map(char::to_lowercase).collect::<String>().trim_end_matches(['.', '…', ':', '：']).to_string()
}

/// What scrolls under a screen point: its visible bounds, and for the given direction how far along it is and
/// how much of it shows (both 0..100; None when the program does not say). For the overlay's scroll bar.
pub struct ScrollArea {
    pub rect: Rect,
    pub pos: Option<f64>,
    pub view: Option<f64>,
}

pub fn scroll_area_at(x: i32, y: i32, horizontal: bool) -> Option<ScrollArea> {
    let auto = automation().ok()?;
    let el = unsafe { auto.ElementFromPoint(POINT { x, y }) }.ok()?;
    let walker = unsafe { auto.ControlViewWalker() }.ok()?;
    let mut cur = el;
    for _ in 0..30 {
        if let Some(p) = pattern::<IUIAutomationScrollPattern>(&cur, UIA_ScrollPatternId) {
            let can = unsafe { if horizontal { p.CurrentHorizontallyScrollable() } else { p.CurrentVerticallyScrollable() } }.map(|b| b.as_bool()).unwrap_or(false);
            if can {
                let r = unsafe { cur.CurrentBoundingRectangle() }.ok()?;
                if r.right - r.left < 40 || r.bottom - r.top < 40 {
                    return None;
                }
                let pos = unsafe { if horizontal { p.CurrentHorizontalScrollPercent() } else { p.CurrentVerticalScrollPercent() } }.ok().filter(|v| (0.0..=100.0).contains(v));
                let view = unsafe { if horizontal { p.CurrentHorizontalViewSize() } else { p.CurrentVerticalViewSize() } }.ok().filter(|v| *v > 0.0 && *v <= 100.0);
                return Some(ScrollArea { rect: (r.left, r.top, r.right, r.bottom), pos, view });
            }
        }
        cur = unsafe { walker.GetParentElement(&cur) }.ok()?;
    }
    None
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

/// The control with the keyboard focus right now: (what it is, its value when it has one, its window, a
/// reference when it has been read before)
pub struct Focus {
    pub role: &'static str,
    pub label: String,
    pub value: Option<String>,
    pub password: bool,
}

/// The `n` characters just before the caret in the control with the keyboard focus, when it is a text control
/// that tells (a document, a text box); None when it can't be told
pub fn text_before_caret(n: i32) -> Option<String> {
    let auto = automation().ok()?;
    let el = unsafe { auto.GetFocusedElement() }.ok()?;
    if unsafe { el.CurrentProcessId() }.ok()? as u32 == std::process::id() || unsafe { el.CurrentIsPassword() }.is_ok_and(|b| b.as_bool()) {
        return None;
    }
    let tp = pattern::<IUIAutomationTextPattern>(&el, UIA_TextPatternId)?;
    let sel = unsafe { tp.GetSelection() }.ok()?;
    if unsafe { sel.Length() }.ok()? < 1 {
        return None;
    }
    let r = unsafe { sel.GetElement(0) }.ok()?;
    // The caret is an empty range: stretch its start back over the last n characters
    unsafe { r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -n) }.ok()?;
    let t = unsafe { r.GetText(n * 2 + 8) }.ok()?;
    Some(newlines(&t.to_string()))
}

pub fn focused() -> Option<Focus> {
    let auto = automation().ok()?;
    let el = unsafe { auto.GetFocusedElement() }.ok()?;
    if unsafe { el.CurrentProcessId() }.ok()? as u32 == std::process::id() {
        return None;
    }
    let role = unsafe { el.CurrentControlType() }.map(type_name).unwrap_or("Element");
    let name = unsafe { el.CurrentName() }.map(|b| b.to_string()).unwrap_or_default();
    let password = unsafe { el.CurrentIsPassword() }.map(|b| b.as_bool()).unwrap_or(false);
    let vp = pattern::<IUIAutomationValuePattern>(&el, UIA_ValuePatternId);
    let value = if password { None } else { vp.as_ref().and_then(|p| unsafe { p.CurrentValue() }.ok()).map(|b| b.to_string()) };
    let label = if name.trim().is_empty() { role.to_string() } else { format!("{role} \"{}\"", clip(&name, 60)) };
    Some(Focus { role, label, value, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_captioned_check_box_is_aimed_at_its_mark_and_everything_else_at_its_middle() {
        assert_eq!(aim("CheckBox", (30, 100, 330, 145)), (40, 122));
        assert_eq!(aim("RadioButton", (0, 0, 120, 18)), (6, 9));
        // A bare box, and any other kind of control
        assert_eq!(aim("CheckBox", (10, 10, 30, 30)), (20, 20));
        assert_eq!(aim("Button", (30, 100, 330, 145)), (180, 122));
    }

    fn node(rf: u32, depth: usize, role: &'static str, name: &str, flags: u32) -> Node {
        Node { rf, depth, role, name: name.into(), value: String::new(), rect: (0, 0, 100, 20), flags }
    }

    fn at(x: i32, y: i32) -> String {
        format!("@({x},{y})")
    }

    #[test]
    fn names_compare_without_spacing_accelerators_or_trailing_dots() {
        assert_eq!(norm("Save &As..."), "saveas");
        assert_eq!(norm("  保 存 "), "保存");
        assert_eq!(norm("Open\tCtrl+O"), "open");
        assert_eq!(norm("Name:"), "name");
    }

    #[test]
    fn a_line_says_what_where_and_in_what_state() {
        let mut n = node(12, 0, "CheckBox", "Agree", flag::TOGGLE | flag::CHECKED | flag::FOCUSED);
        assert_eq!(n.line(&at), "[e12] CheckBox \"Agree\" (focused, checked) @(6,10)");
        n = node(3, 0, "Edit", "Name", flag::VALUE);
        n.value = "Alice".into();
        assert_eq!(n.line(&at), "[e3] Edit \"Name\" = \"Alice\" @(50,10)");
    }

    #[test]
    fn changes_are_told_against_what_was_last_shown() {
        let mut a = Scene { hwnd: 77, title: "T".into(), ..Default::default() };
        a.nodes = vec![node(1, 0, "CheckBox", "Agree", flag::TOGGLE | flag::UNCHECKED), node(2, 0, "Button", "Go", flag::INVOKE), node(3, 0, "Text", "Waiting", 0)];
        assert!(changes(&a, 40, &at).is_none(), "nothing to compare with before the first reading is shown");
        remember(&a);
        assert_eq!(changes(&a, 40, &at).as_deref(), Some(""));
        let mut b = a.clone();
        b.nodes[0].flags = flag::TOGGLE | flag::CHECKED;
        b.nodes[2].name = "Done".into();
        b.nodes.remove(1);
        b.nodes.push(node(9, 0, "Button", "Close", flag::INVOKE));
        let d = changes(&b, 40, &at).unwrap();
        assert!(d.contains("~ [e1] CheckBox \"Agree\": now checked"), "{d}");
        assert!(!d.contains("no longer unchecked"), "{d}");
        assert!(d.contains("~ [e3] Text \"Waiting\": now \"Done\""), "{d}");
        assert!(d.contains("+ [e9] Button \"Close\""), "{d}");
        assert!(d.contains("- gone: Button \"Go\""), "{d}");
    }

    #[test]
    fn a_window_showing_something_else_is_listed_whole() {
        let mut a = Scene { hwnd: 78, ..Default::default() };
        a.nodes = (1..=8).map(|i| node(i, 0, "Button", "old", flag::INVOKE)).collect();
        remember(&a);
        let mut b = Scene { hwnd: 78, ..Default::default() };
        b.nodes = (20..=30).map(|i| node(i, 0, "Button", "new", flag::INVOKE)).collect();
        let d = changes(&b, 40, &at).unwrap();
        assert!(d.starts_with("The window now shows (11 controls):"), "{d}");
    }

    #[test]
    fn a_long_scene_keeps_its_controls_and_drops_plain_text() {
        let mut s = Scene::default();
        for i in 0..30 {
            s.nodes.push(node(i * 2 + 1, 1, "Text", "words", 0));
            s.nodes.push(node(i * 2 + 2, 1, "Button", "b", flag::INVOKE));
        }
        let out = render(&s, &Render { query: "", max: 40, at: &at });
        assert_eq!(out.matches("Button").count(), 30);
        assert!(out.contains("30 lines of plain text left out"), "{out}");
        let q = render(&s, &Render { query: "BUTTON", max: 5, at: &at });
        assert!(q.contains("… 25 more match"), "{q}");
    }
}

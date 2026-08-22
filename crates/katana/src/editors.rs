//! User-facing Keywords / Todos / Settings windows.

#![cfg(windows)]

use std::sync::atomic::{AtomicIsize, Ordering};

use katana_core::Keyword;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, SetBkColor, SetBkMode, SetTextColor};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    MoveWindow, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, LB_ADDSTRING,
    LB_GETCURSEL,
    LB_RESETCONTENT, LB_SETCURSEL, SW_HIDE, SW_SHOW, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WINDOW_STYLE, WNDCLASSW, WS_BORDER,
    WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::persist::{
    clip_history_choice_index, clip_history_limit_from_index, keywords_path, save_keywords,
    set_autostart, settings_path, shot_dir, CLIP_HISTORY_CHOICES, Settings,
};
use crate::ui::STATE;

const ID_TAB_KW: usize = 201;
const ID_TAB_TODO: usize = 202;
const ID_TAB_SET: usize = 203;
const ID_LIST: usize = 301;
const ID_E1: usize = 401;
const ID_E2: usize = 402;
const ID_E3: usize = 403;
const ID_NEW: usize = 501;
const ID_SAVE: usize = 502;
const ID_DEL: usize = 503;
const ID_CHK_AUTO: usize = 601;
const ID_CHK_SHOT: usize = 602;
const ID_OPEN_SHOTS: usize = 603;
const ID_ADD_FOLDER: usize = 604;
const ID_REM_FOLDER: usize = 605;
const ID_KIND: usize = 606;
const ID_STATUS: usize = 607;
const ID_PRI: usize = 608;
const ID_CLEAR_CLIPS: usize = 609;
const ID_CLIP_LIMIT: usize = 610;
const ID_L1: usize = 701;
const ID_L2: usize = 702;
const ID_L3: usize = 703;
const ID_L_PCT: usize = 704;
const ID_L_CLIP: usize = 705;

const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BST_CHECKED: isize = 1;
const BST_UNCHECKED: isize = 0;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const CBS_HASSTRINGS: u32 = 0x0200;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: usize = 1;
const LBN_SELCHANGE: usize = 1;
const LBN_DBLCLK: usize = 2;
const LBS_NOTIFY: u32 = 0x0001;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const ES_AUTOHSCROLL: u32 = 0x0080;

static STUDIO: AtomicIsize = AtomicIsize::new(0);
static BG_BRUSH: AtomicIsize = AtomicIsize::new(0);
static EDIT_BRUSH: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Keywords,
    Todos,
    Settings,
}

struct Studio {
    tab: Tab,
    hwnd: HWND,
    list: HWND,
    e1: HWND,
    e2: HWND,
    e3: HWND,
    l1: HWND,
    l2: HWND,
    l3: HWND,
    l_pct: HWND,
    combo_kind: HWND,
    combo_status: HWND,
    combo_pri: HWND,
    chk_auto: HWND,
    chk_shot: HWND,
    btn_open: HWND,
    btn_add: HWND,
    btn_rem: HWND,
    btn_new: HWND,
    btn_save: HWND,
    btn_del: HWND,
    btn_clear_clips: HWND,
    l_clip: HWND,
    combo_clip: HWND,
    keywords: Vec<Keyword>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe fn text_of(hwnd: HWND) -> String {
    let n = GetWindowTextLengthW(hwnd) as usize;
    if n == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; n + 1];
    let got = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..got as usize])
}

unsafe fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(w.as_ptr()));
}

unsafe fn list_clear(list: HWND) {
    SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
}

unsafe fn list_add(list: HWND, s: &str) {
    let w = wide(s);
    SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
}

unsafe fn list_sel(list: HWND) -> i32 {
    SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32
}

unsafe fn checked(hwnd: HWND) -> bool {
    SendMessageW(hwnd, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == BST_CHECKED
}

unsafe fn set_checked(hwnd: HWND, on: bool) {
    SendMessageW(
        hwnd,
        BM_SETCHECK,
        WPARAM(if on {
            BST_CHECKED as usize
        } else {
            BST_UNCHECKED as usize
        }),
        LPARAM(0),
    );
}

unsafe fn combo_add(hwnd: HWND, s: &str) {
    let w = wide(s);
    SendMessageW(hwnd, CB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
}

unsafe fn combo_get(hwnd: HWND) -> i32 {
    SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32
}

unsafe fn combo_set(hwnd: HWND, i: i32) {
    SendMessageW(hwnd, CB_SETCURSEL, WPARAM(i as usize), LPARAM(0));
}

fn kw_label(k: &Keyword) -> String {
    let names = k.names.join(", ");
    let kind = if k.url.is_some() {
        "website"
    } else if k.path.is_some() {
        "program"
    } else {
        "command"
    };
    format!("{names}   ·   {kind}")
}

/// Fields shown when a keyword is selected: names, kind index, value, args.
pub fn keyword_form(k: &Keyword) -> (String, i32, String, String) {
    let names = k.names.join(", ");
    let (kind, val) = if let Some(u) = &k.url {
        (0, u.clone())
    } else if let Some(p) = &k.path {
        (1, p.clone())
    } else {
        (2, k.command.clone().unwrap_or_default())
    };
    (names, kind, val, k.args.clone().unwrap_or_default())
}

/// Combo indices for a todo row: (status, priority).
pub fn todo_label(t: &katana_todo::Task) -> String {
    let mark = if t.status == "completed" { "✓" } else { "○" };
    format!(
        "{mark}  {}   ·  {}   (#{})",
        t.title,
        katana_todo::format_progress(t.progress),
        t.id
    )
}

pub fn todo_combo_indices(status: &str, priority: &str) -> (i32, i32) {
    let si = match status {
        "in_progress" | "in progress" | "in-progress" => 1,
        "completed" | "done" => 2,
        "cancelled" | "canceled" => 3,
        _ => 0,
    };
    let pi = match priority {
        "low" => 0,
        "high" => 2,
        "urgent" => 3,
        _ => 1,
    };
    (si, pi)
}

unsafe fn refill_keywords(st: &Studio) {
    list_clear(st.list);
    for k in &st.keywords {
        list_add(st.list, &kw_label(k));
    }
}

unsafe fn refill_todos(st: &Studio) {
    list_clear(st.list);
    if let Ok(g) = STATE.lock() {
        if let Some(ln) = g.as_ref() {
            if let Ok(tasks) = ln.todos.list_all() {
                for t in tasks {
                    list_add(st.list, &todo_label(&t));
                }
            }
        }
    }
}

unsafe fn refill_folders(st: &Studio) {
    list_clear(st.list);
    let s = Settings::load(&settings_path());
    if s.crawl_folders.is_empty() {
        list_add(
            st.list,
            "(default: Documents, Desktop, Downloads, and src)",
        );
    }
    for f in &s.crawl_folders {
        list_add(st.list, f);
    }
}

unsafe fn show_set(hwnd: HWND, vis: bool) {
    let _ = ShowWindow(hwnd, if vis { SW_SHOW } else { SW_HIDE });
}

unsafe fn place(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    let _ = MoveWindow(hwnd, x, y, w, h, true);
}

fn read_percent(hwnd: HWND) -> Option<i64> {
    unsafe { katana_todo::parse_percent(&text_of(hwnd)) }
}

unsafe fn update_kind_label(st: &Studio) {
    match combo_get(st.combo_kind) {
        1 => set_text(st.l2, "Program or file to open"),
        2 => set_text(st.l2, "Command to run"),
        _ => set_text(st.l2, "Website address"),
    }
}

unsafe fn show_tab(st: &mut Studio, tab: Tab) {
    st.tab = tab;
    let settings = tab == Tab::Settings;
    let keywords = tab == Tab::Keywords;
    let todos = tab == Tab::Todos;
    show_set(st.chk_auto, settings);
    show_set(st.chk_shot, settings);
    show_set(st.btn_open, settings);
    show_set(st.btn_add, settings);
    show_set(st.btn_rem, settings);
    show_set(st.btn_clear_clips, settings);
    show_set(st.l_clip, settings);
    show_set(st.combo_clip, settings);
    show_set(st.btn_new, !settings);
    show_set(st.btn_save, !settings);
    show_set(st.btn_del, !settings);
    show_set(st.combo_kind, keywords);
    show_set(st.combo_status, todos);
    show_set(st.combo_pri, todos);
    show_set(st.l_pct, todos);
    show_set(st.e1, !settings);
    show_set(st.e2, !todos);
    show_set(st.e3, keywords || todos);
    match tab {
        Tab::Keywords => {
            place(st.l3, 16, 374, 772, 18);
            place(st.e3, 16, 394, 772, 26);
            set_text(st.l1, "Names you type  (comma-separated, e.g. g, google)");
            set_text(st.l3, "Extra arguments — $P$ becomes whatever you type after the name");
            set_text(st.btn_new, "New shortcut");
            set_text(st.btn_save, "Save shortcut");
            set_text(st.btn_del, "Delete");
            set_text(st.e1, "");
            set_text(st.e2, "");
            set_text(st.e3, "");
            combo_set(st.combo_kind, 0);
            update_kind_label(st);
            refill_keywords(st);
        }
        Tab::Todos => {
            place(st.l3, 16, 374, 380, 18);
            place(st.combo_pri, 16, 394, 380, 160);
            place(st.l_pct, 416, 374, 220, 18);
            place(st.e3, 416, 394, 140, 26);
            set_text(st.l1, "Task title");
            set_text(st.l2, "Status");
            set_text(st.l3, "Priority");
            set_text(st.l_pct, "Completion (0–100%)");
            set_text(st.btn_new, "New task");
            set_text(st.btn_save, "Save task");
            set_text(st.btn_del, "Delete");
            set_text(st.e1, "");
            set_text(st.e3, "0");
            combo_set(st.combo_status, 0);
            combo_set(st.combo_pri, 1);
            refill_todos(st);
        }
        Tab::Settings => {
            set_text(
                st.l1,
                "Folders Katana searches for files  (select one, then Remove folder)",
            );
            set_text(
                st.l2,
                "Or type a folder path below, then Add folder — or click Add folder to browse",
            );
            let dir = shot_dir();
            set_text(
                st.l3,
                &format!("Screenshots save to  {}", dir.display()),
            );
            set_text(st.e2, "");
            let s = Settings::load(&settings_path());
            set_checked(st.chk_auto, s.autostart);
            set_checked(st.chk_shot, s.save_shots);
            combo_set(st.combo_clip, clip_history_choice_index(s.clip_history_limit));
            refill_folders(st);
        }
    }
}

unsafe fn on_select(st: &Studio) {
    let i = list_sel(st.list);
    if i < 0 {
        return;
    }
    match st.tab {
        Tab::Keywords => {
            if let Some(k) = st.keywords.get(i as usize) {
                let (names, kind, val, args) = keyword_form(k);
                set_text(st.e1, &names);
                combo_set(st.combo_kind, kind);
                update_kind_label(st);
                set_text(st.e2, &val);
                set_text(st.e3, &args);
            }
        }
        Tab::Todos => {
            if let Ok(g) = STATE.lock() {
                if let Some(ln) = g.as_ref() {
                    if let Ok(tasks) = ln.todos.list_all() {
                        if let Some(t) = tasks.get(i as usize) {
                            set_text(st.e1, &t.title);
                            let (si, pi) = todo_combo_indices(&t.status, &t.priority);
                            combo_set(st.combo_status, si);
                            combo_set(st.combo_pri, pi);
                            set_text(st.e3, &t.progress.to_string());
                        }
                    }
                }
            }
        }
        Tab::Settings => {
            let s = Settings::load(&settings_path());
            if s.crawl_folders.is_empty() {
                return;
            }
            if let Some(p) = s.crawl_folders.get(i as usize) {
                set_text(st.e2, p);
            }
        }
    }
}

unsafe fn on_new(st: &mut Studio) {
    match st.tab {
        Tab::Keywords => {
            st.keywords.push(Keyword {
                names: vec!["name".into()],
                url: Some("https://".into()),
                path: None,
                args: None,
                steps: None,
                command: None,
            });
            refill_keywords(st);
            SendMessageW(
                st.list,
                LB_SETCURSEL,
                WPARAM(st.keywords.len().saturating_sub(1)),
                LPARAM(0),
            );
            on_select(st);
        }
        Tab::Todos => {
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    let title = text_of(st.e1);
                    let title = if title.is_empty() { "New task" } else { &title };
                    let pri = match combo_get(st.combo_pri) {
                        0 => katana_todo::Priority::Low,
                        2 => katana_todo::Priority::High,
                        3 => katana_todo::Priority::Urgent,
                        _ => katana_todo::Priority::Medium,
                    };
                    let status = match combo_get(st.combo_status) {
                        1 => katana_todo::Status::InProgress,
                        2 => katana_todo::Status::Completed,
                        3 => katana_todo::Status::Cancelled,
                        _ => katana_todo::Status::Pending,
                    };
                    let mut pct = read_percent(st.e3).unwrap_or(0);
                    if status == katana_todo::Status::Completed {
                        pct = 100;
                    }
                    let _ = ln.todos.add(title, "", pri, status, pct, "[]", None);
                }
            }
            refill_todos(st);
        }
        Tab::Settings => add_folder(st),
    }
}

fn save_clip_history_limit(st: &Studio) {
    let n = clip_history_limit_from_index(unsafe { combo_get(st.combo_clip) });
    let mut s = Settings::load(&settings_path());
    s.clip_history_limit = n;
    let _ = s.save(&settings_path());
    if let Ok(mut g) = STATE.lock() {
        if let Some(ln) = g.as_mut() {
            ln.apply_clip_history_limit(n);
        }
    }
}

fn clear_clip_history(parent: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO,
    };
    let title = wide("Clear clipboard history");
    let body = wide("Remove every saved clip? This cannot be undone.");
    let ans = unsafe {
        MessageBoxW(
            parent,
            windows::core::PCWSTR(body.as_ptr()),
            windows::core::PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    if ans != IDYES {
        return;
    }
    if let Ok(mut g) = STATE.lock() {
        if let Some(ln) = g.as_mut() {
            let _ = ln.clear_clips();
        }
    }
}

unsafe fn add_folder(st: &mut Studio) {
    let mut path = text_of(st.e2);
    if path.is_empty() {
        path = browse_folder(st.hwnd).unwrap_or_default();
    }
    path = path.trim().trim_matches('"').to_string();
    if path.is_empty() {
        return;
    }
    let mut s = Settings::load(&settings_path());
    if !s.crawl_folders.iter().any(|p| p == &path) {
        s.crawl_folders.push(path);
        let _ = s.save(&settings_path());
        if let Ok(mut g) = STATE.lock() {
            if let Some(ln) = g.as_mut() {
                ln.engine.files = crate::launcher::rebuild_file_index();
            }
        }
    }
    set_text(st.e2, "");
    refill_folders(st);
}

fn browse_folder(parent: HWND) -> Option<String> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS,
        BROWSEINFOW,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let title = wide("Choose a folder Katana should search");
        let bi = BROWSEINFOW {
            hwndOwner: parent,
            lpszTitle: windows::core::PCWSTR(title.as_ptr()),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
            ..Default::default()
        };
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut buf = [0u16; 260];
        let ok = SHGetPathFromIDListW(pidl, &mut buf);
        CoTaskMemFree(Some(pidl.cast()));
        if ok.as_bool() {
            let s = String::from_utf16_lossy(&buf)
                .trim_end_matches('\0')
                .to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        } else {
            None
        }
    }
}

unsafe fn on_save(st: &mut Studio) {
    match st.tab {
        Tab::Keywords => {
            let i = list_sel(st.list);
            let names: Vec<String> = text_of(st.e1)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if names.is_empty() {
                return;
            }
            let val = text_of(st.e2);
            let args = text_of(st.e3);
            let mut kw = Keyword {
                names,
                url: None,
                path: None,
                args: if args.is_empty() { None } else { Some(args) },
                steps: None,
                command: None,
            };
            match combo_get(st.combo_kind) {
                1 => kw.path = Some(val),
                2 => kw.command = Some(val),
                _ => kw.url = Some(val),
            }
            if i >= 0 && (i as usize) < st.keywords.len() {
                st.keywords[i as usize] = kw;
            } else {
                st.keywords.push(kw);
            }
            let _ = save_keywords(&keywords_path(), &st.keywords);
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    ln.engine.keywords = st.keywords.clone();
                }
            }
            refill_keywords(st);
        }
        Tab::Todos => {
            let i = list_sel(st.list);
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    if let Ok(tasks) = ln.todos.list_all() {
                        if i >= 0 {
                            if let Some(t) = tasks.get(i as usize) {
                                let title = text_of(st.e1);
                                let status = match combo_get(st.combo_status) {
                                    1 => katana_todo::Status::InProgress,
                                    2 => katana_todo::Status::Completed,
                                    3 => katana_todo::Status::Cancelled,
                                    _ => katana_todo::Status::Pending,
                                };
                                let pri = match combo_get(st.combo_pri) {
                                    0 => katana_todo::Priority::Low,
                                    2 => katana_todo::Priority::High,
                                    3 => katana_todo::Priority::Urgent,
                                    _ => katana_todo::Priority::Medium,
                                };
                                let _ = ln.todos.update_fields(
                                    t.id,
                                    if title.is_empty() { None } else { Some(&title) },
                                    None,
                                    Some(pri),
                                    None,
                                );
                                let pct = read_percent(st.e3).unwrap_or(t.progress);
                                let _ = ln.todos.set_status(t.id, status, Some(pct));
                            }
                        }
                    }
                }
            }
            refill_todos(st);
        }
        Tab::Settings => {
            let mut s = Settings::load(&settings_path());
            s.autostart = checked(st.chk_auto);
            s.save_shots = checked(st.chk_shot);
            s.clip_history_limit = clip_history_limit_from_index(combo_get(st.combo_clip));
            let _ = s.save(&settings_path());
            let _ = set_autostart(s.autostart);
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    ln.apply_clip_history_limit(s.clip_history_limit);
                }
            }
        }
    }
}

unsafe fn on_del(st: &mut Studio) {
    let i = list_sel(st.list);
    if i < 0 {
        return;
    }
    match st.tab {
        Tab::Keywords => {
            if (i as usize) < st.keywords.len() {
                st.keywords.remove(i as usize);
                let _ = save_keywords(&keywords_path(), &st.keywords);
                if let Ok(mut g) = STATE.lock() {
                    if let Some(ln) = g.as_mut() {
                        ln.engine.keywords = st.keywords.clone();
                    }
                }
                refill_keywords(st);
            }
        }
        Tab::Todos => {
            if let Ok(mut g) = STATE.lock() {
                if let Some(ln) = g.as_mut() {
                    if let Ok(tasks) = ln.todos.list_all() {
                        if let Some(t) = tasks.get(i as usize) {
                            let _ = ln.todos.delete(t.id, true);
                        }
                    }
                }
            }
            refill_todos(st);
        }
        Tab::Settings => {
            let mut s = Settings::load(&settings_path());
            if s.crawl_folders.is_empty() {
                return;
            }
            if (i as usize) < s.crawl_folders.len() {
                s.crawl_folders.remove(i as usize);
                let _ = s.save(&settings_path());
                if let Ok(mut g) = STATE.lock() {
                    if let Some(ln) = g.as_mut() {
                        ln.engine.files = crate::launcher::rebuild_file_index();
                    }
                }
                refill_folders(st);
            }
        }
    }
}

fn apply_dark_title(hwnd: HWND) {
    unsafe {
        let dark: i32 = 1;
        // 20 = DWMWA_USE_IMMERSIVE_DARK_MODE
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20),
            &dark as *const i32 as *const _,
            4,
        );
    }
}

fn theme_child(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::UI::Controls::SetWindowTheme(hwnd, w!(""), w!(""));
    }
}

fn child(
    parent: HWND,
    class: windows::core::PCWSTR,
    title: windows::core::PCWSTR,
    extra_style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
    extra: windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE,
) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let hwnd = CreateWindowExW(
            extra,
            class,
            title,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | extra_style),
            x,
            y,
            w,
            h,
            parent,
            windows::Win32::UI::WindowsAndMessaging::HMENU(id as *mut _),
            hinst,
            None,
        )
        .unwrap_or_default();
        theme_child(hwnd);
        hwnd
    }
}

unsafe extern "system" fn studio_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        m if m == WM_CTLCOLORSTATIC => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(w.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00D4_D4D8));
            SetBkColor(hdc, COLORREF(0x0016_1618));
            let _ = SetBkMode(hdc, windows::Win32::Graphics::Gdi::TRANSPARENT);
            LRESULT(BG_BRUSH.load(Ordering::SeqCst))
        }
        m if m == WM_CTLCOLOREDIT || m == WM_CTLCOLORLISTBOX => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(w.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00F4_F4F5));
            SetBkColor(hdc, COLORREF(0x0022_2226));
            LRESULT(EDIT_BRUSH.load(Ordering::SeqCst))
        }
        m if m == WM_CTLCOLORBTN => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(w.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00F4_F4F5));
            SetBkColor(hdc, COLORREF(0x0016_1618));
            LRESULT(BG_BRUSH.load(Ordering::SeqCst))
        }
        WM_COMMAND => {
            let id = w.0 & 0xFFFF;
            let notify = (w.0 >> 16) & 0xFFFF;
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr == 0 {
                return DefWindowProcW(hwnd, msg, w, l);
            }
            let st = &mut *(ptr as *mut Studio);
            if id == ID_LIST && (notify == LBN_SELCHANGE || notify == LBN_DBLCLK || notify == 0)
            {
                on_select(st);
                return LRESULT(0);
            }
            if id == ID_KIND && notify == CBN_SELCHANGE {
                update_kind_label(st);
                return LRESULT(0);
            }
            if id == ID_CLIP_LIMIT && notify == CBN_SELCHANGE {
                save_clip_history_limit(st);
                return LRESULT(0);
            }
            match id {
                ID_TAB_KW => show_tab(st, Tab::Keywords),
                ID_TAB_TODO => show_tab(st, Tab::Todos),
                ID_TAB_SET => show_tab(st, Tab::Settings),
                ID_NEW => on_new(st),
                ID_SAVE => on_save(st),
                ID_DEL => on_del(st),
                ID_CHK_AUTO | ID_CHK_SHOT => on_save(st),
                ID_OPEN_SHOTS => {
                    let dir = shot_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
                ID_ADD_FOLDER => add_folder(st),
                ID_REM_FOLDER => on_del(st),
                ID_CLEAR_CLIPS => clear_clip_history(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                drop(Box::from_raw(ptr as *mut Studio));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            STUDIO.store(0, Ordering::SeqCst);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

pub fn open_studio(tab: &str) {
    let existing = STUDIO.load(Ordering::SeqCst);
    if existing != 0 {
        unsafe {
            let hwnd = HWND(existing as *mut _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                let st = &mut *(ptr as *mut Studio);
                let start = match tab {
                    "todos" | "todo" => Tab::Todos,
                    "settings" => Tab::Settings,
                    _ => Tab::Keywords,
                };
                show_tab(st, start);
            }
        }
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let brush = CreateSolidBrush(COLORREF(0x0016_1618));
        let edit_brush = CreateSolidBrush(COLORREF(0x0022_2226));
        BG_BRUSH.store(brush.0 as isize, Ordering::SeqCst);
        EDIT_BRUSH.store(edit_brush.0 as isize, Ordering::SeqCst);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(studio_proc),
            hInstance: hinst.into(),
            hbrBackground: brush,
            lpszClassName: w!("KatanaStudio"),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            w!("KatanaStudio"),
            w!("Katana"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            820,
            620,
            None,
            None,
            hinst,
            None,
        )
        .expect("studio window");
        apply_dark_title(hwnd);
        let icon = crate::ui::load_app_icon_size(32, 32);
        const WM_SETICON: u32 = 0x0080;
        SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(icon.0 as isize));
        SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(icon.0 as isize));

        let _ = child(
            hwnd,
            w!("BUTTON"),
            w!("Shortcuts"),
            0,
            16,
            14,
            110,
            30,
            ID_TAB_KW,
            Default::default(),
        );
        let _ = child(
            hwnd,
            w!("BUTTON"),
            w!("To-dos"),
            0,
            132,
            14,
            100,
            30,
            ID_TAB_TODO,
            Default::default(),
        );
        let _ = child(
            hwnd,
            w!("BUTTON"),
            w!("Settings"),
            0,
            238,
            14,
            110,
            30,
            ID_TAB_SET,
            Default::default(),
        );

        let list = child(
            hwnd,
            w!("LISTBOX"),
            w!(""),
            LBS_NOTIFY | WS_VSCROLL.0 | WS_BORDER.0,
            16,
            54,
            772,
            200,
            ID_LIST,
            WS_EX_CLIENTEDGE,
        );
        let l1 = child(
            hwnd,
            w!("STATIC"),
            w!(""),
            0,
            16,
            264,
            772,
            18,
            ID_L1,
            Default::default(),
        );
        let e1 = child(
            hwnd,
            w!("EDIT"),
            w!(""),
            ES_AUTOHSCROLL | WS_BORDER.0,
            16,
            284,
            772,
            26,
            ID_E1,
            WS_EX_CLIENTEDGE,
        );
        let l2 = child(
            hwnd,
            w!("STATIC"),
            w!(""),
            0,
            16,
            318,
            772,
            18,
            ID_L2,
            Default::default(),
        );
        let combo_kind = child(
            hwnd,
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL.0,
            16,
            338,
            170,
            160,
            ID_KIND,
            Default::default(),
        );
        let e2 = child(
            hwnd,
            w!("EDIT"),
            w!(""),
            ES_AUTOHSCROLL | WS_BORDER.0,
            196,
            338,
            592,
            26,
            ID_E2,
            WS_EX_CLIENTEDGE,
        );
        let combo_status = child(
            hwnd,
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL.0,
            16,
            338,
            772,
            160,
            ID_STATUS,
            Default::default(),
        );
        let l3 = child(
            hwnd,
            w!("STATIC"),
            w!(""),
            0,
            16,
            374,
            772,
            18,
            ID_L3,
            Default::default(),
        );
        let l_pct = child(
            hwnd,
            w!("STATIC"),
            w!("Completion (0–100%)"),
            0,
            416,
            374,
            220,
            18,
            ID_L_PCT,
            Default::default(),
        );
        let e3 = child(
            hwnd,
            w!("EDIT"),
            w!(""),
            ES_AUTOHSCROLL | WS_BORDER.0,
            16,
            394,
            772,
            26,
            ID_E3,
            WS_EX_CLIENTEDGE,
        );
        let combo_pri = child(
            hwnd,
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL.0,
            16,
            394,
            772,
            160,
            ID_PRI,
            Default::default(),
        );

        combo_add(combo_kind, "Website");
        combo_add(combo_kind, "Program");
        combo_add(combo_kind, "Command");
        combo_set(combo_kind, 0);
        combo_add(combo_status, "Pending");
        combo_add(combo_status, "In progress");
        combo_add(combo_status, "Completed");
        combo_add(combo_status, "Cancelled");
        combo_set(combo_status, 0);
        combo_add(combo_pri, "Low");
        combo_add(combo_pri, "Medium");
        combo_add(combo_pri, "High");
        combo_add(combo_pri, "Urgent");
        combo_set(combo_pri, 1);

        let l_clip = child(
            hwnd,
            w!("STATIC"),
            w!("Clipboard history  (oldest unpinned items are dropped)"),
            0,
            450,
            434,
            340,
            18,
            ID_L_CLIP,
            Default::default(),
        );
        let combo_clip = child(
            hwnd,
            w!("COMBOBOX"),
            w!(""),
            CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL.0,
            450,
            454,
            260,
            160,
            ID_CLIP_LIMIT,
            Default::default(),
        );
        for n in CLIP_HISTORY_CHOICES {
            combo_add(combo_clip, &format!("Keep {n} items"));
        }
        combo_set(combo_clip, clip_history_choice_index(100));

        let btn_new = child(
            hwnd,
            w!("BUTTON"),
            w!("New shortcut"),
            0,
            16,
            438,
            130,
            30,
            ID_NEW,
            Default::default(),
        );
        let btn_save = child(
            hwnd,
            w!("BUTTON"),
            w!("Save shortcut"),
            0,
            154,
            438,
            130,
            30,
            ID_SAVE,
            Default::default(),
        );
        let btn_del = child(
            hwnd,
            w!("BUTTON"),
            w!("Delete"),
            0,
            292,
            438,
            90,
            30,
            ID_DEL,
            Default::default(),
        );

        let chk_auto = child(
            hwnd,
            w!("BUTTON"),
            w!("Start Katana when I sign in to Windows"),
            BS_AUTOCHECKBOX,
            16,
            438,
            420,
            24,
            ID_CHK_AUTO,
            Default::default(),
        );
        let chk_shot = child(
            hwnd,
            w!("BUTTON"),
            w!("Save screenshots to Pictures\\Katana"),
            BS_AUTOCHECKBOX,
            16,
            466,
            420,
            24,
            ID_CHK_SHOT,
            Default::default(),
        );
        let btn_open = child(
            hwnd,
            w!("BUTTON"),
            w!("Open screenshots folder"),
            0,
            16,
            500,
            200,
            30,
            ID_OPEN_SHOTS,
            Default::default(),
        );
        let btn_add = child(
            hwnd,
            w!("BUTTON"),
            w!("Add folder…"),
            0,
            228,
            500,
            130,
            30,
            ID_ADD_FOLDER,
            Default::default(),
        );
        let btn_rem = child(
            hwnd,
            w!("BUTTON"),
            w!("Remove folder"),
            0,
            366,
            500,
            130,
            30,
            ID_REM_FOLDER,
            Default::default(),
        );
        let btn_clear_clips = child(
            hwnd,
            w!("BUTTON"),
            w!("Clear clipboard history"),
            0,
            506,
            500,
            200,
            30,
            ID_CLEAR_CLIPS,
            Default::default(),
        );

        let kws = if let Ok(g) = STATE.lock() {
            g.as_ref()
                .map(|l| l.engine.keywords.clone())
                .unwrap_or_else(katana_core::default_keywords)
        } else {
            katana_core::default_keywords()
        };

        let mut st = Box::new(Studio {
            tab: Tab::Keywords,
            hwnd,
            list,
            e1,
            e2,
            e3,
            l1,
            l2,
            l3,
            l_pct,
            combo_kind,
            combo_status,
            combo_pri,
            chk_auto,
            chk_shot,
            btn_open,
            btn_add,
            btn_rem,
            btn_new,
            btn_save,
            btn_del,
            btn_clear_clips,
            l_clip,
            combo_clip,
            keywords: kws,
        });
        let start = match tab {
            "todos" | "todo" => Tab::Todos,
            "settings" => Tab::Settings,
            _ => Tab::Keywords,
        };
        show_tab(&mut st, start);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);
        STUDIO.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn apply_keyword_edit(
    kws: &mut Vec<Keyword>,
    index: Option<usize>,
    names: &str,
    value: &str,
    args: &str,
) -> Vec<Keyword> {
    let names: Vec<String> = names
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut kw = Keyword {
        names,
        url: None,
        path: None,
        args: if args.is_empty() {
            None
        } else {
            Some(args.into())
        },
        steps: None,
        command: None,
    };
    if value.starts_with("http://") || value.starts_with("https://") || value.contains("$U$") {
        kw.url = Some(value.into());
    } else if value.len() >= 2 && value.as_bytes().get(1) == Some(&b':') {
        kw.path = Some(value.into());
    } else {
        kw.command = Some(value.into());
    }
    match index {
        Some(i) if i < kws.len() => kws[i] = kw,
        _ => kws.push(kw),
    }
    kws.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_keyword_edit_adds_url_shortcut() {
        let mut kws = vec![];
        apply_keyword_edit(
            &mut kws,
            None,
            "so, stack",
            "https://stackoverflow.com/search?q=$U$",
            "",
        );
        assert_eq!(kws[0].names, ["so", "stack"]);
        assert!(kws[0].url.as_deref().unwrap().contains("stackoverflow"));
    }

    #[test]
    fn keyword_form_fills_all_fields() {
        let k = Keyword {
            names: vec!["g".into(), "google".into()],
            url: Some("https://google.com/search?q=$P$".into()),
            path: None,
            args: Some("$P$".into()),
            steps: None,
            command: None,
        };
        let (names, kind, val, args) = keyword_form(&k);
        assert_eq!(names, "g, google");
        assert_eq!(kind, 0);
        assert!(val.contains("google.com"));
        assert_eq!(args, "$P$");
        let p = Keyword {
            names: vec!["npp".into()],
            url: None,
            path: Some(r"C:\npp.exe".into()),
            args: None,
            steps: None,
            command: None,
        };
        let (_, kind, val, args) = keyword_form(&p);
        assert_eq!((kind, val.as_str(), args.as_str()), (1, r"C:\npp.exe", ""));
    }

    #[test]
    fn todo_combo_indices_map_plain_status() {
        assert_eq!(todo_combo_indices("in_progress", "high"), (1, 2));
        assert_eq!(todo_combo_indices("pending", "medium"), (0, 1));
        assert_eq!(todo_combo_indices("completed", "urgent"), (2, 3));
    }

    #[test]
    fn todo_label_includes_percent() {
        let t = katana_todo::Task {
            id: 3,
            title: "Ship".into(),
            description: String::new(),
            status: "in_progress".into(),
            priority: "medium".into(),
            progress: 40,
            tags: "[]".into(),
            due_date: None,
            position: 1,
        };
        let s = todo_label(&t);
        assert!(s.contains("40%"), "{s}");
        assert!(s.contains("Ship"), "{s}");
        assert!(s.contains("#3"), "{s}");
    }
}

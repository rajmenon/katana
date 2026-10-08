# Changelog

## 0.3.5 — 2026-10-08

### UI
- Clipboard history and the to-do list now scroll like file search (wheel, scrollbar, PgUp/PgDn)

## 0.3.4 — 2026-10-05

### UI
- The selected row is now deep indigo, so it stands out against the amber accent

## 0.3.3 — 2026-10-01

### Home
- Command rows no longer repeat the command name. The second line is only the description

## 0.3.2 — 2026-10-01

### Home
- `/k` and `/f` are no longer on the home list. Type them; Win+Alt+Space still opens files

### Keywords
- **Ctrl+V** (or Shift+Insert) after a URL keyword pastes the search. The text is percent-encoded into the address
- URL keywords open with the shell, not `cmd /C`, so a pasted `&` cannot start another program
- Paste is ignored for shell keywords (`ps`), `>`, and shortcuts that launch a program

### Todo
- `/` filters by progress and includes completed tasks: `/`, `/0..10`, `/10..`, `/50..`, `/100`, `/45..85`, `/..50`
- `%` still sets progress on the selected task
- Junk or an inverted range matches nothing. The bounds are bound parameters, not part of the SQL text
- `katana todo /100` is the same filter on the command line

## 0.3.1 — 2026-09-28

### Files (`/f`)
- Typing no longer hangs. File search matches the name directly instead of fuzzy-scanning the whole index on every keystroke
- `/f terminal.exe` finds Windows Terminal, not unrelated component exes
- Long names stay in the name column, so **Date modified** stays visible
- Program indexing no longer walks deep vendor trees (JetBrains, SDKs)

## 0.3.0 — 2026-09-28

### Files (`/f`)
- **Date modified** shows for files and folders, in local time
- **Ctrl+C** copies the full path of the selected file, folder, or program
- A file inside two crawled folders (Home and Documents) is listed once

### Launch
- Installed `.exe` files are indexed from Program Files and `%LOCALAPPDATA%\Programs`
- **PowerShell**, **Windows Terminal**, **Command Prompt**, and **Notepad** launch by name. No keyword required
- `g` is no longer on the home command list. `g rust` still searches Google

## 0.2.0 — 2026-09-08

Windows tray swiss knife. **Alt+Space** still opens the prompt.

### Clipboard (`/clip`) — **Win+Alt+C**
- Opens clipboard history from the hotkey
- Enter pastes into the last focused field (not clipboard-only)
- Image preview is reliable for history items (including typical Windows PNGs)

### Files (`/f`) — **Win+Alt+Space**
- Prompt opens as `/f ` so you can type immediately
- Explorer-style details table: name, date, type, size; click headers to sort
- Insertion caret (← → Home End, click to place); long queries scroll so the caret stays visible
- Scrollbar, mouse wheel, and Page Up/Down
- Empty `/f` lists **Documents, Downloads, Desktop, Pictures, Videos, Music, Home**
- Keywords: `documents`, `downloads`/`dl`, `desktop`, `pictures`/`pics`, `videos`, `music`, `home`
  (`docs` remains docs.rs)

### Screenshots
- `/sr` region · `/sw` window · `/sf` full browser page (scroll-stitch; otherwise window)
- Every capture goes to the **clipboard** and **Pictures\Katana**
- Click the toast to open the file
- Tray → **Open screenshots folder**

### Launch (combined `/k` / `/a` / `/cmd`)
- One blade: apps, keywords, bookmarks, plus **Run command**
- `/a`, `/cmd`, `/b` still work; they open the same list
- Failed commands (non-zero exit) keep the overlay open and **flash it red**

### Other
- **Esc** steps back one level (`/clip foo` → `/clip` → home → hide)
- Overlay sits next to the caret or mouse
- Tray → **Keep screen awake** (checked / highlighted when on), or `/awake`
- File-search stability: panics in the UI no longer take down the process; `/f` no longer stats hundreds of files per keystroke

### Notes
- Keep-awake lasts for this session only
- `>` still runs a shell line immediately
- Config remains `%APPDATA%\Katana`

## 0.1.0 — 2026-08

Initial public release.

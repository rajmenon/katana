# Katana

<p align="center">
  <img src="icon.jpg" width="720" alt="Katana">
</p>

<p align="center"><strong>A Japanese swiss knife for Windows.</strong><br>
Fast. Small. Many-talented.</p>

Katana lives in the tray. Press **Alt+Space** and it is in your hands — one thin prompt, many blades. No webview, one process, a couple of megabytes. **Win+Alt+C** opens clipboard history; **Win+Alt+Space** opens file search. **Ctrl+Alt+R** toggles screen recording.

## Blades

- **Search** files and apps as you type. Wildcards `*.pdf`, `inv?ice.*` · regex `re:inv.*` or `/inv.*pdf/`
- **Launch** (`/k`, also `/a` `/cmd` `/b`) — apps, your keywords (`g rust`, `gh`, `docs`), browser bookmarks, and **Run command** for whatever you typed. `$P$` is whatever you type after a keyword; in a URL keyword that text is percent-encoded. **Tab** completes a keyword (Tab again cycles matches; Shift+Tab goes back). Empty Tab fills the last keyword when no recent is listed. Type the keyword, then **Ctrl+V** to paste the search. Type `powershell` or `terminal` to launch them; no keyword needed. `/k` and `/f` are not listed on the home screen; type them.
- **Recents** — Enter remembers apps, files, folders, bookmarks, keyword lines, `>` commands, URLs, and pasted clips (50, newest first, `%APPDATA%\Katana\recents.json`). An empty prompt lists them above the commands; Tab and Shift+Tab move through them, Delete forgets one. While typing, the best match shows in grey; Tab or → accepts it.
- **`/todo`** tasks with progress, same store as `katana todo` on the command line. `/` filters by progress and includes completed items: `/ 0..10`, `/10..`, `/50..`, `/100`, `/45..85`. `%` still sets progress on the selected task.
- **`/clip`** clipboard history (text and images). Enter pastes into the last focused field; Delete removes one. **Win+Alt+C**
- **`/shot`** and **PrintScreen** — `/sr` region, `/sw` window, `/sf` full browser page (otherwise window). Copy to clipboard **and** save under **Pictures\Katana**. Click the toast to open the file; tray → **Open screenshots folder**.
- **`/rec`** — Record full screen, Record region (drag to select), or Stop recording. **Ctrl+Alt+R** toggles full-screen recording. A corner dot and the tray icon show while it is recording; click the dot to stop. A thin red border sits just outside the recorded region (along the monitor edge for full screen). The border is click-through, and both it and the dot are left out of the video. Video is H.264 MP4 at 30 fps, saved to **Videos\Katana** as `Katana-YYYYMMDD-HHMMSS.mp4`. The toast and the next two rows offer Open file and Open folder. System audio and the microphone stay off unless you turn them on.
- **`/f`** Explorer-style details table: name, date modified, type, size. **Ctrl+C** copies the path. Empty `/f` lists **Documents, Downloads, Desktop, Pictures, Videos, Music, Home**. Type to search; those folders also match by name. Keywords: `documents`, `downloads`, `desktop`, `pictures`, `videos`, `music`, `home`, `dl`. Every file under an indexed folder is searchable, including `.exe` and build output such as `target\release`. Extensions are not filtered. Directories that are skipped are `index_skip_dirs` in `%APPDATA%\Katana\config.toml` (defaults include `node_modules` and `.git`, not `target` or `dist`). `index_dotfiles` defaults to true. `index_max_depth` defaults to 24. An explicit `index_skip_dirs = []` walks every directory name.
- Tray → **Keep screen awake** (checked / highlighted when on) · Shortcuts / Todos / Settings. Or `/awake` in the prompt.

Special commands start with **`/`**. Short forms: `/t` todo, `/c` clipboard, `/s` screenshot (`/sr` `/sw` `/sf`), `/k` launch (apps/shortcuts/cmd), `/f` files. Everything else is search. `>` still runs a shell line immediately.

**Alt+Space** opens Katana. **Win+Alt+C** clipboard. **Win+Alt+Space** files. **Ctrl+Alt+R** records the screen. **Tab** completes keywords; on an empty prompt with history, Tab walks recents, and with no history it recalls the last keyword. **Esc** steps back one level (e.g. `/clip foo` → `/clip` → home); **Esc** on home hides. Home lists recents, then `/todo`, `/clip`, `/shot`, `/rec`, `/awake`, `/settings` — not `/k` or `/f`. Todo: `+` add, Enter edit, `%` progress, `/` progress filter (completed included), **Alt+V** done. `/todo showall` lists every status. `katana todo /100` is the same filter on the command line.

Recording keys in `%APPDATA%\Katana\config.toml` (hotkeys are read at startup, same as the others):

```toml
rec_hotkey = "ctrl+alt+r"   # toggle full screen / stop
rec_fps = 30                # 1–60
rec_audio_system = false    # WASAPI loopback
rec_audio_mic = false       # default microphone
rec_output_dir = ""         # empty → %USERPROFILE%\Videos\Katana
```

## Download

**[Katana 0.3.8 for Windows](https://github.com/rajmenon/katana/releases/latest)** — `Katana-0.3.8-windows-x86_64.exe` (~2.8 MB). Run it; it stays in the tray. **Alt+Space** opens the prompt.

## Run

```
katana              # tray + hotkey
katana todo add "…" # same todo store as the prompt
```

From source: `cargo build --release -p katana`

Config: `%APPDATA%\Katana`.

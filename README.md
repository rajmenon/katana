# Katana

<p align="center">
  <img src="icon.jpg" width="720" alt="Katana">
</p>

<p align="center"><strong>A Japanese swiss knife for Windows.</strong><br>
Fast. Small. Many-talented.</p>

Katana lives in the tray. Press **Alt+Space** and it is in your hands — one thin prompt, many blades. No webview, one process, a couple of megabytes. **Win+Alt+C** opens clipboard history; **Win+Alt+Space** opens file search.

## Blades

- **Search** files and apps as you type. Wildcards `*.pdf`, `inv?ice.*` · regex `re:inv.*` or `/inv.*pdf/`
- **Launch** (`/k`, also `/a` `/cmd` `/b`) — apps, your keywords (`g rust`, `gh`, `docs`), browser bookmarks, and **Run command** for whatever you typed. `$P$` is whatever you type after a keyword.
- **`/todo`** tasks with progress, same store as `katana todo` on the command line
- **`/clip`** clipboard history (text and images). Enter pastes into the last focused field; Delete removes one. **Win+Alt+C**
- **`/shot`** and **PrintScreen** — `/sr` region, `/sw` window, `/sf` full browser page (otherwise window). Copy to clipboard **and** save under **Pictures\Katana**. Click the toast to open the file; tray → **Open screenshots folder**.
- **`/f`** Explorer-style details table. Empty `/f` lists **Documents, Downloads, Desktop, Pictures, Videos, Music, Home**. Type to search; those folders also match by name. Keywords: `documents`, `downloads`, `desktop`, `pictures`, `videos`, `music`, `home`, `dl`.
- Tray → **Keep screen awake** (checked / highlighted when on) · Shortcuts / Todos / Settings. Or `/awake` in the prompt.

Special commands start with **`/`**. Short forms: `/t` todo, `/c` clipboard, `/s` screenshot (`/sr` `/sw` `/sf`), `/k` launch (apps/shortcuts/cmd), `/f` files. Everything else is search. `>` still runs a shell line immediately.

**Alt+Space** opens Katana. **Win+Alt+C** clipboard. **Win+Alt+Space** files. **Esc** steps back one level (e.g. `/clip foo` → `/clip` → home); **Esc** on home hides. Todo: `+` add, Enter edit, `%` progress, **Alt+V** done. `/todo showall` includes completed.

## Download

**[Katana 0.2.0 for Windows](https://github.com/rajmenon/katana/releases/latest)** — `Katana-0.2.0-windows-x86_64.exe` (~2.8 MB). Run it; it stays in the tray. **Alt+Space** opens the prompt.

## Run

```
katana              # tray + hotkey
katana todo add "…" # same todo store as the prompt
```

From source: `cargo build --release -p katana`

Config: `%APPDATA%\Katana`.

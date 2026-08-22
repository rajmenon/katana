# Katana

<p align="center">
  <img src="icon.jpg" width="720" alt="Katana">
</p>

<p align="center"><strong>A Japanese swiss knife for Windows.</strong><br>
Fast. Small. Many-talented.</p>

Katana lives in the tray. Press **Alt+Space** and it is in your hands — one thin prompt, many blades. No webview, one process, a couple of megabytes.

## Blades

- **Search** files and apps as you type. Wildcards `*.pdf`, `inv?ice.*` · regex `re:inv.*` or `/inv.*pdf/`
- **Shortcuts** — your keywords (`g rust`, `gh`, `docs`) plus browser bookmarks (Chrome / Edge / Firefox / Favorites). `/k` or `/b`. `$P$` is whatever you type after a keyword.
- **`/todo`** tasks with progress, same store as `katana todo` on the command line
- **`/cmd`** run a command
- **`/clip`** clipboard history (text and images). Enter pastes back; Delete removes one
- **`/shot`** and **PrintScreen** — region, window, screen, last. Files land in **Pictures\Katana**
- **`/f`** file columns (name / type / size / icon) · **`/apps`** application shortcuts
- Tray → Shortcuts / Todos / Settings

Special commands start with **`/`**. Short forms: `/t` todo, `/c` clipboard, `/s` screenshot, `/k` `/b` shortcuts, `/a` apps, `/f` files. Everything else is search.

**Alt+Space** opens Katana. **Esc** in a tool returns home; **Esc** on home hides. Todo: `+` add, Enter edit, `%` progress, **Alt+V** done. `/todo showall` includes completed.

## Download

**[Katana 0.1.0 for Windows](https://github.com/rajmenon/katana/releases/latest)** — `Katana-0.1.0-windows-x86_64.exe` (~2.4 MB). Run it; it stays in the tray. **Alt+Space** opens the prompt.

## Run

```
katana              # tray + hotkey
katana todo add "…" # same todo store as the prompt
```

From source: `cargo build --release -p katana`

Config: `%APPDATA%\Katana`.
